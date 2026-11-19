//! What the composer asks about a chat's folder: its git branch, project instructions and skill
//! folders, and a fuzzy file search for `@` mentions.
//!
//! Single responsibility: cheap, read-only looks at a folder. Nothing runs a process: the branch
//! comes from `.git/HEAD`. Everything here blocks on the disk, so a shell calls it from the
//! blocking pool.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agent::git_head::read_head;
use crate::agent::instructions::select;
use crate::agent::skills::discover_skills;
use crate::agent::tools::fs::PathGuard;
use crate::error::AppError;

use crate::app::{AppState, command_error};

/// Skill folders of a workspace, relative to it (the folders `agent::skills` scans).
const WORKSPACE_SKILL_DIRS: [&str; 2] = [".claude/skills", ".agents/skills"];

/// Directories no walk descends into (the file tools' skip list).
const SKIPPED_DIRS: [&str; 3] = [".git", "node_modules", "target"];

/// Directory entries one search looks at before it gives up on a huge tree.
const MAX_VISITED: usize = 50_000;

/// Deepest directory nesting a search follows.
const MAX_DEPTH: usize = 24;

/// Matches collected before ranking; the walk stops here.
const MAX_MATCHES: usize = 2_000;

/// Results returned when the caller gives no limit.
const DEFAULT_LIMIT: u32 = 50;

/// Most results one search returns.
const MAX_LIMIT: u32 = 200;

/// Longest query accepted, in characters.
const MAX_QUERY_CHARS: usize = 200;

/// A folder of skills inside the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSkillDir {
    /// The folder relative to the workspace (`.claude/skills`).
    pub dir: String,
    /// Skills found in it.
    pub skills: u32,
}

/// What `agent_workspace_info` reports about a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// The folder as given.
    pub path: String,
    /// The folder exists and is a directory.
    pub exists: bool,
    /// The checked-out branch, or the short commit id of a detached HEAD; `None` outside a git
    /// repository, for an untrusted folder, or when `HEAD` fails the trust checks.
    pub git_branch: Option<String>,
    /// HEAD is a commit, not a branch.
    pub git_detached: bool,
    /// `AGENTS.md`, else `CLAUDE.md`, when the folder has one.
    pub instructions_file: Option<String>,
    /// The workspace skill folders that exist, with their skill counts.
    pub skill_dirs: Vec<WorkspaceSkillDir>,
    /// The user trusted this folder ("Trust this folder?"): only then are the instructions and
    /// the folder skills loaded into the chat; untrusted, they are listed here but not injected
    pub trusted: bool,
    /// The folder is trusted but its instructions file or its skills changed since
    /// ("Instructions changed"): neither is loaded until the user trusts the folder again.
    pub instructions_changed: bool,
}

fn require_absolute(path: &str) -> Result<&Path, AppError> {
    let folder = Path::new(path);
    if folder.is_absolute() {
        Ok(folder)
    } else {
        Err(AppError::validation("the folder path must be absolute"))
    }
}

/// The skill folders of the canonical workspace `root` that stay inside it.
fn skill_dirs(root: &Path) -> Vec<WorkspaceSkillDir> {
    WORKSPACE_SKILL_DIRS
        .iter()
        .filter_map(|dir| {
            let real = fs::canonicalize(root.join(dir)).ok()?;
            if !real.starts_with(root) || !real.is_dir() {
                return None;
            }
            let label = (*dir).to_owned();
            let found = discover_skills(&[real], std::slice::from_ref(&label));
            Some(WorkspaceSkillDir {
                dir: label,
                skills: u32::try_from(found.len()).unwrap_or(u32::MAX),
            })
        })
        .collect()
}

/// The folder's git branch, project instructions file and skill folders.
///
/// A folder that does not exist is a valid answer (`exists: false`): the chip shows "Folder
/// missing".
///
/// # Errors
/// [`AppError::Validation`] when `path` is not absolute.
pub fn agent_workspace_info_impl(state: &AppState, path: &str) -> Result<WorkspaceInfo, AppError> {
    let folder = require_absolute(path)?;
    let mut info = WorkspaceInfo {
        path: path.to_owned(),
        exists: false,
        git_branch: None,
        git_detached: false,
        instructions_file: None,
        skill_dirs: Vec::new(),
        trusted: false,
        instructions_changed: false,
    };
    let Ok(root) = fs::canonicalize(folder) else {
        return Ok(info);
    };
    if !root.is_dir() {
        return Ok(info);
    }
    info.exists = true;
    info.skill_dirs = skill_dirs(&root);
    let key = crate::platform::normalize_display_path(&root);
    info.trusted = state
        .store()
        .is_folder_trusted(&key)
        .map_err(command_error)?;
    if info.trusted {
        let trust = super::trust::folder_trust(state, &root);
        info.instructions_changed = !trust.pinned;
        info.instructions_file = trust.instructions.map(|selected| selected.name.to_owned());
    } else {
        info.instructions_file = select(&root).map(|selected| selected.name.to_owned());
    }
    // The branch of an untrusted folder is not shown (the prompt leaves it out too).
    if info.trusted
        && let Some(head) = read_head(&root)
    {
        info.git_branch = Some(head.name);
        info.git_detached = head.detached;
    }
    Ok(info)
}

/// Rank of a match (lower is better): the query as a substring of the file name, of the path, then
/// as a subsequence of the name, of the path; `None` when it is not even a subsequence.
fn match_rank(path: &str, query: &str) -> Option<u8> {
    let path = path.to_lowercase();
    let name = path.rsplit('/').next().unwrap_or(&path);
    if name.contains(query) {
        return Some(0);
    }
    if path.contains(query) {
        return Some(1);
    }
    if is_subsequence(query, name) {
        return Some(2);
    }
    is_subsequence(query, &path).then_some(3)
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut rest = haystack.chars();
    needle.chars().all(|c| rest.any(|h| h == c))
}

struct Search<'a> {
    root: &'a Path,
    guard: &'a PathGuard,
    /// Lowercased query; empty matches every file.
    query: &'a str,
    visited: usize,
    found: Vec<(u8, String)>,
}

impl Search<'_> {
    /// Walks `dir` in name order; returns `true` when a cap ended the search.
    fn walk(&mut self, dir: &Path, depth: usize) -> bool {
        let Ok(read) = fs::read_dir(dir) else {
            return false;
        };
        let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            self.visited += 1;
            if self.visited > MAX_VISITED || self.found.len() >= MAX_MATCHES {
                return true;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_symlink() || self.guard.denies(&path) {
                continue;
            }
            if kind.is_dir() {
                let skipped = SKIPPED_DIRS.iter().any(|skip| entry.file_name() == *skip);
                if !skipped && depth < MAX_DEPTH && self.walk(&path, depth + 1) {
                    return true;
                }
            } else if kind.is_file() {
                let relative = relative_slashed(self.root, &path);
                if let Some(rank) = match_rank(&relative, self.query) {
                    self.found.push((rank, relative));
                }
            }
        }
        false
    }
}

/// `path` below `root` with `/` separators.
fn relative_slashed(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let parts: Vec<_> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    parts.join("/")
}

/// Files below `path` whose relative path fuzzy-matches `query` (case-insensitive subsequence;
/// name hits before path hits, then shorter paths first), at most `limit` (default 50, cap 200).
/// An empty query lists every file, shortest path first. Symlinks, `.git`, `node_modules`, `target` and the
/// locations no agent may touch are skipped, and a huge tree is cut off (50,000 entries).
///
/// # Errors
/// [`AppError::Validation`] for a relative or missing folder or a query over 200 characters.
pub fn workspace_search_files_impl(
    state: &AppState,
    path: &str,
    query: &str,
    limit: Option<u32>,
) -> Result<Vec<String>, AppError> {
    let folder = require_absolute(path)?;
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(AppError::validation(format!(
            "the search text is longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    let root = fs::canonicalize(folder)
        .ok()
        .filter(|root| root.is_dir())
        .ok_or_else(|| AppError::validation(format!("{path} is not an accessible folder")))?;
    let guard = PathGuard::for_process(state.data_dirs().map(|dirs| dirs.root.as_path()));
    let query = query.trim().to_lowercase();
    let mut search = Search {
        root: &root,
        guard: &guard,
        query: &query,
        visited: 0,
        found: Vec::new(),
    };
    search.walk(&root, 0);
    let mut found = search.found;
    found.sort_by(|a, b| (a.0, a.1.len(), &a.1).cmp(&(b.0, b.1.len(), &b.1)));
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    Ok(found
        .into_iter()
        .take(limit)
        .map(|(_, path)| path)
        .collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    use super::*;

    struct Folder(PathBuf);

    impl Folder {
        fn new(label: &str) -> Self {
            let path = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("pingdeck-wsinfo-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self, relative: &str, text: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A fresh state that trusts `path` (only a trusted folder reports its branch).
    fn trusting(path: &str) -> AppState {
        let state = AppState::open_in_memory().unwrap();
        crate::agent_service::agent_folder_trust_set_impl(&state, path, true).unwrap();
        state
    }

    #[test]
    fn saf3_only_a_trusted_folder_reports_its_branch() {
        let folder = Folder::new("saf3");
        folder.file(".git/HEAD", "ref: refs/heads/main\n");
        let state = AppState::open_in_memory().unwrap();
        let info = agent_workspace_info_impl(&state, folder.path()).unwrap();
        assert!(!info.trusted && info.git_branch.is_none());
        let info = agent_workspace_info_impl(&trusting(folder.path()), folder.path()).unwrap();
        assert_eq!(info.git_branch.as_deref(), Some("main"));
    }

    #[test]
    fn info_reads_branch_instructions_and_skill_folders() {
        let folder = Folder::new("info");
        folder.file(".git/HEAD", "ref: refs/heads/feature/chat\n");
        folder.file("CLAUDE.md", "rules");
        folder.file(
            ".agents/skills/alpha/SKILL.md",
            "---\nname: alpha\n---\nbody",
        );
        let info = agent_workspace_info_impl(&trusting(folder.path()), folder.path()).unwrap();
        assert!(info.exists);
        assert_eq!(info.git_branch.as_deref(), Some("feature/chat"));
        assert!(!info.git_detached);
        assert_eq!(info.instructions_file.as_deref(), Some("CLAUDE.md"));
        assert_eq!(
            info.skill_dirs,
            [WorkspaceSkillDir {
                dir: ".agents/skills".into(),
                skills: 1
            }]
        );

        folder.file("AGENTS.md", "more rules");
        folder.file(".git/HEAD", "0123456789abcdef0123456789abcdef01234567\n");
        let info = agent_workspace_info_impl(&trusting(folder.path()), folder.path()).unwrap();
        assert_eq!(info.instructions_file.as_deref(), Some("AGENTS.md"));
        assert_eq!(info.git_branch.as_deref(), Some("0123456"));
        assert!(info.git_detached);
    }

    #[test]
    fn info_follows_a_worktree_pointer_and_an_ancestor_repository() {
        let folder = Folder::new("worktree");
        folder.file("real/HEAD", "ref: refs/heads/wt\n");
        folder.file("tree/.git", "gitdir: ../real\n");
        let tree = folder.0.join("tree");
        let info =
            agent_workspace_info_impl(&trusting(tree.to_str().unwrap()), tree.to_str().unwrap())
                .unwrap();
        assert_eq!(info.git_branch.as_deref(), Some("wt"));

        folder.file(".git/HEAD", "ref: refs/heads/main\n");
        folder.file("pkg/src/lib.rs", "");
        let src = folder.0.join("pkg/src");
        let info =
            agent_workspace_info_impl(&trusting(src.to_str().unwrap()), src.to_str().unwrap())
                .unwrap();
        assert_eq!(info.git_branch.as_deref(), Some("main"));
    }

    #[test]
    fn info_of_a_missing_folder_says_so_and_a_relative_path_is_refused() {
        let folder = Folder::new("missing");
        let missing = folder.0.join("gone");
        let info = agent_workspace_info_impl(
            &AppState::open_in_memory().unwrap(),
            missing.to_str().unwrap(),
        )
        .unwrap();
        assert!(!info.exists);
        assert!(info.git_branch.is_none() && info.skill_dirs.is_empty());
        assert!(matches!(
            agent_workspace_info_impl(&AppState::open_in_memory().unwrap(), "relative/dir"),
            Err(AppError::Validation { .. })
        ));
    }

    #[test]
    fn a_skill_folder_that_leaves_the_workspace_is_not_reported() {
        let outside = Folder::new("outside");
        outside.file("skills/x/SKILL.md", "---\nname: x\n---\n");
        let folder = Folder::new("link");
        fs::create_dir_all(folder.0.join(".claude")).unwrap();
        symlink(outside.0.join("skills"), folder.0.join(".claude/skills")).unwrap();
        assert!(
            agent_workspace_info_impl(&AppState::open_in_memory().unwrap(), folder.path())
                .unwrap()
                .skill_dirs
                .is_empty()
        );
    }

    fn search(folder: &Folder, query: &str, limit: Option<u32>) -> Vec<String> {
        let state = AppState::open_in_memory().unwrap();
        workspace_search_files_impl(&state, folder.path(), query, limit).unwrap()
    }

    #[test]
    fn search_ranks_name_hits_first_and_skips_bulky_and_linked_folders() {
        let folder = Folder::new("search");
        folder.file("src/app/main.rs", "");
        folder.file("src/app/domain/model.rs", "");
        folder.file("docs/main-notes.md", "");
        folder.file("node_modules/main.js", "");
        folder.file("target/debug/main", "");
        folder.file(".git/main", "");
        let outside = Folder::new("search-out");
        outside.file("main.rs", "");
        symlink(&outside.0, folder.0.join("linked")).unwrap();

        // Name substring hits first (shorter path first), then a path-only substring hit.
        assert_eq!(
            search(&folder, "MAIN", None),
            [
                "src/app/main.rs",
                "docs/main-notes.md",
                "src/app/domain/model.rs"
            ]
        );
        assert_eq!(
            search(&folder, "adm", None),
            ["src/app/domain/model.rs"],
            "subsequence over the whole path"
        );
        assert!(search(&folder, "zzz", None).is_empty());
    }

    #[test]
    fn search_with_no_query_lists_every_file_and_honours_the_limit() {
        let folder = Folder::new("limit");
        for name in ["c.txt", "a.txt", "b.txt"] {
            folder.file(name, "");
        }
        assert_eq!(search(&folder, "", Some(2)), ["a.txt", "b.txt"]);
        assert_eq!(
            search(&folder, "", Some(0)),
            ["a.txt"],
            "a limit of 0 is raised to 1"
        );
    }

    #[test]
    fn search_rejects_a_long_query_and_a_missing_folder() {
        let state = AppState::open_in_memory().unwrap();
        let folder = Folder::new("bad");
        assert!(matches!(
            workspace_search_files_impl(&state, folder.path(), &"x".repeat(201), None),
            Err(AppError::Validation { .. })
        ));
        let gone = folder.0.join("gone");
        assert!(matches!(
            workspace_search_files_impl(&state, gone.to_str().unwrap(), "a", None),
            Err(AppError::Validation { .. })
        ));
    }
}
