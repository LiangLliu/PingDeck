//! State container, top resource tabs, environment sub-tabs, and modals for `EnvironmentsView`.

use gpui_kit::{Context, Entity, Subscription, Task, Window};
use pingdeck_desktop::core::app::{
    CompareRowDto, DeclaredHostDto, EnvironmentDto, EnvironmentVariableDto, GlobalsDto,
    ResolvedVariable, TlsCertificateDto, UndoToken, UnresolvedVariableDto,
};
use pingdeck_desktop::core::model::CookieJarEntry;
use pingdeck_desktop::core::model::models::TlsCertificateKind;
use pingdeck_desktop::core::model::settings::Language;

use crate::ui::{EditorState, InputState, Tone};

/// Top-level workspace resource tabs inside the Environments module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvResourceTab {
    Environments,
    Globals,
    Cookies,
    Certificates,
}

impl EnvResourceTab {
    pub const ALL: [Self; 4] = [
        Self::Environments,
        Self::Globals,
        Self::Cookies,
        Self::Certificates,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Environments => 0,
            Self::Globals => 1,
            Self::Cookies => 2,
            Self::Certificates => 3,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Globals,
            2 => Self::Cookies,
            3 => Self::Certificates,
            _ => Self::Environments,
        }
    }

    #[must_use]
    pub const fn label(self, zh: bool) -> &'static str {
        match (self, zh) {
            (Self::Environments, true) => "环境",
            (Self::Environments, false) => "Environments",
            (Self::Globals, true) => "全局变量",
            (Self::Globals, false) => "Globals",
            (Self::Cookies, true) => "Cookies",
            (Self::Cookies, false) => "Cookies",
            (Self::Certificates, true) => "TLS 证书",
            (Self::Certificates, false) => "Certificates",
        }
    }
}

/// Sub-tabs inside a selected Environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvDetailTab {
    Variables,
    Resolved,
    Compare,
    Protection,
}

impl EnvDetailTab {
    pub const ALL: [Self; 4] = [
        Self::Variables,
        Self::Resolved,
        Self::Compare,
        Self::Protection,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Variables => 0,
            Self::Resolved => 1,
            Self::Compare => 2,
            Self::Protection => 3,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Resolved,
            2 => Self::Compare,
            3 => Self::Protection,
            _ => Self::Variables,
        }
    }

    #[must_use]
    pub const fn label(self, zh: bool) -> &'static str {
        match (self, zh) {
            (Self::Variables, true) => "变量",
            (Self::Variables, false) => "Variables",
            (Self::Resolved, true) => "解析顺序与扫描",
            (Self::Resolved, false) => "Resolved & Scan",
            (Self::Compare, true) => "环境对比",
            (Self::Compare, false) => "Compare",
            (Self::Protection, true) => "安全保护与主机",
            (Self::Protection, false) => "Guard & Hosts",
        }
    }
}

/// Editable variable draft row (supports secret values without echoing stored secrets back).
#[derive(Debug, Clone)]
pub struct EditableVarRow {
    pub id: Option<String>,
    pub key: String,
    /// Plain value, or newly entered secret value; `None` keeps an already stored secret intact.
    pub value: Option<String>,
    pub is_secret: bool,
    pub has_secret: bool,
    pub secret_hint: Option<String>,
    pub enabled: bool,
    pub note: String,
}

impl From<&EnvironmentVariableDto> for EditableVarRow {
    fn from(v: &EnvironmentVariableDto) -> Self {
        Self {
            id: Some(v.id.clone()),
            key: v.key.clone(),
            value: if v.is_secret { None } else { v.value.clone() },
            is_secret: v.is_secret,
            has_secret: v.has_secret,
            secret_hint: v.secret_hint.clone(),
            enabled: v.enabled,
            note: v.note.clone(),
        }
    }
}

/// Modal dialogs owned by `EnvironmentsView`.
#[derive(Debug, Clone)]
pub enum EnvModal {
    DeleteEnv {
        id: String,
        name: String,
    },
    ImportEnv,
    ExportEnv {
        id: String,
        name: String,
        include_secrets: bool,
    },
    EditVar {
        is_global: bool,
        index: usize,
        is_secret: bool,
    },
    AddCookie {
        secure: bool,
        http_only: bool,
    },
    AddCert {
        kind: TlsCertificateKind,
    },
    DeleteCert {
        id: String,
        label: String,
    },
}

/// Full stateful Environments, Globals, Cookies, and TLS Certificates module (`NavModule::Environments`).
pub struct EnvironmentsView {
    pub workspace_id: Option<String>,
    pub lang: Language,
    pub resource_tab: EnvResourceTab,
    pub detail_tab: EnvDetailTab,

    // Environments
    pub environments: Vec<EnvironmentDto>,
    pub selected_env_id: Option<String>,
    pub env_draft_name: String,
    pub env_draft_vars: Vec<EditableVarRow>,
    pub env_draft_confirm: bool,
    pub env_draft_confirm_safe: bool,
    pub env_draft_hosts: Vec<String>,
    pub env_draft_revision: Option<String>,
    pub env_dirty: bool,
    pub env_undo: Option<(UndoToken, String)>,

    // Analysis caches
    pub resolved_vars: Vec<ResolvedVariable>,
    pub unresolved_vars: Vec<UnresolvedVariableDto>,
    pub declared_hosts: Vec<DeclaredHostDto>,
    pub compare_target_id: Option<String>,
    pub compare_include_globals: bool,
    pub compare_rows: Vec<CompareRowDto>,

    // Globals
    pub globals_dto: Option<GlobalsDto>,
    pub globals_draft: Vec<EditableVarRow>,
    pub globals_dirty: bool,

    // Cookies
    pub cookies: Vec<CookieJarEntry>,
    pub selected_cookie_id: Option<String>,
    pub deleted_cookies_undo: Option<Vec<CookieJarEntry>>,

    // Certificates
    pub certificates: Vec<TlsCertificateDto>,

    // Shared inputs
    pub env_search_input: Entity<InputState>,
    pub env_name_input: Entity<InputState>,
    pub var_filter_input: Entity<InputState>,
    pub new_var_key: Entity<InputState>,
    pub new_var_val: Entity<InputState>,
    pub new_var_note: Entity<InputState>,
    pub new_var_secret: bool,
    pub edit_var_key: Entity<InputState>,
    pub edit_var_val: Entity<InputState>,
    pub edit_var_note: Entity<InputState>,
    pub host_input: Entity<InputState>,
    pub import_editor: Entity<EditorState>,
    pub cookie_search_input: Entity<InputState>,
    pub cookie_edit_val: Entity<InputState>,
    pub ck_domain_input: Entity<InputState>,
    pub ck_name_input: Entity<InputState>,
    pub ck_value_input: Entity<InputState>,
    pub ck_path_input: Entity<InputState>,
    pub cert_host_input: Entity<InputState>,
    pub cert_path_input: Entity<InputState>,
    pub cert_key_input: Entity<InputState>,
    pub cert_pass_input: Entity<InputState>,

    // Modals, feedback, sync
    pub active_modal: Option<EnvModal>,
    pub status_msg: Option<(Tone, String)>,
    pub pending_sync: bool,
    pub syncing: bool,
    pub(super) _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl EnvironmentsView {
    #[must_use]
    pub fn is_zh(&self) -> bool {
        self.lang == Language::ZhCn
    }

    #[must_use]
    pub fn t(&self, zh: &'static str, en: &'static str) -> &'static str {
        if self.is_zh() { zh } else { en }
    }

    pub fn load_env_into_draft(&mut self, env: &EnvironmentDto) {
        self.selected_env_id = Some(env.id.clone());
        self.env_draft_name = env.name.clone();
        self.env_draft_vars = env.variables.iter().map(EditableVarRow::from).collect();
        self.env_draft_confirm = env.confirm_before_send;
        self.env_draft_confirm_safe = env.confirm_safe_methods;
        self.env_draft_hosts = env.declared_hosts.clone();
        self.env_draft_revision = Some(env.revision.clone());
        self.env_dirty = false;
        self.pending_sync = true;
    }

    pub fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_sync = false;
        self.syncing = true;
        self.env_name_input.update(cx, |s, cx| {
            s.set_value(self.env_draft_name.clone(), window, cx)
        });
        if let Some(cid) = &self.selected_cookie_id
            && let Some(ck) = self.cookies.iter().find(|c| c.id.as_str() == cid.as_str())
        {
            self.cookie_edit_val
                .update(cx, |s, cx| s.set_value(ck.value.clone(), window, cx));
        }
        self.syncing = false;
    }
}
