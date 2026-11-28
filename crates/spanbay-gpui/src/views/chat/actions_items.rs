//! Extended Chat item actions: Message Edit/Retry/Branch/Delete, Tool Output paging,
//! File Changes Diff/Keep/Undo, Background Process kill, and Context Compact.

use std::path::Path;

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;
use gpui_kit::Context;
use spanbay_desktop::core::agent::model::AgentSessionFilter;
use spanbay_desktop::core::app::{
    AgentPageRequest, agent_change_keep_impl, agent_change_undo_impl, agent_changes_list_impl,
    agent_context_compact_impl, agent_process_kill_impl, agent_process_list_impl,
    agent_session_branch_impl, agent_session_delete_message_impl, agent_session_get_impl,
    agent_session_list_impl, agent_tool_output_read_impl, open_path_impl,
};
use spanbay_desktop::services::resolve_workspace_open_target;
use spanbay_desktop::state::clean_tool_output_for_modal;

use super::ChatView;
use super::types::ReviewToolOutput;

impl ChatView {
    pub fn branch_from_message(&mut self, message_id: String, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_branch_impl(state, &sid, &message_id, None),
            |this, res, cx| {
                match res {
                    Ok(forked) => {
                        this.refresh_catalog(cx);
                        this.open_session(forked.id, cx);
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).chat().act_branched().into(),
                        ));
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_message(&mut self, message_id: String, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() || self.is_run_active() {
            return;
        }
        self.context_breakdown = None;
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_delete_message_impl(state, &sid, &message_id),
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                match res {
                    Ok(detail) => {
                        this.thread
                            .replace_snapshot(detail.session.title.clone(), detail.messages);
                        this.has_earlier_messages = detail.has_more;
                        this.has_later_messages = detail.has_more_after;
                        this.load_later_failed = false;
                        this.session = Some(detail.session);
                        this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn open_tool_output(
        &mut self,
        call_id: String,
        tool_name: String,
        modal_title: String,
        inline_full: Option<String>,
        spilled: bool,
        cx: &mut Context<Self>,
    ) {
        if !spilled && let Some(content) = inline_full {
            self.review_tool_output = Some(ReviewToolOutput {
                call_id,
                name: modal_title,
                content,
                has_more: false,
                next_offset: 0,
            });
            self.review_open = true;
            self.sidebar_collapsed = true;
            cx.notify();
            return;
        }
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            if let Some(content) = inline_full {
                self.review_tool_output = Some(ReviewToolOutput {
                    call_id,
                    name: modal_title,
                    content,
                    has_more: false,
                    next_offset: 0,
                });
                self.review_open = true;
                self.sidebar_collapsed = true;
                cx.notify();
            }
            return;
        }
        let cid = call_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_tool_output_read_impl(state, &sid, &cid, 0, 32_768),
            move |this, res, cx| {
                match res {
                    Ok(chunk) => {
                        let cleaned = clean_tool_output_for_modal(&tool_name, &chunk.text);
                        this.review_tool_output = Some(ReviewToolOutput {
                            call_id: call_id.clone(),
                            name: modal_title.clone(),
                            content: cleaned,
                            has_more: !chunk.eof,
                            next_offset: chunk.next_offset,
                        });
                        this.review_open = true;
                        this.sidebar_collapsed = true;
                    }
                    Err(e) => {
                        if let Some(fallback) = inline_full.clone() {
                            this.review_tool_output = Some(ReviewToolOutput {
                                call_id: call_id.clone(),
                                name: modal_title.clone(),
                                content: fallback,
                                has_more: false,
                                next_offset: 0,
                            });
                            this.review_open = true;
                            this.sidebar_collapsed = true;
                        } else {
                            this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                        }
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn load_more_tool_output(
        &mut self,
        call_id: String,
        next_offset: u64,
        cx: &mut Context<Self>,
    ) {
        if self.tool_output_loading_more {
            return;
        }
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let sid_check = sid.clone();
        let cid = call_id.clone();
        self.tool_output_loading_more = true;
        let task = spawn_blocking_into(
            cx,
            move |state| agent_tool_output_read_impl(state, &sid, &cid, next_offset, 32_768),
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                this.tool_output_loading_more = false;
                match res {
                    Ok(chunk) => {
                        if let Some(out) = &mut this.review_tool_output
                            && out.call_id == call_id
                            && out.next_offset == next_offset
                        {
                            out.content.push_str(&chunk.text);
                            out.has_more = !chunk.eof;
                            out.next_offset = chunk.next_offset;
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn open_workspace_file_external(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(ws_dir) = self.session.as_ref().and_then(|s| s.workspace_dir.clone()) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let target = resolve_workspace_open_target(Path::new(&ws_dir), &path)?;
                open_path_impl(state, &target.to_string_lossy())
            },
            |this, res, cx| {
                if let Err(e) = res {
                    this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }
    pub fn open_change_diff(&mut self, change_id: String, path: String, cx: &mut Context<Self>) {
        self.review_selected_change_id = Some(change_id);
        self.open_review(Some(path), cx);
    }

    pub fn keep_changes(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                agent_change_keep_impl(state, &ids)?;
                agent_changes_list_impl(state, &sid)
            },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                match res {
                    Ok(list) => {
                        this.persisted_changes = list;
                        this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                        this.review_diff_cache = None;
                        if this.review_open {
                            this.load_review_diff(cx);
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn undo_change(&mut self, change_id: String, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                agent_change_undo_impl(state, &[change_id], false)?;
                agent_changes_list_impl(state, &sid)
            },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                match res {
                    Ok(list) => {
                        this.persisted_changes = list;
                        this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                        this.review_diff_cache = None;
                        if this.review_open {
                            this.load_review_diff(cx);
                        }
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).chat().act_reverted_change().into(),
                        ));
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn kill_process(&mut self, process_id: String, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                agent_process_kill_impl(state, &process_id)?;
                agent_process_list_impl(state, Some(&sid))
            },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                if let Ok(procs) = res {
                    this.processes = procs;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn compact_context(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() || self.is_run_active() {
            return;
        }
        self.context_breakdown = None;
        let sid_check = sid.clone();
        self.status_msg = Some((
            Tone::Accent,
            crate::ui::tr(cx).chat().act_compacting().into(),
        ));
        cx.notify();
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_context_compact_impl(&state, &sid).await },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                match res {
                    Ok(_) => {
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).chat().act_compacted().into(),
                        ));
                        this.refresh_after_turn(cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn load_more_sessions(&mut self, cx: &mut Context<Self>) {
        if self.sessions_loading_more {
            return;
        }
        let Some(cursor) = self.sessions_next_cursor.clone() else {
            return;
        };
        let include_archived = self.show_archived;
        let query_raw = self.search_input.read(cx).value().trim().to_owned();
        let query_check = query_raw.clone();
        let query = (!query_raw.is_empty()).then_some(query_raw);
        let filter_epoch = self.catalog_filter_epoch;
        self.sessions_loading_more = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let filter = AgentSessionFilter {
                    include_archived,
                    query,
                    ..AgentSessionFilter::default()
                };
                agent_session_list_impl(state, Some(filter), Some(50), Some(cursor))
            },
            move |this, res, cx| {
                if this.catalog_filter_epoch != filter_epoch
                    || this.show_archived != include_archived
                    || this.search_input.read(cx).value().trim() != query_check
                {
                    return;
                }
                this.sessions_loading_more = false;
                if let Ok(page) = res {
                    for s in page.sessions {
                        if !this.sessions.iter().any(|existing| existing.id == s.id) {
                            this.sessions.push(s);
                        }
                    }
                    this.sessions_next_cursor = page.next_cursor;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn load_earlier_messages(&mut self, cx: &mut Context<Self>) {
        if self.loading_earlier_messages || !self.has_earlier_messages {
            return;
        }
        let sid = self.thread.session_id.clone();
        let Some(before_seq) = self.thread.messages.first().map(|m| m.seq) else {
            return;
        };
        if sid.is_empty() {
            return;
        }
        let sid_check = sid.clone();
        self.loading_earlier_messages = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                agent_session_get_impl(
                    state,
                    &sid,
                    AgentPageRequest {
                        before_seq: Some(before_seq),
                        limit: Some(50),
                        ..AgentPageRequest::default()
                    },
                )
            },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                this.loading_earlier_messages = false;
                if let Ok(detail) = res {
                    this.thread.prepend_earlier_messages(detail.messages);
                    this.merge_audit_tool_calls(detail.tool_calls);
                    this.loaded_earlier_once = true;
                    this.has_earlier_messages = detail.has_more;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    /// Pages forward from a mid-history window opened around a search hit (`has_later_messages`).
    pub fn load_later_messages(&mut self, cx: &mut Context<Self>) {
        if self.loading_later_messages || self.load_later_failed || !self.has_later_messages {
            return;
        }
        let sid = self.thread.session_id.clone();
        let Some(after_seq) = self.thread.messages.last().map(|m| m.seq) else {
            return;
        };
        if sid.is_empty() {
            return;
        }
        let sid_check = sid.clone();
        self.loading_later_messages = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                agent_session_get_impl(
                    state,
                    &sid,
                    AgentPageRequest {
                        after_seq: Some(after_seq),
                        limit: Some(50),
                        ..AgentPageRequest::default()
                    },
                )
            },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                this.loading_later_messages = false;
                match res {
                    Ok(detail) => {
                        let existing_ids: std::collections::HashSet<String> =
                            this.thread.messages.iter().map(|m| m.id.clone()).collect();
                        let mut added = 0usize;
                        for m in detail.messages {
                            if !existing_ids.contains(&m.id) {
                                this.thread.messages.push(m);
                                added += 1;
                            }
                        }
                        if added > 0 {
                            this.thread.messages_version =
                                this.thread.messages_version.wrapping_add(1);
                        }
                        this.merge_audit_tool_calls(detail.tool_calls);
                        this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                        // Guard against a stalled loop if the backend reports `has_more_after`
                        // with zero new rows.
                        this.has_later_messages = detail.has_more_after && added > 0;
                        this.load_later_failed = false;
                    }
                    Err(e) => {
                        // Stop render-driven auto-retry until the user navigates or clicks Jump.
                        this.load_later_failed = true;
                        this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn load_more_trash(&mut self, cx: &mut Context<Self>) {
        if self.trash_loading_more {
            return;
        }
        let Some(cursor) = self.trash_next_cursor.clone() else {
            return;
        };
        self.trash_loading_more = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let filter = AgentSessionFilter {
                    deleted_only: true,
                    ..AgentSessionFilter::default()
                };
                agent_session_list_impl(state, Some(filter), Some(50), Some(cursor))
            },
            |this, res, cx| {
                this.trash_loading_more = false;
                if let Ok(page) = res {
                    for s in page.sessions {
                        if !this
                            .trash_sessions
                            .iter()
                            .any(|existing| existing.id == s.id)
                        {
                            this.trash_sessions.push(s);
                        }
                    }
                    this.trash_next_cursor = page.next_cursor;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}
