//! What belongs to a branch of a chat, and what a branch switch leaves behind
//!
//! Single responsibility: the run-side effects of moving a chat's head. The read-before-write
//! record of the file tools is per branch head: [`on_head_moved`] clears it, so the first
//! `edit_file` on the new branch fails with "Read <path> first" until the file is read there; a git
//! branch switch outside the app counts too, seen at the next run's start ([`on_run_start`]). The
//! files an abandoned branch changed stay on disk: [`agent_changes_off_branch_impl`] lists its
//! pending changes (the Changes tab shows "Files on disk may not match this branch" with "Undo
//! this branch's changes"; the undo is `agent_change_undo_impl` over those ids). The plan (task
//! list) of a branch is the store's: it re-derives `AgentSession.todos` from the new head.

use crate::agent::model::FileChange;
use crate::error::AppError;

use super::sessions::load_session;
use crate::app::{AppState, command_error};

/// The head of `session_id` moved (a branch switch, an edit, a retry, a deleted message): what the
/// file tools last read belonged to the branch that was left.
pub fn on_head_moved(state: &AppState, session_id: &str) {
    state.agent.read_hashes.forget_reads(session_id);
}

/// A run of `session_id` starts with the trusted folder on `branch` (`None`: untrusted, or no
/// branch): when an earlier run saw another branch, git switched it underneath the chat, so the
/// head moved for the file tools too.
pub(super) fn on_run_start(state: &AppState, session_id: &str, branch: Option<&str>) {
    if state.agent.read_hashes.note_branch(session_id, branch) {
        on_head_moved(state, session_id);
    }
}

/// The pending file changes of the chat's other branches, newest first: files on disk that may not
/// match the branch the chat is on now.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown chat.
pub fn agent_changes_off_branch_impl(
    state: &AppState,
    session_id: &str,
) -> Result<Vec<FileChange>, AppError> {
    load_session(state, session_id)?;
    let store = state.store();
    let ids = store
        .off_branch_pending_change_ids(session_id)
        .map_err(command_error)?;
    let mut rows = store.get_file_changes(&ids).map_err(command_error)?;
    rows.reverse();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn a_branch_switch_between_runs_voids_the_read_before_write_record() {
        let state = AppState::open_in_memory().unwrap();
        let hashes = &state.agent.read_hashes;
        let file = Path::new("/w/src/lib.rs");
        on_run_start(&state, "s", Some("main"));
        hashes.record("s", file, "h1".to_owned());
        on_run_start(&state, "s", Some("main"));
        assert_eq!(hashes.get("s", file).as_deref(), Some("h1"));
        on_run_start(&state, "s", Some("feature"));
        assert_eq!(
            hashes.get("s", file),
            None,
            "edit_file must read the file again"
        );
    }
}
