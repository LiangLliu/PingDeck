//! Changes a run's commands made to the chat's folder outside the file tools.
//!
//! Single responsibility: before the first call of a run that can change files without the
//! journal (`shell` and the other process tools, MCP tools), take a bounded listing of the folder
//! (path, size and modification time of every file `.gitignore` does not exclude, at most
//! [`MAX_FILES`]); when the run ends, list it again and name the files that were created, changed
//! or removed and are not covered by the journal. The Changes tab cannot undo those, so the run
//! says so in a coded notice (`untracked_changes`). This is a listing, not a snapshot: nothing is
//! copied and nothing can be restored from it.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::UNIX_EPOCH;

use crate::agent::model::{AgentEvent, FileChange};
use crate::app::{AppState, current_time_ms};
use crate::crypto::sha256_hex;

/// Files a listing covers; a larger folder is not checked.
pub(super) const MAX_FILES: usize = 20_000;
/// Paths the notice names before it says how many more.
const NAMED_PATHS: usize = 5;
/// The notice code the UI localises.
pub(super) const UNTRACKED_CHANGES: &str = "untracked_changes";

/// Size and modification time of one file.
type Stamp = (u64, u128);

/// Every listed file by its folder-relative path (`/`-separated).
type Listing = HashMap<String, Stamp>;

/// Lists `folder`: files only, `.git` skipped, `.gitignore` and `.ignore` rules applied, hidden
/// files included. `None` when it holds more than [`MAX_FILES`] files or cannot be read.
pub(super) fn list_folder(folder: &Path) -> Option<Listing> {
    let mut listing = Listing::new();
    let walker = ignore::WalkBuilder::new(folder)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for entry in walker {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(folder) else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos());
        if listing.len() >= MAX_FILES {
            return None;
        }
        listing.insert(
            relative.to_string_lossy().replace('\\', "/"),
            (metadata.len(), modified),
        );
    }
    Some(listing)
}

/// The paths that differ between two listings: created, changed or removed, sorted.
pub(super) fn changed_paths(before: &Listing, after: &Listing) -> BTreeSet<String> {
    let mut changed: BTreeSet<String> = after
        .iter()
        .filter(|(path, stamp)| before.get(*path) != Some(*stamp))
        .map(|(path, _)| path.clone())
        .collect();
    changed.extend(
        before
            .keys()
            .filter(|path| !after.contains_key(*path))
            .cloned(),
    );
    changed
}

/// Drops from `changed` the files whose newest journaled change (made in `folder` since
/// `since_ms`) is still what the file holds: the Changes tab covers those.
pub(super) fn without_journaled(
    mut changed: BTreeSet<String>,
    journal: &[FileChange],
    folder: &Path,
    since_ms: i64,
) -> BTreeSet<String> {
    let folder_text = folder.to_string_lossy();
    let mut newest: HashMap<&str, Option<&str>> = HashMap::new();
    for row in journal.iter().filter(|row| {
        row.created_at_ms >= since_ms && row.folder.as_deref() == Some(folder_text.as_ref())
    }) {
        // The journal is oldest first: the last row of a path is its newest change.
        newest.insert(row.path.as_str(), row.after_sha256.as_deref());
    }
    changed.retain(|path| match newest.get(path.as_str()) {
        Some(after) => {
            let now = std::fs::read(folder.join(path))
                .ok()
                .map(|bytes| sha256_hex(&bytes));
            now.as_deref() != *after
        }
        None => true,
    });
    changed
}

/// The listing taken before the first unjournaled call of a run.
struct Armed {
    folder: PathBuf,
    since_ms: i64,
    /// `None`: the folder was too large to list, so nothing is reported.
    before: Option<Listing>,
}

/// One run's watch over its folder.
#[derive(Default)]
pub(super) struct UntrackedWatch(Mutex<Option<Armed>>);

impl UntrackedWatch {
    /// Takes the "before" listing of `folder` unless this run already did. Lists on the blocking
    /// pool.
    pub(super) async fn arm(&self, folder: Option<&Path>) {
        let Some(folder) = folder else { return };
        if self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
        {
            return;
        }
        let since_ms = current_time_ms();
        let path = folder.to_path_buf();
        let before = tokio::task::spawn_blocking(move || list_folder(&path))
            .await
            .ok()
            .flatten();
        let mut armed = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if armed.is_none() {
            *armed = Some(Armed {
                folder: folder.to_path_buf(),
                since_ms,
                before,
            });
        }
    }

    /// At the end of the run: the notice naming the files its commands changed outside the
    /// journal of chat `chat_id`, or `None` when there are none (or the folder was not listed).
    pub(super) async fn report(&self, state: &AppState, chat_id: &str) -> Option<AgentEvent> {
        let armed = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()?;
        let before = armed.before?;
        let (state, chat_id) = (state.clone(), chat_id.to_owned());
        let changed = tokio::task::spawn_blocking(move || {
            let after = list_folder(&armed.folder)?;
            let changed = changed_paths(&before, &after);
            if changed.is_empty() {
                return Some(changed);
            }
            let journal = state.store().list_chat_file_changes(&chat_id).ok()?;
            Some(without_journaled(
                changed,
                &journal,
                &armed.folder,
                armed.since_ms,
            ))
        })
        .await
        .ok()
        .flatten()?;
        (!changed.is_empty()).then(|| notice(&changed))
    }
}

/// The `untracked_changes` notice for `changed`.
fn notice(changed: &BTreeSet<String>) -> AgentEvent {
    let count = changed.len();
    let named: Vec<&str> = changed
        .iter()
        .take(NAMED_PATHS)
        .map(String::as_str)
        .collect();
    let more = count.saturating_sub(named.len());
    let paths = if more > 0 {
        format!("{}, and {more} more", named.join(", "))
    } else {
        named.join(", ")
    };
    AgentEvent::coded_notice(
        UNTRACKED_CHANGES,
        format!(
            "{count} file{} in the folder changed during this turn outside the file tools \
             (commands, MCP tools or other programs): {paths}. The Changes tab does not list \
             these and cannot undo them.",
            if count == 1 { "" } else { "s" }
        ),
        &[("count", count.to_string()), ("paths", paths)],
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::agent::model::{FileChangeKind, FileChangeStatus};

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pd-untracked-{}-{}",
            std::process::id(),
            crate::app::factory_service::new_entity_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_listing_names_created_changed_and_removed_files_and_honours_gitignore() {
        let dir = temp_dir();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.join("kept.txt"), "same").unwrap();
        std::fs::write(dir.join("edited.txt"), "one").unwrap();
        std::fs::write(dir.join("removed.txt"), "gone soon").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let before = list_folder(&dir).unwrap();

        std::fs::write(dir.join("edited.txt"), "one more").unwrap();
        std::fs::remove_file(dir.join("removed.txt")).unwrap();
        std::fs::write(dir.join("created.txt"), "new").unwrap();
        std::fs::write(dir.join("target/build.bin"), "ignored").unwrap();
        std::fs::write(dir.join(".git/index"), "ignored too").unwrap();
        let after = list_folder(&dir).unwrap();

        let changed: Vec<String> = changed_paths(&before, &after).into_iter().collect();
        assert_eq!(changed, ["created.txt", "edited.txt", "removed.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_the_journal_covers_is_dropped_only_while_it_still_holds_the_journaled_bytes() {
        let dir = temp_dir();
        std::fs::write(dir.join("tool.txt"), "written by edit_file").unwrap();
        std::fs::write(dir.join("both.txt"), "then a command changed it").unwrap();
        let row = |path: &str, after: &str| FileChange {
            id: path.into(),
            session_id: "s".into(),
            run_id: "r".into(),
            call_id: "c".into(),
            path: path.into(),
            folder: Some(dir.to_string_lossy().into_owned()),
            kind: FileChangeKind::Edit,
            before_sha256: None,
            after_sha256: Some(sha256_hex(after.as_bytes())),
            added: 1,
            removed: 0,
            status: FileChangeStatus::Pending,
            created_at_ms: 10,
        };
        let journal = vec![
            row("tool.txt", "written by edit_file"),
            row("both.txt", "written by edit_file"),
        ];
        let changed: BTreeSet<String> = ["tool.txt", "both.txt", "other.txt"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let left: Vec<String> = without_journaled(changed.clone(), &journal, &dir, 5)
            .into_iter()
            .collect();
        assert_eq!(left, ["both.txt", "other.txt"]);
        // A journal row from before the watch started covers nothing.
        assert_eq!(without_journaled(changed, &journal, &dir, 11).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_notice_counts_every_file_and_names_the_first_five() {
        let changed: BTreeSet<String> = (0..7).map(|n| format!("f{n}.txt")).collect();
        let AgentEvent::Notice { code, args, .. } = notice(&changed) else {
            panic!("a notice");
        };
        assert_eq!(code.as_deref(), Some(UNTRACKED_CHANGES));
        let args = args.unwrap();
        assert_eq!(args["count"], "7");
        assert!(args["paths"].ends_with("and 2 more"));
    }
}
