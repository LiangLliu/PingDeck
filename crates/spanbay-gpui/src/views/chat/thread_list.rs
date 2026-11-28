//! Virtualized conversation thread for AI Chat: a gpui `list` over turn-grouped entries
//! ([`ThreadTurnEntry`]), the pending user bubble, inline notices, stop marker, and run error
//! banner, with O(n) version-cached turn grouping, tail-following, Jump to latest, and "Load earlier".

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, Context, Div, FollowMode, IntoElement, ListOffset, ParentElement as _,
    SharedString, Styled as _, div, list, px,
};
use spanbay_desktop::core::agent::model::AgentFailureKind;
use spanbay_desktop::state::{ThreadTurnEntry, TurnViewCache};
use spanbay_desktop::streams::AGENT_RESUBSCRIBE_BACKOFF;

use crate::ui::{
    Disableable as _, ScrollableElement as _, Tone, UiIcon, alert_banner, btn_ghost, btn_icon,
    btn_secondary, h_row, labeled_divider, mono_font_family, palette, to_hsla, v_col,
};

use super::ChatView;
use super::context_meter::render_inline_notices;
use super::run_actions::{render_stop_marker, stop_marker};
use super::thread::{render_assistant_turn, render_pending_user_bubble, render_user_message};
use super::welcome::render_welcome_empty;

thread_local! {
    /// Foreground-thread turn-grouping cache avoiding per-frame recomputation.
    static TURN_VIEW_CACHE: RefCell<TurnViewCache> = RefCell::new(TurnViewCache::default());
}
/// One row of the virtualized thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThreadItem {
    LoadEarlier,
    /// Index into the cached `Arc<[ThreadTurnEntry]>` slice.
    TurnEntry(usize),
    PendingUser,
    Notices,
    Error,
    StopMarker,
}

/// Identity of a thread row across frames so list updates splice only the rows that changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadItemKey {
    LoadEarlier,
    Message(i64),
    Turn(i64),
    PendingUser,
    Notices,
    Error,
    StopMarker,
}

impl ChatView {
    /// Returns the cached [`ThreadTurnEntry`] slice for the current thread state.
    pub fn cached_turn_entries(&self) -> Arc<[ThreadTurnEntry]> {
        let run_active = self.is_run_active();
        let session_ws_id = self
            .session
            .as_ref()
            .and_then(|s| s.spanbay_workspace_id.as_deref())
            .or(self.active_workspace_id.as_deref());
        TURN_VIEW_CACHE.with(|cell| {
            cell.borrow_mut().get_or_build(
                &self.thread,
                self.turn_inputs_version,
                &self.audit_tool_calls,
                &self.persisted_changes,
                &self.thinking_durations,
                session_ws_id,
                run_active,
                self.density,
            )
        })
    }

    fn thread_items(&self, entries: &[ThreadTurnEntry]) -> Vec<ThreadItem> {
        let mut items = Vec::with_capacity(entries.len() + 4);
        if self.has_earlier_messages || self.loaded_earlier_once {
            items.push(ThreadItem::LoadEarlier);
        }
        for ix in 0..entries.len() {
            items.push(ThreadItem::TurnEntry(ix));
        }
        if !self.has_later_messages {
            if self.pending_user_text.is_some() {
                items.push(ThreadItem::PendingUser);
            }
            let has_live_turn = entries
                .iter()
                .any(|e| matches!(e, ThreadTurnEntry::AssistantTurn(t) if t.is_live));
            if !has_live_turn {
                if !self.thread.notices.is_empty() {
                    items.push(ThreadItem::Notices);
                }
                if stop_marker(self).is_some() {
                    items.push(ThreadItem::StopMarker);
                }
            }
            if self.thread.last_error.is_some() {
                items.push(ThreadItem::Error);
            }
        }
        items
    }

    /// Splices `list_state` from the previous frame's rows to `items` and re-measures toggled or live rows.
    fn sync_thread_list(
        &mut self,
        items: &[ThreadItem],
        entries: &[ThreadTurnEntry],
        has_live: bool,
    ) {
        let keys: Vec<ThreadItemKey> = items
            .iter()
            .map(|item| match *item {
                ThreadItem::LoadEarlier => ThreadItemKey::LoadEarlier,
                ThreadItem::TurnEntry(eix) => match entries.get(eix) {
                    Some(
                        ThreadTurnEntry::UserTurn { msg_index }
                        | ThreadTurnEntry::SummaryDivider { msg_index },
                    ) => ThreadItemKey::Message(
                        self.thread.messages.get(*msg_index).map_or(0, |m| m.seq),
                    ),
                    Some(ThreadTurnEntry::AssistantTurn(turn)) => {
                        ThreadItemKey::Turn(turn.turn_key_seq)
                    }
                    None => ThreadItemKey::Message(0),
                },
                ThreadItem::PendingUser => ThreadItemKey::PendingUser,
                ThreadItem::Notices => ThreadItemKey::Notices,
                ThreadItem::Error => ThreadItemKey::Error,
                ThreadItem::StopMarker => ThreadItemKey::StopMarker,
            })
            .collect();
        let old = &self.list_keys;
        if *old != keys {
            let prefix = old.iter().zip(&keys).take_while(|(a, b)| a == b).count();
            let suffix = old[prefix..]
                .iter()
                .rev()
                .zip(keys[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            let (start, end, inserted) = (prefix, old.len() - suffix, keys.len() - prefix - suffix);
            let at_head =
                start == 0 || (start == 1 && old.first() == Some(&ThreadItemKey::LoadEarlier));
            let keep = (at_head && suffix > 0 && !self.list_state.is_following_tail())
                .then(|| self.list_state.logical_scroll_top());
            self.list_state.splice(start..end, inserted);
            if let Some(top) = keep {
                self.list_state.scroll_to(if top.item_ix >= end {
                    ListOffset {
                        item_ix: top.item_ix - (end - start) + inserted,
                        offset_in_item: top.offset_in_item,
                    }
                } else {
                    ListOffset {
                        item_ix: start + inserted,
                        offset_in_item: px(0.0),
                    }
                });
            }
            self.list_keys = keys;
        }
        let mut combined_toggles = self.expanded_reasoning.clone();
        combined_toggles.extend(self.expanded_traces.iter().cloned());
        combined_toggles.insert(format!("density-{:?}-{has_live}", self.density));
        if self.last_expanded_reasoning != combined_toggles {
            self.last_expanded_reasoning = combined_toggles;
            if !items.is_empty() {
                self.list_state.remeasure_items(0..items.len());
            }
        } else if has_live
            && let Some(ix) = items
                .iter()
                .rposition(|i| matches!(i, ThreadItem::TurnEntry(_)))
        {
            self.list_state.remeasure_items(ix..ix + 1);
        }
        if !self.thread.messages.is_empty()
            && let Some(target_id) = self.pending_scroll_message_id.take()
            && let Some(target_mx) = self.thread.messages.iter().position(|m| m.id == target_id)
            && let Some(row_ix) = items.iter().rposition(|item| match *item {
                ThreadItem::TurnEntry(eix) => match entries.get(eix) {
                    Some(
                        ThreadTurnEntry::UserTurn { msg_index }
                        | ThreadTurnEntry::SummaryDivider { msg_index },
                    ) => *msg_index <= target_mx,
                    Some(ThreadTurnEntry::AssistantTurn(turn)) => turn
                        .message_indices
                        .first()
                        .is_some_and(|&first_mx| first_mx <= target_mx),
                    None => false,
                },
                _ => false,
            })
        {
            self.list_state.pause_following_tail();
            self.list_state.scroll_to_reveal_item(row_ix);
        }
    }
}

pub fn render_messages(view: &mut ChatView, cx: &mut Context<ChatView>) -> Div {
    let has_live = view.is_run_active()
        || !view.thread.streaming_text.is_empty()
        || !view.thread.streaming_reasoning.is_empty()
        || !view.thread.tool_calls.is_empty();
    let ch = crate::ui::tr(cx).chat();
    let notice = view
        .session
        .as_ref()
        .is_some_and(|s| s.notice.as_deref() == Some("full_access_off"))
        .then(|| {
            let act = btn_ghost("chat-notice-dismiss", ch.notice_dismiss())
                .on_click(cx.listener(|this, _, _, cx| this.dismiss_session_notice(cx)))
                .into_any_element();
            super::chat_column(div().pt(px(10.0)).child(alert_banner(
                Tone::Neutral,
                ch.notice_full_access_off(),
                Some(act),
                cx,
            )))
        });
    let degraded = view.thread.stream_degraded.then(|| {
        let secs = AGENT_RESUBSCRIBE_BACKOFF.as_secs();
        let act = btn_secondary("chat-reconnect-now", ch.thread_reconnect_now())
            .on_click(cx.listener(|this, _, _, cx| {
                this.subscribe_attempts = 0;
                this.snapshot_and_subscribe(cx);
            }))
            .into_any_element();
        super::chat_column(div().pt(px(10.0)).child(alert_banner(
            Tone::Warning,
            ch.thread_stream_degraded(secs),
            Some(act),
            cx,
        )))
    });
    let root = v_col()
        .flex_1()
        .min_h_0()
        .w_full()
        .children(degraded)
        .children(notice);
    if view.thread.messages.is_empty() && !has_live && view.pending_user_text.is_none() {
        view.sync_thread_list(&[], &[], false);
        return root.child(super::chat_column(
            v_col()
                .gap(px(16.0))
                .py(px(16.0))
                .child(render_welcome_empty(cx))
                .children(
                    view.thread
                        .last_error
                        .is_some()
                        .then(|| render_error(view, cx)),
                ),
        ));
    }

    let entries = view.cached_turn_entries();
    let items = view.thread_items(&entries);
    view.sync_thread_list(&items, &entries, has_live);
    if view.has_later_messages
        && !view.loading_later_messages
        && !view.load_later_failed
        && view.pending_scroll_message_id.is_none()
        && (view.list_state.is_following_tail()
            || view.list_state.logical_scroll_top().item_ix + 8 >= items.len())
    {
        view.list_state.set_follow_mode(FollowMode::Normal);
        view.load_later_messages(cx);
    } else if !view.has_later_messages
        && !view.list_state.is_following_tail()
        && view.list_state.is_scrolled_to_end() == Some(true)
    {
        view.list_state.set_follow_mode(FollowMode::Tail);
    }
    let items_rc: Rc<[ThreadItem]> = items.into();
    let entries_arc = Arc::clone(&entries);
    let thread_list = list(
        view.list_state.clone(),
        cx.processor(move |this, ix: usize, _, cx| {
            let Some(&item) = items_rc.get(ix) else {
                return div().into_any_element();
            };
            let mut row =
                super::chat_column(v_col().child(render_item(this, item, &entries_arc, cx)))
                    .pt(px(10.0));
            if ix + 1 == items_rc.len() {
                row = row.pb(px(14.0));
            }
            row.into_any_element()
        }),
    )
    .size_full();

    let jump_label = crate::ui::tr(cx).chat().thread_jump_to_latest();
    root.child(
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(thread_list)
            .vertical_scrollbar(&view.list_state)
            .children(
                (view.has_later_messages
                    || (!view.list_state.is_following_tail()
                        && view.list_state.is_scrolled_to_end() != Some(true)))
                .then(|| {
                    div().absolute().right(px(18.0)).bottom(px(10.0)).child(
                        btn_secondary("chat-jump-bottom", jump_label)
                            .on_click(cx.listener(|this, _, _, cx| this.jump_to_latest(cx))),
                    )
                }),
            ),
    )
}

fn render_item(
    view: &ChatView,
    item: ThreadItem,
    entries: &[ThreadTurnEntry],
    cx: &mut Context<ChatView>,
) -> AnyElement {
    match item {
        ThreadItem::LoadEarlier => {
            let p = palette(cx);
            let ch = crate::ui::tr(cx).chat();
            h_row()
                .w_full()
                .justify_center()
                .child(if view.has_earlier_messages {
                    btn_secondary(
                        "chat-load-earlier",
                        if view.loading_earlier_messages {
                            crate::ui::tr(cx).common().loading()
                        } else {
                            ch.thread_load_earlier()
                        },
                    )
                    .disabled(view.loading_earlier_messages)
                    .on_click(cx.listener(|this, _, _, cx| this.load_earlier_messages(cx)))
                    .into_any_element()
                } else {
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.faint))
                        .child(ch.thread_earliest())
                        .into_any_element()
                })
                .into_any_element()
        }
        ThreadItem::TurnEntry(eix) => match entries.get(eix) {
            Some(ThreadTurnEntry::UserTurn { msg_index }) => {
                view.thread.messages.get(*msg_index).map_or_else(
                    || div().into_any_element(),
                    |msg| render_user_message(view, msg, cx).into_any_element(),
                )
            }
            Some(ThreadTurnEntry::SummaryDivider { .. }) => {
                labeled_divider(crate::ui::tr(cx).chat().thread_context_compacted(), cx)
                    .into_any_element()
            }
            Some(ThreadTurnEntry::AssistantTurn(turn)) => {
                render_assistant_turn(view, turn, cx).into_any_element()
            }
            None => div().into_any_element(),
        },
        ThreadItem::PendingUser => view.pending_user_text.as_deref().map_or_else(
            || div().into_any_element(),
            |text| render_pending_user_bubble(text, cx).into_any_element(),
        ),
        ThreadItem::Notices => render_inline_notices(&view.thread.notices, cx)
            .map_or_else(|| div().into_any_element(), |el| el.into_any_element()),
        ThreadItem::Error => render_error(view, cx),
        ThreadItem::StopMarker => render_stop_marker(view, cx),
    }
}

/// The run's error, titled by its failure class, with Retry (reruns the latest turn), Dismiss
/// and the core's own wording as expandable technical detail.
fn render_error(view: &ChatView, cx: &mut Context<ChatView>) -> AnyElement {
    let Some(err) = &view.thread.last_error else {
        return div().into_any_element();
    };
    let ch = crate::ui::tr(cx).chat();
    let title = err.failure.as_ref().map_or_else(
        || ch.failure_generic(),
        |f| match f.kind {
            AgentFailureKind::AllTargetsFailed => ch.failure_all_targets_failed(),
            AgentFailureKind::KeyRejected => ch.failure_key_rejected(),
            AgentFailureKind::RefusedOrEmpty => ch.failure_refused_or_empty(),
            AgentFailureKind::Stalled => ch.failure_stalled(),
            AgentFailureKind::McpNeedsToken => ch.failure_mcp_needs_token(),
            AgentFailureKind::ContextFull => ch.failure_context_full(),
            AgentFailureKind::Offline => ch.failure_offline(),
        },
    );
    let can_retry = !view.is_run_active()
        && (!view.thread.messages.is_empty()
            || view
                .session
                .as_ref()
                .and_then(|s| s.head_message_id.as_ref())
                .is_some());
    let actions = h_row()
        .gap(px(4.0))
        .child(
            btn_ghost("chat-error-details", ch.thread_error_details()).on_click(cx.listener(
                |this, _, _, cx| {
                    this.error_details_open = !this.error_details_open;
                    cx.notify();
                },
            )),
        )
        .children(can_retry.then(|| {
            btn_ghost("chat-error-retry", crate::ui::tr(cx).common().retry())
                .on_click(cx.listener(|this, _, _, cx| this.retry_last_turn(cx)))
        }))
        .child(
            btn_icon("chat-error-dismiss", UiIcon::Close)
                .tooltip(crate::ui::tr(cx).common().close())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.thread.last_error = None;
                    cx.notify();
                })),
        );
    let p = palette(cx);
    let mono = mono_font_family(cx);
    let targets = err
        .failure
        .as_ref()
        .map_or(&[][..], |f| f.targets.as_slice());
    v_col()
        .w_full()
        .gap(px(4.0))
        .child(alert_banner(
            Tone::Danger,
            SharedString::from(title),
            Some(actions.into_any_element()),
            cx,
        ))
        .children(view.error_details_open.then(|| {
            let mut box_el = v_col()
                .px(px(10.0))
                .py(px(6.0))
                .gap(px(3.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken));
            if !targets.is_empty() {
                for t in targets {
                    box_el = box_el.child(
                        div()
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .child(SharedString::from(format!("{} · {}", t.target, t.reason))),
                    );
                }
            } else if !err.message.is_empty() {
                box_el = box_el.child(
                    div()
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(err.message.clone())),
                );
            }
            box_el.child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.faint))
                    .child(ch.failure_details_note()),
            )
        }))
        .into_any_element()
}
