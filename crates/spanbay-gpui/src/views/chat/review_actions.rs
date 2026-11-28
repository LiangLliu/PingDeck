//! Review panel state management, unified diff hunk parser, and density/review actions on
//! [`ChatView`].

use gpui_kit::Context;
use spanbay_desktop::core::agent::model::{FileChange, FileChangeKind, FileChangeStatus};
use spanbay_desktop::core::app::{
    agent_change_diff_impl, agent_change_net_diff_impl, agent_change_revert_hunk_impl,
    agent_change_undo_impl, agent_changes_list_impl,
};

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::Tone;

use super::ChatView;
use super::types::{DiffCommentChip, ReviewDiffMode, ReviewScope, TranscriptDensity};

/// Maximum diff lines rendered in the review panel to keep large files bounded and fast.
pub(super) const MAX_RENDERED_DIFF_LINES: usize = 400;

#[derive(Debug, Clone)]
pub(super) struct GroupedFileChange {
    pub path: String,
    pub kind: FileChangeKind,
    pub status: FileChangeStatus,
    pub added: u32,
    pub removed: u32,
    pub change_ids: Vec<String>,
    pub active_ids: Vec<String>,
    pub is_deleted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Add,
    Del,
}

#[derive(Debug, Clone)]
pub struct ParsedDiffLine {
    pub kind: DiffLineKind,
    pub line_no: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct ParsedHunk {
    pub header: String,
    pub lines: Vec<ParsedDiffLine>,
}

#[derive(Debug, Clone)]
pub struct CachedReviewDiff {
    pub key: String,
    pub hunks: Vec<ParsedHunk>,
    pub omitted: usize,
    pub binary: bool,
}

fn parse_hunk_coord(header: &str, prefix: char) -> usize {
    header
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix(prefix))
        .and_then(|s| s.split(',').next())
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(1)
}

pub(super) fn parse_unified_diff_hunks(diff: &str) -> (Vec<ParsedHunk>, usize) {
    let mut hunks: Vec<ParsedHunk> = Vec::new();
    let mut old_ln = 1usize;
    let mut new_ln = 1usize;
    let mut total_lines = 0usize;
    for raw in diff.lines() {
        if raw.starts_with("--- ") || raw.starts_with("+++ ") {
            continue;
        }
        if raw.starts_with("@@") {
            old_ln = parse_hunk_coord(raw, '-');
            new_ln = parse_hunk_coord(raw, '+');
            hunks.push(ParsedHunk {
                header: raw.to_owned(),
                lines: Vec::new(),
            });
            continue;
        }
        let Some(cur) = hunks.last_mut() else {
            continue;
        };
        total_lines += 1;
        if total_lines > MAX_RENDERED_DIFF_LINES {
            continue;
        }
        let (kind, line_no) = if raw.starts_with('+') {
            let n = new_ln;
            new_ln += 1;
            (DiffLineKind::Add, n)
        } else if raw.starts_with('-') {
            let n = old_ln;
            old_ln += 1;
            (DiffLineKind::Del, n)
        } else {
            let n = new_ln;
            old_ln += 1;
            new_ln += 1;
            (DiffLineKind::Context, n)
        };
        cur.lines.push(ParsedDiffLine {
            kind,
            line_no,
            text: raw.to_owned(),
        });
    }
    let omitted = total_lines.saturating_sub(MAX_RENDERED_DIFF_LINES);
    (hunks, omitted)
}

pub(super) fn merge_file_status(a: FileChangeStatus, b: FileChangeStatus) -> FileChangeStatus {
    use FileChangeStatus::{Applying, Conflict, Kept, Pending, Undone};
    match (a, b) {
        (Conflict, _) | (_, Conflict) => Conflict,
        (Pending, _) | (_, Pending) => Pending,
        (Applying, _) | (_, Applying) => Applying,
        (Kept, _) | (_, Kept) => Kept,
        _ => Undone,
    }
}

impl ChatView {
    pub fn set_density(&mut self, density: TranscriptDensity, cx: &mut Context<Self>) {
        if self.density != density {
            self.density = density;
            cx.notify();
        }
    }

    pub fn cycle_density(&mut self, cx: &mut Context<Self>) {
        self.density = self.density.next();
        cx.notify();
    }

    pub fn set_view_mode(&mut self, mode: TranscriptDensity, cx: &mut Context<Self>) {
        self.set_density(mode, cx);
    }

    pub fn cycle_view_mode(&mut self, cx: &mut Context<Self>) {
        self.cycle_density(cx);
    }

    /// Opens the right-hand Review Panel (optionally focused on `file`), auto-collapsing the
    /// session sidebar so the chat thread and diff panel both have generous width.
    pub fn open_review(&mut self, file: Option<String>, cx: &mut Context<Self>) {
        self.review_open = true;
        self.sidebar_collapsed = true;
        self.review_tool_output = None;
        self.review_undo_precheck = false;
        if let Some(path) = file {
            self.review_selected_path = Some(path);
            self.review_selected_change_id = None;
        }
        self.ensure_review_selection_and_load(cx);
        cx.notify();
    }

    pub fn close_review(&mut self, cx: &mut Context<Self>) {
        if self.review_open {
            self.review_open = false;
            self.sidebar_collapsed = false;
            self.review_undo_precheck = false;
            self.review_tool_output = None;
            self.review_comment_target = None;
            cx.notify();
        }
    }

    pub fn toggle_review(&mut self, cx: &mut Context<Self>) {
        if self.review_open {
            self.close_review(cx);
        } else {
            self.open_review(None, cx);
        }
    }

    pub fn restore_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }

    pub fn set_review_panel_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let clamped = width.clamp(340.0, 760.0);
        self.review_panel_width = clamped;
        let rt = runtime(cx);
        let mut ui = rt.load_ui_state();
        ui.pane_widths.insert("chat.review_panel".into(), clamped);
        rt.save_ui_state(&ui);
        cx.notify();
    }

    pub fn set_review_scope(&mut self, scope: ReviewScope, cx: &mut Context<Self>) {
        if self.review_scope != scope {
            self.review_scope = scope;
            self.ensure_review_selection_and_load(cx);
            cx.notify();
        }
    }

    pub fn set_review_diff_mode(&mut self, mode: ReviewDiffMode, cx: &mut Context<Self>) {
        if self.review_diff_mode != mode {
            self.review_diff_mode = mode;
            self.load_review_diff(cx);
            cx.notify();
        }
    }

    pub fn select_review_file(&mut self, path: String, cx: &mut Context<Self>) {
        self.review_selected_path = Some(path);
        self.review_selected_change_id = None;
        self.review_comment_target = None;
        self.load_review_diff(cx);
        cx.notify();
    }

    pub fn open_undo_precheck(&mut self, cx: &mut Context<Self>) {
        if !self.review_open {
            self.open_review(None, cx);
        }
        self.review_undo_precheck = true;
        cx.notify();
    }

    pub fn revert_change_hunk(
        &mut self,
        change_id: String,
        hunk_index: usize,
        cx: &mut Context<Self>,
    ) {
        let sid = self.thread.session_id.clone();
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                agent_change_revert_hunk_impl(state, &change_id, hunk_index)?;
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
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).chat().act_reverted_change().into(),
                        ));
                        this.load_review_diff(cx);
                    }
                    Err(e) => {
                        this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                        this.refresh_changes_only(cx);
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn undo_changes_with_force(
        &mut self,
        change_ids: Vec<String>,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        if change_ids.is_empty() {
            return;
        }
        let sid = self.thread.session_id.clone();
        let sid_check = sid.clone();
        self.review_undo_precheck = false;
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let touched = agent_change_undo_impl(state, &change_ids, force)?;
                let list = agent_changes_list_impl(state, &sid)?;
                Ok::<_, spanbay_desktop::core::error::AppError>((touched, list))
            },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                match res {
                    Ok((touched, list)) => {
                        let any_conflict = touched
                            .iter()
                            .any(|c| c.status == FileChangeStatus::Conflict);
                        this.persisted_changes = list;
                        this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                        this.review_diff_cache = None;
                        if any_conflict && !force {
                            this.review_undo_precheck = true;
                        } else {
                            this.status_msg = Some((
                                Tone::Success,
                                crate::ui::tr(cx).chat().act_reverted_change().into(),
                            ));
                        }
                        this.load_review_diff(cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn refresh_changes_only(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_changes_list_impl(state, &sid),
            move |this, res, cx| {
                if this.thread.session_id == sid_check
                    && let Ok(list) = res
                {
                    this.persisted_changes = list;
                    this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                    this.load_review_diff(cx);
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn submit_review_comment(&mut self, cx: &mut Context<Self>) {
        let Some((path, line)) = self.review_comment_target.take() else {
            return;
        };
        let comment = self.review_comment_input.read(cx).value().trim().to_owned();
        if !comment.is_empty() {
            self.diff_comments.push(DiffCommentChip {
                path,
                line,
                comment,
            });
        }
        cx.notify();
    }

    pub(super) fn scoped_changes(&self) -> Vec<&FileChange> {
        if self.review_scope == ReviewScope::Turn {
            let turn_run_id = self
                .thread
                .active_run
                .as_ref()
                .map(|r| r.run_id.as_str())
                .or(self.latest_session_run_id.as_deref())
                .or_else(|| self.persisted_changes.first().map(|c| c.run_id.as_str()));
            if let Some(rid) = turn_run_id {
                let turn_rows: Vec<&FileChange> = self
                    .persisted_changes
                    .iter()
                    .filter(|c| c.run_id == rid)
                    .collect();
                if !turn_rows.is_empty() {
                    return turn_rows;
                }
            }
        }
        self.persisted_changes.iter().collect()
    }

    pub(super) fn grouped_review_files(&self) -> Vec<GroupedFileChange> {
        let rows = self.scoped_changes();
        let mut groups: Vec<GroupedFileChange> = Vec::new();
        for fc in rows {
            if let Some(g) = groups.iter_mut().find(|g| g.path == fc.path) {
                g.kind = fc.kind;
                if fc.status != FileChangeStatus::Undone {
                    g.added = g.added.saturating_add(fc.added);
                    g.removed = g.removed.saturating_add(fc.removed);
                    g.active_ids.push(fc.id.clone());
                }
                g.change_ids.push(fc.id.clone());
                g.status = merge_file_status(g.status, fc.status);
            } else {
                let active = fc.status != FileChangeStatus::Undone;
                groups.push(GroupedFileChange {
                    path: fc.path.clone(),
                    kind: fc.kind,
                    status: fc.status,
                    added: if active { fc.added } else { 0 },
                    removed: if active { fc.removed } else { 0 },
                    change_ids: vec![fc.id.clone()],
                    active_ids: if active {
                        vec![fc.id.clone()]
                    } else {
                        Vec::new()
                    },
                    is_deleted: fc.after_sha256.is_none(),
                });
            }
        }
        groups
    }

    #[must_use]
    pub fn can_revert_hunk_in_current_view(&self, path: &str) -> bool {
        let groups = self.grouped_review_files();
        let Some(selected) = groups.iter().find(|g| g.path == path) else {
            return false;
        };
        let target_id = self
            .review_selected_change_id
            .clone()
            .filter(|id| selected.change_ids.contains(id))
            .or_else(|| selected.active_ids.first().cloned());
        let is_latest_active =
            target_id.is_some() && target_id == selected.active_ids.first().cloned();
        let indices_match_core =
            self.review_diff_mode == ReviewDiffMode::PerEdit || selected.active_ids.len() == 1;
        selected.status == FileChangeStatus::Pending
            && !selected.is_deleted
            && is_latest_active
            && indices_match_core
    }

    pub(super) fn ensure_review_selection_and_load(&mut self, cx: &mut Context<Self>) {
        let groups = self.grouped_review_files();
        let valid = self
            .review_selected_path
            .as_ref()
            .is_some_and(|p| groups.iter().any(|g| &g.path == p));
        if !valid {
            self.review_selected_path = groups.first().map(|g| g.path.clone());
            self.review_selected_change_id = None;
        }
        self.load_review_diff(cx);
    }

    pub(super) fn load_review_diff(&mut self, cx: &mut Context<Self>) {
        let groups = self.grouped_review_files();
        let Some(selected) = self
            .review_selected_path
            .as_ref()
            .and_then(|p| groups.iter().find(|g| &g.path == p))
            .or_else(|| groups.first())
        else {
            self.review_diff_cache = None;
            self.review_diff_error = None;
            return;
        };
        let ids_for_net = selected.active_ids.clone();
        let target_id = self
            .review_selected_change_id
            .clone()
            .filter(|id| selected.change_ids.contains(id))
            .or_else(|| selected.active_ids.first().cloned())
            .or_else(|| selected.change_ids.first().cloned());
        let Some(single_id) = target_id else {
            return;
        };
        let use_net = self.review_diff_mode == ReviewDiffMode::Net && ids_for_net.len() > 1;
        let cache_key = if use_net {
            format!("net:{}", ids_for_net.join(","))
        } else {
            format!("single:{single_id}")
        };
        if self
            .review_diff_cache
            .as_ref()
            .is_some_and(|c| c.key == cache_key)
        {
            return;
        }
        let key_check = cache_key;
        let task = spawn_blocking_into(
            cx,
            move |state| {
                if use_net {
                    agent_change_net_diff_impl(state, &ids_for_net)
                        .or_else(|_| agent_change_diff_impl(state, &single_id))
                } else {
                    agent_change_diff_impl(state, &single_id)
                }
            },
            move |this, res, cx| {
                match res {
                    Ok(diff) => {
                        let (hunks, omitted) = if diff.binary {
                            (Vec::new(), 0)
                        } else {
                            parse_unified_diff_hunks(&diff.unified_diff)
                        };
                        this.review_diff_cache = Some(CachedReviewDiff {
                            key: key_check,
                            hunks,
                            omitted,
                            binary: diff.binary,
                        });
                        this.review_diff_error = None;
                    }
                    Err(e) => {
                        this.review_diff_error = Some((key_check, e.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn set_active_binding(
        &mut self,
        workspace_id: Option<String>,
        env_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let ws_changed = self.active_workspace_id != workspace_id;
        let env_changed = self.active_environment_id != env_id;
        if !ws_changed && !env_changed {
            return;
        }
        self.active_workspace_id = workspace_id;
        self.turn_inputs_version = self.turn_inputs_version.wrapping_add(1);
        self.active_environment_id = env_id;
        if ws_changed {
            self.refresh_catalog(cx);
        }
        cx.notify();
    }

    pub fn set_active_workspace(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) {
        if self.active_workspace_id == workspace_id {
            return;
        }
        self.active_workspace_id = workspace_id;
        self.turn_inputs_version = self.turn_inputs_version.wrapping_add(1);
        self.active_environment_id = None;
        self.refresh_catalog(cx);
        cx.notify();
    }

    pub fn set_active_environment(&mut self, env_id: Option<String>, cx: &mut Context<Self>) {
        if self.active_environment_id == env_id {
            return;
        }
        self.active_environment_id = env_id;
        cx.notify();
    }

    pub fn has_any_active_board_run(&self) -> bool {
        self.board.as_ref().is_some_and(|b| {
            !b.running.is_empty() || b.needs_you.iter().any(|r| r.run.status.is_active())
        })
    }

    #[must_use]
    pub fn pending_attention_count(&self) -> usize {
        self.board.as_ref().map_or_else(
            || {
                self.thread.pending_approvals.len()
                    + self.thread.open_gates.len()
                    + usize::from(self.thread.cost_pause.is_some())
            },
            |b| b.needs_you.len(),
        )
    }
}
