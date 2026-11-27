//! The text of one journaled file change: what the Changes tab shows when a file row is
//! expanded.
//!
//! Single responsibility: rebuild the before and after bytes of a [`FileChange`] and render them
//! as a unified diff. The before bytes are the journal's blob. The after bytes are not stored, so
//! they are the blob a later change of the same file kept as its "before" (a file's changes chain),
//! or, for its newest change, the file on disk while it still holds what the change produced.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agent::model::FileChange;
use crate::agent::tools::fs::{resolve_workspace_path, unified_diff};
use crate::crypto::sha256_hex;
use crate::error::AppError;

use super::attachments::agent_blob_read_impl;
use crate::app::{AppState, command_error};

/// Largest diff text returned, in bytes (the UI shows 120 lines and a "Show more").
const MAX_DIFF_BYTES: usize = 512 * 1024;

/// Largest file side that is diffed, in bytes (the file tools cannot write more).
const MAX_SIDE_BYTES: usize = 5 * 1024 * 1024;

/// The change of one file as the Changes tab expands it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeDiff {
    /// `diff -u` text with three lines of context; empty for a binary file.
    pub unified_diff: String,
    /// One side is not text: nothing to show but "Binary file changed".
    pub binary: bool,
}

/// Whether `bytes` are not text: a NUL byte or invalid UTF-8.
fn is_binary(bytes: &[u8]) -> bool {
    memchr::memchr(0, bytes).is_some() || std::str::from_utf8(bytes).is_err()
}

/// The bytes the file had after `row` (none after a delete).
fn after_bytes(
    state: &AppState,
    row: &FileChange,
    chat_journal: &[FileChange],
    workspace: &Path,
) -> Result<Vec<u8>, AppError> {
    if row.after_sha256.is_none() && row.before_sha256.is_some() {
        return Ok(Vec::new());
    }
    let later = chat_journal
        .iter()
        .skip_while(|other| other.id != row.id)
        .skip(1)
        .find(|other| {
            other.path == row.path && other.folder == row.folder && other.before_sha256.is_some()
        });
    if let Some(sha256) = later.and_then(|other| other.before_sha256.as_deref()) {
        return agent_blob_read_impl(state, sha256);
    }
    let target = resolve_workspace_path(workspace, &row.path, false)?;
    let bytes =
        std::fs::read(&target).map_err(|error| AppError::io(format!("{}: {error}", row.path)))?;
    if Some(sha256_hex(&bytes).as_str()) != row.after_sha256.as_deref() {
        return Err(AppError::conflict(
            "the file changed after this edit, so the edit can no longer be shown",
        ));
    }
    Ok(bytes)
}

/// Most journal rows one net diff spans (one file's edits in one turn).
const MAX_NET_ROWS: usize = 500;

/// The folder a change was made in and the journal of its chat, oldest first.
fn change_scope(state: &AppState, row: &FileChange) -> Result<(String, Vec<FileChange>), AppError> {
    let session = state
        .store()
        .get_agent_session(&row.session_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("chat session {} not found", row.session_id)))?;
    // The folder the change recorded; an old row without one is read in the chat's folder.
    let workspace = row
        .folder
        .clone()
        .or(session.workspace_dir)
        .ok_or_else(|| AppError::validation("This chat has no folder"))?;
    let chat = session.parent_session_id.unwrap_or(session.id);
    let journal = state
        .store()
        .list_chat_file_changes(&chat)
        .map_err(command_error)?;
    Ok((workspace, journal))
}

/// `path` from the file before the first change (its blob, nothing for a creation) to the file
/// after the last one.
fn render(
    state: &AppState,
    path: &str,
    before_sha256: Option<&str>,
    after: &[u8],
) -> Result<ChangeDiff, AppError> {
    let before = before_sha256
        .map(|sha256| agent_blob_read_impl(state, sha256))
        .transpose()?;
    if before
        .as_ref()
        .is_some_and(|bytes| bytes.len() > MAX_SIDE_BYTES)
        || after.len() > MAX_SIDE_BYTES
    {
        return Err(AppError::validation("The file is too large to show a diff"));
    }
    if is_binary(after) || before.as_deref().is_some_and(is_binary) {
        return Ok(ChangeDiff {
            unified_diff: String::new(),
            binary: true,
        });
    }
    // Both sides were checked as UTF-8 above.
    let before_text = before.as_deref().map(String::from_utf8_lossy);
    Ok(ChangeDiff {
        unified_diff: unified_diff(
            path,
            before_text.as_deref(),
            &String::from_utf8_lossy(after),
            MAX_DIFF_BYTES,
        ),
        binary: false,
    })
}

/// The unified diff of a journaled change. Blocking: the store, the blob store and the disk.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown change or a missing blob, [`AppError::Validation`] for a
/// chat without a folder, a path outside it or a side over 5 MiB, [`AppError::Conflict`] when the
/// file was changed or removed since (and no later change recorded its content).
pub fn agent_change_diff_impl(state: &AppState, change_id: &str) -> Result<ChangeDiff, AppError> {
    let row = state
        .store()
        .get_file_changes(&[change_id.to_owned()])
        .map_err(command_error)?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::not_found(format!("change {change_id} not found")))?;
    if super::request_changes::is_request_change(&row) {
        return super::request_changes::request_change_diff(state, &row);
    }
    let (workspace, journal) = change_scope(state, &row)?;
    let after = after_bytes(state, &row, &journal, Path::new(&workspace))?;
    render(state, &row.path, row.before_sha256.as_deref(), &after)
}

/// The net change of one file over several journaled changes (the edits of one turn): the file
/// before the earliest of them against the file after the latest, so edits that undo each other
/// show as no change. Blocking, like [`agent_change_diff_impl`].
///
/// # Errors
/// As [`agent_change_diff_impl`], plus [`AppError::Validation`] when the changes are not all of
/// one file of one chat, name a saved request (each of its edits has its own diff), or number
/// none or more than 500.
pub fn agent_change_net_diff_impl(
    state: &AppState,
    change_ids: &[String],
) -> Result<ChangeDiff, AppError> {
    if change_ids.is_empty() || change_ids.len() > MAX_NET_ROWS {
        return Err(AppError::validation(format!(
            "A combined diff spans 1 to {MAX_NET_ROWS} changes"
        )));
    }
    let rows = state
        .store()
        .get_file_changes(change_ids)
        .map_err(command_error)?;
    let Some(first) = rows.first() else {
        return Err(AppError::not_found("change not found"));
    };
    if rows.len() != change_ids.len() {
        return Err(AppError::not_found("change not found"));
    }
    if rows.iter().any(|row| {
        row.session_id != first.session_id || row.path != first.path || row.folder != first.folder
    }) {
        return Err(AppError::validation(
            "A combined diff needs changes of one file in one chat",
        ));
    }
    if super::request_changes::is_request_change(first) {
        return Err(AppError::validation(
            "A saved request's edits are shown one by one",
        ));
    }
    let (workspace, journal) = change_scope(state, first)?;
    // The journal's order (oldest first) decides which change is the earliest and the latest.
    let position = |row: &FileChange| journal.iter().position(|other| other.id == row.id);
    let earliest = rows.iter().min_by_key(|row| position(row)).unwrap_or(first);
    let latest = rows.iter().max_by_key(|row| position(row)).unwrap_or(first);
    let after = after_bytes(state, latest, &journal, Path::new(&workspace))?;
    render(
        state,
        &first.path,
        earliest.before_sha256.as_deref(),
        &after,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::model::{AgentSessionInput, FileChangeKind, FileChangeStatus};
    use crate::agent_service::{agent_blob_put_impl, agent_session_create_impl};

    struct Fixture {
        state: AppState,
        /// The temp folder holding both the data directory and the chat's folder.
        top: std::path::PathBuf,
        root: std::path::PathBuf,
        session_id: String,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("pd-diff-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let top = root.canonicalize().unwrap();
            std::fs::create_dir_all(top.join("data")).unwrap();
            let state = AppState::open(top.join("data").join("spanbay.sqlite3")).unwrap();
            state.on_start().unwrap();
            let root = top.join("ws");
            std::fs::create_dir_all(&root).unwrap();
            let session = agent_session_create_impl(
                &state,
                AgentSessionInput {
                    workspace_dir: Some(root.to_string_lossy().into_owned()),
                    ..AgentSessionInput::default()
                },
            )
            .unwrap();
            Self {
                state,
                top,
                root,
                session_id: session.id,
            }
        }

        /// Journals a change from `before` to `after`; `at` orders the rows.
        fn change(
            &self,
            id: &str,
            path: &str,
            before: Option<&[u8]>,
            after: &[u8],
            at: i64,
        ) -> String {
            let row = FileChange {
                id: id.to_owned(),
                session_id: self.session_id.clone(),
                run_id: "run".to_owned(),
                call_id: format!("call_{id}"),
                path: path.to_owned(),
                folder: Some(self.root.to_string_lossy().into_owned()),
                kind: if before.is_some() {
                    FileChangeKind::Edit
                } else {
                    FileChangeKind::Create
                },
                before_sha256: before.map(|bytes| agent_blob_put_impl(&self.state, bytes).unwrap()),
                after_sha256: Some(sha256_hex(after)),
                added: 0,
                removed: 0,
                status: FileChangeStatus::Pending,
                created_at_ms: at,
            };
            self.state.store().insert_file_change(&row).unwrap();
            row.id
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.top);
        }
    }

    #[test]
    fn an_edit_is_diffed_against_the_file_and_a_creation_against_nothing() {
        let fx = Fixture::new("edit");
        std::fs::write(fx.root.join("a.txt"), "one\nthree\n").unwrap();
        let edit = fx.change("e1", "a.txt", Some(b"one\ntwo\n"), b"one\nthree\n", 1);
        let diff = agent_change_diff_impl(&fx.state, &edit).unwrap();
        assert!(!diff.binary);
        assert!(diff.unified_diff.starts_with("--- a/a.txt\n+++ b/a.txt\n"));
        assert!(
            diff.unified_diff.contains("-two\n+three\n"),
            "{}",
            diff.unified_diff
        );

        std::fs::write(fx.root.join("new.txt"), "fresh\n").unwrap();
        let create = fx.change("c1", "new.txt", None, b"fresh\n", 2);
        let diff = agent_change_diff_impl(&fx.state, &create).unwrap();
        assert!(
            diff.unified_diff
                .starts_with("--- /dev/null\n+++ b/new.txt\n")
        );
        assert!(diff.unified_diff.contains("+fresh\n"));
    }

    #[test]
    fn an_earlier_edit_is_shown_from_the_blob_the_next_edit_kept() {
        let fx = Fixture::new("chain");
        std::fs::write(fx.root.join("a.txt"), "v3\n").unwrap();
        let first = fx.change("e1", "a.txt", Some(b"v1\n"), b"v2\n", 1);
        fx.change("e2", "a.txt", Some(b"v2\n"), b"v3\n", 2);
        let diff = agent_change_diff_impl(&fx.state, &first).unwrap();
        assert!(
            diff.unified_diff.contains("-v1\n+v2\n"),
            "{}",
            diff.unified_diff
        );
    }

    #[test]
    fn a_file_changed_since_conflicts_and_binary_files_are_flagged() {
        let fx = Fixture::new("conflict");
        let edit = fx.change("e1", "a.txt", Some(b"one\n"), b"two\n", 1);
        std::fs::write(fx.root.join("a.txt"), "someone else\n").unwrap();
        assert!(matches!(
            agent_change_diff_impl(&fx.state, &edit),
            Err(AppError::Conflict { .. })
        ));
        std::fs::remove_file(fx.root.join("a.txt")).unwrap();
        assert!(
            agent_change_diff_impl(&fx.state, &edit).is_err(),
            "a removed file has no after"
        );

        std::fs::write(fx.root.join("b.bin"), [0_u8, 159, 146, 150]).unwrap();
        let binary = fx.change("b1", "b.bin", None, &[0_u8, 159, 146, 150], 2);
        let diff = agent_change_diff_impl(&fx.state, &binary).unwrap();
        assert!(diff.binary && diff.unified_diff.is_empty());

        assert!(matches!(
            agent_change_diff_impl(&fx.state, "nope"),
            Err(AppError::NotFound { .. })
        ));
    }

    #[test]
    fn a_net_diff_spans_the_earliest_before_and_the_latest_after() {
        let fx = Fixture::new("net");
        std::fs::write(fx.root.join("a.txt"), "v1\n").unwrap();
        let first = fx.change("e1", "a.txt", Some(b"v1\n"), b"v1\nadded\n", 1);
        let second = fx.change("e2", "a.txt", Some(b"v1\nadded\n"), b"v1\n", 2);
        // The second edit took back what the first added: no net change.
        let net = agent_change_net_diff_impl(&fx.state, &[second.clone(), first.clone()]).unwrap();
        assert!(!net.unified_diff.contains("added"), "{}", net.unified_diff);

        let other = fx.change("o1", "b.txt", None, b"b\n", 3);
        assert!(matches!(
            agent_change_net_diff_impl(&fx.state, &[first, other]),
            Err(AppError::Validation { .. })
        ));
        assert!(matches!(
            agent_change_net_diff_impl(&fx.state, &[]),
            Err(AppError::Validation { .. })
        ));
        assert!(matches!(
            agent_change_net_diff_impl(&fx.state, &[second, "nope".to_owned()]),
            Err(AppError::NotFound { .. })
        ));
    }
}
