//! Shell navigation, settings taxonomy (all 20 pages), utility dialogs (all 6 tools),
//! toast notifications, and shell-level ViewModel state.

use pingdeck_core::{
    agent::attention::AgentAttention,
    app::RunningWork,
    model::{Workspace, settings::Settings},
};
use serde::{Deserialize, Serialize};

use crate::persistence::UiLocalState;
use crate::theme::PaletteId;

/// Sidebar navigation group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavGroup {
    Workspace,
    AiShared,
    Footer,
}

/// All 9 top-level navigation modules of PingDeck.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NavModule {
    #[default]
    Requests,
    Mock,
    Flows,
    History,
    Environments,
    Chat,
    Providers,
    Usage,
    Settings,
}

impl NavModule {
    /// Workspace-scoped API workbench modules (top sidebar section).
    pub const WORKSPACE_MODULES: [Self; 5] = [
        Self::Requests,
        Self::Mock,
        Self::Flows,
        Self::History,
        Self::Environments,
    ];

    /// Shared AI workbench modules (middle sidebar section).
    pub const AI_MODULES: [Self; 3] = [Self::Chat, Self::Providers, Self::Usage];

    pub const ALL: [Self; 9] = [
        Self::Requests,
        Self::Mock,
        Self::Flows,
        Self::History,
        Self::Environments,
        Self::Chat,
        Self::Providers,
        Self::Usage,
        Self::Settings,
    ];

    #[must_use]
    pub const fn group(self) -> NavGroup {
        match self {
            Self::Requests | Self::Mock | Self::Flows | Self::History | Self::Environments => {
                NavGroup::Workspace
            }
            Self::Chat | Self::Providers | Self::Usage => NavGroup::AiShared,
            Self::Settings => NavGroup::Footer,
        }
    }

    #[must_use]
    pub const fn shortcut_digit(self) -> u8 {
        match self {
            Self::Requests => 1,
            Self::Mock => 2,
            Self::Flows => 3,
            Self::History => 4,
            Self::Environments => 5,
            Self::Chat => 6,
            Self::Providers => 7,
            Self::Usage => 8,
            Self::Settings => 9,
        }
    }

    #[must_use]
    pub const fn from_digit(digit: u8) -> Option<Self> {
        match digit {
            1 => Some(Self::Requests),
            2 => Some(Self::Mock),
            3 => Some(Self::Flows),
            4 => Some(Self::History),
            5 => Some(Self::Environments),
            6 => Some(Self::Chat),
            7 => Some(Self::Providers),
            8 => Some(Self::Usage),
            9 => Some(Self::Settings),
            _ => None,
        }
    }

    /// Bilingual `(zh-CN, en)` module label (also resolvable via `i18n`).
    #[must_use]
    pub const fn label_pair(self) -> (&'static str, &'static str) {
        match self {
            Self::Requests => ("请求", "Requests"),
            Self::Mock => ("Mock 服务", "Mock"),
            Self::Flows => ("流程", "Flows"),
            Self::History => ("历史", "History"),
            Self::Environments => ("环境", "Environments"),
            Self::Chat => ("对话", "Chat"),
            Self::Providers => ("服务商", "Providers"),
            Self::Usage => ("用量与日志", "Usage & Logs"),
            Self::Settings => ("设置", "Settings"),
        }
    }
}

/// All 6 utility dialogs / drawers outside the 9 primary navigation modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UtilityDialog {
    ImportExport,
    Sync,
    Cookies,
    Certificates,
    Console,
    Runner,
}

impl UtilityDialog {
    pub const ALL: [Self; 6] = [
        Self::ImportExport,
        Self::Sync,
        Self::Cookies,
        Self::Certificates,
        Self::Console,
        Self::Runner,
    ];

    #[must_use]
    pub const fn label_pair(self) -> (&'static str, &'static str) {
        match self {
            Self::ImportExport => ("导入与导出", "Import & Export"),
            Self::Sync => ("文件夹同步", "Folder Sync"),
            Self::Cookies => ("Cookie 管理器", "Cookies"),
            Self::Certificates => ("客户端证书与 CA", "Certificates"),
            Self::Console => ("诊断控制台", "Console"),
            Self::Runner => ("集合运行器", "Collection Runner"),
        }
    }
}

/// Settings navigation category group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsGroup {
    General,
    RequestsAndData,
    Agent,
    Models,
    System,
}

impl SettingsGroup {
    pub const ALL: [Self; 5] = [
        Self::General,
        Self::RequestsAndData,
        Self::Agent,
        Self::Models,
        Self::System,
    ];

    #[must_use]
    pub const fn label_pair(self) -> (&'static str, &'static str) {
        match self {
            Self::General => ("通用", "General"),
            Self::RequestsAndData => ("请求与数据", "Requests & Data"),
            Self::Agent => ("智能体", "Agent"),
            Self::Models => ("模型", "Models"),
            Self::System => ("系统", "System"),
        }
    }
}

/// All 20 Settings pages in PingDeck.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsSection {
    #[default]
    General,
    Appearance,
    Shortcuts,
    Requests,
    Network,
    Mock,
    Data,
    Agent,
    Personalization,
    Memory,
    Subagents,
    Tools,
    Permissions,
    Mcp,
    Skills,
    Routing,
    Catalog,
    Usage,
    Advanced,
    About,
}

impl SettingsSection {
    pub const ALL: [Self; 20] = [
        Self::General,
        Self::Appearance,
        Self::Shortcuts,
        Self::Requests,
        Self::Network,
        Self::Mock,
        Self::Data,
        Self::Agent,
        Self::Personalization,
        Self::Memory,
        Self::Subagents,
        Self::Tools,
        Self::Permissions,
        Self::Mcp,
        Self::Skills,
        Self::Routing,
        Self::Catalog,
        Self::Usage,
        Self::Advanced,
        Self::About,
    ];

    #[must_use]
    pub const fn group(self) -> SettingsGroup {
        match self {
            Self::General | Self::Appearance | Self::Shortcuts => SettingsGroup::General,
            Self::Requests | Self::Network | Self::Mock | Self::Data => {
                SettingsGroup::RequestsAndData
            }
            Self::Agent
            | Self::Personalization
            | Self::Memory
            | Self::Subagents
            | Self::Tools
            | Self::Permissions
            | Self::Mcp
            | Self::Skills => SettingsGroup::Agent,
            Self::Routing | Self::Catalog | Self::Usage => SettingsGroup::Models,
            Self::Advanced | Self::About => SettingsGroup::System,
        }
    }

    #[must_use]
    pub const fn label_pair(self) -> (&'static str, &'static str) {
        match self {
            Self::General => ("通用", "General"),
            Self::Appearance => ("外观", "Appearance"),
            Self::Shortcuts => ("快捷键", "Shortcuts"),
            Self::Requests => ("请求默认值", "Request Defaults"),
            Self::Network => ("网络", "Network"),
            Self::Mock => ("Mock 服务", "Mock"),
            Self::Data => ("数据与同步", "Data & Sync"),
            Self::Agent => ("默认", "Defaults"),
            Self::Personalization => ("个性与指令", "Personalization"),
            Self::Memory => ("项目指令与信任", "Project Instructions & Trust"),
            Self::Subagents => ("子智能体", "Subagents"),
            Self::Tools => ("工具", "Tools"),
            Self::Permissions => ("权限", "Permissions"),
            Self::Mcp => ("MCP 服务器", "MCP Servers"),
            Self::Skills => ("技能", "Skills"),
            Self::Routing => ("重试与故障转移", "Routing & Failover"),
            Self::Catalog => ("模型与价格", "Models & Pricing"),
            Self::Usage => ("用量统计", "Usage"),
            Self::Advanced => ("高级", "Advanced"),
            Self::About => ("关于", "About"),
        }
    }
}

/// Toast severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

/// Ephemeral feedback toast shown by the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastMessage {
    pub id: u64,
    pub kind: ToastKind,
    pub title: String,
    pub detail: Option<String>,
}

/// Headless shell ViewModel holding cross-module navigation, workspace, and status state.
#[derive(Debug, Clone)]
pub struct ShellState {
    pub active_module: NavModule,
    pub previous_non_settings_module: NavModule,
    pub active_settings_section: SettingsSection,
    pub palette: PaletteId,
    pub sidebar_collapsed: bool,
    pub sidebar_width: f32,
    pub workspaces: Vec<Workspace>,
    pub active_workspace_id: Option<String>,
    pub active_environment_id: Option<String>,
    pub settings: Settings,
    pub attention: AgentAttention,
    pub running_work: RunningWork,
    pub gateway_port: Option<u16>,
    pub startup_notices: Vec<String>,
    pub palette_open: bool,
    pub active_utility: Option<UtilityDialog>,
    pub toasts: Vec<ToastMessage>,
    next_toast_id: u64,
}

impl ShellState {
    #[must_use]
    pub fn new(ui: &UiLocalState, settings: Settings, workspaces: Vec<Workspace>) -> Self {
        let active_workspace_id = ui
            .active_workspace_id
            .as_ref()
            .filter(|id| workspaces.iter().any(|w| w.id.as_str() == id.as_str()))
            .cloned()
            .or_else(|| workspaces.first().map(|w| w.id.as_str().to_owned()));

        let prev = if ui.active_module == NavModule::Settings {
            NavModule::Requests
        } else {
            ui.active_module
        };

        Self {
            active_module: ui.active_module,
            previous_non_settings_module: prev,
            active_settings_section: ui.active_settings_section,
            palette: ui.palette,
            sidebar_collapsed: ui.sidebar_collapsed,
            sidebar_width: ui.sidebar_width.clamp(180.0, 320.0),
            workspaces,
            active_workspace_id,
            active_environment_id: ui.active_environment_id.clone(),
            settings,
            attention: AgentAttention::default(),
            running_work: RunningWork::default(),
            gateway_port: None,
            startup_notices: Vec::new(),
            palette_open: false,
            active_utility: None,
            toasts: Vec::new(),
            next_toast_id: 1,
        }
    }

    pub fn navigate(&mut self, module: NavModule) {
        if module == NavModule::Settings && self.active_module != NavModule::Settings {
            self.previous_non_settings_module = self.active_module;
        }
        self.active_module = module;
    }

    pub fn leave_settings(&mut self) {
        self.active_module = self.previous_non_settings_module;
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
    }

    pub fn push_toast(
        &mut self,
        kind: ToastKind,
        title: impl Into<String>,
        detail: Option<String>,
    ) {
        let id = self.next_toast_id;
        self.next_toast_id = self.next_toast_id.saturating_add(1);
        self.toasts.push(ToastMessage {
            id,
            kind,
            title: title.into(),
            detail,
        });
        if self.toasts.len() > 5 {
            self.toasts.remove(0);
        }
    }

    pub fn dismiss_toast(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
    }

    /// Exports persistent fields back into [`UiLocalState`] for atomic disk save.
    pub fn sync_to_local(&self, local: &mut UiLocalState) {
        local.palette = self.palette;
        local.active_module = self.active_module;
        local.active_settings_section = self.active_settings_section;
        local.sidebar_collapsed = self.sidebar_collapsed;
        local.sidebar_width = self.sidebar_width;
        local.active_workspace_id = self.active_workspace_id.clone();
        local.active_environment_id = self.active_environment_id.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_navigation_remembers_previous_module() {
        let mut shell = ShellState::new(&UiLocalState::default(), Settings::default(), vec![]);
        shell.navigate(NavModule::Flows);
        shell.navigate(NavModule::Settings);
        assert_eq!(shell.active_module, NavModule::Settings);
        shell.leave_settings();
        assert_eq!(shell.active_module, NavModule::Flows);
    }

    #[test]
    fn exposes_all_20_settings_sections_and_9_modules() {
        assert_eq!(NavModule::ALL.len(), 9);
        assert_eq!(SettingsSection::ALL.len(), 20);
        assert_eq!(UtilityDialog::ALL.len(), 6);
    }
}
