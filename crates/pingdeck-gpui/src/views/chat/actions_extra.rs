//! Extended Chat actions: Board/Trash loading, Plan & Question gates, Queue & Steer,
//! Attachments, Workspace/Folder binding, and Redacted Export.

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::agent::model::{
    AgentExportFormat, AgentQuestionAnswer, AgentSessionFilter, AgentSessionPatch,
};
use pingdeck_desktop::core::app::{
    agent_attachment_put_path_impl, agent_plan_approve_impl, agent_plan_set_impl,
    agent_question_answer_impl, agent_queue_add_impl, agent_queue_clear_impl,
    agent_queue_remove_impl, agent_queue_resume_impl, agent_queue_steer_impl, agent_run_steer_impl,
    agent_run_unsubscribe_impl, agent_session_bind_pingdeck_impl, agent_session_delete_impl,
    agent_session_export_redacted_impl, agent_session_list_impl, agent_session_restore_impl,
};
use pingdeck_desktop::core::model::hub::HubPart;
use pingdeck_desktop::core::model::settings::ToolApproval;
use pingdeck_desktop::services::{FileFilter, FilePickerSpec, HostDialog, RfdDialog, SaveFileSpec};
use pingdeck_desktop::state::ChatThreadState;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::ChatView;
use super::types::{ChatModal, ChatSidebarMode, PendingAttachment};

impl ChatView {
    pub fn set_sidebar_mode(&mut self, mode: ChatSidebarMode, cx: &mut Context<Self>) {
        self.sidebar_mode = mode;
        match mode {
            ChatSidebarMode::Time => self.refresh_catalog(cx),
            ChatSidebarMode::Board => {
                self.load_board(cx);
                self.start_elapsed_ticker(cx);
            }
            ChatSidebarMode::Trash => self.load_trash(cx),
        }
        cx.notify();
    }

    pub fn load_board(&mut self, cx: &mut Context<Self>) {
        self.refresh_board_and_selection(cx);
    }

    pub fn load_trash(&mut self, cx: &mut Context<Self>) {
        self.trash_loading_more = false;
        let task = spawn_blocking_into(
            cx,
            |state| {
                let filter = AgentSessionFilter {
                    deleted_only: true,
                    ..AgentSessionFilter::default()
                };
                agent_session_list_impl(state, Some(filter), Some(50), None)
            },
            |this, res, cx| {
                if let Ok(page) = res {
                    this.trash_sessions = page.sessions;
                    this.trash_next_cursor = page.next_cursor;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn restore_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_restore_impl(state, &session_id),
            |this, res, cx| {
                match res {
                    Ok(restored) => {
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).chat().act_restored().into(),
                        ));
                        this.load_trash(cx);
                        this.refresh_catalog(cx);
                        this.open_session(restored.id, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn toggle_plan_mode(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            self.plan_mode = !self.plan_mode;
            cx.notify();
            return;
        }
        let next = !self.plan_mode;
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_plan_set_impl(&state, &sid, next).await },
            |this, res, cx| {
                match res {
                    Ok(on) => this.plan_mode = on,
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn approve_plan(&mut self, mode: Option<ToolApproval>, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_plan_approve_impl(&state, &sid, None, mode).await },
            |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.plan_mode = false;
                        this.thread.open_gates.clear();
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).chat().act_plan_approved().into(),
                        ));
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn answer_question(
        &mut self,
        run_id: String,
        call_id: String,
        skipped: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let choices = self
            .question_draft_choices
            .remove(&call_id)
            .unwrap_or_default();
        let other_raw = self.question_other_input.read(cx).value().trim().to_owned();
        let other = (!other_raw.is_empty()).then_some(other_raw);
        let answer = if skipped {
            AgentQuestionAnswer {
                choices: Vec::new(),
                other: None,
                skipped: true,
            }
        } else {
            AgentQuestionAnswer {
                choices,
                other,
                skipped: false,
            }
        };
        self.question_other_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        let cid_check = call_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_question_answer_impl(state, &run_id, &call_id, answer),
            move |this, res, cx| {
                match res {
                    Ok(_) => this
                        .thread
                        .open_gates
                        .retain(|(_, g)| g.call_id != cid_check),
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn enqueue_turn(&mut self, text: String, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let skills = std::mem::take(&mut self.selected_skills);
        let _atts = std::mem::take(&mut self.pending_attachments);
        let parts = vec![HubPart::Text {
            text,
            citations: Vec::new(),
            origin: None,
            signature: None,
        }];
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_queue_add_impl(&state, &sid, parts, skills, &[]).await },
            |this, res, cx| {
                match res {
                    Ok(q) => {
                        this.thread.queued_turns = q;
                        this.status_msg =
                            Some((Tone::Accent, crate::ui::tr(cx).chat().act_queued().into()));
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn steer_active_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = &self.thread.active_run else {
            return;
        };
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        let run_id = run.run_id.clone();
        let att_ids: Vec<String> = self.pending_attachments.drain(..).map(|a| a.id).collect();
        let skills = std::mem::take(&mut self.selected_skills);
        let task = spawn_blocking_into(
            cx,
            move |state| agent_run_steer_impl(state, &run_id, &text, &att_ids, skills, &[]),
            |this, res, cx| {
                match res {
                    Ok(true) => {
                        this.status_msg =
                            Some((Tone::Accent, crate::ui::tr(cx).chat().act_steered().into()));
                    }
                    Ok(false) => {
                        this.status_msg = Some((
                            Tone::Warning,
                            crate::ui::tr(cx).chat().act_no_active_run().into(),
                        ));
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn queue_resume(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_queue_resume_impl(&state, &sid).await },
            |this, res, cx| {
                if let Ok(q) = res {
                    this.thread.queued_turns = q;
                    this.thread.queue_paused = false;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn queue_steer(&mut self, queue_id: String, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_queue_steer_impl(&state, &sid, &queue_id).await },
            |this, res, cx| {
                if let Err(e) = res {
                    this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn queue_remove(&mut self, queue_id: String, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let task = spawn_blocking_into(
            cx,
            move |state| agent_queue_remove_impl(state, &sid, &queue_id),
            |this, res, cx| {
                if let Ok(q) = res {
                    this.thread.queued_turns = q;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn queue_clear(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let task = spawn_blocking_into(
            cx,
            move |state| agent_queue_clear_impl(state, &sid),
            |this, res, cx| {
                if let Ok(q) = res {
                    this.thread.queued_turns = q;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn pick_attachments(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let paths = RfdDialog.pick_files(&FilePickerSpec::default());
                let mut added = Vec::new();
                for path in paths {
                    let s = path.to_string_lossy().into_owned();
                    if let Ok(att) = agent_attachment_put_path_impl(state, &s, None) {
                        added.push(PendingAttachment {
                            id: att.id,
                            name: att.name,
                            size_bytes: att.size_bytes,
                        });
                    }
                }
                added
            },
            |this, added, cx| {
                this.pending_attachments.extend(added);
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn bind_workspace_env(
        &mut self,
        workspace_id: String,
        env_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_bind_pingdeck_impl(state, &sid, &workspace_id, env_id),
            |this, res, cx| {
                match res {
                    Ok(updated) => this.session = Some(updated),
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn pick_bound_folder(&mut self, cx: &mut Context<Self>) {
        if self.thread.session_id.is_empty() {
            return;
        }
        let task = spawn_blocking_into(
            cx,
            move |_state| {
                RfdDialog
                    .pick_folder(None)
                    .map(|p| p.to_string_lossy().into_owned())
            },
            |this, picked, cx| {
                if let Some(dir) = picked {
                    this.patch_session(
                        AgentSessionPatch {
                            workspace_dir: Some(dir),
                            ..Default::default()
                        },
                        cx,
                    );
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn export_chat_redacted(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let default_name = format!("{}.md", self.thread.title.replace('/', "_"));
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let text = agent_session_export_redacted_impl(
                    state,
                    &sid,
                    AgentExportFormat::Markdown,
                    Some(true),
                )?;
                let spec = SaveFileSpec {
                    title: None,
                    starting_dir: None,
                    file_name: Some(default_name),
                    filters: vec![FileFilter {
                        name: "Markdown / JSON".into(),
                        extensions: vec!["md".into(), "json".into()],
                    }],
                };
                let Some(dest) = RfdDialog.save_file(&spec) else {
                    return Ok(None);
                };
                std::fs::write(&dest, text).map_err(|e| {
                    pingdeck_desktop::core::error::AppError::io(format!("export write failed: {e}"))
                })?;
                Ok::<Option<String>, pingdeck_desktop::core::error::AppError>(Some(
                    dest.display().to_string(),
                ))
            },
            |this, res, cx| {
                match res {
                    Ok(Some(path)) => {
                        this.status_msg =
                            Some((Tone::Success, crate::ui::tr(cx).chat().act_exported(path)));
                    }
                    Ok(None) => {}
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn request_delete_current(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        if self.is_run_active() {
            self.active_modal = Some(ChatModal::DeleteRunningConfirm {
                session_id: sid,
                title: self.thread.title.clone(),
            });
            cx.notify();
        } else {
            self.delete_session_by_id(sid, cx);
        }
    }

    pub fn delete_session_by_id(&mut self, sid: String, cx: &mut Context<Self>) {
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_delete_impl(state, &sid),
            move |this, res, cx| {
                if res.is_ok() {
                    if this.thread.session_id == sid_check {
                        if let Some(sub_id) = this.active_sub_id.take() {
                            agent_run_unsubscribe_impl(runtime(cx).state(), sub_id);
                        }
                        this.subscribed_session_id = None;
                        this.thread = ChatThreadState::default();
                        this.session = None;
                        this.audit_tool_calls.clear();
                        this.persisted_changes.clear();
                        this.stream_task = None;
                        this.tick_task = None;
                        this.submitting = false;
                        this.pending_user_text = None;
                    }
                    this.reload_sessions(cx);
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}
