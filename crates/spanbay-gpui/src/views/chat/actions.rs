//! Chat mutations, optimistic send/queue/auto-model binding, session lifecycle, and approval handlers.

use std::collections::HashMap;
use std::time::Instant;

use gpui_kit::{Context, FollowMode, Window};
use spanbay_desktop::core::agent::approvals::ApprovalAnswer;
use spanbay_desktop::core::agent::dto::AgentRunInput;
use spanbay_desktop::core::agent::model::{
    AgentRole, AgentRunStatus, AgentSessionInput, AgentSessionPatch, AgentTarget, ApprovalDecision,
    ApprovalScope,
};
use spanbay_desktop::core::app::{
    AgentPageRequest, AppState, agent_approval_answer_impl, agent_plan_set_impl,
    agent_run_continue_impl, agent_run_start_impl, agent_run_stop_impl, agent_session_create_impl,
    agent_session_get_impl, agent_session_update_impl, agent_workspace_info_impl,
    environment_list_impl, llm_model_list_impl, llm_provider_list_impl,
};
use spanbay_desktop::state::{ChatThreadState, RunError, provisional_run_summary};

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::actions_extra::resolve_active_binding;
use super::types::LastApprovalNotice;
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
            let current_text = self
                .pending_draft_sync
                .take()
                .unwrap_or_else(|| self.composer.read(cx).value().to_string());
            self.drafts.set(&prev_sid, current_text);
            runtime(cx).save_chat_drafts(&self.drafts);
            self.reset_session_view(String::new(), None, cx);
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
        if self.editing_queue_id.is_some() {
            self.save_edited_queued_turn(window, cx);
            return;
        }
        let mut text = self.composer.read(cx).value().trim().to_owned();
        if !self.diff_comments.is_empty() {
            let prefix: String = self
                .diff_comments
                .drain(..)
                .map(|c| format!("> @{}:{} — {}", c.path, c.line, c.comment))
                .collect::<Vec<_>>()
                .join("\n");
            text = if text.is_empty() {
                prefix
            } else {
                format!("{prefix}\n\n{text}")
            };
        }
        if text.is_empty() && self.pending_attachments.is_empty() {
            return;
        }
        // A waiting plan or question takes the text as a revision (design §7), not a queued turn.
        if !self.thread.open_gates.is_empty() {
            self.steer_active_run(window, cx);
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
        self.plan_revising = false;
        self.last_approval_notice = None;
        self.run_started_at = Some(Instant::now());
        self.start_elapsed_ticker(cx);
        self.thinking_started_at = None;
        self.thinking_ended_at = None;
        if self.has_later_messages {
            self.thread.messages.clear();
            self.thread.messages_version = self.thread.messages_version.wrapping_add(1);
            self.turn_inputs_version = self.turn_inputs_version.wrapping_add(1);
            self.has_later_messages = false;
            self.load_later_failed = false;
            self.loaded_earlier_once = false;
            self.resync(cx);
        }
        self.pending_user_text = Some(text.clone());
        self.collapsed_live_reasoning = false;
        self.thread.last_error = None;
        self.status_msg = None;
        self.list_state.set_follow_mode(FollowMode::Tail);
        cx.notify();

        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            let text_for_run = text;
            let active_ws = self.active_workspace_id.clone();
            let active_env = self.active_environment_id.clone();
            let want_plan = self.plan_mode;
            let epoch = self.stream_epoch;
            let task = spawn_blocking_into(
                cx,
                move |state| {
                    let target = resolve_fallback_target(state);
                    let (spanbay_workspace_id, spanbay_environment_id) =
                        resolve_active_binding(state, active_ws, active_env);
                    let session = agent_session_create_impl(
                        state,
                        AgentSessionInput {
                            target,
                            spanbay_workspace_id,
                            spanbay_environment_id,
                            ..AgentSessionInput::default()
                        },
                    )?;
                    let envs = session
                        .spanbay_workspace_id
                        .as_deref()
                        .and_then(|wid| environment_list_impl(state, wid.to_owned()).ok())
                        .unwrap_or_default();
                    let ws_info = session
                        .workspace_dir
                        .as_deref()
                        .and_then(|p| agent_workspace_info_impl(state, p).ok());
                    Ok::<_, spanbay_desktop::core::error::AppError>((session, envs, ws_info))
                },
                move |this, res, cx| {
                    if this.stream_epoch != epoch || !this.thread.session_id.is_empty() {
                        return;
                    }
                    match res {
                        Ok((session, envs, ws_info)) => {
                            let id = session.id.clone();
                            this.thread = ChatThreadState::new(id.clone(), session.title.clone());
                            this.environments = envs;
                            this.workspace_info = ws_info;
                            this.session = Some(session);
                            this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                            this.plan_mode = want_plan;
                            this.ensure_subscribed(&id, cx);
                            this.refresh_catalog(cx);
                            this.dispatch_run(
                                id,
                                text_for_run,
                                attachment_ids,
                                skills,
                                want_plan,
                                cx,
                            );
                        }
                        Err(err) => {
                            this.submitting = false;
                            this.run_started_at = None;
                            this.tick_task = None;
                            this.pending_user_text = None;
                            this.pending_draft_sync = Some(text_for_run);
                            let msg = err.message().to_owned();
                            this.thread.last_error = Some(RunError {
                                message: msg.clone(),
                                failure: None,
                            });
                            this.status_msg = Some((Tone::Danger, msg));
                            cx.notify();
                        }
                    }
                },
            );
            self._tasks.push(task);
            return;
        }

        self.drafts.set(&sid, String::new());
        runtime(cx).save_chat_drafts(&self.drafts);
        self.ensure_subscribed(&sid, cx);
        self.dispatch_run(sid, text, attachment_ids, skills, false, cx);
    }

    fn dispatch_run(
        &mut self,
        sid: String,
        text: String,
        attachment_ids: Vec<String>,
        skills: Vec<String>,
        enable_plan: bool,
        cx: &mut Context<Self>,
    ) {
        let original_text = text.clone();
        let needs_model = self.session.as_ref().is_none_or(|s| s.target.is_none());
        let needs_binding = self
            .session
            .as_ref()
            .is_none_or(|s| s.spanbay_workspace_id.is_none());
        let active_ws = needs_binding
            .then(|| self.active_workspace_id.clone())
            .flatten();
        let active_env = needs_binding
            .then(|| self.active_environment_id.clone())
            .flatten();
        let check_first_turn = !self.title_generated_sessions.contains(&sid);
        let sid_for_cb = sid.clone();
        let rt_bind = runtime(cx);
        let task = spawn_async_into(
            cx,
            move |state| async move {
                let mut auto_session = None;
                let mut eligible_for_auto_title = false;
                let mut run_ws_id = None;
                let mut run_env_id = None;
                if needs_model || check_first_turn || active_ws.is_some() {
                    let sid_pre = sid.clone();
                    if let Ok(Ok((updated, eligible, ws_id, env_id))) = rt_bind
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
                            let (ws_id, env_id) = if detail.session.spanbay_workspace_id.is_none() {
                                resolve_active_binding(st, active_ws, active_env)
                            } else {
                                (None, None)
                            };
                            Ok::<_, spanbay_desktop::core::error::AppError>((
                                updated, eligible, ws_id, env_id,
                            ))
                        })
                        .await
                    {
                        auto_session = updated;
                        eligible_for_auto_title = eligible;
                        run_ws_id = ws_id;
                        run_env_id = env_id;
                    }
                }
                if enable_plan {
                    let _ = agent_plan_set_impl(&state, &sid, true).await;
                }
                let sid_post = sid.clone();
                let bound_on_start = run_ws_id.is_some();
                let input = AgentRunInput {
                    session_id: sid,
                    text,
                    attachment_ids,
                    skills,
                    workspace_id: run_ws_id,
                    environment_id: run_env_id,
                    contexts: Vec::new(),
                    context_use: Default::default(),
                };
                let res = agent_run_start_impl(&state, input).await;
                let mut provisional_title = None;
                let mut bound_refresh = None;
                if res.is_ok()
                    && (eligible_for_auto_title || bound_on_start)
                    && let Ok(Ok((after, envs))) = rt_bind
                        .run_state(move |st| {
                            let after =
                                agent_session_get_impl(st, &sid_post, AgentPageRequest::default())?;
                            let envs = if bound_on_start {
                                after
                                    .session
                                    .spanbay_workspace_id
                                    .as_deref()
                                    .and_then(|wid| environment_list_impl(st, wid.to_owned()).ok())
                                    .unwrap_or_default()
                            } else {
                                Vec::new()
                            };
                            Ok::<_, spanbay_desktop::core::error::AppError>((after, envs))
                        })
                        .await
                {
                    if eligible_for_auto_title && !after.session.title_locked {
                        provisional_title = Some(after.session.title.clone());
                    }
                    if bound_on_start {
                        bound_refresh = Some((after.session, envs));
                    }
                }
                (res, auto_session, provisional_title, bound_refresh)
            },
            move |this, (res, auto_session, provisional_title, bound_refresh), cx| {
                if let Some((bound_session, envs)) = bound_refresh
                    && this.thread.session_id == bound_session.id
                {
                    this.session = Some(bound_session);
                    this.environments = envs;
                } else if let Some(updated) = auto_session
                    && this.thread.session_id == updated.id
                {
                    this.session = Some(updated);
                }
                this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                match res {
                    Ok(started) => {
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
                        if this.thread.session_id == sid_for_cb {
                            this.latest_session_run_id = Some(started.run_id.clone());
                            // Seed `active_run` if `RunStarted` has not arrived yet (e.g.
                            // `dispatch_run` beat `ensure_subscribed`) so Stop/Steer/queue work.
                            if this
                                .thread
                                .active_run
                                .as_ref()
                                .is_none_or(|r| r.run_id != started.run_id && !r.status.is_active())
                            {
                                this.thread.active_run = Some(provisional_run_summary(
                                    started.run_id,
                                    sid_for_cb.clone(),
                                    AgentRunStatus::Queued,
                                    this.session.as_ref().and_then(|s| s.target.clone()),
                                    None,
                                    0,
                                ));
                            }
                            this.submitting = false;
                        }
                    }
                    Err(err) => {
                        if this.thread.session_id != sid_for_cb {
                            return;
                        }
                        this.submitting = false;
                        this.run_started_at = None;
                        this.tick_task = None;
                        this.pending_user_text = None;
                        this.pending_draft_sync = Some(original_text);
                        let msg = err.message().to_owned();
                        this.thread.last_error = Some(RunError {
                            message: msg.clone(),
                            failure: None,
                        });
                        this.status_msg = Some((Tone::Danger, msg));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn stop_active_run(&mut self, cx: &mut Context<Self>) {
        let active_run_id = self
            .thread
            .active_run
            .as_ref()
            .filter(|r| r.status.is_active())
            .map(|r| r.run_id.clone());
        let sid = self.thread.session_id.clone();
        if active_run_id.is_none() && sid.is_empty() {
            self.submitting = false;
            self.tick_task = None;
            cx.notify();
            return;
        }
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let id = active_run_id.or_else(|| {
                    state
                        .agent
                        .runs
                        .active_for_session(&sid)
                        .map(|r| r.run_id.clone())
                });
                id.is_some_and(|run_id| agent_run_stop_impl(state, &run_id))
            },
            |_, _, _| {},
        );
        self._tasks.push(task);
    }

    pub fn continue_cost_pause(&mut self, run_id: String, proceed: bool, cx: &mut Context<Self>) {
        let sid_check = self.thread.session_id.clone();
        let run_check = run_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_run_continue_impl(state, &run_id, proceed),
            move |this, _, cx| {
                if this.thread.session_id == sid_check
                    && this
                        .thread
                        .cost_pause
                        .as_ref()
                        .is_some_and(|p| p.run_id == run_check)
                {
                    this.thread.cost_pause = None;
                    cx.notify();
                }
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
        self.send_approval_answer(run_id, call_id, answer, cx);
    }

    pub(super) fn send_approval_answer(
        &mut self,
        run_id: String,
        call_id: String,
        answer: ApprovalAnswer,
        cx: &mut Context<Self>,
    ) {
        let sid_check = self.thread.session_id.clone();
        let rid_done = run_id.clone();
        let cid_done = call_id.clone();
        let notice = LastApprovalNotice {
            call_id: call_id.clone(),
            decision: answer.decision,
            scope: answer.scope,
            reason: answer.message.clone(),
        };
        let task = spawn_blocking_into(
            cx,
            move |state| agent_approval_answer_impl(state, &run_id, &call_id, answer),
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                match res {
                    Ok(true) => {
                        this.last_approval_notice = Some(notice);
                        this.thread
                            .pending_approvals
                            .retain(|a| !(a.run_id == rid_done && a.call_id == cid_done));
                        this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                    }
                    Ok(false) => {
                        this.thread
                            .pending_approvals
                            .retain(|a| !(a.run_id == rid_done && a.call_id == cid_done));
                        this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
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

    /// Banner "Retry": reruns the session's head turn (never an earlier search slice end).
    pub(super) fn retry_last_turn(&mut self, cx: &mut Context<Self>) {
        let target_id = if self.has_later_messages {
            self.session
                .as_ref()
                .and_then(|s| s.head_message_id.clone())
        } else {
            self.thread
                .messages
                .last()
                .map(|m| m.id.clone())
                .or_else(|| {
                    self.session
                        .as_ref()
                        .and_then(|s| s.head_message_id.clone())
                })
        };
        if let Some(id) = target_id {
            self.thread.last_error = None;
            if self.has_later_messages {
                self.thread.messages.clear();
                self.thread.messages_version = self.thread.messages_version.wrapping_add(1);
                self.turn_inputs_version = self.turn_inputs_version.wrapping_add(1);
                self.has_later_messages = false;
                self.load_later_failed = false;
                self.loaded_earlier_once = false;
                self.list_state.set_follow_mode(FollowMode::Tail);
                self.resync(cx);
            }
            self.retry_message(id, cx);
        }
    }
}
