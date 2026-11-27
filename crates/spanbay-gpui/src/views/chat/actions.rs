//! Chat mutations, optimistic send/queue/auto-model binding, session lifecycle, and approval handlers.

use std::collections::HashMap;
use std::time::Instant;

use gpui_kit::{Context, Window};
use spanbay_desktop::core::agent::approvals::ApprovalAnswer;
use spanbay_desktop::core::agent::dto::AgentRunInput;
use spanbay_desktop::core::agent::model::{
    AgentRole, AgentRunStatus, AgentSessionInput, AgentSessionPatch, AgentTarget, ApprovalDecision,
    ApprovalScope, ReasoningEffort,
};
use spanbay_desktop::core::app::{
    AgentPageRequest, AppState, agent_approval_answer_impl, agent_run_continue_impl,
    agent_run_start_impl, agent_run_stop_impl, agent_run_unsubscribe_impl,
    agent_session_create_impl, agent_session_get_impl, agent_session_update_impl,
    llm_model_list_impl, llm_provider_list_impl,
};
use spanbay_desktop::core::model::settings::ToolApproval;
use spanbay_desktop::state::ChatThreadState;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::reasoning::message_has_reasoning;
use super::types::ChatModal;
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
                    ..Default::default()
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
            crate::ui::tr(cx).chat().act_context_attached(&title),
        ));
        cx.notify();
    }

    pub fn send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() && self.pending_attachments.is_empty() {
            return;
        }
        if self.submitting || self.is_run_active() {
            if !self.thread.session_id.is_empty() {
                self.composer
                    .update(cx, |c, cx| c.set_value("", window, cx));
                self.enqueue_turn(text, cx);
            }
            return;
        }

        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        let attachment_ids: Vec<String> =
            self.pending_attachments.drain(..).map(|a| a.id).collect();
        let skills = std::mem::take(&mut self.selected_skills);

        self.submitting = true;
        self.run_started_at = Some(Instant::now());
        self.start_elapsed_ticker(cx);
        self.thinking_started_at = None;
        self.thinking_ended_at = None;
        self.pending_user_text = Some(text.clone());
        self.collapsed_live_reasoning = false;
        self.thread.last_error = None;
        self.status_msg = None;
        self.stick_to_bottom = true;
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
                        this.dispatch_run(id, text_for_run, attachment_ids, skills, cx);
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
        self.dispatch_run(sid, text, attachment_ids, skills, cx);
    }

    fn dispatch_run(
        &mut self,
        sid: String,
        text: String,
        attachment_ids: Vec<String>,
        skills: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let original_text = text.clone();
        let needs_model = self.session.as_ref().is_none_or(|s| s.target.is_none());
        let check_first_turn = !self.title_generated_sessions.contains(&sid);
        let sid_for_cb = sid.clone();
        let rt_bind = runtime(cx);
        let task = spawn_async_into(
            cx,
            move |state| async move {
                let mut auto_session = None;
                let mut eligible_for_auto_title = false;
                if needs_model || check_first_turn {
                    let sid_pre = sid.clone();
                    if let Ok(Ok((updated, eligible))) = rt_bind
                        .run_state(move |st| {
                            let detail =
                                agent_session_get_impl(st, &sid_pre, AgentPageRequest::default())?;
                            let eligible = check_first_turn
                                && st.settings().ai.agent.auto_title
                                && !detail.session.title_locked
                                && detail.messages.iter().all(|m| m.role != AgentRole::User);
                            let mut updated = None;
                            if needs_model
                                && detail.session.target.is_none()
                                && let Some(target) = resolve_fallback_target(st)
                            {
                                let s = agent_session_update_impl(
                                    st,
                                    &sid_pre,
                                    AgentSessionPatch {
                                        target: Some(target),
                                        ..AgentSessionPatch::default()
                                    },
                                )?;
                                updated = Some(s);
                            }
                            Ok::<_, spanbay_desktop::core::error::AppError>((updated, eligible))
                        })
                        .await
                    {
                        auto_session = updated;
                        eligible_for_auto_title = eligible;
                    }
                }
                let sid_post = sid.clone();
                let input = AgentRunInput {
                    session_id: sid,
                    text,
                    attachment_ids,
                    skills,
                    workspace_id: None,
                    environment_id: None,
                    contexts: Vec::new(),
                    context_use: Default::default(),
                };
                let res = agent_run_start_impl(&state, input).await;
                let mut provisional_title = None;
                if res.is_ok()
                    && eligible_for_auto_title
                    && let Ok(Ok(after)) = rt_bind
                        .run_state(move |st| {
                            agent_session_get_impl(st, &sid_post, AgentPageRequest::default())
                        })
                        .await
                    && !after.session.title_locked
                {
                    provisional_title = Some(after.session.title);
                }
                (res, auto_session, provisional_title)
            },
            move |this, (res, auto_session, provisional_title), cx| {
                if let Some(updated) = auto_session
                    && this.thread.session_id == updated.id
                {
                    this.session = Some(updated);
                }
                match res {
                    Ok(_) => {
                        if let Some(provisional) = provisional_title
                            && this.title_generated_sessions.insert(sid_for_cb.clone())
                        {
                            this.apply_session_title_locally(&sid_for_cb, &provisional, false);
                            this.spawn_auto_title_upgrade(
                                sid_for_cb.clone(),
                                Some(provisional),
                                cx,
                            );
                        }
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
                        this.auto_scroll_if_following();
                    }
                    Err(err) => {
                        this.submitting = false;
                        this.run_started_at = None;
                        this.tick_task = None;
                        this.pending_user_text = None;
                        this.pending_draft_sync = Some(original_text);
                        let msg = err.message().to_owned();
                        this.thread.last_error = Some(msg.clone());
                        this.status_msg = Some((Tone::Danger, msg));
                        this.auto_scroll_if_following();
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
            self.tick_task = None;
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

    pub fn answer_approval_ext(
        &mut self,
        run_id: String,
        call_id: String,
        decision: ApprovalDecision,
        scope: ApprovalScope,
        allow_network_once: bool,
        cx: &mut Context<Self>,
    ) {
        let answer = ApprovalAnswer {
            decision,
            scope,
            message: None,
            edited_arguments: None,
            allow_network_once,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| agent_approval_answer_impl(state, &run_id, &call_id, answer),
            |this, res, cx| {
                if let Err(err) = res {
                    this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn patch_session(&mut self, patch: AgentSessionPatch, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if !sid.is_empty() {
            self.patch_session_by_id(sid, patch, cx);
        }
    }

    pub fn patch_session_by_id(
        &mut self,
        sid: String,
        patch: AgentSessionPatch,
        cx: &mut Context<Self>,
    ) {
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_update_impl(state, &sid, patch),
            move |this, res, cx| {
                match res {
                    Ok(updated) => {
                        if this.thread.session_id == sid_check {
                            this.thread.title = updated.title.clone();
                            this.session = Some(updated);
                        }
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

    pub fn request_permission_mode(&mut self, mode: ToolApproval, cx: &mut Context<Self>) {
        if mode == ToolApproval::AutoAll {
            self.active_modal = Some(ChatModal::FullAccessConfirm {
                acknowledged: false,
            });
            cx.notify();
        } else {
            self.set_permission_mode(mode, cx);
        }
    }

    pub fn set_permission_mode(&mut self, mode: ToolApproval, cx: &mut Context<Self>) {
        self.patch_session(
            AgentSessionPatch {
                permission_mode: Some(mode),
                ..Default::default()
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
                ..Default::default()
            },
            cx,
        );
    }

    pub fn toggle_web_enabled(&mut self, cx: &mut Context<Self>) {
        let next = !self.session.as_ref().is_some_and(|s| s.web_enabled);
        self.patch_session(
            AgentSessionPatch {
                web_enabled: Some(next),
                ..Default::default()
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
                ..Default::default()
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
        self.status_msg = Some((
            Tone::Success,
            crate::ui::tr(cx).chat().actions_saved_instructions().into(),
        ));
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
}
