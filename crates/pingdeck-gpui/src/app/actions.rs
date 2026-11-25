//! Application-wide GPUI keyboard actions, canonical shortcut registry, and live rebinding.

use std::collections::{BTreeMap, HashSet};

use gpui_kit::{Action, App, KeyBinding, NoAction};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::i18n::{Settings as SettingsTr, tr};

gpui_kit::actions!(
    pingdeck,
    [
        QuitApp,
        HideWindow,
        OpenSettings,
        ToggleSidebar,
        TogglePalette,
        Nav1,
        Nav2,
        Nav3,
        Nav4,
        Nav5,
        Nav6,
        Nav7,
        Nav8,
        Nav9,
        CloseOverlay,
        SaveActive,
        SendRequest,
        ToggleConsole,
        NewTab,
        CloseTab,
    ]
);

/// Logical grouping of keyboard shortcuts shown in Settings › Shortcuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutCategory {
    Global,
    Workbench,
    Navigation,
}

impl ShortcutCategory {
    pub const ALL: [Self; 3] = [Self::Global, Self::Workbench, Self::Navigation];

    #[must_use]
    pub const fn label(self, lang: Language) -> &'static str {
        let s = tr(lang).settings();
        match self {
            Self::Global => s.shortcut_cat_global(),
            Self::Workbench => s.shortcut_cat_workbench(),
            Self::Navigation => s.shortcut_cat_navigation(),
        }
    }
}

/// Metadata for one bindable application command.
#[derive(Debug, Clone, Copy)]
pub struct ShortcutSpec {
    pub id: &'static str,
    pub default_key: &'static str,
    pub category: ShortcutCategory,
    title_fn: fn(SettingsTr) -> &'static str,
    desc_fn: fn(SettingsTr) -> &'static str,
}

impl ShortcutSpec {
    #[must_use]
    pub fn title(self, lang: Language) -> &'static str {
        (self.title_fn)(tr(lang).settings())
    }

    #[must_use]
    pub fn desc(self, lang: Language) -> &'static str {
        (self.desc_fn)(tr(lang).settings())
    }
}

/// Complete registry of all GPUI keyboard commands (single source of truth for startup binding,
/// live rebinding, and Settings › Shortcuts).
pub const SHORTCUT_SPECS: &[ShortcutSpec] = &[
    ShortcutSpec {
        id: "app.palette",
        default_key: "cmd-k",
        category: ShortcutCategory::Global,
        title_fn: SettingsTr::shortcut_app_palette_title,
        desc_fn: SettingsTr::shortcut_app_palette_desc,
    },
    ShortcutSpec {
        id: "app.sidebar",
        default_key: "cmd-b",
        category: ShortcutCategory::Global,
        title_fn: SettingsTr::shortcut_app_sidebar_title,
        desc_fn: SettingsTr::shortcut_app_sidebar_desc,
    },
    ShortcutSpec {
        id: "app.settings",
        default_key: "cmd-,",
        category: ShortcutCategory::Global,
        title_fn: SettingsTr::shortcut_app_settings_title,
        desc_fn: SettingsTr::shortcut_app_settings_desc,
    },
    ShortcutSpec {
        id: "app.close_overlay",
        default_key: "escape",
        category: ShortcutCategory::Global,
        title_fn: SettingsTr::shortcut_app_close_overlay_title,
        desc_fn: SettingsTr::shortcut_app_close_overlay_desc,
    },
    ShortcutSpec {
        id: "app.quit",
        default_key: "cmd-q",
        category: ShortcutCategory::Global,
        title_fn: SettingsTr::shortcut_app_quit_title,
        desc_fn: SettingsTr::shortcut_app_quit_desc,
    },
    ShortcutSpec {
        id: "req.send",
        default_key: "cmd-enter",
        category: ShortcutCategory::Workbench,
        title_fn: SettingsTr::shortcut_req_send_title,
        desc_fn: SettingsTr::shortcut_req_send_desc,
    },
    ShortcutSpec {
        id: "req.save",
        default_key: "cmd-s",
        category: ShortcutCategory::Workbench,
        title_fn: SettingsTr::shortcut_req_save_title,
        desc_fn: SettingsTr::shortcut_req_save_desc,
    },
    ShortcutSpec {
        id: "req.console",
        default_key: "cmd-j",
        category: ShortcutCategory::Workbench,
        title_fn: SettingsTr::shortcut_req_console_title,
        desc_fn: SettingsTr::shortcut_req_console_desc,
    },
    ShortcutSpec {
        id: "req.new_tab",
        default_key: "cmd-t",
        category: ShortcutCategory::Workbench,
        title_fn: SettingsTr::shortcut_req_new_tab_title,
        desc_fn: SettingsTr::shortcut_req_new_tab_desc,
    },
    ShortcutSpec {
        id: "req.close_tab",
        default_key: "cmd-w",
        category: ShortcutCategory::Workbench,
        title_fn: SettingsTr::shortcut_req_close_tab_title,
        desc_fn: SettingsTr::shortcut_req_close_tab_desc,
    },
    ShortcutSpec {
        id: "nav.1",
        default_key: "cmd-1",
        category: ShortcutCategory::Navigation,
        title_fn: SettingsTr::shortcut_nav_1_title,
        desc_fn: SettingsTr::shortcut_nav_1_desc,
    },
    ShortcutSpec {
        id: "nav.2",
        default_key: "cmd-2",
        category: ShortcutCategory::Navigation,
        title_fn: SettingsTr::shortcut_nav_2_title,
        desc_fn: SettingsTr::shortcut_nav_2_desc,
    },
    ShortcutSpec {
        id: "nav.3",
        default_key: "cmd-3",
        category: ShortcutCategory::Navigation,
        title_fn: SettingsTr::shortcut_nav_3_title,
        desc_fn: SettingsTr::shortcut_nav_3_desc,
    },
    ShortcutSpec {
        id: "nav.4",
        default_key: "cmd-4",
        category: ShortcutCategory::Navigation,
        title_fn: SettingsTr::shortcut_nav_4_title,
        desc_fn: SettingsTr::shortcut_nav_4_desc,
    },
    ShortcutSpec {
        id: "nav.5",
        default_key: "cmd-5",
        category: ShortcutCategory::Navigation,
        title_fn: SettingsTr::shortcut_nav_5_title,
        desc_fn: SettingsTr::shortcut_nav_5_desc,
    },
    ShortcutSpec {
        id: "nav.6",
        default_key: "cmd-6",
        category: ShortcutCategory::Navigation,
        title_fn: SettingsTr::shortcut_nav_6_title,
        desc_fn: SettingsTr::shortcut_nav_6_desc,
    },
    ShortcutSpec {
        id: "nav.7",
        default_key: "cmd-7",
        category: ShortcutCategory::Navigation,
        title_fn: SettingsTr::shortcut_nav_7_title,
        desc_fn: SettingsTr::shortcut_nav_7_desc,
    },
    ShortcutSpec {
        id: "nav.8",
        default_key: "cmd-8",
        category: ShortcutCategory::Navigation,
        title_fn: SettingsTr::shortcut_nav_8_title,
        desc_fn: SettingsTr::shortcut_nav_8_desc,
    },
    ShortcutSpec {
        id: "nav.9",
        default_key: "cmd-9",
        category: ShortcutCategory::Navigation,
        title_fn: SettingsTr::shortcut_nav_9_title,
        desc_fn: SettingsTr::shortcut_nav_9_desc,
    },
];

/// Returns the active keystroke for `spec`, honoring user overrides when present.
#[must_use]
pub fn effective_key(spec: &ShortcutSpec, overrides: &BTreeMap<String, String>) -> String {
    overrides
        .get(spec.id)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(spec.default_key)
        .to_owned()
}

/// Validates and normalizes a user-typed GPUI keystroke expression (e.g., `cmd-shift-p`, `ctrl-k`).
pub fn normalize_keystroke(raw: &str, lang: Language, cx: &App) -> Result<String, String> {
    let s = tr(lang).settings();
    let normalized = raw
        .trim()
        .to_ascii_lowercase()
        .replace('⌘', "cmd-")
        .replace('⇧', "shift-")
        .replace('⌥', "alt-")
        .replace('⌃', "ctrl-")
        .replace('+', "-")
        .replace(' ', "");
    let cleaned = normalized
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if cleaned.is_empty() || cleaned.len() > 64 {
        return Err(s.shortcut_err_empty().to_owned());
    }
    KeyBinding::load(&cleaned, Box::new(NoAction), None, false, None, cx)
        .map_err(|_| s.shortcut_err_invalid(raw))?;
    Ok(cleaned)
}

/// Formats a GPUI keystroke (`cmd-shift-enter`) into clean desktop cap symbols (`⌘⇧↵`).
#[must_use]
pub fn format_key_display(raw: &str) -> String {
    let mut out = String::new();
    for token in raw.trim().split('-') {
        match token.to_ascii_lowercase().as_str() {
            "cmd" | "secondary" | "super" => out.push('⌘'),
            "shift" => out.push('⇧'),
            "alt" | "opt" | "option" => out.push('⌥'),
            "ctrl" | "control" => out.push('⌃'),
            "enter" | "return" => out.push('↵'),
            "escape" | "esc" => out.push_str("Esc"),
            "backspace" => out.push('⌫'),
            "tab" => out.push('⇥'),
            "space" => out.push_str("Space"),
            "up" => out.push('↑'),
            "down" => out.push('↓'),
            "left" => out.push('←'),
            "right" => out.push('→'),
            other if other.len() == 1 => out.push_str(&other.to_ascii_uppercase()),
            other => out.push_str(other),
        }
    }
    out
}

fn action_for_id(id: &str) -> Option<Box<dyn Action>> {
    Some(match id {
        "app.quit" => Box::new(QuitApp),
        "app.settings" => Box::new(OpenSettings),
        "app.sidebar" => Box::new(ToggleSidebar),
        "app.palette" => Box::new(TogglePalette),
        "app.close_overlay" => Box::new(CloseOverlay),
        "req.save" => Box::new(SaveActive),
        "req.send" => Box::new(SendRequest),
        "req.console" => Box::new(ToggleConsole),
        "req.new_tab" => Box::new(NewTab),
        "req.close_tab" => Box::new(CloseTab),
        "nav.1" => Box::new(Nav1),
        "nav.2" => Box::new(Nav2),
        "nav.3" => Box::new(Nav3),
        "nav.4" => Box::new(Nav4),
        "nav.5" => Box::new(Nav5),
        "nav.6" => Box::new(Nav6),
        "nav.7" => Box::new(Nav7),
        "nav.8" => Box::new(Nav8),
        "nav.9" => Box::new(Nav9),
        _ => return None,
    })
}

/// Applies all PingDeck shortcuts (defaults + overrides) live onto GPUI without wiping
/// `gpui-component`'s built-in text-editor bindings.
pub fn bind_app_shortcuts(overrides: &BTreeMap<String, String>, cx: &mut App) {
    let mut active_keys: HashSet<String> = HashSet::new();
    let mut bindings: Vec<KeyBinding> = Vec::with_capacity(SHORTCUT_SPECS.len() + 4);

    for spec in SHORTCUT_SPECS {
        let Some(action) = action_for_id(spec.id) else {
            continue;
        };
        let key = effective_key(spec, overrides);
        if let Ok(binding) = KeyBinding::load(&key, action.boxed_clone(), None, false, None, cx) {
            active_keys.insert(key);
            bindings.push(binding);
        } else if let Ok(binding) =
            KeyBinding::load(spec.default_key, action, None, false, None, cx)
        {
            active_keys.insert(spec.default_key.to_owned());
            bindings.push(binding);
        }
    }

    // Silence any default keystroke that was reassigned away and is no longer used by any action.
    for spec in SHORTCUT_SPECS {
        if !active_keys.contains(spec.default_key)
            && let Ok(binding) =
                KeyBinding::load(spec.default_key, Box::new(NoAction), None, false, None, cx)
        {
            bindings.push(binding);
        }
    }

    cx.bind_keys(bindings);
}
