//! Where the full output of tool calls is kept and how it goes away.
//!
//! Single responsibility: the layout `<data dir>/tool-output/<session id>/<call id>` of the spill
//! files, the three retention caps, and their removal (with the chat, on purge, or by eviction).
//! The files live in the app's data folder, outside every Folder and unreadable from the sandbox;
//! the runner writes them (already redacted) when a result is cut, `read_output` and the UI read
//! them in pages; they are not part of the chat search index. They are plain files, not encrypted.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::agent::limits::{
    TOOL_OUTPUT_CHAT_BYTES, TOOL_OUTPUT_FILE_BYTES, TOOL_OUTPUT_TOTAL_BYTES,
};
use crate::agent::tools::shell_meta::valid_call_id;

/// The name of the tool that reads a spill file back.
pub const READ_OUTPUT: &str = "read_output";

/// What a handle whose file was evicted or purged answers.
pub const CLEARED: &str =
    "This output was cleared to free space; run the tool again if you still need it.";

/// The retention caps of spill files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpillCaps {
    /// Bytes kept of one call's output.
    pub file: u64,
    /// Bytes kept for one chat; its oldest files go first.
    pub chat: u64,
    /// Bytes kept app-wide; the least recently used chat directory goes first.
    pub total: u64,
}

impl SpillCaps {
    /// The caps of the run limits table.
    pub const DEFAULT: Self = Self {
        file: TOOL_OUTPUT_FILE_BYTES,
        chat: TOOL_OUTPUT_CHAT_BYTES,
        total: TOOL_OUTPUT_TOTAL_BYTES,
    };
}

/// What a purge removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Purged {
    /// Files removed.
    pub files: u64,
    /// Their bytes.
    pub bytes: u64,
}

/// The line a cut result ends with: how much the model sees and how to read the rest.
#[must_use]
pub fn truncation_marker(handle: &str, shown: u64, total: u64) -> String {
    format!(
        "[output truncated: showing {shown} of {total} bytes. Read the rest with read_output {{\"handle\":\"{handle}\",\"offset\":{shown}}}]"
    )
}

/// Keeps the head (and, when `cap >= 512`, a 30 % tail preview so trailing compiler/test errors
/// are immediately visible) of `text` plus the [`truncation_marker`] for `handle`, at most about
/// `cap` bytes. `total` is the size of the spill file, whose prefix matches the shown head bytes.
#[must_use]
pub fn head_with_marker(text: &str, cap: usize, handle: &str, total: u64) -> String {
    let reserve = truncation_marker(handle, total, total).len() + 1;
    let body_cap = cap.saturating_sub(reserve).min(text.len());
    const MIN_HEAD_TAIL_CAP: usize = 512;
    const OMITTED_RESERVE: usize = 48;
    if body_cap >= MIN_HEAD_TAIL_CAP && text.len() > body_cap {
        let usable = body_cap.saturating_sub(OMITTED_RESERVE);
        let head_budget = (usable * 7) / 10;
        let tail_budget = usable.saturating_sub(head_budget);
        let mut head_end = head_budget.min(text.len());
        while !text.is_char_boundary(head_end) {
            head_end -= 1;
        }
        let mut tail_start = text.len().saturating_sub(tail_budget).max(head_end);
        while tail_start < text.len() && !text.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        if tail_start > head_end && tail_start < text.len() {
            let omitted = tail_start - head_end;
            let shown = u64::try_from(head_end).unwrap_or(u64::MAX);
            return format!(
                "{}\n[… {omitted} bytes omitted …]\n{}\n{}",
                &text[..head_end],
                &text[tail_start..],
                truncation_marker(handle, shown, total)
            );
        }
    }
    let mut end = body_cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let shown = u64::try_from(end).unwrap_or(u64::MAX);
    format!(
        "{}\n{}",
        &text[..end],
        truncation_marker(handle, shown, total)
    )
}

/// The directory of one chat's spilled output below `root` (`<data dir>/tool-output`), or `None`
/// when `session_id` is not a plain file name.
#[must_use]
pub fn session_dir(root: &Path, session_id: &str) -> Option<PathBuf> {
    valid_call_id(session_id).then(|| root.join(session_id))
}

/// Writes the (already redacted) full output `text` of `call_id` for chat `session_id`, cut to
/// `caps.file` on a character boundary, after making room: the chat's oldest files go until it
/// fits `caps.chat`, then whole directories of other chats, least recently used first, until the
/// app fits `caps.total`. Chats in `busy` (running now) are never evicted. Returns the bytes kept.
///
/// # Errors
/// An invalid id or a failed write.
pub fn write_spill(
    root: &Path,
    session_id: &str,
    call_id: &str,
    text: &str,
    caps: SpillCaps,
    busy: &[String],
) -> io::Result<u64> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "not a valid output name");
    let dir = session_dir(root, session_id).ok_or_else(invalid)?;
    if !valid_call_id(call_id) {
        return Err(invalid());
    }
    let mut end = usize::try_from(caps.file)
        .unwrap_or(usize::MAX)
        .min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let body = &text.as_bytes()[..end];
    let size = body.len() as u64;
    pingdeck_base::platform::create_private_dir_all(&dir)?;
    let path = dir.join(call_id);
    if fs::symlink_metadata(&path).is_ok() {
        fs::remove_file(&path)?;
    }
    let mut files = files_of(&dir);
    files.sort_by_key(|file| file.1);
    let mut used: u64 = files.iter().map(|file| file.2).sum();
    for (file, _, len) in files {
        if used + size <= caps.chat {
            break;
        }
        if fs::remove_file(&file).is_ok() {
            used -= len;
        }
    }
    evict_chats(root, session_id, used + size, caps.total, busy);
    pingdeck_base::platform::write_private_file(&path, body)?;
    Ok(size)
}

/// Marks a chat's output as used now, so app-wide eviction takes it last.
pub fn touch_session(root: &Path, session_id: &str) {
    if let Some(dir) = session_dir(root, session_id)
        && let Ok(handle) = fs::File::open(&dir)
    {
        let _ = handle.set_modified(SystemTime::now());
    }
}

/// Removes the spill files of chat `session_id`, or of every chat with `None`, and reports what
/// went. Symlinks are removed, never followed.
pub fn purge(root: &Path, session_id: Option<&str>) -> Purged {
    let dirs: Vec<PathBuf> = match session_id {
        Some(id) => session_dir(root, id).into_iter().collect(),
        None => fs::read_dir(root).map_or_else(
            |_| Vec::new(),
            |entries| entries.flatten().map(|entry| entry.path()).collect(),
        ),
    };
    let mut purged = Purged::default();
    for dir in dirs {
        let Ok(meta) = fs::symlink_metadata(&dir) else {
            continue;
        };
        if !meta.is_dir() {
            if fs::remove_file(&dir).is_ok() && meta.is_file() {
                purged.files += 1;
                purged.bytes += meta.len();
            }
            continue;
        }
        let files = files_of(&dir);
        if fs::remove_dir_all(&dir).is_ok() {
            purged.files += files.len() as u64;
            purged.bytes += files.iter().map(|file| file.2).sum::<u64>();
        }
    }
    purged
}

/// The regular files of `dir`: path, last change, size.
fn files_of(dir: &Path) -> Vec<(PathBuf, SystemTime, u64)> {
    fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let meta = entry.metadata().ok()?;
                    meta.is_file().then(|| {
                        let changed = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                        (entry.path(), changed, meta.len())
                    })
                })
                .collect()
        },
    )
}

/// Removes other chats' directories, least recently used first, until `own` bytes plus the rest
/// fit `total`.
fn evict_chats(root: &Path, session_id: &str, own: u64, total: u64, busy: &[String]) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut others: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if name == session_id || !meta.is_dir() {
            continue;
        }
        let bytes = files_of(&entry.path()).iter().map(|file| file.2).sum();
        let used = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        others.push((entry.path(), used, bytes));
    }
    others.sort_by_key(|other| other.1);
    let mut used = own + others.iter().map(|other| other.2).sum::<u64>();
    for (dir, _, bytes) in others {
        if used <= total {
            break;
        }
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        // A running chat is counted but never evicted.
        if busy.iter().any(|id| id == name) {
            continue;
        }
        if fs::remove_dir_all(&dir).is_ok() {
            used -= bytes;
        }
    }
}

/// Deletes everything a chat spilled; returns how many files went. A symlink in its place is
/// removed, never followed. A missing directory is zero.
pub fn remove_session(root: &Path, session_id: &str) -> usize {
    usize::try_from(purge(root, Some(session_id)).files).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chats_output_is_removed_with_it_and_only_it() {
        let root = std::env::temp_dir().join(format!("pd-tool-output-{}", std::process::id()));
        let mine = root.join("as_1");
        let other = root.join("as_2");
        fs::create_dir_all(&mine).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(mine.join("call_a"), "x").unwrap();
        fs::write(mine.join("call_b"), "y").unwrap();
        fs::write(other.join("call_c"), "z").unwrap();

        assert_eq!(remove_session(&root, "as_1"), 2);
        assert!(!mine.exists());
        assert!(other.join("call_c").exists());
        assert_eq!(remove_session(&root, "as_1"), 0);
        assert_eq!(remove_session(&root, "../as_2"), 0);
        assert!(other.exists());
        fs::remove_dir_all(&root).unwrap();
    }

    fn age(path: &Path, secs_ago: u64) {
        let when = SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        fs::File::open(path).unwrap().set_modified(when).unwrap();
    }

    // With small caps: a chat drops its oldest files first; the app drops the least
    // recently used other chat whole, never a busy one.
    #[test]
    fn eviction_goes_oldest_file_first_then_least_recently_used_chat() {
        let root =
            std::env::temp_dir().join(format!("pd-tool-output-evict-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let caps = SpillCaps {
            file: 10,
            chat: 20,
            total: 50,
        };
        let none: &[String] = &[];
        assert_eq!(
            write_spill(&root, "a", "c1", &"é".repeat(8), caps, none).unwrap(),
            10
        );
        age(&root.join("a").join("c1"), 30);
        write_spill(&root, "a", "c2", "0123456789", caps, none).unwrap();
        write_spill(&root, "a", "c3", "0123456789", caps, none).unwrap();
        assert!(
            !root.join("a").join("c1").exists(),
            "oldest file of the chat went"
        );
        assert!(root.join("a").join("c2").exists() && root.join("a").join("c3").exists());

        write_spill(&root, "b", "c1", "0123456789", caps, none).unwrap();
        write_spill(&root, "busy", "c1", "0123456789", caps, none).unwrap();
        age(&root.join("a"), 300);
        age(&root.join("busy"), 600);
        age(&root.join("b"), 100);
        // 20 + 10 + 10 + 20 new > 50: "busy" is the oldest but running, so "a" goes.
        let busy = vec!["busy".to_owned()];
        write_spill(&root, "new", "c1", "0123456789", caps, &busy).unwrap();
        write_spill(&root, "new", "c2", "0123456789", caps, &busy).unwrap();
        assert!(!root.join("a").exists());
        assert!(root.join("busy").exists() && root.join("b").exists());
        assert!(write_spill(&root, "../x", "c1", "x", caps, none).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn purge_reports_what_it_removed() {
        let root =
            std::env::temp_dir().join(format!("pd-tool-output-purge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let caps = SpillCaps::DEFAULT;
        write_spill(&root, "a", "c1", "12345", caps, &[]).unwrap();
        write_spill(&root, "a", "c2", "123", caps, &[]).unwrap();
        write_spill(&root, "b", "c1", "1234567", caps, &[]).unwrap();
        assert_eq!(purge(&root, Some("a")), Purged { files: 2, bytes: 8 });
        assert_eq!(purge(&root, None), Purged { files: 1, bytes: 7 });
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_is_removed_not_followed() {
        let root = std::env::temp_dir().join(format!("pd-tool-output-link-{}", std::process::id()));
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), "k").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("as_9")).unwrap();

        remove_session(&root, "as_9");
        assert!(outside.join("keep").exists());
        assert!(fs::symlink_metadata(root.join("as_9")).is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
