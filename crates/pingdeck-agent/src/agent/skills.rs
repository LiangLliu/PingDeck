//! Skill discovery: folders holding a `SKILL.md` with YAML frontmatter.
//!
//! Single responsibility: find skills in the configured directories, the managed directory and the
//! session workspace, expose their name and description to the system prompt, read a skill's text
//! and supporting files on demand, and import a skill folder into PingDeck's managed directory.
//! Nothing here executes skill content.

use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use pingdeck_base::{error::AppError, yaml};
use pingdeck_model::agent::{SkillUndelivered, SkillUndeliveredReason};
use pingdeck_model::settings::Settings;

const SKILL_FILE: &str = "SKILL.md";
const MAX_SKILLS_PER_ROOT: usize = 200;
const FRONTMATTER_READ_BYTES: u64 = 64 * 1024;
const MAX_DESCRIPTION_CHARS: usize = 1024;
/// Most bytes the skills catalogue of the system prompt lists; the skills past it are
/// named in one line as left out.
pub const SKILL_CATALOG_BYTES: usize = 8 * 1024;
const MAX_IMPORT_BYTES: u64 = 5 << 20;
const MAX_IMPORT_FILES: usize = 500;
/// Largest supporting file [`read_skill_file`] returns, and the most of `SKILL.md` `read_skill`
/// shows (256 KiB).
pub const MAX_SKILL_READ_BYTES: usize = 256 * 1024;
/// Label of the app-managed skill folder in [`SkillInfo::origin`].
pub const MANAGED_ORIGIN: &str = "managed";
/// Label of the session workspace's skill folders in [`SkillInfo::origin`].
pub const WORKSPACE_ORIGIN: &str = "workspace";
/// Skill folders of a workspace, relative to it.
const WORKSPACE_SKILL_DIRS: [&str; 2] = [".claude/skills", ".agents/skills"];

/// A discovered skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInfo {
    /// Skill name (frontmatter `name`, else the folder name).
    pub name: String,
    /// Frontmatter `description` (empty when absent).
    pub description: String,
    /// Absolute directory containing `SKILL.md`.
    pub path: String,
    /// The scanned root as given, or `"managed"`.
    pub origin: String,
}

#[derive(Default, Deserialize)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
}

/// Expands a leading `~` using `HOME` (or `USERPROFILE`); other paths are returned unchanged.
#[must_use]
pub fn expand_home(dir: &str) -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok();
    expand_home_with(dir, home.as_deref())
}

fn expand_home_with(dir: &str, home: Option<&str>) -> PathBuf {
    let Some(home) = home.filter(|home| !home.is_empty()) else {
        return PathBuf::from(dir);
    };
    if dir == "~" {
        PathBuf::from(home)
    } else if let Some(rest) = dir.strip_prefix("~/").or_else(|| dir.strip_prefix("~\\")) {
        Path::new(home).join(rest)
    } else {
        PathBuf::from(dir)
    }
}

/// Parses the `---` delimited YAML block at the top of a skill file; no block means defaults.
fn parse_frontmatter(text: &str) -> Result<Frontmatter, yaml::Error> {
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Ok(Frontmatter::default());
    }
    let rest: Vec<&str> = lines.collect();
    let Some(end) = rest.iter().position(|line| line.trim_end() == "---") else {
        return Ok(Frontmatter::default());
    };
    let block = rest[..end].join("\n");
    if block.trim().is_empty() {
        return Ok(Frontmatter::default());
    }
    yaml::from_str(&block)
}

/// Whether `path` is itself a regular file: a symlink never counts, so a skill folder cannot
/// point discovery, `read_skill` or import at files outside of it.
fn is_plain_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file())
}

/// Loads the skill in `dir`; `None` when `SKILL.md` is missing, a symlink, unreadable or its YAML
/// is invalid.
fn load_skill(dir: &Path, origin: &str) -> Option<SkillInfo> {
    let skill_file = dir.join(SKILL_FILE);
    if !is_plain_file(&skill_file) {
        return None;
    }
    let mut buffer = Vec::new();
    fs::File::open(skill_file)
        .ok()?
        .take(FRONTMATTER_READ_BYTES)
        .read_to_end(&mut buffer)
        .ok()?;
    let meta = parse_frontmatter(&String::from_utf8_lossy(&buffer)).ok()?;
    let path = std::path::absolute(dir).ok()?;
    let folder = path.file_name()?.to_string_lossy().into_owned();
    let name = meta
        .name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or(folder);
    Some(SkillInfo {
        name,
        description: meta
            .description
            .map(|text| text.trim().to_owned())
            .unwrap_or_default(),
        path: path.to_string_lossy().into_owned(),
        origin: origin.to_owned(),
    })
}

/// Scans each root (and its immediate child folders) for skills.
///
/// `origin_labels[i]` labels `dirs[i]`; a missing label falls back to the path. Duplicate names
/// keep the first root's skill; the result is sorted by name. Unreadable or invalid entries are
/// skipped, at most 200 skills are taken per root.
#[must_use]
pub fn discover_skills(dirs: &[PathBuf], origin_labels: &[String]) -> Vec<SkillInfo> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for (index, root) in dirs.iter().enumerate() {
        let origin = origin_labels
            .get(index)
            .cloned()
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        let mut candidates = Vec::new();
        if is_plain_file(&root.join(SKILL_FILE)) {
            candidates.push(root.clone());
        }
        if let Ok(entries) = fs::read_dir(root) {
            let mut children: Vec<PathBuf> = entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .map(|entry| entry.path())
                .collect();
            children.sort();
            candidates.extend(children);
        }
        let skills = candidates
            .iter()
            .filter_map(|dir| load_skill(dir, &origin))
            .take(MAX_SKILLS_PER_ROOT);
        for skill in skills {
            if seen.insert(skill.name.clone()) {
                found.push(skill);
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The skill folders of `workspace`: each of `.claude/skills` and `.agents/skills` that is a real
/// directory whose canonical path stays inside the canonical workspace (a symlink leading out of
/// it is ignored).
fn workspace_skill_roots(workspace: &Path) -> Vec<PathBuf> {
    let Ok(workspace) = fs::canonicalize(workspace) else {
        return Vec::new();
    };
    WORKSPACE_SKILL_DIRS
        .iter()
        .filter_map(|dir| fs::canonicalize(workspace.join(dir)).ok())
        .filter(|root| root.starts_with(&workspace) && root.is_dir())
        .collect()
}

/// The `SKILL.md` files of the skills in `workspace`'s own skill folders, as (path of the skill
/// folder relative to the workspace, absolute `SKILL.md` path): what the folder trust digest
/// covers (`instructions::trust_digest`). Blocking.
#[must_use]
pub fn workspace_skill_files(workspace: &Path) -> Vec<(String, PathBuf)> {
    let Ok(root) = fs::canonicalize(workspace) else {
        return Vec::new();
    };
    let roots = workspace_skill_roots(&root);
    let labels = vec![WORKSPACE_ORIGIN.to_owned(); roots.len()];
    discover_skills(&roots, &labels)
        .into_iter()
        .filter_map(|skill| {
            let dir = PathBuf::from(&skill.path);
            let relative = dir.strip_prefix(&root).ok()?.to_string_lossy().into_owned();
            Some((relative, dir.join(SKILL_FILE)))
        })
        .collect()
}

/// Skills found in the configured folders (`ai.agent.skill_dirs`), the managed folder and, when
/// given and `trusted`, the workspace's `.claude/skills` and `.agents/skills`, in that order: on a
/// duplicate name the earlier root wins, so a cloned repository can never shadow a skill the user
/// chose. An untrusted folder is never scanned: `trusted` is the answer of
/// `WorkspaceStore::is_folder_trusted` for the chat's folder.
/// Takes the folders instead of the state so the auto-exporter thread, which has none, can use it.
#[must_use]
pub fn discover(
    settings: &Settings,
    managed_dir: Option<&Path>,
    workspace_dir: Option<&Path>,
    trusted: bool,
) -> Vec<SkillInfo> {
    let mut dirs: Vec<PathBuf> = settings
        .ai
        .agent
        .skill_dirs
        .iter()
        .map(|dir| expand_home(dir))
        .collect();
    let mut labels = settings.ai.agent.skill_dirs.clone();
    if let Some(managed) = managed_dir {
        dirs.push(managed.to_path_buf());
        labels.push(MANAGED_ORIGIN.to_owned());
    }
    for root in workspace_dir
        .filter(|_| trusted)
        .map(workspace_skill_roots)
        .unwrap_or_default()
    {
        dirs.push(root);
        labels.push(WORKSPACE_ORIGIN.to_owned());
    }
    discover_skills(&dirs, &labels)
}

/// Reads the supporting text file `relative_path` of the skill folder (a `references/*.md`, a
/// script, a template).
///
/// The path must be relative, free of `..` and, after resolving symlinks, still inside the
/// skill folder. At most 256 KiB, UTF-8 text only.
///
/// # Errors
/// `Validation` for an escaping, oversized or non-text file; `NotFound` when it does not exist;
/// `Io` when it cannot be read.
pub fn read_skill_file(skill: &SkillInfo, relative_path: &str) -> Result<String, AppError> {
    let relative = Path::new(relative_path);
    let confined = !relative_path.is_empty()
        && !relative_path.contains('\0')
        && !relative.is_absolute()
        && relative
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
    if !confined {
        return Err(AppError::validation(
            "the path must be relative to the skill folder and must not contain `..`",
        ));
    }
    let io = |error: std::io::Error| match error.kind() {
        std::io::ErrorKind::NotFound => AppError::not_found(format!(
            "`{relative_path}` not found in skill `{}`",
            skill.name
        )),
        _ => AppError::io(format!("cannot read `{relative_path}`: {error}")),
    };
    let root = fs::canonicalize(&skill.path).map_err(io)?;
    let target = fs::canonicalize(root.join(relative)).map_err(io)?;
    if !target.starts_with(&root) {
        return Err(AppError::validation(format!(
            "`{relative_path}` leads outside the skill folder"
        )));
    }
    let meta = fs::metadata(&target).map_err(io)?;
    if !meta.is_file() {
        return Err(AppError::validation(format!(
            "`{relative_path}` is not a file"
        )));
    }
    if meta.len() > MAX_SKILL_READ_BYTES as u64 {
        return Err(AppError::validation(format!(
            "`{relative_path}` is larger than {} KiB",
            MAX_SKILL_READ_BYTES / 1024
        )));
    }
    let bytes = fs::read(&target).map_err(io)?;
    match String::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => Ok(text),
        _ => Err(AppError::validation(format!(
            "`{relative_path}` is not a text file"
        ))),
    }
}

/// Reads the whole `SKILL.md` of `info`, cut to `max_bytes` with a truncation marker.
///
/// # Errors
/// `Io` when the file cannot be read.
pub fn read_skill_text(info: &SkillInfo, max_bytes: usize) -> Result<String, AppError> {
    let io =
        |error: std::io::Error| AppError::io(format!("cannot read skill `{}`: {error}", info.name));
    let skill_file = Path::new(&info.path).join(SKILL_FILE);
    if !is_plain_file(&skill_file) {
        return Err(AppError::io(format!(
            "cannot read skill `{}`: {SKILL_FILE} is missing or a symlink",
            info.name
        )));
    }
    let file = fs::File::open(skill_file).map_err(io)?;
    let total = file.metadata().map_err(io)?.len();
    let mut buffer = Vec::new();
    file.take(u64::try_from(max_bytes).unwrap_or(u64::MAX))
        .read_to_end(&mut buffer)
        .map_err(io)?;
    let remaining = total.saturating_sub(u64::try_from(buffer.len()).unwrap_or(u64::MAX));
    let mut text = String::from_utf8_lossy(&buffer).into_owned();
    if remaining > 0 {
        if text.ends_with('\u{FFFD}') {
            text.pop();
        }
        text.push_str(&format!("\n… [truncated, {remaining} more bytes]"));
    }
    Ok(text)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The system-prompt block listing the skills; empty when there are none. The list stops at
/// [`SKILL_CATALOG_BYTES`]; the skills past it are named in one line so `read_skill` still finds
/// them.
#[must_use]
pub fn skills_prompt_section(skills: &[SkillInfo]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut lines = String::new();
    let mut omitted = Vec::new();
    for skill in skills {
        let name = one_line(&skill.name);
        let description: String = one_line(&skill.description)
            .chars()
            .take(MAX_DESCRIPTION_CHARS)
            .collect();
        let description = description.trim_end();
        let line = if description.is_empty() {
            format!("- {name}\n")
        } else {
            format!("- {name}: {description}\n")
        };
        if omitted.is_empty() && lines.len() + line.len() <= SKILL_CATALOG_BYTES {
            lines.push_str(&line);
        } else {
            omitted.push(name);
        }
    }
    if !omitted.is_empty() {
        lines.push_str(&format!(
            "- Omitted for the catalogue budget: {}\n",
            omitted.join(", ")
        ));
    }
    format!(
        "## Available skills\n\n{lines}\nBefore using a skill, call the `read_skill` tool with the skill name to load its full instructions.\n"
    )
}

/// Most of one skill's text inlined into a message (32 KiB).
pub const SKILL_INLINE_MAX_BYTES: usize = 32 * 1024;
/// Most skill text inlined into one message in total (64 KiB).
pub const SKILLS_INLINE_TOTAL_BYTES: usize = 64 * 1024;

/// An attached skill's text, or why it could not be read.
pub type SkillBody = (String, Result<String, SkillUndeliveredReason>);

/// Reads the text of each attached skill `names` (deduplicated, in order) among `available`;
/// a text over [`SKILL_INLINE_MAX_BYTES`] comes back cut with its marker, so it renders as too long.
#[must_use]
pub fn skill_bodies(available: &[SkillInfo], names: &[String]) -> Vec<SkillBody> {
    let mut seen = HashSet::new();
    names
        .iter()
        .filter(|name| seen.insert(name.as_str()))
        .map(|name| {
            let body = match available.iter().find(|skill| &skill.name == name) {
                None => Err(SkillUndeliveredReason::Unavailable),
                Some(skill) => read_skill_text(skill, SKILL_INLINE_MAX_BYTES)
                    .map_err(|_| SkillUndeliveredReason::Unreadable),
            };
            (name.clone(), body)
        })
        .collect()
}

fn attribute(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// The `<skill name="x">…</skill>` blocks the model reads with a message, and the skills that did
/// not arrive in full. Each text may use [`SKILL_INLINE_MAX_BYTES`] and all of them
/// [`SKILLS_INLINE_TOTAL_BYTES`]; a text over either points the model to `read_skill`, a missing
/// skill leaves one line saying so. Empty text when `bodies` is empty.
#[must_use]
pub fn render_skill_blocks(bodies: &[SkillBody]) -> (String, Vec<SkillUndelivered>) {
    let (mut blocks, mut undelivered, mut used) = (Vec::new(), Vec::new(), 0usize);
    for (name, body) in bodies {
        let label = attribute(name);
        let reason = match body {
            Ok(text)
                if text.len() <= SKILL_INLINE_MAX_BYTES
                    && used + text.len() <= SKILLS_INLINE_TOTAL_BYTES =>
            {
                used += text.len();
                blocks.push(format!("<skill name=\"{label}\">\n{text}\n</skill>"));
                continue;
            }
            Ok(_) => {
                blocks.push(format!(
                    "<skill name=\"{label}\">The skill text is too long to attach here; call \
                     `read_skill` with name \"{label}\" first.</skill>"
                ));
                SkillUndeliveredReason::TooLong
            }
            Err(reason) => {
                blocks.push(format!(
                    "[The user attached the skill \"{label}\", but it is not available now.]"
                ));
                *reason
            }
        };
        undelivered.push(SkillUndelivered {
            name: name.clone(),
            reason,
        });
    }
    (blocks.join("\n\n"), undelivered)
}

/// Keeps ASCII alphanumerics, `-` and `_`; other runs become one `-`, edges are trimmed.
fn sanitize_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

fn import_io(error: std::io::Error) -> AppError {
    AppError::io(format!("cannot import skill: {error}"))
}

#[derive(Default)]
struct Walk {
    /// Relative paths and whether each is a directory; parents come before children.
    entries: Vec<(PathBuf, bool)>,
    files: usize,
    bytes: u64,
}

/// Lists regular files and directories below `root/rel`, skipping symlinks and enforcing limits.
fn walk(root: &Path, rel: &Path, found: &mut Walk) -> Result<(), AppError> {
    for entry in fs::read_dir(root.join(rel)).map_err(import_io)? {
        let entry = entry.map_err(import_io)?;
        let file_type = entry.file_type().map_err(import_io)?;
        let rel = rel.join(entry.file_name());
        if file_type.is_dir() {
            found.entries.push((rel.clone(), true));
            walk(root, &rel, found)?;
        } else if file_type.is_file() {
            found.files += 1;
            found.bytes += entry.metadata().map_err(import_io)?.len();
            if found.files > MAX_IMPORT_FILES || found.bytes > MAX_IMPORT_BYTES {
                return Err(AppError::validation(format!(
                    "skill is too large to import (limit {} MiB and {MAX_IMPORT_FILES} files)",
                    MAX_IMPORT_BYTES >> 20
                )));
            }
            found.entries.push((rel, false));
        }
    }
    Ok(())
}

/// Copies the skill folder `source_dir` into `managed_dir/<sanitized name>`.
///
/// Symlinks are skipped; the copy is limited to 5 MiB and 500 files. `managed_dir` is created
/// when missing.
///
/// # Errors
/// Validation when `SKILL.md` is missing/invalid, the name sanitizes to nothing or the folder is
/// too large; `Conflict` when the target exists; `Io` when copying fails.
pub fn import_skill(source_dir: &Path, managed_dir: &Path) -> Result<SkillInfo, AppError> {
    if !is_plain_file(&source_dir.join(SKILL_FILE)) {
        return Err(AppError::validation(format!(
            "the folder must contain a {SKILL_FILE} file"
        )));
    }
    let info = load_skill(source_dir, "managed")
        .ok_or_else(|| AppError::validation(format!("{SKILL_FILE} could not be read or parsed")))?;
    let folder = sanitize_name(&info.name);
    if folder.is_empty() {
        return Err(AppError::validation(format!(
            "skill name `{}` has no usable ASCII letters or digits",
            info.name
        )));
    }

    let mut found = Walk::default();
    walk(source_dir, Path::new(""), &mut found)?;

    fs::create_dir_all(managed_dir).map_err(import_io)?;
    let target = managed_dir.join(&folder);
    match fs::create_dir(&target) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(AppError::conflict(format!(
                "a skill folder named `{folder}` already exists"
            )));
        }
        Err(error) => return Err(import_io(error)),
    }
    let copied = found.entries.iter().try_for_each(|(rel, is_dir)| {
        if *is_dir {
            fs::create_dir_all(target.join(rel))
        } else {
            fs::copy(source_dir.join(rel), target.join(rel)).map(drop)
        }
    });
    if let Err(error) = copied {
        let _ = fs::remove_dir_all(&target);
        return Err(import_io(error));
    }
    let path = std::path::absolute(&target).map_err(import_io)?;
    Ok(SkillInfo {
        path: path.to_string_lossy().into_owned(),
        ..info
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_dir::TempDir;

    fn write_skill(root: &Path, folder: &str, content: &str) -> PathBuf {
        let dir = root.join(folder);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(SKILL_FILE), content).unwrap();
        dir
    }

    fn names(skills: &[SkillInfo]) -> Vec<&str> {
        skills.iter().map(|skill| skill.name.as_str()).collect()
    }

    #[test]
    fn frontmatter_is_parsed_with_defaults_and_invalid_entries_are_skipped() {
        let temp = TempDir::new("frontmatter");
        let root = &temp.0;
        let alpha = write_skill(
            root,
            "a",
            "---\nname: Alpha\ndescription: >\n  first line\n  second line\nextra: [1, 2]\n---\n# Body\n---\nmore\n",
        );
        write_skill(root, "b", "---\ndescription: bee\n---\nbody");
        write_skill(root, "c", "# no frontmatter at all");
        write_skill(root, "d", "---\nname: [unclosed\n---\n");
        write_skill(root, "e", "---\nname: never closed\n");
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("notes.txt"), "x").unwrap();

        let skills = discover_skills(std::slice::from_ref(root), &["label".to_owned()]);
        assert_eq!(names(&skills), ["Alpha", "b", "c", "e"]);
        assert_eq!(skills[0].description, "first line second line");
        assert_eq!(skills[0].path, alpha.to_string_lossy());
        assert_eq!(skills[0].origin, "label");
        assert_eq!(skills[1].description, "bee");
        assert_eq!(skills[2].description, "");
    }

    #[test]
    fn first_root_wins_duplicates_and_a_root_with_skill_md_counts_itself() {
        let first = TempDir::new("first");
        let second = TempDir::new("second");
        write_skill(
            &first.0,
            "dup",
            "---\nname: dup\ndescription: from first\n---\n",
        );
        write_skill(&first.0, "zeta", "---\nname: zeta\n---\n");
        write_skill(
            &second.0,
            "dup-folder",
            "---\nname: dup\ndescription: from second\n---\n",
        );
        fs::write(second.0.join(SKILL_FILE), "---\nname: self\n---\n").unwrap();

        let skills = discover_skills(&[first.0.clone(), second.0.clone()], &["one".to_owned()]);
        assert_eq!(names(&skills), ["dup", "self", "zeta"]);
        assert_eq!(skills[0].description, "from first");
        assert_eq!(skills[0].origin, "one");
        assert_eq!(skills[1].origin, second.0.to_string_lossy());
        assert_eq!(skills[1].path, second.0.to_string_lossy());
        assert!(discover_skills(&[first.0.join("missing")], &[]).is_empty());
    }

    #[test]
    fn at_most_200_skills_are_taken_per_root() {
        let temp = TempDir::new("cap");
        for index in 0..205 {
            write_skill(&temp.0, &format!("s{index:03}"), "x");
        }
        assert_eq!(
            discover_skills(std::slice::from_ref(&temp.0), &[]).len(),
            200
        );
    }

    #[test]
    fn read_skill_text_truncates_with_a_marker() {
        let temp = TempDir::new("read");
        write_skill(&temp.0, "s", "0123456789");
        let skills = discover_skills(std::slice::from_ref(&temp.0), &[]);
        assert_eq!(read_skill_text(&skills[0], 100).unwrap(), "0123456789");
        assert_eq!(
            read_skill_text(&skills[0], 4).unwrap(),
            "0123\n… [truncated, 6 more bytes]"
        );
        let gone = SkillInfo {
            path: temp.0.join("gone").to_string_lossy().into_owned(),
            ..skills[0].clone()
        };
        assert!(matches!(
            read_skill_text(&gone, 4),
            Err(AppError::Io { .. })
        ));
    }

    #[test]
    fn expand_home_replaces_only_a_leading_tilde() {
        let home = Some("/home/me");
        assert_eq!(expand_home_with("~", home), PathBuf::from("/home/me"));
        assert_eq!(
            expand_home_with("~/.claude/skills", home),
            PathBuf::from("/home/me/.claude/skills")
        );
        assert_eq!(
            expand_home_with("~other/x", home),
            PathBuf::from("~other/x")
        );
        assert_eq!(
            expand_home_with("/abs/~/x", home),
            PathBuf::from("/abs/~/x")
        );
        assert_eq!(expand_home_with("~/x", None), PathBuf::from("~/x"));
        assert_eq!(expand_home_with("~/x", Some("")), PathBuf::from("~/x"));
    }

    #[test]
    fn prompt_section_lists_one_line_per_skill() {
        assert_eq!(skills_prompt_section(&[]), "");
        let skill = |name: &str, description: &str| SkillInfo {
            name: name.to_owned(),
            description: description.to_owned(),
            path: String::new(),
            origin: String::new(),
        };
        let long = "x".repeat(500);
        let section = skills_prompt_section(&[
            skill("one", "multi\nline  text"),
            skill("two", &long),
            skill("three", ""),
        ]);
        assert!(section.starts_with("## Available skills\n\n- one: multi line text\n- two: xxxx"));
        assert!(section.contains(&format!("- two: {}\n- three\n", "x".repeat(500))));
        assert!(section.contains("`read_skill`"));
        // Past the catalogue budget the rest is only named.
        let many: Vec<SkillInfo> = (0..20)
            .map(|index| skill(&format!("s{index:02}"), &"d".repeat(1_000)))
            .collect();
        let section = skills_prompt_section(&many);
        assert!(section.contains("- s07: ") && !section.contains("- s08: "));
        assert!(section.contains("- Omitted for the catalogue budget: s08, s09,"));
        assert!(section.len() < SKILL_CATALOG_BYTES + 400);
    }

    fn no_configured_dirs() -> Settings {
        let mut settings = Settings::default();
        settings.ai.agent.skill_dirs = Vec::new();
        settings
    }

    #[test]
    fn workspace_skills_come_from_both_folders_and_rank_after_configured_ones() {
        let workspace = TempDir::new("ws");
        let configured = TempDir::new("ws-configured");
        write_skill(
            &workspace.0.join(".claude/skills"),
            "claude-one",
            "---\nname: claude-one\n---\n",
        );
        write_skill(
            &workspace.0.join(".agents/skills"),
            "agents-one",
            "---\nname: agents-one\ndescription: d\n---\n",
        );
        write_skill(
            &workspace.0.join(".agents/skills"),
            "dup",
            "---\nname: dup\ndescription: from the repo\n---\n",
        );
        write_skill(
            &configured.0,
            "dup",
            "---\nname: dup\ndescription: chosen by the user\n---\n",
        );
        let mut settings = no_configured_dirs();
        settings.ai.agent.skill_dirs = vec![configured.0.to_string_lossy().into_owned()];

        let found = discover(&settings, None, Some(&workspace.0), true);
        assert_eq!(names(&found), ["agents-one", "claude-one", "dup"]);
        let origin = |name: &str| {
            found
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .origin
                .clone()
        };
        assert_eq!(origin("claude-one"), WORKSPACE_ORIGIN);
        assert_eq!(origin("agents-one"), WORKSPACE_ORIGIN);
        assert_eq!(
            found.iter().find(|s| s.name == "dup").unwrap().description,
            "chosen by the user",
            "a repository cannot shadow a configured skill"
        );
        assert!(
            discover(&settings, None, None, false)
                .iter()
                .all(|s| s.name == "dup")
        );
        // A workspace that does not exist or has no skill folders adds nothing.
        assert!(
            discover(
                &no_configured_dirs(),
                None,
                Some(&workspace.0.join("nope")),
                true
            )
            .is_empty()
        );
        assert!(discover(&no_configured_dirs(), None, Some(&configured.0), true).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_skill_folders_that_leave_the_workspace_are_ignored() {
        use std::os::unix::fs::symlink;

        let outside = TempDir::new("ws-outside");
        write_skill(
            &outside.0.join("skills"),
            "stolen",
            "---\nname: stolen\n---\n",
        );
        let workspace = TempDir::new("ws-links");
        // `.claude` itself and `.agents/skills` both lead out of the workspace.
        symlink(&outside.0, workspace.0.join(".claude")).unwrap();
        fs::create_dir_all(workspace.0.join(".agents")).unwrap();
        symlink(outside.0.join("skills"), workspace.0.join(".agents/skills")).unwrap();
        assert!(discover(&no_configured_dirs(), None, Some(&workspace.0), true).is_empty());

        // A symlink that stays inside the workspace is fine; a skill folder linked from outside
        // into a real skills folder is still skipped.
        let inside = TempDir::new("ws-inside");
        write_skill(&inside.0.join("shared"), "ok", "---\nname: ok\n---\n");
        fs::create_dir_all(inside.0.join(".agents")).unwrap();
        symlink(inside.0.join("shared"), inside.0.join(".agents/skills")).unwrap();
        fs::create_dir_all(inside.0.join(".claude/skills")).unwrap();
        symlink(
            outside.0.join("skills/stolen"),
            inside.0.join(".claude/skills/stolen"),
        )
        .unwrap();
        assert_eq!(
            names(&discover(
                &no_configured_dirs(),
                None,
                Some(&inside.0),
                true
            )),
            ["ok"]
        );
    }

    fn skill_with_files() -> (TempDir, SkillInfo) {
        let temp = TempDir::new("files");
        let dir = write_skill(&temp.0, "s", "---\nname: s\n---\nbody");
        fs::create_dir_all(dir.join("references/deep")).unwrap();
        fs::write(dir.join("references/api.md"), "api notes").unwrap();
        fs::write(dir.join("references/deep/x.txt"), "deep").unwrap();
        let info = discover_skills(std::slice::from_ref(&temp.0), &[]).remove(0);
        (temp, info)
    }

    #[test]
    fn supporting_files_are_read_relative_to_the_skill_folder() {
        let (_temp, skill) = skill_with_files();
        assert_eq!(
            read_skill_file(&skill, "references/api.md").unwrap(),
            "api notes"
        );
        assert_eq!(
            read_skill_file(&skill, "./references/deep/x.txt").unwrap(),
            "deep"
        );
        assert_eq!(
            read_skill_file(&skill, SKILL_FILE).unwrap(),
            "---\nname: s\n---\nbody"
        );
        assert!(matches!(
            read_skill_file(&skill, "references/none.md"),
            Err(AppError::NotFound { .. })
        ));
        for bad in [
            "",
            "../outside.txt",
            "references/../../outside.txt",
            "/etc/hosts",
            "references/../..",
            "a\0b",
        ] {
            assert!(
                matches!(
                    read_skill_file(&skill, bad),
                    Err(AppError::Validation { .. })
                ),
                "{bad:?}"
            );
        }
        // A folder is not a file.
        assert!(matches!(
            read_skill_file(&skill, "references"),
            Err(AppError::Validation { .. })
        ));
    }

    #[test]
    fn supporting_files_must_be_small_text() {
        let (_temp, skill) = skill_with_files();
        let dir = Path::new(&skill.path);
        fs::write(dir.join("exact.txt"), vec![b'x'; MAX_SKILL_READ_BYTES]).unwrap();
        assert_eq!(
            read_skill_file(&skill, "exact.txt").unwrap().len(),
            MAX_SKILL_READ_BYTES
        );
        fs::write(dir.join("big.txt"), vec![b'x'; MAX_SKILL_READ_BYTES + 1]).unwrap();
        fs::write(dir.join("binary.bin"), [0_u8, 159, 146, 150]).unwrap();
        fs::write(dir.join("nul.txt"), "a\0b").unwrap();
        for file in ["big.txt", "binary.bin", "nul.txt"] {
            assert!(
                matches!(
                    read_skill_file(&skill, file),
                    Err(AppError::Validation { .. })
                ),
                "{file}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_supporting_file_symlink_may_not_leave_the_skill_folder() {
        use std::os::unix::fs::symlink;

        let outside = TempDir::new("files-outside");
        fs::write(outside.0.join("secret.txt"), "TOP SECRET").unwrap();
        let (_temp, skill) = skill_with_files();
        let dir = Path::new(&skill.path);
        symlink(outside.0.join("secret.txt"), dir.join("leak.txt")).unwrap();
        symlink(&outside.0, dir.join("leakdir")).unwrap();
        symlink(dir.join("references/api.md"), dir.join("alias.md")).unwrap();
        for escape in ["leak.txt", "leakdir/secret.txt"] {
            let error = read_skill_file(&skill, escape).unwrap_err();
            assert!(
                matches!(error, AppError::Validation { .. }),
                "{escape}: {error:?}"
            );
            assert!(!error.message().contains("TOP SECRET"));
        }
        assert_eq!(read_skill_file(&skill, "alias.md").unwrap(), "api notes");
    }

    #[test]
    fn import_copies_files_and_skips_symlinks() {
        let source = TempDir::new("import-src");
        let managed = TempDir::new("import-dst");
        let managed_dir = managed.0.join("nested/managed");
        let skill = write_skill(
            &source.0,
            "My Skill",
            "---\nname: My Skill!\ndescription: d\n---\nbody",
        );
        fs::create_dir_all(skill.join("refs/deep")).unwrap();
        fs::write(skill.join("refs/deep/a.txt"), "deep").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hosts", skill.join("link.txt")).unwrap();
            std::os::unix::fs::symlink("/etc", skill.join("linkdir")).unwrap();
        }

        let info = import_skill(&skill, &managed_dir).unwrap();
        let target = managed_dir.join("My-Skill");
        assert_eq!(info.name, "My Skill!");
        assert_eq!(info.description, "d");
        assert_eq!(info.origin, "managed");
        assert_eq!(info.path, target.to_string_lossy());
        assert_eq!(
            fs::read_to_string(target.join("refs/deep/a.txt")).unwrap(),
            "deep"
        );
        assert!(target.join(SKILL_FILE).is_file());
        assert!(fs::symlink_metadata(target.join("link.txt")).is_err());
        assert!(fs::symlink_metadata(target.join("linkdir")).is_err());

        let again = import_skill(&skill, &managed_dir).unwrap_err();
        assert!(matches!(again, AppError::Conflict { .. }), "{again:?}");
        assert_eq!(
            names(&discover_skills(&[managed_dir], &["managed".to_owned()])),
            ["My Skill!"]
        );
    }

    #[test]
    fn import_rejects_bad_sources() {
        let source = TempDir::new("import-bad");
        let managed = TempDir::new("import-bad-dst");

        let no_file = source.0.join("none");
        fs::create_dir_all(&no_file).unwrap();
        assert!(matches!(
            import_skill(&no_file, &managed.0),
            Err(AppError::Validation { .. })
        ));

        let unicode = write_skill(&source.0, "u", "---\nname: 日本語\n---\n");
        assert!(matches!(
            import_skill(&unicode, &managed.0),
            Err(AppError::Validation { .. })
        ));

        let many = write_skill(&source.0, "many", "x");
        for index in 0..=MAX_IMPORT_FILES {
            fs::write(many.join(format!("f{index}")), "").unwrap();
        }
        assert!(matches!(
            import_skill(&many, &managed.0),
            Err(AppError::Validation { .. })
        ));

        let big = write_skill(&source.0, "big", "x");
        fs::write(
            big.join("blob"),
            vec![0_u8; (MAX_IMPORT_BYTES + 1) as usize],
        )
        .unwrap();
        assert!(matches!(
            import_skill(&big, &managed.0),
            Err(AppError::Validation { .. })
        ));
        assert!(!managed.0.join("many").exists() && !managed.0.join("big").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_skill_files_and_folders_are_never_followed() {
        use std::os::unix::fs::symlink;

        let outside = TempDir::new("outside");
        let secret = outside.0.join("secret.md");
        fs::write(&secret, "---\nname: stolen\n---\nTOP SECRET").unwrap();
        let elsewhere = write_skill(&outside.0, "real", "---\nname: elsewhere\n---\n");

        let root = TempDir::new("symlinks");
        let good = write_skill(&root.0, "good", "---\nname: good\n---\nbody");
        // A folder whose SKILL.md points outside, and a folder symlinked in from outside.
        let linked_file = root.0.join("linked-file");
        fs::create_dir_all(&linked_file).unwrap();
        symlink(&secret, linked_file.join(SKILL_FILE)).unwrap();
        symlink(&elsewhere, root.0.join("linked-dir")).unwrap();

        let skills = discover_skills(std::slice::from_ref(&root.0), &[]);
        assert_eq!(names(&skills), ["good"]);

        // A root whose own SKILL.md is a symlink does not count as a skill either.
        let self_root = TempDir::new("symlink-root");
        symlink(&secret, self_root.0.join(SKILL_FILE)).unwrap();
        assert!(discover_skills(std::slice::from_ref(&self_root.0), &[]).is_empty());

        // read_skill must refuse a SKILL.md swapped for a symlink after discovery.
        let info = skills[0].clone();
        assert!(read_skill_text(&info, 1000).unwrap().contains("body"));
        fs::remove_file(good.join(SKILL_FILE)).unwrap();
        symlink(&secret, good.join(SKILL_FILE)).unwrap();
        assert!(matches!(
            read_skill_text(&info, 1000),
            Err(AppError::Io { .. })
        ));

        // Import refuses a symlinked SKILL.md.
        let managed = TempDir::new("symlink-managed");
        assert!(matches!(
            import_skill(&good, &managed.0),
            Err(AppError::Validation { .. })
        ));
        assert!(matches!(
            import_skill(&linked_file, &managed.0),
            Err(AppError::Validation { .. })
        ));
        assert_eq!(fs::read_dir(&managed.0).unwrap().count(), 0);
    }

    #[test]
    fn skill_blocks_keep_the_per_skill_and_total_budgets() {
        let body = |n: usize| Ok("x".repeat(n));
        let bodies = vec![
            ("a".to_owned(), body(SKILL_INLINE_MAX_BYTES)),
            ("b".to_owned(), body(SKILL_INLINE_MAX_BYTES + 1)),
            ("c".to_owned(), body(SKILL_INLINE_MAX_BYTES)),
            ("d".to_owned(), body(1)),
            ("e\"".to_owned(), Err(SkillUndeliveredReason::Unavailable)),
        ];
        let (text, undelivered) = render_skill_blocks(&bodies);
        let reasons: Vec<_> = undelivered
            .iter()
            .map(|skill| (skill.name.as_str(), skill.reason))
            .collect();
        // `a` and `c` fill the 64 KiB total exactly, so `d` no longer fits.
        assert_eq!(
            reasons,
            [
                ("b", SkillUndeliveredReason::TooLong),
                ("d", SkillUndeliveredReason::TooLong),
                ("e\"", SkillUndeliveredReason::Unavailable),
            ]
        );
        assert_eq!(text.matches("xxxxxxxx\n</skill>").count(), 2);
        assert!(text.contains("call `read_skill` with name \"b\""));
        assert!(text.contains("skill \"e&quot;\", but it is not available"));
        assert_eq!(render_skill_blocks(&[]), (String::new(), Vec::new()));
    }

    #[test]
    fn skill_bodies_read_each_attached_skill_once_and_report_missing_ones() {
        let root = TempDir::new("bodies");
        write_skill(&root.0, "one", "---\nname: one\n---\nbody one");
        let available = discover_skills(std::slice::from_ref(&root.0), &["r".to_owned()]);
        let names = ["one", "gone", "one"].map(str::to_owned);
        let bodies = skill_bodies(&available, &names);
        assert_eq!(bodies.len(), 2);
        assert!(bodies[0].1.as_ref().unwrap().ends_with("body one"));
        assert_eq!(bodies[1].1, Err(SkillUndeliveredReason::Unavailable));
    }
}
