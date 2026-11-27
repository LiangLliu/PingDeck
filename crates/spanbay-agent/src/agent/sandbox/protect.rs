//! Workspace locations a sandboxed command may read but never write.
//!
//! Single responsibility: the one list of workspace paths that change what runs on this machine
//! later (git hooks and config of every repository below the Folder, the agent-instruction and
//! editor folders, `.husky`, `.cargo/config.toml`, shell and direnv startup files; the same set
//! the file tools force an approval for on a write) and its two renderings. Seatbelt gets path-pattern rules that also cover repositories the command creates
//! itself; `bwrap` can only bind paths that exist, so [`existing_paths`] finds them with a bounded
//! walk.
//!
//! Limits of the `bwrap` rendering (a Seatbelt-only guarantee otherwise): a repository, agent
//! folder or startup file the command creates itself is not protected, and neither is one below
//! [`SKIPPED_DIRS`] or past [`WALK_BUDGET`] directory entries.

use std::fs;
use std::path::{Path, PathBuf};

/// Shell and direnv startup files; writing one runs code in every later terminal.
pub(crate) const STARTUP_FILES: [&str; 9] = [
    ".zshrc",
    ".bashrc",
    ".profile",
    ".zprofile",
    ".zshenv",
    ".zlogin",
    ".bash_profile",
    ".bash_login",
    ".envrc",
];

/// Folders whose files steer later agent sessions (skills, instructions, settings), run on a git
/// commit (`.husky`) or run when the folder is opened in an editor (tasks, launch configs).
pub(crate) const PROTECTED_DIRS: [&str; 6] = [
    ".claude", ".agents", ".cursor", ".husky", ".vscode", ".idea",
];

/// Cargo's per-project configuration (`.cargo/<name>`): it can swap the linker or a runner.
pub(crate) const CARGO_CONFIGS: [&str; 2] = ["config.toml", "config"];

/// What inside a git directory runs code or redirects git: hooks and the configuration files.
const GIT_NAMES: [&str; 3] = ["hooks", "config", "config.worktree"];

/// Directories the `bwrap` walk does not enter (huge, and rebuilt by tools).
const SKIPPED_DIRS: [&str; 2] = ["node_modules", "target"];

/// Directory entries the `bwrap` walk looks at before it gives up.
const WALK_BUDGET: usize = 20_000;

/// Deepest directory the `bwrap` walk enters.
const WALK_DEPTH: usize = 12;

/// `text` as a case-insensitive regex that matches it literally, byte for byte: letters become
/// `[xX]`, `.` is escaped and every other byte outside `[0-9/_-]` becomes `.`. Nothing can leave
/// the Seatbelt string literal or add regex syntax; a path with an odd character is matched a
/// little wider, which only makes a deny rule stricter. (Case-insensitive because the file systems
/// of macOS are, so `.CLAUDE` names the same folder.)
fn literal(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' => {
                format!(
                    "[{}{}]",
                    byte.to_ascii_lowercase() as char,
                    byte.to_ascii_uppercase() as char
                )
            }
            b'0'..=b'9' | b'/' | b'_' | b'-' => (byte as char).to_string(),
            b'.' => "\\.".to_owned(),
            _ => ".".to_owned(),
        })
        .collect()
}

/// `(a|b|c)` of [`literal`] alternatives.
fn alternatives(names: &[&str]) -> String {
    let names: Vec<String> = names.iter().map(|name| literal(name)).collect();
    format!("({})", names.join("|"))
}

/// The Seatbelt `(regex #"…")` deny rules (without the surrounding `(deny file-write* …)`) for
/// writes below `workspace` (canonical, valid UTF-8): git hooks and config at any depth (also of
/// submodules and worktrees), agent folders and shell startup files at any depth.
pub(super) fn seatbelt_regexes(workspace: &str) -> Vec<String> {
    let root = literal(workspace.trim_end_matches('/'));
    let git_names = alternatives(&GIT_NAMES);
    let git = format!(
        "^{root}/(.*/)?{}/({git_names}|(modules|worktrees)/.+/{git_names})(/|$)",
        literal(".git")
    );
    let agents = format!("^{root}/(.*/)?{}(/|$)", alternatives(&PROTECTED_DIRS));
    let startup = format!("^{root}/(.*/)?{}$", alternatives(&STARTUP_FILES));
    let cargo = format!(
        "^{root}/(.*/)?{}/{}$",
        literal(".cargo"),
        alternatives(&CARGO_CONFIGS)
    );
    [git, agents, startup, cargo]
        .into_iter()
        .map(|regex| format!("(regex #\"{regex}\")"))
        .collect()
}

/// The protected paths below `workspace` that exist now, for read-only binds: `.git/{hooks,config,
/// config.worktree}` of every repository (and of its submodules and worktrees), agent folders and
/// startup files. Symlinks are never followed or returned.
pub(super) fn existing_paths(workspace: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut budget = WALK_BUDGET;
    walk(workspace, 0, &mut budget, &mut found);
    found
}

fn walk(dir: &Path, depth: usize, budget: &mut usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let path = entry.path();
        let is = |expected: &str| name.eq_ignore_ascii_case(expected);
        if is(".git") {
            if kind.is_dir() {
                git_dir(&path, found);
            }
        } else if is(".cargo") && kind.is_dir() {
            for name in CARGO_CONFIGS {
                let config = path.join(name);
                if fs::symlink_metadata(&config).is_ok_and(|meta| meta.file_type().is_file()) {
                    found.push(config);
                }
            }
        } else if PROTECTED_DIRS.iter().any(|dir| is(dir))
            || (kind.is_file() && STARTUP_FILES.iter().any(|file| is(file)))
        {
            found.push(path);
        } else if kind.is_dir() && depth < WALK_DEPTH && !SKIPPED_DIRS.iter().any(|skip| is(skip)) {
            walk(&path, depth + 1, budget, found);
        }
    }
}

/// The hooks and config of the git directory `git`, and of every module and worktree below it.
fn git_dir(git: &Path, found: &mut Vec<PathBuf>) {
    for name in GIT_NAMES {
        let path = git.join(name);
        if fs::symlink_metadata(&path).is_ok_and(|meta| !meta.file_type().is_symlink()) {
            found.push(path);
        }
    }
    for nested in ["modules", "worktrees"] {
        let Ok(entries) = fs::read_dir(git.join(nested)) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                git_dir(&entry.path(), found);
            }
        }
    }
}
