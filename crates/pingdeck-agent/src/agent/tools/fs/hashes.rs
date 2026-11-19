//! The read-before-edit guard: what each session's file tools last read.
//!
//! Single responsibility: remember the SHA-256 of a file's content when `read_file` returned it
//! (or a write/edit of this session produced it), keyed by session and resolved path. `write_file`
//! and `edit_file` on an existing file compare the file's current hash with this record and refuse
//! when it differs or is missing, so the model never overwrites changes it has not seen.
//! A session keeps its [`MAX_PER_SESSION`] most recently recorded files; an evicted file must be
//! read again before it can be edited, which is what the refusal tells the model.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// Files one session remembers; a long chat that reads tens of thousands of files stays bounded.
const MAX_PER_SESSION: usize = 2_000;

/// One session's record: path → (recording order, hash).
#[derive(Debug, Default)]
struct SessionHashes {
    files: HashMap<PathBuf, (u64, String)>,
    /// Order of the latest recording.
    clock: u64,
    /// The folder's git branch when the session's last run started.
    branch: Option<String>,
}

/// `session id` → what the session last saw of each path (hex SHA-256 of the content). Clones
/// share the same map.
#[derive(Debug, Clone, Default)]
pub struct ReadHashes {
    seen: Arc<Mutex<HashMap<String, SessionHashes>>>,
}

impl ReadHashes {
    /// An empty record.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Remembers that `session_id` saw `path` with content hash `sha256`, dropping the session's
    /// least recently recorded file when it is over [`MAX_PER_SESSION`].
    pub fn record(&self, session_id: &str, path: &Path, sha256: String) {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let session = seen.entry(session_id.to_owned()).or_default();
        session.clock += 1;
        session
            .files
            .insert(path.to_path_buf(), (session.clock, sha256));
        if session.files.len() > MAX_PER_SESSION
            && let Some(oldest) = session
                .files
                .iter()
                .min_by_key(|(_, (order, _))| *order)
                .map(|(path, _)| path.clone())
        {
            session.files.remove(&oldest);
        }
    }

    /// The hash `session_id` last saw of `path`.
    #[must_use]
    pub fn get(&self, session_id: &str, path: &Path) -> Option<String> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session_id)?
            .files
            .get(path)
            .map(|(_, sha256)| sha256.clone())
    }

    /// Records the folder branch `branch` at the start of a run of `session_id`; true when an
    /// earlier run of the session saw another branch (what it read may no longer be on disk).
    pub fn note_branch(&self, session_id: &str, branch: Option<&str>) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let session = seen.entry(session_id.to_owned()).or_default();
        let moved = session.clock > 0 && session.branch.as_deref() != branch;
        session.branch = branch.map(str::to_owned);
        moved
    }

    /// Forgets what a session read and keeps its branch (the head moved).
    pub fn forget_reads(&self, session_id: &str) {
        if let Some(session) = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(session_id)
        {
            session.files.clear();
        }
    }

    /// Forgets everything a session read (chat delete).
    pub fn forget_session(&self, session_id: &str) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_keeps_only_its_most_recent_files() {
        let hashes = ReadHashes::new();
        for index in 0..=MAX_PER_SESSION {
            hashes.record("s", Path::new(&format!("/w/{index}")), format!("h{index}"));
        }
        hashes.record("other", Path::new("/w/0"), "kept".to_owned());
        assert_eq!(hashes.get("s", Path::new("/w/0")), None, "the oldest went");
        assert_eq!(
            hashes.get("s", Path::new("/w/1")).as_deref(),
            Some("h1"),
            "the rest stay"
        );
        assert_eq!(
            hashes
                .get("s", Path::new(&format!("/w/{MAX_PER_SESSION}")))
                .as_deref(),
            Some(format!("h{MAX_PER_SESSION}").as_str())
        );
        // Re-recording refreshes a file; sessions never evict each other.
        hashes.record("s", Path::new("/w/1"), "new".to_owned());
        hashes.record("s", Path::new("/w/new"), "n".to_owned());
        assert_eq!(hashes.get("s", Path::new("/w/1")).as_deref(), Some("new"));
        assert_eq!(hashes.get("s", Path::new("/w/2")), None);
        assert_eq!(
            hashes.get("other", Path::new("/w/0")).as_deref(),
            Some("kept")
        );
        hashes.forget_session("s");
        assert_eq!(hashes.get("s", Path::new("/w/1")), None);
    }
}
