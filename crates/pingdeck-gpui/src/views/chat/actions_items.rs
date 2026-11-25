//! Extended Chat item actions: Message Edit/Retry/Branch/Delete, Tool Output paging,
//! File Changes Diff/Keep/Undo, Background Process kill, and Context Compact.

use gpui_kit::Context;
use pingdeck_desktop::core::app::{
    agent_change_diff_impl, agent_change_keep_impl, agent_change_undo_impl,
    agent_changes_list_impl, agent_context_compact_impl, agent_message_edit_impl,
    agent_message_retry_impl, agent_process_kill_impl, agent_process_list_impl,
    agent_session_branch_impl, agent_session_delete_message_impl, agent_tool_output_read_impl,
};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::ChatView;
use super::types::ChatModal;

impl ChatView {
    pub fn retry_message(&mut self, message_id: String, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() || self.is_run_active() {
            return;
        }
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_message_retry_impl(&state, &sid, &message_id, false).await },
            |this, res, cx| {
                if let Err(e) = res {
                    this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn confirm_edit_message(&mut self, message_id: String, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        let text = self.modal_textarea.read(cx).value().trim().to_owned();
        if sid.is_empty() || text.is_empty() {
            return;
        }
        self.active_modal = None;
        let task = spawn_async_into(
            cx,
            move |state| async move {
                agent_message_edit_impl(&state, &sid, &message_id, text, Vec::new(), false).await
            },
            |this, res, cx| {
                if let Err(e) = res {
                    this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

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
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_delete_message_impl(state, &sid, &message_id),
            |this, res, cx| {
                match res {
                    Ok(detail) => {
                        this.thread
                            .replace_snapshot(detail.session.title.clone(), detail.messages);
                        this.session = Some(detail.session);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn open_tool_output(&mut self, call_id: String, name: String, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let cid = call_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_tool_output_read_impl(state, &sid, &cid, 0, 32_768),
            move |this, res, cx| {
                match res {
                    Ok(chunk) => {
                        this.active_modal = Some(ChatModal::ToolOutput {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            content: chunk.text,
                            has_more: !chunk.eof,
                            next_offset: chunk.next_offset,
                        });
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn open_change_diff(&mut self, change_id: String, path: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_change_diff_impl(state, &change_id),
            move |this, res, cx| {
                match res {
                    Ok(diff) => {
                        this.active_modal = Some(ChatModal::DiffPreview {
                            path: path.clone(),
                            diff: diff.unified_diff,
                            binary: diff.binary,
                        });
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn keep_changes(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                agent_change_keep_impl(state, &ids)?;
                agent_changes_list_impl(state, &sid)
            },
            |this, res, cx| {
                match res {
                    Ok(list) => this.persisted_changes = list,
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn undo_change(&mut self, change_id: String, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                agent_change_undo_impl(state, &[change_id], false)?;
                agent_changes_list_impl(state, &sid)
            },
            |this, res, cx| {
                match res {
                    Ok(list) => {
                        this.persisted_changes = list;
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
        let task = spawn_blocking_into(
            cx,
            move |state| {
                agent_process_kill_impl(state, &process_id)?;
                agent_process_list_impl(state, Some(&sid))
            },
            |this, res, cx| {
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
        self.status_msg = Some((
            Tone::Accent,
            crate::ui::tr(cx).chat().act_compacting().into(),
        ));
        cx.notify();
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_context_compact_impl(&state, &sid).await },
            |this, res, cx| {
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
}
