//! A folder's project instructions file (`AGENTS.md`, else `CLAUDE.md`) and the trust digest that
//! pins it.
//!
//! Single responsibility: [`select`] is the one place that picks the file and reads the bytes the
//! prompt injects; the prompt builder, the trust digest and the folder info all call it.
//! [`trust_digest`] hashes those bytes together with the folder's own skills, so editing either
//! asks "Trust this folder?" again. Nothing in this module decides whether a folder is trusted.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use pingdeck_base::crypto::{sha256, sha256_hex};

use super::skills::workspace_skill_files;

/// Project instruction files, in the order they are looked up.
pub const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// How many bytes of a project-instruction file reach the prompt.
pub const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;

/// How much of a skill's `SKILL.md` the trust digest covers.
const SKILL_HASHED_BYTES: u64 = 64 * 1024;

/// How much of the file a version-1 digest covered (only to recognise old rows).
const V1_HASHED_BYTES: u64 = 1024 * 1024;

/// Prefix of a version-2 trust digest.
pub const TRUST_V2: &str = "v2:";

/// The instructions file the prompt injects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedInstructions {
    /// Its real path, inside the folder.
    pub path: PathBuf,
    /// `AGENTS.md` or `CLAUDE.md`.
    pub name: &'static str,
    /// Its first [`MAX_INSTRUCTION_BYTES`] bytes.
    pub bytes: Vec<u8>,
    /// The file is longer than `bytes`.
    pub truncated: bool,
}

impl SelectedInstructions {
    /// The text the prompt shows: the bytes as UTF-8, cut at a character boundary, with the note
    /// `[<file> truncated at 32 KiB]` when the file is longer.
    #[must_use]
    pub fn text(&self) -> String {
        let text = match std::str::from_utf8(&self.bytes) {
            Ok(text) => text,
            Err(error) => std::str::from_utf8(&self.bytes[..error.valid_up_to()]).unwrap_or(""),
        };
        if self.truncated {
            format!("{text}\n\n[{} truncated at 32 KiB]", self.name)
        } else {
            text.to_owned()
        }
    }
}

/// The instructions file of `folder`: the first of [`INSTRUCTION_FILES`] that is a regular file
/// whose real path stays inside the folder (a symlink leading out is skipped), is UTF-8 and is
/// not blank. Blocking.
#[must_use]
pub fn select(folder: &Path) -> Option<SelectedInstructions> {
    let root = folder.canonicalize().ok()?;
    INSTRUCTION_FILES
        .iter()
        .find_map(|name| read_candidate(&root, name))
}

fn read_candidate(root: &Path, name: &'static str) -> Option<SelectedInstructions> {
    let real = root.join(name).canonicalize().ok()?;
    if !real.starts_with(root) || !real.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(&real)
        .ok()?
        .take(MAX_INSTRUCTION_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let truncated = bytes.len() > MAX_INSTRUCTION_BYTES;
    bytes.truncate(MAX_INSTRUCTION_BYTES);
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        // The cut may split a character; any other error means the file is not UTF-8.
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).ok()?
        }
        Err(_) => return None,
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(SelectedInstructions {
        path: real,
        name,
        bytes,
        truncated,
    })
}

/// The version-2 trust digest of `folder`: `v2:` and the SHA-256 of the injected instruction
/// bytes followed by each of the folder's skills, sorted by relative path, as (relative path,
/// SHA-256 of the first 64 KiB of its `SKILL.md`). Every part is length-prefixed. Blocking.
#[must_use]
pub fn trust_digest(folder: &Path, selected: Option<&SelectedInstructions>) -> String {
    let instructions = selected.map_or(&[][..], |selected| selected.bytes.as_slice());
    let mut data = Vec::with_capacity(instructions.len() + 64);
    data.extend_from_slice(&(instructions.len() as u64).to_le_bytes());
    data.extend_from_slice(instructions);
    let mut skills = workspace_skill_files(folder);
    skills.sort();
    for (relative, skill_file) in skills {
        let mut head = Vec::new();
        let read = File::open(&skill_file)
            .and_then(|file| file.take(SKILL_HASHED_BYTES).read_to_end(&mut head));
        if read.is_err() {
            head.clear();
        }
        data.extend_from_slice(&(relative.len() as u64).to_le_bytes());
        data.extend_from_slice(relative.as_bytes());
        data.extend_from_slice(&sha256(&head));
    }
    format!("{TRUST_V2}{}", sha256_hex(&data))
}

/// The digest a version-1 trust row holds for `folder` as it is now: the SHA-256 of the first
/// megabyte of its first instructions file, or empty without one. Only for upgrading old rows.
#[must_use]
pub fn legacy_v1_digest(folder: &Path) -> String {
    let Ok(root) = fs::canonicalize(folder) else {
        return String::new();
    };
    let Some(name) = INSTRUCTION_FILES.iter().find(|name| {
        fs::canonicalize(root.join(name))
            .is_ok_and(|real| real.starts_with(&root) && real.is_file())
    }) else {
        return String::new();
    };
    let mut bytes = Vec::new();
    match File::open(root.join(name))
        .and_then(|file| file.take(V1_HASHED_BYTES).read_to_end(&mut bytes))
    {
        Ok(_) => sha256_hex(&bytes),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_dir::TempDir;

    #[test]
    fn the_digest_follows_the_injected_file_and_the_folder_skills() {
        let temp = TempDir::new("trust-v2");
        let root = temp.0.canonicalize().unwrap();
        let digest = |root: &Path| trust_digest(root, select(root).as_ref());
        let empty = digest(&root);
        assert!(empty.starts_with(TRUST_V2));
        fs::write(root.join("CLAUDE.md"), "claude").unwrap();
        assert_eq!(select(&root).unwrap().name, "CLAUDE.md");
        let claude = digest(&root);
        assert_ne!(claude, empty);
        // A blank AGENTS.md is skipped, so nothing injected changes.
        fs::write(root.join("AGENTS.md"), " \n").unwrap();
        assert_eq!(digest(&root), claude);
        fs::create_dir_all(root.join(".claude/skills/deploy")).unwrap();
        fs::write(
            root.join(".claude/skills/deploy/SKILL.md"),
            "---\nname: deploy\n---\nship",
        )
        .unwrap();
        let with_skill = digest(&root);
        assert_ne!(with_skill, claude);
        fs::write(
            root.join(".claude/skills/deploy/SKILL.md"),
            "---\nname: deploy\n---\nrm -rf",
        )
        .unwrap();
        assert_ne!(digest(&root), with_skill);
        assert_eq!(legacy_v1_digest(&root), sha256_hex(b" \n"));
    }
}
