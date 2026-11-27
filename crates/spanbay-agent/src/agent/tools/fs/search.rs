//! The tree-walking tools: `glob_files` and `grep_files`.
//!
//! Single responsibility: walk the workspace without ever leaving it (symlinks are not followed,
//! protected locations and bulky build directories are skipped), compile globs and
//! collect matches under hard caps. Synchronous; the tool wrapper runs it in `spawn_blocking`.

use std::fs;
use std::io::Read;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use regex::{Regex, RegexBuilder};

use super::ops::open_read;
use super::paths::approval_reason;
use super::{
    Env, GlobArgs, GrepArgs, MAX_GLOB_RESULTS, MAX_GREP_FILE_BYTES, MAX_GREP_LINE_CHARS,
    MAX_GREP_RESULTS,
};
use crate::agent::tools::ToolOutput;
use spanbay_base::cancel::CancelToken;
use spanbay_base::error::AppError;

/// Directories no walk descends into.
const SKIPPED_DIRS: [&str; 3] = [".git", "node_modules", "target"];

/// Most directory entries a walk looks at; a walk of a huge tree (a home folder) stops here.
const MAX_VISITED: usize = 200_000;

/// Deepest directory nesting a walk follows.
const MAX_DEPTH: usize = 64;

/// Largest compiled regex program (1 MiB), so a hostile pattern cannot exhaust memory.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// Why a walk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Walked {
    /// Every reachable file was visited, or the visitor stopped on purpose.
    Done,
    /// The visit budget ran out first.
    Limit,
}

/// Visits every regular file below `root` (or `root` itself when it is a file) in name order.
fn walk_files(
    env: &Env,
    root: &Path,
    cancel: &CancelToken,
    visit: &mut dyn FnMut(&Path) -> ControlFlow<()>,
) -> Result<Walked, AppError> {
    if cancel.is_cancelled() {
        return Err(AppError::cancelled());
    }
    if root.is_file() {
        let _ = visit(root);
        return Ok(Walked::Done);
    }
    let guard = env.guard.clone();
    let visited = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&visited);
    let cancelled = cancel.clone();
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .follow_links(false)
        .max_depth(Some(MAX_DEPTH + 1))
        .sort_by_file_name(std::cmp::Ord::cmp)
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            // Yield a sentinel so the consumer can stop even when every entry is filtered out.
            if seen.fetch_add(1, Ordering::Relaxed) >= MAX_VISITED || cancelled.is_cancelled() {
                return true;
            }
            if entry.file_type().is_some_and(|ft| ft.is_symlink()) || guard.denies(entry.path()) {
                return false;
            }
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                let name = entry.file_name();
                if SKIPPED_DIRS.iter().any(|skip| name == *skip) {
                    return false;
                }
            }
            true
        })
        .build();
    for result in walker {
        if cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        let Ok(entry) = result else {
            continue;
        };
        if entry.depth() == 0 {
            continue;
        }
        if visited.load(Ordering::Relaxed) > MAX_VISITED {
            return Ok(Walked::Limit);
        }
        if entry.file_type().is_some_and(|ft| ft.is_file()) && visit(entry.path()).is_break() {
            break;
        }
    }
    Ok(Walked::Done)
}

/// A compiled glob matcher over `/`-separated relative paths.
pub(super) struct Glob {
    regex: Regex,
    /// The pattern has no `/`: it matches the file name at any depth.
    name_only: bool,
}

impl Glob {
    /// Compiles `pattern` with literal path-separator matching.
    pub(super) fn new(pattern: &str) -> Result<Self, AppError> {
        let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
        if pattern.is_empty() {
            return Err(AppError::validation("The glob pattern is empty"));
        }
        let glob = globset::GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .empty_alternates(true)
            .allow_unclosed_class(true)
            .build()
            .map_err(|error| AppError::validation(format!("Invalid glob '{pattern}': {error}")))?;
        // Keep Unicode `?`/classes and the existing 1 MiB regex ceiling.
        let regex = RegexBuilder::new(glob.regex().strip_prefix("(?-u)").unwrap_or(glob.regex()))
            .size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map_err(|error| AppError::validation(format!("Invalid glob '{pattern}': {error}")))?;
        Ok(Self {
            regex,
            name_only: !pattern.contains('/'),
        })
    }

    /// Whether the `/`-separated path `relative` matches.
    pub(super) fn is_match(&self, relative: &str) -> bool {
        let subject = if self.name_only {
            relative.rsplit('/').next().unwrap_or(relative)
        } else {
            relative
        };
        self.regex.is_match(subject)
    }
}

/// `glob_files`: files whose path matches a glob, newest first.
pub(super) fn glob_files(
    env: &Env,
    args: &GlobArgs,
    cancel: &CancelToken,
) -> Result<ToolOutput, AppError> {
    let glob = Glob::new(&args.pattern)?;
    let root = env.resolve(args.path.as_deref().unwrap_or("."), false)?;
    if !root.is_dir() {
        return Err(AppError::validation(format!(
            "'{}' is not a directory",
            env.relative(&root)
        )));
    }
    let mut matches: Vec<(SystemTime, String)> = Vec::new();
    let walked = walk_files(env, &root, cancel, &mut |path| {
        let within_root = path.strip_prefix(&root).unwrap_or(path);
        if glob.is_match(&within_root.to_string_lossy().replace('\\', "/")) {
            let modified = fs::metadata(path)
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            matches.push((modified, env.relative(path)));
        }
        ControlFlow::Continue(())
    })?;
    if matches.is_empty() {
        return Ok(ToolOutput::text(format!(
            "No files match '{}'{}",
            args.pattern,
            limit_note(walked)
        )));
    }
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let total = matches.len();
    let mut out: String = matches
        .iter()
        .take(MAX_GLOB_RESULTS)
        .map(|(_, path)| format!("{path}\n"))
        .collect();
    if total > MAX_GLOB_RESULTS {
        out.push_str(&format!(
            "[Showing the {MAX_GLOB_RESULTS} newest of {total} matches; narrow the pattern or path]\n"
        ));
    }
    out.push_str(&limit_note(walked));
    Ok(ToolOutput::text(out))
}

/// The note appended when a walk gave up early.
fn limit_note(walked: Walked) -> String {
    match walked {
        Walked::Done => String::new(),
        Walked::Limit => format!(
            "[The search stopped after looking at {MAX_VISITED} entries; use a narrower path]\n"
        ),
    }
}

/// The first `MAX_GREP_LINE_CHARS` characters of `line`.
fn clip_line(line: &str) -> String {
    let mut chars = line.chars();
    let mut text: String = chars.by_ref().take(MAX_GREP_LINE_CHARS).collect();
    if chars.next().is_some() {
        text.push_str(" … [truncated]");
    }
    text
}

/// `grep_files`: `path:line:text` for every line matching a regex.
pub(super) fn grep_files(
    env: &Env,
    args: &GrepArgs,
    cancel: &CancelToken,
) -> Result<ToolOutput, AppError> {
    let regex = compile_regex(&args.pattern, args.case_insensitive)?;
    let glob = args.glob.as_deref().map(Glob::new).transpose()?;
    let root = env.resolve(args.path.as_deref().unwrap_or("."), false)?;
    let max = args
        .max_results
        .unwrap_or(super::DEFAULT_GREP_RESULTS)
        .clamp(1, MAX_GREP_RESULTS);
    let mut hits: Vec<String> = Vec::new();
    let mut full = false;
    let walked = walk_files(env, &root, cancel, &mut |path| {
        let within_root = path.strip_prefix(&root).unwrap_or(path);
        // A file named as the root was approved as such; the filters apply to files found by walking.
        if path != root {
            if let Some(glob) = &glob
                && !glob.is_match(&within_root.to_string_lossy().replace('\\', "/"))
            {
                return ControlFlow::Continue(());
            }
            // Secret files are never searched silently; reading one needs its own approval.
            if approval_reason(&env.workspace, path, false).is_some() {
                return ControlFlow::Continue(());
            }
        }
        // Every file is read through a handle confirmed against its name: a link swapped in
        // after the walk saw a regular file makes the open fail the check and the file is skipped.
        let rel = env.relative(path);
        let Ok(mut file) = open_read(env, path, &rel) else {
            return ControlFlow::Continue(());
        };
        let mut bytes = Vec::new();
        if file
            .by_ref()
            .take(MAX_GREP_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_GREP_FILE_BYTES
            || memchr::memchr(0, &bytes[..bytes.len().min(8192)]).is_some()
        {
            return ControlFlow::Continue(());
        }
        for (index, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
            if regex.is_match(line) {
                hits.push(format!("{rel}:{}:{}", index + 1, clip_line(line)));
                if hits.len() >= max {
                    full = true;
                    return ControlFlow::Break(());
                }
            }
        }
        ControlFlow::Continue(())
    })?;
    if hits.is_empty() {
        return Ok(ToolOutput::text(format!(
            "No matches for '{}'{}",
            args.pattern,
            limit_note(walked)
        )));
    }
    let mut out = hits.join("\n");
    out.push('\n');
    if full {
        out.push_str(&format!(
            "[Stopped after {max} matches; narrow the pattern, path or glob, or raise max_results]\n"
        ));
    }
    out.push_str(&limit_note(walked));
    Ok(ToolOutput::text(out))
}

/// The regex of a `grep_files` call, with memory bounds.
pub(super) fn compile_regex(pattern: &str, case_insensitive: bool) -> Result<Regex, AppError> {
    if pattern.is_empty() {
        return Err(AppError::validation("The search pattern is empty"));
    }
    RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .size_limit(REGEX_SIZE_LIMIT)
        .nest_limit(64)
        .build()
        .map_err(|error| AppError::validation(format!("Invalid regex: {error}")))
}
