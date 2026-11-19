//! Atomic `0600` local UI-state and unsent chat-draft persistence (replaces browser `localStorage`).
//!
//! Covers the only two persisted client-side buckets previously kept in `localStorage`:
//! - `ui_state.json` (replaces `pingdeck.ui.v2`): active module, eye-care palette, sidebar width/mode,
//!   active workspace, pane widths, command palette frecency, and open request tab ids;
//! - `chat_drafts.json` (replaces `pingdeck.chat.drafts.v1`): unsent message text per session.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use pingdeck_core::error::AppError;
use serde::{Deserialize, Serialize};

use crate::state::{NavModule, SettingsSection};
use crate::theme::PaletteId;

/// Persisted local desktop shell preferences & layout state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiLocalState {
    pub version: u32,
    pub palette: PaletteId,
    pub active_module: NavModule,
    pub active_settings_section: SettingsSection,
    pub sidebar_collapsed: bool,
    pub sidebar_width: f32,
    pub active_workspace_id: Option<String>,
    pub active_environment_id: Option<String>,
    pub pane_widths: BTreeMap<String, f32>,
    /// Command palette frecency map: `item_id -> (use_count, last_used_ms)`.
    pub palette_frecency: BTreeMap<String, (u32, i64)>,
    pub open_request_tab_ids: Vec<String>,
    pub active_request_tab_id: Option<String>,
}

impl Default for UiLocalState {
    fn default() -> Self {
        Self {
            version: 2,
            palette: PaletteId::Slate,
            active_module: NavModule::Requests,
            active_settings_section: SettingsSection::General,
            sidebar_collapsed: false,
            sidebar_width: 224.0,
            active_workspace_id: None,
            active_environment_id: None,
            pane_widths: BTreeMap::new(),
            palette_frecency: BTreeMap::new(),
            open_request_tab_ids: Vec::new(),
            active_request_tab_id: None,
        }
    }
}

impl UiLocalState {
    #[must_use]
    pub fn load_or_default(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Self>(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), AppError> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::io(format!("serialize ui_state: {e}")))?;
        atomic_write_private(path, json.as_bytes())
    }
}

/// Persisted unsent chat composer drafts keyed by `session_id` (or `"__new__"`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatDraftsStore {
    pub version: u32,
    pub drafts: BTreeMap<String, String>,
}

impl ChatDraftsStore {
    #[must_use]
    pub fn load_or_default(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Self>(&raw).ok())
            .unwrap_or(Self {
                version: 1,
                drafts: BTreeMap::new(),
            })
    }

    pub fn set(&mut self, session_id: &str, text: String) {
        if text.trim().is_empty() {
            self.drafts.remove(session_id);
        } else {
            self.drafts.insert(session_id.to_owned(), text);
        }
    }

    #[must_use]
    pub fn get(&self, session_id: &str) -> Option<&str> {
        self.drafts.get(session_id).map(String::as_str)
    }

    pub fn save(&self, path: &Path) -> Result<(), AppError> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::io(format!("serialize chat_drafts: {e}")))?;
        atomic_write_private(path, json.as_bytes())
    }
}

fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::io(format!("create {}: {e}", parent.display())))?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = open_private_file(&tmp)?;
    file.write_all(bytes)
        .map_err(|e| AppError::io(format!("write {}: {e}", tmp.display())))?;
    file.sync_all()
        .map_err(|e| AppError::io(format!("sync {}: {e}", tmp.display())))?;
    drop(file);
    fs::rename(&tmp, path).map_err(|e| AppError::io(format!("rename {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(unix)]
fn open_private_file(path: &Path) -> Result<fs::File, AppError> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| AppError::io(format!("open {}: {e}", path.display())))
}

#[cfg(not(unix))]
fn open_private_file(path: &Path) -> Result<fs::File, AppError> {
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| AppError::io(format!("open {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_ui_state_and_chat_drafts_atomically() {
        let dir =
            std::env::temp_dir().join(format!("pingdeck-desktop-persist-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let ui_path = dir.join("ui_state.json");
        let ui = UiLocalState {
            palette: PaletteId::Nord,
            active_module: NavModule::Chat,
            sidebar_collapsed: true,
            ..UiLocalState::default()
        };
        ui.save(&ui_path).unwrap();
        let loaded = UiLocalState::load_or_default(&ui_path);
        assert_eq!(loaded, ui);

        let draft_path = dir.join("chat_drafts.json");
        let mut drafts = ChatDraftsStore::default();
        drafts.set("s1", "hello draft".into());
        drafts.set("s2", "   ".into());
        drafts.save(&draft_path).unwrap();
        let loaded_drafts = ChatDraftsStore::load_or_default(&draft_path);
        assert_eq!(loaded_drafts.get("s1"), Some("hello draft"));
        assert_eq!(loaded_drafts.get("s2"), None);

        let _ = fs::remove_dir_all(&dir);
    }
}
