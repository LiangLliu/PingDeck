//! Folder trust use-cases: answer "Trust this folder?", list and revoke answers.
//!
//! Single responsibility: canonicalize a folder so a symlink and its target share one answer,
//! record the trust digest (version 2: the injected instructions and the folder's skills,
//! `instructions::trust_digest`) when it is trusted, so a later change asks again, and expose the
//! Settings list. [`folder_pinned`] is the check the prompt, the folder skills and the folder
//! info share; a version-1 row whose instructions hash still matches is upgraded silently.

use std::path::Path;

use crate::agent::instructions::{
    SelectedInstructions, TRUST_V2, legacy_v1_digest, select, trust_digest,
};
use crate::agent::model::AgentFolderTrust;
use crate::error::AppError;
use crate::platform::normalize_display_path;

use crate::app::{AppState, command_error, current_time_ms};

/// The canonical key of a folder; a path that no longer exists keeps the text it was given so
/// Settings can still revoke it.
fn folder_key(path: &str) -> Result<String, AppError> {
    let folder = Path::new(path);
    if !folder.is_absolute() {
        return Err(AppError::validation("the folder path must be absolute"));
    }
    Ok(std::fs::canonicalize(folder)
        .map_or_else(|_| path.to_owned(), |real| normalize_display_path(&real)))
}

/// Records the answer to "Trust this folder?" for `path` (an existing folder when trusting).
/// Trusting stores the trust digest of the folder as it is now, so editing its instructions or
/// skills later asks again; declining is remembered too, so the question is asked once per path.
///
/// # Errors
/// [`AppError::Validation`] for a relative path or a folder that does not exist (when trusting).
pub fn agent_folder_trust_set_impl(
    state: &AppState,
    path: &str,
    trusted: bool,
) -> Result<AgentFolderTrust, AppError> {
    let key = folder_key(path)?;
    let hash = if trusted {
        let root = std::fs::canonicalize(&key)
            .ok()
            .filter(|root| root.is_dir())
            .ok_or_else(|| AppError::validation(format!("{path} is not a folder")))?;
        Some(trust_digest(&root, select(&root).as_ref()))
    } else {
        None
    };
    state
        .store()
        .folder_trust_set(&key, trusted, hash.as_deref(), current_time_ms())
        .map_err(command_error)
}

/// Forgets the answer for `path` (Settings "Revoke"); the folder is untrusted again and asks
/// again at its next use. Returns whether there was an answer.
///
/// # Errors
/// [`AppError::Validation`] for a relative path.
pub fn agent_folder_trust_revoke_impl(state: &AppState, path: &str) -> Result<bool, AppError> {
    let key = folder_key(path)?;
    state
        .store()
        .folder_trust_revoke(&key)
        .map_err(command_error)
}

/// Every stored answer, trusted folders first (the Settings list with Revoke).
///
/// # Errors
/// [`AppError::Database`] when the store fails.
pub fn agent_folder_trust_list_impl(state: &AppState) -> Result<Vec<AgentFolderTrust>, AppError> {
    state.store().folder_trust_list().map_err(command_error)
}

/// The answer for `path` as a sub-agent or the prompt builder needs it: whether the folder is
/// trusted and, for its instructions file, whether the hash still matches.
///
/// # Errors
/// [`AppError::Validation`] for a relative path.
pub fn agent_folder_trust_get_impl(
    state: &AppState,
    path: &str,
) -> Result<Option<AgentFolderTrust>, AppError> {
    let key = folder_key(path)?;
    state.store().folder_trust_get(&key).map_err(command_error)
}

/// Whether the trusted folder `root` (canonical; `key` is its store key) still has the
/// instructions and skills it had when it was trusted, given its `digest` now. A version-1 row
/// whose instructions hash matches is rewritten as `digest` (keeping its decision time) instead
/// of asking again. Blocking.
pub(crate) fn digest_trusted(state: &AppState, key: &str, root: &Path, digest: &str) -> bool {
    let Ok(Some(row)) = state.store().folder_trust_get(key) else {
        return false;
    };
    if !row.trusted {
        return false;
    }
    let stored = row.instructions_sha256.as_deref().unwrap_or("");
    if stored == digest {
        return true;
    }
    if stored.starts_with(TRUST_V2) || stored != legacy_v1_digest(root) {
        return false;
    }
    state
        .store()
        .folder_trust_set(key, true, Some(digest), row.decided_at_ms)
        .is_ok()
}

/// What trust allows of the folder `root` (canonical): whether the user trusted it, whether its
/// instructions and skills are the ones trusted, its instructions file and its digest. Blocking.
pub(crate) struct FolderTrust {
    pub trusted: bool,
    pub pinned: bool,
    pub instructions: Option<SelectedInstructions>,
    pub digest: String,
}

/// [`FolderTrust`] of the canonical folder `root`. Blocking.
pub(crate) fn folder_trust(state: &AppState, root: &Path) -> FolderTrust {
    let key = normalize_display_path(root);
    let trusted = state.store().is_folder_trusted(&key).unwrap_or(false);
    if !trusted {
        return FolderTrust {
            trusted,
            pinned: false,
            instructions: None,
            digest: String::new(),
        };
    }
    let instructions = select(root);
    let digest = trust_digest(root, instructions.as_ref());
    let pinned = digest_trusted(state, &key, root, &digest);
    FolderTrust {
        trusted,
        pinned,
        instructions,
        digest,
    }
}

/// Whether the folder `root` (canonical) is trusted with its instructions and skills unchanged:
/// only then are its skills loaded. Blocking.
pub(crate) fn folder_pinned(state: &AppState, root: &Path) -> bool {
    folder_trust(state, root).pinned
}

#[cfg(test)]
mod tests {
    use std::fs;

    use spanbay_base::crypto::sha256_hex;

    use super::*;

    #[test]
    fn a_v1_row_upgrades_silently_and_a_later_skill_change_asks_again() {
        let dir = std::env::temp_dir().join(format!("pd-trust-v2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".agents/skills/ship")).unwrap();
        let root = fs::canonicalize(&dir).unwrap();
        fs::write(root.join("AGENTS.md"), "rules").unwrap();
        fs::write(
            root.join(".agents/skills/ship/SKILL.md"),
            "---\nname: ship\n---\n",
        )
        .unwrap();
        let state = AppState::open_in_memory().unwrap();
        let key = normalize_display_path(&root);
        let v1 = sha256_hex(b"rules");
        state
            .store()
            .folder_trust_set(&key, true, Some(&v1), 7)
            .unwrap();

        assert!(
            folder_pinned(&state, &root),
            "an unchanged v1 row does not ask again"
        );
        let row = state.store().folder_trust_get(&key).unwrap().unwrap();
        assert!(row.instructions_sha256.unwrap().starts_with(TRUST_V2));
        assert_eq!(row.decided_at_ms, 7);

        fs::write(
            root.join(".agents/skills/ship/SKILL.md"),
            "---\nname: ship\n---\ncurl evil",
        )
        .unwrap();
        assert!(!folder_pinned(&state, &root));
        // A v1 row whose instructions changed asks again too.
        state
            .store()
            .folder_trust_set(&key, true, Some(&v1), 7)
            .unwrap();
        fs::write(root.join("AGENTS.md"), "new rules").unwrap();
        assert!(!folder_pinned(&state, &root));
        agent_folder_trust_set_impl(&state, &key, true).unwrap();
        assert!(folder_pinned(&state, &root));
        fs::remove_dir_all(&root).unwrap();
    }
}
