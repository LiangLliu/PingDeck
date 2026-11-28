//! Live event stream of the open chat (architecture §12 client rules): the snapshot is read and
//! the subscription opened in one background step (from the snapshot's live position, so nothing
//! is missed or applied twice), sequence gaps resync from a fresh snapshot without resubscribing,
//! and a lost subscription retries after 1 s, 3 s and 9 s, then marks the stream degraded and
//! retries every 60 s until it is back.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{Context, FollowMode};
use spanbay_desktop::core::agent::attention::AgentAttention;
use spanbay_desktop::core::agent::dto::AgentSubscribeTarget;
use spanbay_desktop::core::agent::model::{AgentEnvelope, AgentRole, AgentSessionDetail};
use spanbay_desktop::core::app::{
    AgentPageRequest, agent_attention_subscribe_impl, agent_run_subscribe_impl,
    agent_run_unsubscribe_impl, agent_session_dismiss_notice_impl, agent_session_get_impl,
};
use spanbay_desktop::core::model::events::{EventSink, Sequenced};
use spanbay_desktop::core::model::hub::HubPart;
use spanbay_desktop::state::{BatchEffects, ChatThreadState};
use spanbay_desktop::streams::{
    AGENT_RESUBSCRIBE_BACKOFF, AGENT_RESUBSCRIBE_DELAYS, StreamReceiver, channel_sink,
    coalesced_agent_sink,
};

use crate::bridge::{runtime, spawn_blocking_into, subscribe_stream_into};
use crate::ui::Tone;

use super::metrics::extract_inline_thinking;
use super::session_sync::load_session_extras;
use super::{ChatView, reasoning};

impl ChatView {
    /// Subscribes to the global [`AgentAttention`] feed so sidebar status pills stay synchronized
    /// in real time across all concurrent chats.
    pub(super) fn subscribe_attention(&mut self, cx: &mut Context<Self>) {
        if self.attention_task.is_some() {
            return;
        }
        let rt = runtime(cx);
        let (sink, rx) = channel_sink::<Sequenced<AgentAttention>>();
        if let Ok(sub_id) = agent_attention_subscribe_impl(rt.state(), None, sink) {
            self.attention_sub_id = Some(sub_id);
            self.attention_task = Some(subscribe_stream_into(cx, rx, 16, |this, batch, cx| {
                if !batch.is_empty() {
                    this.load_board(cx);
                }
            }));
        }
    }

    pub(super) fn start_elapsed_ticker(&mut self, cx: &mut Context<Self>) {
        if self.tick_task.is_some() {
            return;
        }
        self.tick_task = Some(cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep_going = weak
                    .update(cx, |this, cx| {
                        let active = this.is_run_active();
                        if !active {
                            this.tick_task = None;
                        }
                        cx.notify();
                        active
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    /// Subscribes the open chat unless its stream is already running.
    pub(super) fn ensure_subscribed(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.subscribed_session_id.as_deref() == Some(session_id) && self.stream_task.is_some() {
            return;
        }
        self.snapshot_and_subscribe(cx);
    }

    /// Drops the current subscription (if any) and its stream task, invalidating in-flight
    /// snapshot/resync callbacks by bumping `stream_epoch`.
    pub(super) fn drop_subscription(&mut self, cx: &mut Context<Self>) {
        self.stream_epoch = self.stream_epoch.wrapping_add(1);
        if let Some(old) = self.active_sub_id.take() {
            agent_run_unsubscribe_impl(runtime(cx).state(), old);
        }
        self.active_stream_sink = None;
        self.stream_task = None;
        self.resubscribe_task = None;
        self.subscribed_session_id = None;
    }
    /// Reads the snapshot and subscribes from its live position off the main thread, then applies
    /// the snapshot and starts consuming the buffered stream.
    pub(super) fn snapshot_and_subscribe(&mut self, cx: &mut Context<Self>) {
        self.snapshot_and_subscribe_page(AgentPageRequest::default(), cx);
    }

    pub(super) fn snapshot_and_subscribe_page(
        &mut self,
        page: AgentPageRequest,
        cx: &mut Context<Self>,
    ) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        self.drop_subscription(cx);
        let epoch = self.stream_epoch;
        let (sink, rx) = coalesced_agent_sink(runtime(cx).tokio_handle());
        let sink_for_sub: Arc<dyn EventSink<AgentEnvelope>> = sink.clone();
        let target = AgentSubscribeTarget::Session {
            session_id: sid.clone(),
        };
        // A chat opened without its session row loads the side panels in the same step.
        let first_load = self.session.is_none();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let detail = agent_session_get_impl(state, &sid, page);
                let sub = detail.as_ref().ok().map(|d| {
                    let from = ChatThreadState::subscribe_from(d.live.as_ref());
                    agent_run_subscribe_impl(state, &target, from, sink_for_sub)
                });
                let extras = detail
                    .as_ref()
                    .ok()
                    .filter(|_| first_load)
                    .map(|d| load_session_extras(state, &d.session));
                (detail, sub, extras)
            },
            move |this, (detail, sub, extras), cx| {
                let current = this.stream_epoch == epoch;
                match (detail, sub) {
                    (Ok(detail), Some(Ok(sub_id))) if current => {
                        this.active_sub_id = Some(sub_id);
                        this.active_stream_sink = Some(sink);
                        this.subscribed_session_id = Some(this.thread.session_id.clone());
                        this.subscribe_attempts = 0;
                        this.thread.stream_degraded = false;
                        if let Some(extras) = extras {
                            this.apply_session_extras(extras);
                        }
                        this.apply_session_detail(detail, cx);
                        this.start_stream(rx, cx);
                    }
                    (_, Some(Ok(sub_id))) => {
                        agent_run_unsubscribe_impl(runtime(cx).state(), sub_id);
                    }
                    _ if current => this.schedule_resubscribe(cx),
                    _ => {}
                }
            },
        );
        self._tasks.push(task);
    }

    fn start_stream(&mut self, rx: StreamReceiver<AgentEnvelope>, cx: &mut Context<Self>) {
        let inner = subscribe_stream_into(cx, rx, 32, |this, batch, cx| {
            let fx = this.thread.apply_batch(batch);
            this.handle_effects(fx, cx);
        });
        self.stream_task = Some(cx.spawn(async move |this, cx| {
            inner.await;
            // The receiver closed underneath an open chat: the subscription is gone.
            let _ = this.update(cx, |this, cx| {
                this.active_sub_id = None;
                this.subscribed_session_id = None;
                this.schedule_resubscribe(cx);
            });
        }));
    }

    fn schedule_resubscribe(&mut self, cx: &mut Context<Self>) {
        let delay = if let Some(delay) = AGENT_RESUBSCRIBE_DELAYS.get(self.subscribe_attempts) {
            *delay
        } else {
            self.thread.stream_degraded = true;
            AGENT_RESUBSCRIBE_BACKOFF
        };
        self.subscribe_attempts = self.subscribe_attempts.saturating_add(1);
        let epoch = self.stream_epoch;
        self.resubscribe_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if this.stream_epoch == epoch {
                    this.snapshot_and_subscribe(cx);
                }
            });
        }));
        cx.notify();
    }

    /// A gap in the stream: re-read the snapshot and let `finish_resync` replay the buffer.
    pub(super) fn resync(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        self.thread.begin_resync();
        let epoch = self.stream_epoch;
        let sink = self.active_stream_sink.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let read = || agent_session_get_impl(state, &sid, AgentPageRequest::default());
                // Flush & bypass the 33 ms coalescer around the snapshot read so a coalesced
                // envelope can never straddle `live.seq` and trigger another resync loop.
                match sink.as_deref() {
                    Some(s) => s.with_bypass(read),
                    None => read(),
                }
            },
            move |this, res, cx| {
                if this.stream_epoch != epoch {
                    return;
                }
                match res {
                    Ok(detail) => this.apply_session_detail(detail, cx),
                    Err(_) => this.schedule_resubscribe(cx),
                }
            },
        );
        self._tasks.push(task);
    }

    /// Applies a session snapshot to the view and the thread (history, live run, session facts).
    pub(super) fn apply_session_detail(
        &mut self,
        detail: AgentSessionDetail,
        cx: &mut Context<Self>,
    ) {
        let first_load = self.session.is_none();
        let was_active_run = self
            .thread
            .active_run
            .as_ref()
            .is_some_and(|r| r.status.is_active())
            || !self.thread.streaming_text.is_empty()
            || !self.thread.streaming_reasoning.is_empty()
            || !self.thread.tool_calls.is_empty();
        self.latest_session_run_id = detail
            .runs
            .first()
            .map(|r| r.run_id.clone())
            .or(self.latest_session_run_id.take());
        self.plan_mode = detail.plan_mode.unwrap_or(false);
        self.thread.queue_paused = detail.queue_paused.unwrap_or(false);
        self.thread.todos = detail.session.todos.clone();
        self.thread.context_used_tokens = detail.session.last_context_tokens;
        self.session_costs = detail.cost_by_currency.unwrap_or_default();
        // Merge instead of overwriting when earlier pages are already loaded so reconnects or
        // resyncs keep audit metadata for earlier turns.
        if first_load || !self.loaded_earlier_once {
            self.audit_tool_calls = detail.tool_calls;
        } else {
            self.merge_audit_tool_calls(detail.tool_calls);
        }
        self.turn_inputs_version = self.turn_inputs_version.wrapping_add(1);
        let earliest_before = self.thread.messages.first().map(|m| m.seq);
        let earliest_now = detail.messages.first().map(|m| m.seq);
        if self.has_later_messages && !detail.has_more_after {
            // Switching from a mid-history search window back to the newest page: drop the
            // older slice so `merge_latest_snapshot` never leaves an unloaded gap in between.
            self.thread.messages.clear();
            self.loaded_earlier_once = false;
        }
        if first_load
            || earliest_before.is_none()
            || earliest_before == earliest_now
            || (self.has_later_messages && !detail.has_more_after)
        {
            self.has_earlier_messages = detail.has_more;
        }
        self.has_later_messages = detail.has_more_after;
        self.loading_earlier_messages = false;
        self.loading_later_messages = false;
        self.load_later_failed = false;
        if detail.has_more_after {
            self.list_state.set_follow_mode(FollowMode::Normal);
        }
        if first_load {
            self.pending_instructions_sync =
                Some(detail.session.instructions.clone().unwrap_or_default());
        }
        // Populate the live run's full summary BEFORE `finish_resync` so `hydrate_live` keeps its
        // metadata (started_at_ms, model_label, iterations) and any buffered `Done`/`Error`
        // replayed inside `finish_resync` transitions it cleanly.
        if let Some(live) = &detail.live
            && let Some(run) = detail
                .runs
                .iter()
                .find(|r| r.run_id == live.run_id)
                .or_else(|| detail.runs.first())
        {
            self.thread.active_run = Some(run.clone());
        }
        let fx = self.thread.finish_resync(
            detail.session.title.clone(),
            detail.messages,
            detail.live.as_ref(),
        );
        match &detail.live {
            Some(_) => {
                self.submitting = false;
                self.run_started_at.get_or_insert_with(Instant::now);
                self.start_elapsed_ticker(cx);
            }
            // Nothing runs live: show the latest persisted run and clean up if we missed `Done`
            // while disconnected or during an overflow.
            None => {
                if !fx.finished
                    && let Some(run) = detail.runs.first()
                {
                    self.thread.active_run = Some(run.clone());
                }
                let missed_done = was_active_run
                    || (self.submitting
                        && detail.runs.first().is_some_and(|r| !r.status.is_active()));
                if missed_done && !fx.finished {
                    self.submitting = false;
                    self.pending_user_text = None;
                    self.run_started_at = None;
                    self.tick_task = None;
                    self.thinking_started_at = None;
                    self.thinking_ended_at = None;
                    self.last_approval_notice = None;
                    self.refresh_after_turn(cx);
                }
            }
        }
        if detail.session.title_locked {
            self.title_generated_sessions
                .insert(detail.session.id.clone());
        }
        self.session = Some(detail.session);
        self.turn_inputs_version = self.turn_inputs_version.wrapping_add(1);
        self.handle_effects(fx, cx);
        cx.notify();
    }

    /// Turns what a batch (or a resync replay) changed into view effects: timers, pending bubble,
    /// reasoning auto-expand, titles, end-of-turn refresh, and another snapshot on a gap.
    pub(super) fn handle_effects(&mut self, fx: BatchEffects, cx: &mut Context<Self>) {
        if let Some(run_id) = fx.run_started {
            self.submitting = false;
            self.plan_revising = false;
            self.last_approval_notice = None;
            self.latest_session_run_id = Some(run_id);
            self.run_started_at.get_or_insert_with(Instant::now);
            self.thinking_started_at = None;
            self.thinking_ended_at = None;
            self.start_elapsed_ticker(cx);
        }
        if !self.thread.pending_approvals.is_empty() || !self.thread.open_gates.is_empty() {
            self.last_approval_notice = None;
        }
        if self.thread.open_gates.is_empty() {
            self.plan_revising = false;
        }
        if fx.user_message {
            self.pending_user_text = None;
        }
        self.track_thinking();
        let mut mid_page_resync = false;
        if self.has_later_messages && (fx.user_message || !fx.saved_assistant_ids.is_empty()) {
            // A new message landed at the tail while viewing a mid-history window: switch to the
            // newest page and resync without swallowing `fx.title` or `fx.finished`.
            self.thread.messages.clear();
            self.thread.messages_version = self.thread.messages_version.wrapping_add(1);
            self.turn_inputs_version = self.turn_inputs_version.wrapping_add(1);
            self.has_later_messages = false;
            self.load_later_failed = false;
            self.loaded_earlier_once = false;
            self.list_state.set_follow_mode(FollowMode::Tail);
            mid_page_resync = true;
        }
        for id in fx.saved_assistant_ids {
            if let Some(started) = self.thinking_started_at.take() {
                let ended = self.thinking_ended_at.take().unwrap_or_else(Instant::now);
                let ms = ended.saturating_duration_since(started).as_millis() as u64;
                if ms > 0 {
                    self.thinking_durations.insert(id.clone(), ms);
                    self.turn_inputs_version = self.turn_inputs_version.wrapping_add(1);
                }
            }
            let plaintext = self.thread.messages.iter().any(|m| {
                m.id == id
                    && reasoning::extract_message_reasoning(m, None).0.kind
                        == reasoning::ReasoningKind::Plaintext
            });
            if plaintext && !self.collapsed_live_reasoning {
                self.expanded_reasoning.insert(format!("reason-{id}"));
            }
        }
        if let Some((title_sid, title)) = fx.title {
            self.apply_session_title_locally(&title_sid, &title, false);
            if self.title_generated_sessions.insert(title_sid.clone()) {
                self.spawn_auto_title_upgrade(title_sid, Some(title), cx);
            }
            if !fx.finished {
                self.refresh_catalog(cx);
            }
        }
        if fx.finished {
            self.submitting = false;
            self.run_started_at = None;
            self.tick_task = None;
            self.pending_user_text = None;
            self.thinking_started_at = None;
            self.thinking_ended_at = None;
            self.last_approval_notice = None;
            self.refresh_after_turn(cx);
        }
        if fx.needs_resync || mid_page_resync {
            self.resync(cx);
        }
        if fx.changed || mid_page_resync {
            cx.notify();
        }
    }

    /// Thinking timer from the live state: starts with reasoning (or an inline `<think>`), ends
    /// with the first visible text or tool call.
    fn track_thinking(&mut self) {
        let t = &self.thread;
        let inline_think =
            t.streaming_text.contains("<think>") || t.streaming_text.contains("<thinking>");
        if self.thinking_started_at.is_none() && (!t.streaming_reasoning.is_empty() || inline_think)
        {
            self.thinking_started_at = Some(Instant::now());
        }
        if self.thinking_started_at.is_some() && self.thinking_ended_at.is_none() {
            let visible = !extract_inline_thinking(&t.streaming_text, true)
                .1
                .is_empty();
            let has_unsaved_tool_call = !t.tool_calls.is_empty() && {
                let saved_ids: HashSet<&str> = t
                    .messages
                    .iter()
                    .filter(|m| m.role == AgentRole::Assistant)
                    .flat_map(|m| m.parts.iter())
                    .filter_map(|pt| match pt {
                        HubPart::ToolCall { id, .. } => Some(id.as_str()),
                        _ => None,
                    })
                    .collect();
                t.tool_calls
                    .iter()
                    .any(|c| !saved_ids.contains(c.call_id.as_str()))
            };
            if visible || has_unsaved_tool_call {
                self.thinking_ended_at = Some(Instant::now());
            }
        }
    }

    /// Scrolls to the newest row and follows new output again (reloading the newest page first
    /// when the view is sitting on a mid-history search window with `has_later_messages`).
    pub fn jump_to_latest(&mut self, cx: &mut Context<Self>) {
        if self.has_later_messages {
            self.thread.messages.clear();
            self.has_later_messages = false;
            self.load_later_failed = false;
            self.loaded_earlier_once = false;
            self.list_state.set_follow_mode(FollowMode::Tail);
            self.snapshot_and_subscribe(cx);
            return;
        }
        self.list_state.scroll_to_end();
        self.list_state.set_follow_mode(FollowMode::Tail);
        cx.notify();
    }

    pub(super) fn dismiss_session_notice(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.notice = None;
        let sid = session.id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_dismiss_notice_impl(state, &sid),
            |this, res, cx| {
                if let Err(e) = res {
                    this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
        cx.notify();
    }
}
