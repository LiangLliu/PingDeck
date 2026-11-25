//! Application-wide GPUI keyboard actions, canonical shortcut registry, and live rebinding.

use std::collections::{BTreeMap, HashSet};

use gpui_kit::{Action, App, KeyBinding, NoAction};

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
    pub const fn label(self, zh: bool) -> &'static str {
        match (self, zh) {
            (Self::Global, true) => "全局与窗口",
            (Self::Global, false) => "Global & Window",
            (Self::Workbench, true) => "请求工作台",
            (Self::Workbench, false) => "Requests Workbench",
            (Self::Navigation, true) => "模块快速切换",
            (Self::Navigation, false) => "Module Navigation",
        }
    }
}

/// Metadata for one bindable application command.
#[derive(Debug, Clone, Copy)]
pub struct ShortcutSpec {
    pub id: &'static str,
    pub default_key: &'static str,
    pub category: ShortcutCategory,
    pub title_zh: &'static str,
    pub title_en: &'static str,
    pub desc_zh: &'static str,
    pub desc_en: &'static str,
}

/// Complete registry of all GPUI keyboard commands (single source of truth for startup binding,
/// live rebinding, and Settings › Shortcuts).
pub const SHORTCUT_SPECS: &[ShortcutSpec] = &[
    ShortcutSpec {
        id: "app.palette",
        default_key: "cmd-k",
        category: ShortcutCategory::Global,
        title_zh: "打开命令面板",
        title_en: "Open Command Palette",
        desc_zh: "搜索请求、工作区、设置页面与快捷动作。",
        desc_en: "Search requests, workspaces, settings pages, and commands.",
    },
    ShortcutSpec {
        id: "app.sidebar",
        default_key: "cmd-b",
        category: ShortcutCategory::Global,
        title_zh: "折叠或展开侧栏",
        title_en: "Toggle Sidebar",
        desc_zh: "在完整导航栏与紧凑图标窄轨之间切换。",
        desc_en: "Switch between full sidebar and compact icon rail.",
    },
    ShortcutSpec {
        id: "app.settings",
        default_key: "cmd-,",
        category: ShortcutCategory::Global,
        title_zh: "打开设置",
        title_en: "Open Settings",
        desc_zh: "快速进入全局设置或返回之前所在模块。",
        desc_en: "Jump to Settings or return to the previous module.",
    },
    ShortcutSpec {
        id: "app.close_overlay",
        default_key: "escape",
        category: ShortcutCategory::Global,
        title_zh: "关闭浮层与弹窗",
        title_en: "Close Active Overlay",
        desc_zh: "关闭命令面板、工具浮层或当前确认对话框。",
        desc_en: "Dismiss the command palette, utility drawer, or active modal.",
    },
    ShortcutSpec {
        id: "app.quit",
        default_key: "cmd-q",
        category: ShortcutCategory::Global,
        title_zh: "优雅退出 PingDeck",
        title_en: "Quit PingDeck",
        desc_zh: "停止后台服务、释放端口与子进程并安全退出。",
        desc_en: "Stop background services, release ports, and quit cleanly.",
    },
    ShortcutSpec {
        id: "req.send",
        default_key: "cmd-enter",
        category: ShortcutCategory::Workbench,
        title_zh: "发送当前请求",
        title_en: "Send Active Request",
        desc_zh: "在请求工作台立即执行当前 HTTP / GraphQL / AI 标签页。",
        desc_en: "Execute the active HTTP, GraphQL, or AI tab immediately.",
    },
    ShortcutSpec {
        id: "req.save",
        default_key: "cmd-s",
        category: ShortcutCategory::Workbench,
        title_zh: "保存当前标签页",
        title_en: "Save Active Tab",
        desc_zh: "将当前请求草稿或集合/文件夹设置写入工作区。",
        desc_en: "Save the active request draft or collection/folder settings.",
    },
    ShortcutSpec {
        id: "req.console",
        default_key: "cmd-j",
        category: ShortcutCategory::Workbench,
        title_zh: "切换底部控制台",
        title_en: "Toggle Console Drawer",
        desc_zh: "展开或收起请求工作台底部的执行与脚本日志控制台。",
        desc_en: "Show or hide the bottom execution and script console drawer.",
    },
    ShortcutSpec {
        id: "req.new_tab",
        default_key: "cmd-t",
        category: ShortcutCategory::Workbench,
        title_zh: "新建请求标签页",
        title_en: "New Request Tab",
        desc_zh: "在请求工作台打开一个新的空白 HTTP 请求草稿。",
        desc_en: "Open a new blank HTTP request draft tab.",
    },
    ShortcutSpec {
        id: "req.close_tab",
        default_key: "cmd-w",
        category: ShortcutCategory::Workbench,
        title_zh: "关闭当前标签页",
        title_en: "Close Active Tab",
        desc_zh: "关闭当前请求标签页（有未保存修改时提示确认）。",
        desc_en: "Close the active tab (prompts if there are unsaved changes).",
    },
    ShortcutSpec {
        id: "nav.1",
        default_key: "cmd-1",
        category: ShortcutCategory::Navigation,
        title_zh: "切换到「请求」",
        title_en: "Switch to Requests",
        desc_zh: "跳转到 API 与 AI 请求工作台。",
        desc_en: "Navigate to the Requests workbench.",
    },
    ShortcutSpec {
        id: "nav.2",
        default_key: "cmd-2",
        category: ShortcutCategory::Navigation,
        title_zh: "切换到「Mock 服务」",
        title_en: "Switch to Mock",
        desc_zh: "跳转到本地 Mock 服务器管理。",
        desc_en: "Navigate to local Mock servers.",
    },
    ShortcutSpec {
        id: "nav.3",
        default_key: "cmd-3",
        category: ShortcutCategory::Navigation,
        title_zh: "切换到「流程」",
        title_en: "Switch to Flows",
        desc_zh: "跳转到多步串联流程编排。",
        desc_en: "Navigate to multi-step Flows.",
    },
    ShortcutSpec {
        id: "nav.4",
        default_key: "cmd-4",
        category: ShortcutCategory::Navigation,
        title_zh: "切换到「历史」",
        title_en: "Switch to History",
        desc_zh: "跳转到工作区请求执行历史。",
        desc_en: "Navigate to execution History.",
    },
    ShortcutSpec {
        id: "nav.5",
        default_key: "cmd-5",
        category: ShortcutCategory::Navigation,
        title_zh: "切换到「环境」",
        title_en: "Switch to Environments",
        desc_zh: "跳转到环境变量、全局变量与客户端证书管理。",
        desc_en: "Navigate to Environments, globals, and client certificates.",
    },
    ShortcutSpec {
        id: "nav.6",
        default_key: "cmd-6",
        category: ShortcutCategory::Navigation,
        title_zh: "切换到「对话」",
        title_en: "Switch to Chat",
        desc_zh: "跳转到本地编码与调试智能体对话。",
        desc_en: "Navigate to AI Agent Chat.",
    },
    ShortcutSpec {
        id: "nav.7",
        default_key: "cmd-7",
        category: ShortcutCategory::Navigation,
        title_zh: "切换到「服务商」",
        title_en: "Switch to Providers",
        desc_zh: "跳转到模型服务商、密钥与本地 AI 网关管理。",
        desc_en: "Navigate to AI Providers and local Gateway.",
    },
    ShortcutSpec {
        id: "nav.8",
        default_key: "cmd-8",
        category: ShortcutCategory::Navigation,
        title_zh: "切换到「用量与日志」",
        title_en: "Switch to Usage & Logs",
        desc_zh: "跳转到调用统计、费用汇总与网关请求日志。",
        desc_en: "Navigate to Usage analytics and gateway logs.",
    },
    ShortcutSpec {
        id: "nav.9",
        default_key: "cmd-9",
        category: ShortcutCategory::Navigation,
        title_zh: "切换到「设置」",
        title_en: "Switch to Settings",
        desc_zh: "跳转到全局应用设置。",
        desc_en: "Navigate to Settings.",
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
pub fn normalize_keystroke(raw: &str, zh: bool, cx: &App) -> Result<String, String> {
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
        return Err(if zh {
            "请输入合法的快捷键组合（例如 cmd-k、cmd-shift-p、ctrl-j）"
        } else {
            "Enter a valid keystroke (e.g. cmd-k, cmd-shift-p, ctrl-j)"
        }
        .to_owned());
    }
    KeyBinding::load(&cleaned, Box::new(NoAction), None, false, None, cx).map_err(|_| {
        if zh {
            format!("无法识别快捷键「{raw}」，请使用如 cmd-k、cmd-shift-p、escape 格式")
        } else {
            format!("Invalid keystroke \"{raw}\"; use formats like cmd-k, cmd-shift-p, escape")
        }
    })?;
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
