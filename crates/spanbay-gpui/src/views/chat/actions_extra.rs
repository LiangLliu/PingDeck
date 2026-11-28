//! Extended Chat actions: Board/Trash loading, Plan & Question gates, Queue & Steer,
//! Attachments, Workspace/Folder binding, and Redacted Export.

use gpui_kit::{Context, Window};
use spanbay_desktop::core::agent::attachments::attachment_parts;
use spanbay_desktop::core::agent::model::{
    AgentAttachment, AgentQuestionAnswer, AgentSessionFilter, AgentSessionInput, AgentSessionPatch,
    AgentTarget,
};
use spanbay_desktop::core::agent::tools::ask_user::check_answer;
use spanbay_desktop::core::app::{
    AppState, PickedKind, agent_attachment_describe_path_impl, agent_attachment_put_bytes_impl,
    agent_attachment_put_path_impl, agent_attachment_read_image_impl, agent_plan_set_impl,
    agent_question_answer_impl, agent_queue_add_impl, agent_queue_clear_impl,
    agent_queue_remove_impl, agent_queue_resume_impl, agent_queue_steer_impl, agent_run_steer_impl,
    agent_session_bind_spanbay_impl, agent_session_create_impl, agent_session_list_impl,
    agent_session_restore_impl, environment_list_impl, workspace_list_impl,
};
use spanbay_desktop::services::{FilePickerSpec, HostDialog, RfdDialog};
use spanbay_desktop::state::RunError;

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::ChatView;
use super::actions::resolve_fallback_target;
use super::types::ChatSidebarMode;

/// Validates `preferred_ws` / `preferred_env` against the live store so session creation and
/// run start bind to the active SpanBay workspace (and only an environment that belongs to it)
/// without failing if either was deleted or switched.
pub(super) fn resolve_active_binding(
    state: &AppState,
    preferred_ws: Option<String>,
    preferred_env: Option<String>,
) -> (Option<String>, Option<String>) {
    let Some(ws_id) = preferred_ws else {
        return (None, None);
    };
    let workspaces = workspace_list_impl(state).unwrap_or_default();
    if !workspaces.iter().any(|w| w.id.as_str() == ws_id) {
        return (None, None);
    }
    let env_id = preferred_env.filter(|eid| {
        environment_list_impl(state, ws_id.clone())
            .unwrap_or_default()
            .iter()
            .any(|e| e.id == *eid)
    });
    (Some(ws_id), env_id)
}

/// Ingests one picked or dropped file path into the attachment store, reading image bytes
/// through [`agent_attachment_read_image_impl`] + [`agent_attachment_put_bytes_impl`] and storing
/// PDFs/text via [`agent_attachment_put_path_impl`].
pub(super) fn ingest_attachment_path(
    state: &AppState,
    path: &std::path::Path,
) -> Result<AgentAttachment, spanbay_desktop::core::error::AppError> {
    let desc = agent_attachment_describe_path_impl(path)?;
    match desc.kind {
        PickedKind::Image => {
            let bytes = agent_attachment_read_image_impl(state, std::path::Path::new(&desc.path))?;
            agent_attachment_put_bytes_impl(state, &desc.name, "", &bytes, None)
        }
        PickedKind::Pdf | PickedKind::Text | PickedKind::Folder | PickedKind::Unsupported => {
            agent_attachment_put_path_impl(state, &desc.path, Some(desc.name))
        }
    }
}
impl ChatView {
    pub fn create_session(&mut self, cx: &mut Context<Self>) {
        self.create_session_with_options(None, None, None, None, cx);
    }

    pub fn select_model(&mut self, target: AgentTarget, cx: &mut Context<Self>) {
        self.context_breakdown = None;
        if self.thread.session_id.is_empty() {
            self.create_session_with_options(Some(target), None, None, None, cx);
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

    pub(super) fn create_session_with_options(
        &mut self,
        explicit_target: Option<AgentTarget>,
        explicit_folder: Option<String>,
        explicit_ws: Option<String>,
        explicit_env: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let active_ws = explicit_ws.or_else(|| self.active_workspace_id.clone());
        let active_env = if active_ws == self.active_workspace_id {
            explicit_env.or_else(|| self.active_environment_id.clone())
        } else {
            explicit_env
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let target = explicit_target.or_else(|| resolve_fallback_target(state));
                let (spanbay_workspace_id, spanbay_environment_id) =
                    resolve_active_binding(state, active_ws, active_env);
                agent_session_create_impl(
                    state,
                    AgentSessionInput {
                        target,
                        workspace_dir: explicit_folder,
                        spanbay_workspace_id,
                        spanbay_environment_id,
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
    pub fn set_sidebar_mode(&mut self, mode: ChatSidebarMode, cx: &mut Context<Self>) {
        self.sidebar_mode = mode;
        match mode {
            ChatSidebarMode::Time => self.refresh_catalog(cx),
            ChatSidebarMode::Trash => self.load_trash(cx),
        }
        cx.notify();
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
        let sid_check = sid.clone();
        let next = !self.plan_mode;
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_plan_set_impl(&state, &sid, next).await },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                match res {
                    Ok(on) => this.plan_mode = on,
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
        let sid_check = self.thread.session_id.clone();
        let is_first = self
            .thread
            .open_gates
            .first()
            .is_some_and(|(_, g)| g.call_id == call_id);
        let choices = self
            .question_draft_choices
            .get(&call_id)
            .cloned()
            .unwrap_or_default();
        let other_raw = if is_first {
            self.question_other_input.read(cx).value().trim().to_owned()
        } else {
            self.question_other_inputs
                .get(&call_id)
                .map(|inp| inp.read(cx).value().trim().to_owned())
                .unwrap_or_default()
        };
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
        if let Some((_, gate)) = self
            .thread
            .open_gates
            .iter()
            .find(|(_, g)| g.call_id == call_id)
            && let Err(e) = check_answer(gate, &answer)
        {
            self.status_msg = Some((Tone::Danger, e.message().to_owned()));
            cx.notify();
            return;
        }
        self.question_draft_choices.remove(&call_id);
        if is_first {
            self.question_other_input
                .update(cx, |s, cx| s.set_value("", window, cx));
        } else {
            self.question_other_inputs.remove(&call_id);
        }
        let cid_check = call_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_question_answer_impl(state, &run_id, &call_id, answer),
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                match res {
                    Ok(_) => {
                        this.thread
                            .open_gates
                            .retain(|(_, g)| g.call_id != cid_check);
                        this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                    }
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
        let sid_check = sid.clone();
        let skills = std::mem::take(&mut self.selected_skills);
        let atts = std::mem::take(&mut self.pending_attachments);
        let parts = attachment_parts(&atts, &text);
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_queue_add_impl(&state, &sid, parts, skills, &[]).await },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
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

    /// Steers the run behind the first waiting plan/question gate (the core settles that gate as a
    /// revision), else the active run.
    pub fn steer_active_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run_id) = self
            .thread
            .open_gates
            .first()
            .map(|(run_id, _)| run_id.clone())
            .or_else(|| self.thread.active_run.as_ref().map(|r| r.run_id.clone()))
        else {
            return;
        };
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
        self.plan_revising = false;
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        self.reset_composer_placeholder(window, cx);
        let atts = std::mem::take(&mut self.pending_attachments);
        let att_ids: Vec<String> = atts.iter().map(|a| a.id.clone()).collect();
        let skills = std::mem::take(&mut self.selected_skills);
        let sid_check = self.thread.session_id.clone();
        let restore_text = text.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_run_steer_impl(state, &run_id, &text, &att_ids, skills, &[]),
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                match res {
                    Ok(true) => {
                        this.status_msg =
                            Some((Tone::Accent, crate::ui::tr(cx).chat().act_steered().into()));
                    }
                    Ok(false) => {
                        this.pending_draft_sync = Some(restore_text);
                        this.pending_attachments = atts;
                        this.status_msg = Some((
                            Tone::Warning,
                            crate::ui::tr(cx).chat().act_no_active_run().into(),
                        ));
                    }
                    Err(e) => {
                        // Nothing was delivered: give the message back instead of losing it.
                        this.pending_draft_sync = Some(restore_text);
                        this.pending_attachments = atts;
                        this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                    }
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
        let sid_check = sid.clone();
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_queue_resume_impl(&state, &sid).await },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
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
        let sid_check = sid.clone();
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_queue_steer_impl(&state, &sid, &queue_id).await },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
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
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_queue_remove_impl(state, &sid, &queue_id),
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
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
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_queue_clear_impl(state, &sid),
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                if let Ok(q) = res {
                    this.thread.queued_turns = q;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn pick_attachments(&mut self, cx: &mut Context<Self>) {
        let sid_check = self.thread.session_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let paths = RfdDialog.pick_files(&FilePickerSpec::default());
                let mut added = Vec::new();
                let mut first_err = None;
                for path in &paths {
                    match ingest_attachment_path(state, path) {
                        Ok(att) => added.push(att),
                        Err(e) if first_err.is_none() => first_err = Some(e),
                        Err(_) => {}
                    }
                }
                (added, first_err)
            },
            move |this, (added, first_err), cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                this.pending_attachments.extend(added);
                if let Some(err) = first_err {
                    this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                }
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
            self.create_session_with_options(None, None, Some(workspace_id), env_id, cx);
            return;
        }
        let sid_check = sid.clone();
        let ws_for_envs = workspace_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let updated = agent_session_bind_spanbay_impl(state, &sid, &workspace_id, env_id)?;
                let envs = environment_list_impl(state, ws_for_envs).unwrap_or_default();
                Ok::<_, spanbay_desktop::core::error::AppError>((updated, envs))
            },
            move |this, res, cx| {
                match res {
                    Ok((updated, envs)) => {
                        if this.thread.session_id == sid_check {
                            this.session = Some(updated);
                            this.environments = envs;
                            this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn bind_folder_path(&mut self, dir: String, cx: &mut Context<Self>) {
        if self.thread.session_id.is_empty() {
            self.create_session_with_options(None, Some(dir), None, None, cx);
        } else {
            self.patch_session(
                AgentSessionPatch {
                    workspace_dir: Some(dir),
                    ..Default::default()
                },
                cx,
            );
        }
    }

    pub fn pick_bound_folder(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |_state| {
                RfdDialog
                    .pick_folder(None)
                    .map(|p| p.to_string_lossy().into_owned())
            },
            |this, picked, cx| {
                if let Some(dir) = picked {
                    this.bind_folder_path(dir, cx);
                }
            },
        );
        self._tasks.push(task);
    }
}
