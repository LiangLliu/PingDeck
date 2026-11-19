//! Sync folder safety: where a folder sits and which of its files hold secret values.
//!
//! Single responsibility: answer "is this folder inside a git working tree or a known cloud-sync
//! folder?" (turning on "Include secrets in exports"
//! there needs a typed confirmation) and rewrite the files of an export that carry values with
//! placeholders ("Remove secrets from this folder").

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;

use super::SyncKind;
use super::export::{fingerprint, pretty, read_regular};
use super::secrets;

/// Where a sync folder sits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFolderRisk {
    /// The folder that was classified, as given.
    pub directory: String,
    /// The folder is inside a git working tree.
    pub in_git: bool,
    /// The cloud-sync service that syncs the folder (`iCloud Drive`, `Dropbox`, `OneDrive`,
    /// `Google Drive`, `Syncthing`), when one does.
    pub cloud_service: Option<String>,
    /// The ancestor that made the folder risky (the git working tree root or the synced folder):
    /// what the confirmation dialog names.
    pub location: Option<String>,
    /// `in_git` or a cloud service: turning on "Include secrets in exports" needs the typed phrase.
    pub risky: bool,
    /// Some file of the folder holds a secret value (the "Remove secrets" action is offered).
    pub holds_secret_values: bool,
    /// How many files hold one (what "Remove secrets" rewrites, and what its confirm names).
    pub files_with_secrets: u32,
}

/// The cloud-sync service whose folder `ancestor` is, judged by its name or a marker inside it.
fn cloud_service_of(ancestor: &Path) -> Option<&'static str> {
    let name = ancestor.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name == "com~apple~clouddocs" || name == "icloud drive" || name == "mobile documents" {
        return Some("iCloud Drive");
    }
    if name == "dropbox" || name.starts_with("dropbox ") || name.starts_with("dropbox-") {
        return Some("Dropbox");
    }
    if name == "onedrive" || name.starts_with("onedrive ") || name.starts_with("onedrive-") {
        return Some("OneDrive");
    }
    if name == "google drive" || name == "my drive" || name.starts_with("googledrive") {
        return Some("Google Drive");
    }
    if ancestor.join(".stfolder").exists() {
        return Some("Syncthing");
    }
    if ancestor.join(".dropbox").exists() || ancestor.join(".dropbox.cache").exists() {
        return Some("Dropbox");
    }
    None
}

/// Classifies `directory` (it need not exist yet: its nearest existing ancestor decides).
#[must_use]
pub fn classify(directory: &Path) -> SyncFolderRisk {
    let resolved = fs::canonicalize(directory)
        .ok()
        .or_else(|| {
            directory
                .ancestors()
                .find_map(|ancestor| fs::canonicalize(ancestor).ok())
        })
        .unwrap_or_else(|| directory.to_path_buf());
    let mut git_root: Option<PathBuf> = None;
    let mut cloud: Option<(&'static str, PathBuf)> = None;
    for ancestor in resolved.ancestors() {
        if git_root.is_none() && ancestor.join(".git").exists() {
            git_root = Some(ancestor.to_path_buf());
        }
        if cloud.is_none()
            && let Some(service) = cloud_service_of(ancestor)
        {
            cloud = Some((service, ancestor.to_path_buf()));
        }
    }
    let location = git_root
        .as_ref()
        .or(cloud.as_ref().map(|(_, path)| path))
        .map(|path| path.display().to_string());
    let secret_files = files_with_secrets(directory).len();
    SyncFolderRisk {
        directory: directory.display().to_string(),
        in_git: git_root.is_some(),
        cloud_service: cloud.map(|(service, _)| service.to_owned()),
        risky: location.is_some(),
        location,
        holds_secret_values: secret_files > 0,
        files_with_secrets: u32::try_from(secret_files).unwrap_or(u32::MAX),
    }
}

/// The kind of document an export file holds, from its path (`settings.json` holds sections).
fn kind_of(relative: &str) -> Option<SyncKind> {
    let parts: Vec<&str> = relative.split('/').collect();
    Some(match parts.as_slice() {
        ["settings.json"] => SyncKind::Settings,
        ["llm", "providers", _] => SyncKind::LlmProvider,
        ["agent", "mcp-servers", _] => SyncKind::McpServer,
        ["workspaces", _, "globals.json"] => SyncKind::Globals,
        ["workspaces", _, "collections", _] => SyncKind::Collection,
        ["workspaces", _, "environments", _] => SyncKind::Environment,
        ["workspaces", _, "requests.json"] => SyncKind::LooseRequests,
        ["workspaces", _, "mocks", _] => SyncKind::Mock,
        ["workspaces", _, "flows", _] => SyncKind::Flow,
        _ => return None,
    })
}

/// The JSON files of `directory` that hold secrets (relative path, parsed content, masked content).
fn masked_files(directory: &Path) -> Vec<(String, Value, Value)> {
    let mut found = Vec::new();
    let mut stack = vec![(directory.to_path_buf(), String::new(), 0_u8)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth < 4 {
                stack.push((entry.path(), relative, depth + 1));
                continue;
            }
            let Some(document) = kind_of(&relative) else {
                continue;
            };
            let Some(original) = read_regular(&entry.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            else {
                continue;
            };
            let mut masked = original.clone();
            match (document, &mut masked) {
                (SyncKind::Settings, Value::Object(sections)) => sections
                    .values_mut()
                    .for_each(|section| secrets::mask(SyncKind::Settings, section)),
                (document, body) => secrets::mask(document, body),
            }
            if masked != original {
                found.push((relative, original, masked));
            }
        }
    }
    found
}

/// Relative paths of the files in `directory` that hold a secret value.
#[must_use]
pub fn files_with_secrets(directory: &Path) -> Vec<String> {
    let mut paths: Vec<String> = masked_files(directory)
        .into_iter()
        .map(|(relative, _, _)| relative)
        .collect();
    paths.sort();
    paths
}

/// Rewrites every file of `directory` that holds a secret value with placeholders in its place,
/// and clears `secrets_included` in the manifest. Returns the files written as `(relative path,
/// fingerprint)` (the manifest included), for the local ledger of written files.
///
/// # Errors
/// [`AppError::Io`] when a file cannot be rewritten; files written before the failure stay
/// rewritten.
pub(crate) fn remove_secrets(directory: &Path) -> Result<Vec<(String, String)>, AppError> {
    let mut written = Vec::new();
    for (relative, _, masked) in masked_files(directory) {
        let bytes = pretty(&masked)?;
        let path = directory.join(&relative);
        crate::platform::write_atomic(&path, &bytes, true)
            .map_err(|error| AppError::io(format!("{}: {error}", path.display())))?;
        written.push((relative, fingerprint(&bytes)));
    }
    let manifest_path = directory.join(super::export::MANIFEST);
    if let Some(mut manifest) = read_regular(&manifest_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        && manifest.get("secrets_included").and_then(Value::as_bool) == Some(true)
    {
        manifest["secrets_included"] = Value::Bool(false);
        let bytes = pretty(&manifest)?;
        crate::platform::write_atomic(&manifest_path, &bytes, true)
            .map_err(|error| AppError::io(format!("{}: {error}", manifest_path.display())))?;
    }
    Ok(written)
}
