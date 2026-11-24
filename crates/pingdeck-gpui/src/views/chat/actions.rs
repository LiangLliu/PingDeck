//! Chat mutations, optimistic send/auto-model binding, session lifecycle, and approval handlers.

use std::collections::HashMap;
use std::time::Instant;

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::agent::approvals::ApprovalAnswer;
use pingdeck_desktop::core::agent::dto::AgentRunInput;
use pingdeck_desktop::core::agent::model::{
    AgentRunStatus, AgentSessionInput, AgentSessionPatch, AgentTarget, ApprovalDecision,
    ApprovalScope, ReasoningEffort,
};
use pingdeck_desktop::core::app::{
    AgentPageRequest, AppState, agent_approval_answer_impl, agent_run_continue_impl,
    agent_run_start_impl, agent_run_stop_impl, agent_run_unsubscribe_impl,
    agent_session_create_impl, agent_session_delete_impl, agent_session_get_impl,
    agent_session_update_impl, llm_model_list_impl, llm_provider_list_impl,
};
use pingdeck_desktop::core::model::settings::ToolApproval;
use pingdeck_desktop::state::ChatThreadState;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::reasoning::message_has_reasoning;
use super::{ChatView, ModelOption, ProviderChatMeta};

pub(super) fn load_model_options(
    state: &AppState,
) -> (Vec<ModelOption>, HashMap<String, ProviderChatMeta>) {
    let providers = llm_provider_list_impl(state).unwrap_or_default();
    let models = llm_model_list_impl(state, None).unwrap_or_default();
    let mut meta = HashMap::with_capacity(providers.len());
    for p in &providers {
        meta.insert(
            p.id.clone(),
            ProviderChatMeta {
                name: p.name.clone(),
                primary_protocol: p.primary_protocol,
                preset_id: p.preset_id.clone(),
                reasoning_request: p.quirks.reasoning_request,
            },
        );
    }
    let mut opts = Vec::with_capacity(models.len());
    for m in models {
        let provider = providers.iter().find(|p| p.id == m.provider_id);
        let pname = provider.map_or_else(|| m.provider_id.clone(), |p| p.name.clone());
        let enabled = m.enabled && provider.is_none_or(|p| p.enabled);
        opts.push(ModelOption {
            provider_id: m.provider_id,
            provider_name: pname,
            model_id: m.model_id,
            enabled,
        });
    }
    // Prefer enabled models at the front so auto-fallback picks a callable model first.
    opts.sort_by_key(|m| !m.enabled);
    (opts, meta)
}

pub(super) fn resolve_fallback_target(state: &AppState) -> Option<AgentTarget> {
    if let Some(target) = state.settings().ai.agent.default_target.clone() {
        return Some(target);
    }
    let (opts, _) = load_model_options(state);
    opts.iter()
        .find(|m| m.enabled)
        .or_else(|| opts.first())
        .map(ModelOption::target)
}

impl ChatView {
    pub fn create_session(&mut self, cx: &mut Context<Self>) {
        self.create_session_with_target(None, cx);
    }

    pub fn select_model(&mut self, target: AgentTarget, cx: &mut Context<Self>) {
        if self.thread.session_id.is_empty() {
            self.create_session_with_target(Some(target), cx);
        } else {
            self.patch_session(
                AgentSessionPatch {
                    target: Some(target),
                    ..AgentSessionPatch::default()
                },
                cx,
            );
        }
    }

    fn create_session_with_target(
        &mut self,
        explicit: Option<AgentTarget>,
        cx: &mut Context<Self>,
    ) {
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let target = explicit.or_else(|| resolve_fallback_target(state));
                agent_session_create_impl(
                    state,
                    AgentSessionInput {
                        target,
                        ..AgentSessionInput::default()
                    },
                )
            },
            |this, res, cx| {
                match res {
                    Ok(session) => {
                        let id = session.id.clone();
                        this.refresh_catalog(cx);
                        this.open_session(id, cx);
                    }
                    Err(err) => {
                        let msg = err.message().to_owned();
                        this.thread.last_error = Some(msg.clone());
                        this.status_msg = Some((Tone::Danger, msg));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn apply_starter(
        &mut self,
        text: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer
            .update(cx, |c, cx| c.set_value(text, window, cx));
        cx.notify();
    }

    /// Populates a Chat draft with sanitized handoff context from Requests / History / Usage
    /// without auto-sending (`docs/design.md §1.3`).
    pub fn accept_handoff_draft(
        &mut self,
        title: String,
        context_markdown: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prev_sid = self.thread.session_id.clone();
        if !prev_sid.is_empty() && (!self.thread.messages.is_empty() || self.is_run_active()) {
            let current_text = self.composer.read(cx).value().to_string();
            self.drafts.set(&prev_sid, current_text);
            runtime(cx).save_chat_drafts(&self.drafts);
            if let Some(sub_id) = self.active_sub_id.take() {
                agent_run_unsubscribe_impl(runtime(cx).state(), sub_id);
            }
            self.subscribed_session_id = None;
            self.stream_task = None;
            self.thread = ChatThreadState::default();
            self.session = None;
            self.audit_tool_calls.clear();
            self.submitting = false;
            self.pending_user_text = None;
        }
        self.composer
            .update(cx, |c, cx| c.set_value(context_markdown, window, cx));
        self.status_msg = Some((
            Tone::Accent,
            format!("已带入上下文：{title}（请确认后发送）"),
        ));
        cx.notify();
    }

    pub fn send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submitting || self.is_run_active() {
            return;
        }
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }

        // Immediate optimistic UI response (0ms feedback: user bubble + live Thinking state).
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        self.submitting = true;
        self.run_started_at = Some(Instant::now());
        self.thinking_started_at = None;
        self.thinking_ended_at = None;
        self.pending_user_text = Some(text.clone());
        self.collapsed_live_reasoning = false;
        self.thread.last_error = None;
        self.status_msg = None;
        self.scroll_handle.scroll_to_bottom();
        cx.notify();

        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            let text_for_run = text;
            let task = spawn_blocking_into(
                cx,
                |state| {
                    let target = resolve_fallback_target(state);
                    agent_session_create_impl(
                        state,
                        AgentSessionInput {
                            target,
                            ..AgentSessionInput::default()
                        },
                    )
                },
                move |this, res, cx| match res {
                    Ok(session) => {
                        let id = session.id.clone();
                        this.thread = ChatThreadState::new(id.clone(), session.title.clone());
                        this.session = Some(session);
                        this.ensure_subscribed(&id, cx);
                        this.refresh_catalog(cx);
                        this.dispatch_run(id, text_for_run, cx);
                    }
                    Err(err) => {
                        this.submitting = false;
                        this.run_started_at = None;
                        this.pending_user_text = None;
                        this.pending_draft_sync = Some(text_for_run);
                        let msg = err.message().to_owned();
                        this.thread.last_error = Some(msg.clone());
                        this.status_msg = Some((Tone::Danger, msg));
                        cx.notify();
                    }
                },
            );
            self._tasks.push(task);
            return;
        }

        self.drafts.set(&sid, String::new());
        runtime(cx).save_chat_drafts(&self.drafts);
        self.ensure_subscribed(&sid, cx);
        self.dispatch_run(sid, text, cx);
    }

    fn dispatch_run(&mut self, sid: String, text: String, cx: &mut Context<Self>) {
        let original_text = text.clone();
        let needs_model = self.session.as_ref().is_none_or(|s| s.target.is_none());
        let sid_for_cb = sid.clone();
        let rt_bind = runtime(cx);
        let task = spawn_async_into(
            cx,
            move |state| async move {
                let mut auto_session = None;
                if needs_model {
                    let sid_bind = sid.clone();
                    if let Ok(Ok(Some(updated))) = rt_bind
                        .run_state(move |st| {
                            let detail =
                                agent_session_get_impl(st, &sid_bind, AgentPageRequest::default())?;
                            if detail.session.target.is_none()
                                && let Some(target) = resolve_fallback_target(st)
                            {
                                let s = agent_session_update_impl(
                                    st,
                                    &sid_bind,
                                    AgentSessionPatch {
                                        target: Some(target),
                                        ..AgentSessionPatch::default()
                                    },
                                )?;
                                return Ok::<_, pingdeck_desktop::core::error::AppError>(Some(s));
                            }
                            Ok(None)
                        })
                        .await
                    {
                        auto_session = Some(updated);
                    }
                }
                let input = AgentRunInput {
                    session_id: sid,
                    text,
                    attachment_ids: Vec::new(),
                    skills: Vec::new(),
                    workspace_id: None,
                    environment_id: None,
                    contexts: Vec::new(),
                    context_use: Default::default(),
                };
                let res = agent_run_start_impl(&state, input).await;
                (res, auto_session)
            },
            move |this, (res, auto_session), cx| {
                if let Some(updated) = auto_session
                    && this.thread.session_id == updated.id
                {
                    this.session = Some(updated);
                }
                match res {
                    Ok(_) => {
                        if this.thread.session_id == sid_for_cb
                            && this.thread.active_run.as_ref().is_some_and(|r| {
                                matches!(
                                    r.status,
                                    AgentRunStatus::Queued
                                        | AgentRunStatus::Running
                                        | AgentRunStatus::WaitingApproval
                                )
                            })
                        {
                            this.submitting = false;
                        }
                        this.scroll_handle.scroll_to_bottom();
                    }
                    Err(err) => {
                        this.submitting = false;
                        this.run_started_at = None;
                        this.pending_user_text = None;
                        this.pending_draft_sync = Some(original_text);
                        let msg = err.message().to_owned();
                        this.thread.last_error = Some(msg.clone());
                        this.status_msg = Some((Tone::Danger, msg));
                        this.scroll_handle.scroll_to_bottom();
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn stop_active_run(&mut self, cx: &mut Context<Self>) {
        let Some(run) = &self.thread.active_run else {
            self.submitting = false;
            cx.notify();
            return;
        };
        let run_id = run.run_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_run_stop_impl(state, &run_id),
            |_, _, _| {},
        );
        self._tasks.push(task);
    }

    pub fn continue_cost_pause(&mut self, run_id: String, proceed: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_run_continue_impl(state, &run_id, proceed),
            |this, _, cx| {
                this.thread.cost_pause = None;
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn answer_approval(
        &mut self,
        run_id: String,
        call_id: String,
        decision: ApprovalDecision,
        scope: ApprovalScope,
        cx: &mut Context<Self>,
    ) {
        let answer = ApprovalAnswer {
            decision,
            scope,
            message: None,
            edited_arguments: None,
            allow_network_once: false,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| agent_approval_answer_impl(state, &run_id, &call_id, answer),
            |_, _, _| {},
        );
        self._tasks.push(task);
    }

    pub fn patch_session(&mut self, patch: AgentSessionPatch, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_update_impl(state, &sid, patch),
            |this, res, cx| {
                match res {
                    Ok(updated) => {
                        this.thread.title = updated.title.clone();
                        this.session = Some(updated);
                        this.refresh_catalog(cx);
                    }
                    Err(err) => {
                        this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn set_permission_mode(&mut self, mode: ToolApproval, cx: &mut Context<Self>) {
        self.patch_session(
            AgentSessionPatch {
                permission_mode: Some(mode),
                ..AgentSessionPatch::default()
            },
            cx,
        );
    }

    pub fn set_reasoning_effort(
        &mut self,
        effort: Option<ReasoningEffort>,
        cx: &mut Context<Self>,
    ) {
        self.patch_session(
            AgentSessionPatch {
                reasoning_effort: Some(effort),
                ..AgentSessionPatch::default()
            },
            cx,
        );
    }

    pub fn toggle_web_enabled(&mut self, cx: &mut Context<Self>) {
        let next = !self.session.as_ref().is_some_and(|s| s.web_enabled);
        self.patch_session(
            AgentSessionPatch {
                web_enabled: Some(next),
                ..AgentSessionPatch::default()
            },
            cx,
        );
    }

    pub fn toggle_pinned(&mut self, cx: &mut Context<Self>) {
        let next = !self
            .session
            .as_ref()
            .is_some_and(|s| s.pinned_at_ms.is_some());
        self.patch_session(
            AgentSessionPatch {
                pinned: Some(next),
                ..AgentSessionPatch::default()
            },
            cx,
        );
    }

    pub fn save_instructions(&mut self, cx: &mut Context<Self>) {
        let text = self.instructions_input.read(cx).value().trim().to_owned();
        self.patch_session(
            AgentSessionPatch {
                instructions: Some(text),
                ..AgentSessionPatch::default()
            },
            cx,
        );
        self.status_msg = Some((Tone::Success, "Saved instructions".into()));
        cx.notify();
    }

    pub fn toggle_trace(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.expanded_traces.remove(&key) {
            self.expanded_traces.insert(key);
        }
        cx.notify();
    }

    pub fn toggle_reasoning(&mut self, key: String, cx: &mut Context<Self>) {
        if key == "live-reasoning" {
            self.collapsed_live_reasoning = !self.collapsed_live_reasoning;
        } else if !self.expanded_reasoning.remove(&key) {
            self.expanded_reasoning.insert(key);
        }
        cx.notify();
    }

    pub fn toggle_all_reasoning(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<String> = self
            .thread
            .messages
            .iter()
            .filter(|m| message_has_reasoning(m))
            .map(|m| format!("reason-{}", m.id))
            .collect();
        if keys.is_empty() {
            return;
        }
        let all_open = keys.iter().all(|k| self.expanded_reasoning.contains(k));
        if all_open {
            for k in keys {
                self.expanded_reasoning.remove(&k);
            }
        } else {
            for k in keys {
                self.expanded_reasoning.insert(k);
            }
        }
        cx.notify();
    }

    pub fn delete_current_session(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_delete_impl(state, &sid),
            |this, res, cx| {
                if res.is_ok() {
                    if let Some(sub_id) = this.active_sub_id.take() {
                        agent_run_unsubscribe_impl(runtime(cx).state(), sub_id);
                    }
                    this.subscribed_session_id = None;
                    this.thread = ChatThreadState::default();
                    this.session = None;
                    this.audit_tool_calls.clear();
                    this.stream_task = None;
                    this.submitting = false;
                    this.pending_user_text = None;
                    this.reload_sessions(cx);
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}
