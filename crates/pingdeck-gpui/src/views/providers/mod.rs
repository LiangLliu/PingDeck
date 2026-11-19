//! AI Providers, Local Gateway Cockpit, Coding Client Integrations & Test Bench (`NavModule::Providers`).
//!
//! Unified master-detail shell (`h_resizable`):
//! - Left pane (`sidebar.rs`): pinned Local Gateway + Test Bench + live-searchable configured Providers
//! - Right pane:
//!   - Local Gateway Cockpit (`gateway.rs`, `gateway_overview.rs`, `gateway_access.rs`,
//!     `gateway_clients.rs`, `gateway_backups.rs`, `gateway_rules.rs`, `connect_sheet.rs`)
//!   - Model Test Bench (`bench.rs`, `bench_table.rs`)
//!   - Provider Detail (`detail.rs`, `detail_keys.rs`, `detail_side.rs`, `detail_models.rs`,
//!     `detail_models_table.rs`, `detail_config.rs`, `detail_config_extra.rs`)
//! - Native Slide-Over Sheets (`sniff_sheet.rs`, `connect_sheet.rs`) via `WindowExt::open_sheet`.

pub mod actions;
pub mod actions_models;
pub mod actions_provider;
pub mod bench;
pub mod bench_table;
pub mod connect_sheet;
pub mod connect_sheet_body;
pub mod detail;
pub mod detail_config;
pub mod detail_config_extra;
pub mod detail_keys;
pub mod detail_models;
pub mod detail_models_table;
pub mod detail_side;
pub mod format;
pub mod gateway;
pub mod gateway_access;
pub mod gateway_backups;
pub mod gateway_clients;
pub mod gateway_overview;
pub mod gateway_rules;
pub mod loader;
pub mod sidebar;
pub mod sniff_sheet;
pub mod sniff_sheet_actions;
pub mod sniff_sheet_steps;

use std::collections::{BTreeSet, HashMap};

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    ClientInspectDto, FallbackCandidate, GatewayOverviewDto, GatewayRule, ManagedTool,
    ProviderHealthDto, ProviderModelView, ProviderView, RoutePreview, VerifyResult,
    settings_get_impl,
};
use pingdeck_desktop::core::client_config::{ClientApp, DetectedClient, DetectedSnippetClient};
use pingdeck_desktop::core::llm::presets::ProviderPreset;
use pingdeck_desktop::core::llm::status::ProviderStatusDto;
use pingdeck_desktop::core::llm::store::{
    GatewayClient, ModelRef, ProbeResult, ProviderKey, RequestLogEntry,
};
use pingdeck_desktop::core::model::settings::{AiGatewaySettings, Language};

use crate::bridge::runtime;
use crate::ui::{InputEvent, InputState, Tone, h_resizable, palette, resizable_panel, to_hsla};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LeftSelection {
    #[default]
    Gateway,
    Bench,
    Provider(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GatewayTab {
    #[default]
    Overview,
    Clients,
    Access,
    Rules,
}

impl GatewayTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Clients => 1,
            Self::Access => 2,
            Self::Rules => 3,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Clients,
            2 => Self::Access,
            3 => Self::Rules,
            _ => Self::Overview,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderTab {
    #[default]
    Overview,
    Models,
    Config,
}

impl ProviderTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Models => 1,
            Self::Config => 2,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Models,
            2 => Self::Config,
            _ => Self::Overview,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelFilterKind {
    #[default]
    All,
    Chat,
    Embedding,
    Image,
    Failed,
    New,
    Removed,
}

pub struct RuleEditorState {
    pub primary: ModelRef,
    pub chain: Vec<ModelRef>,
    pub candidates: Vec<FallbackCandidate>,
}

pub struct ProvidersView {
    pub selection: LeftSelection,
    pub gateway_tab: GatewayTab,
    pub provider_tab: ProviderTab,
    pub providers: Vec<ProviderView>,
    pub statuses: HashMap<String, ProviderStatusDto>,
    pub keys: Vec<ProviderKey>,
    pub models: Vec<ProviderModelView>,
    pub presets: Vec<ProviderPreset>,
    pub overview: Option<GatewayOverviewDto>,
    pub gateway_settings: AiGatewaySettings,
    pub detected_clients: Vec<DetectedClient>,
    pub snippet_clients: Vec<DetectedSnippetClient>,
    pub inspections: HashMap<ClientApp, ClientInspectDto>,
    pub managed_tools: Vec<ManagedTool>,
    pub gateway_clients: Vec<GatewayClient>,
    pub gateway_rules: Vec<GatewayRule>,
    pub probe_results: Vec<ProbeResult>,
    pub provider_health: Option<ProviderHealthDto>,
    pub recent_logs: Vec<RequestLogEntry>,

    // Interactive state & inputs.
    pub search_input: Entity<InputState>,
    pub quick_url_input: Entity<InputState>,
    pub model_search_input: Entity<InputState>,
    pub gw_port_input: Entity<InputState>,
    pub gw_extra_hosts_input: Entity<InputState>,
    pub new_client_name_input: Entity<InputState>,
    pub client_rpm_input: Entity<InputState>,
    pub client_conc_input: Entity<InputState>,
    pub new_key_name_input: Entity<InputState>,
    pub new_key_secret_input: Entity<InputState>,
    pub new_key_group_input: Entity<InputState>,
    pub new_key_user_id_input: Entity<InputState>,
    pub new_model_id_input: Entity<InputState>,
    pub new_model_ctx_input: Entity<InputState>,

    pub editing_client_policy: Option<String>,
    pub revealed_client_token: Option<(String, String)>,
    pub client_verify_results: HashMap<ClientApp, VerifyResult>,
    pub rule_editor: Option<RuleEditorState>,
    pub route_preview: Option<RoutePreview>,

    pub adding_key_open: bool,
    pub revealed_provider_key: Option<(String, String)>,
    pub evidence_open: bool,
    pub refreshing_balance: bool,
    pub refreshing_models: bool,
    pub adding_model_open: bool,
    pub model_filter: ModelFilterKind,
    pub models_page_limit: usize,
    pub selected_models: BTreeSet<String>,
    pub last_bulk_undo: Option<(String, String)>,

    pub config_form: Option<Entity<detail_config::ProviderConfigForm>>,
    pub bench_view: Option<Entity<bench::BenchView>>,
    pub status_msg: Option<(Tone, String)>,
    _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl ProvidersView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = settings_get_impl(runtime(cx).state());
        let zh = settings.general.language == Language::ZhCn;
        let gw_settings = settings.ai.gateway.clone();

        let mk =
            |ph: &'static str, init: &str, masked: bool, w: &mut Window, cx: &mut Context<Self>| {
                cx.new(|cx| {
                    let mut st = InputState::new(w, cx).placeholder(ph);
                    if masked {
                        st = st.masked(true);
                    }
                    if !init.is_empty() {
                        st.set_value(init.to_owned(), w, cx);
                    }
                    st
                })
            };

        let search_input = mk(
            if zh {
                "搜索服务商"
            } else {
                "Search providers"
            },
            "",
            false,
            window,
            cx,
        );
        let quick_url_input = mk("https://api.deepseek.com", "", false, window, cx);
        let model_search_input = mk(
            if zh {
                "筛选模型…"
            } else {
                "Filter models…"
            },
            "",
            false,
            window,
            cx,
        );
        let gw_port_input = mk("4141", &gw_settings.port.to_string(), false, window, cx);
        let gw_extra_hosts_input = mk(
            "host.docker.internal",
            &gw_settings.extra_allowed_hosts.join(", "),
            false,
            window,
            cx,
        );
        let new_client_name_input = mk(
            if zh {
                "新客户端名称…"
            } else {
                "New client name…"
            },
            "",
            false,
            window,
            cx,
        );
        let client_rpm_input = mk("60", "", false, window, cx);
        let client_conc_input = mk("8", "", false, window, cx);
        let new_key_name_input = mk(
            if zh {
                "密钥名称（如 主密钥）"
            } else {
                "Key name"
            },
            "",
            false,
            window,
            cx,
        );
        let new_key_secret_input = mk(
            if zh {
                "API 密钥（sk-…）"
            } else {
                "API Secret (sk-…)"
            },
            "",
            true,
            window,
            cx,
        );
        let new_key_group_input = mk(
            if zh {
                "分组名（可选）"
            } else {
                "Group (optional)"
            },
            "",
            false,
            window,
            cx,
        );
        let new_key_user_id_input = mk("New-Api-User (opt)", "", false, window, cx);
        let new_model_id_input = mk(
            if zh {
                "输入模型 ID（如 gpt-5-mini）"
            } else {
                "Model ID"
            },
            "",
            false,
            window,
            cx,
        );
        let new_model_ctx_input = mk(
            if zh {
                "上下文（可选）"
            } else {
                "Context (opt)"
            },
            "",
            false,
            window,
            cx,
        );

        let sub_s = cx.subscribe(&search_input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        let sub_m = cx.subscribe(&model_search_input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });

        let mut view = Self {
            selection: LeftSelection::Gateway,
            gateway_tab: GatewayTab::Overview,
            provider_tab: ProviderTab::Overview,
            providers: Vec::new(),
            statuses: HashMap::new(),
            keys: Vec::new(),
            models: Vec::new(),
            presets: Vec::new(),
            overview: None,
            gateway_settings: gw_settings,
            detected_clients: Vec::new(),
            snippet_clients: Vec::new(),
            inspections: HashMap::new(),
            managed_tools: Vec::new(),
            gateway_clients: Vec::new(),
            gateway_rules: Vec::new(),
            probe_results: Vec::new(),
            provider_health: None,
            recent_logs: Vec::new(),
            search_input,
            quick_url_input,
            model_search_input,
            gw_port_input,
            gw_extra_hosts_input,
            new_client_name_input,
            client_rpm_input,
            client_conc_input,
            new_key_name_input,
            new_key_secret_input,
            new_key_group_input,
            new_key_user_id_input,
            new_model_id_input,
            new_model_ctx_input,
            editing_client_policy: None,
            revealed_client_token: None,
            client_verify_results: HashMap::new(),
            rule_editor: None,
            route_preview: None,
            adding_key_open: false,
            revealed_provider_key: None,
            evidence_open: false,
            refreshing_balance: false,
            refreshing_models: false,
            adding_model_open: false,
            model_filter: ModelFilterKind::All,
            models_page_limit: 80,
            selected_models: BTreeSet::new(),
            last_bulk_undo: None,
            config_form: None,
            bench_view: None,
            status_msg: None,
            _subscriptions: vec![sub_s, sub_m],
            _tasks: Vec::new(),
        };
        view.reload_all(cx);
        view
    }
}

impl Render for ProvidersView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let zh = settings_get_impl(runtime(cx).state()).general.language == Language::ZhCn;

        if let LeftSelection::Provider(id) = self.selection.clone()
            && self.provider_tab == ProviderTab::Config
        {
            self.ensure_config_form(&id, window, cx);
        }

        let left_pane = sidebar::render_sidebar(self, zh, cx);
        let right_content: gpui_kit::AnyElement = match &self.selection {
            LeftSelection::Gateway => {
                gateway::render_gateway_cockpit(self, zh, cx).into_any_element()
            }
            LeftSelection::Bench => {
                if let Some(bv) = &self.bench_view {
                    bv.clone().into_any_element()
                } else {
                    gateway::render_gateway_cockpit(self, zh, cx).into_any_element()
                }
            }
            LeftSelection::Provider(id) => {
                if let Some(pv) = self
                    .providers
                    .iter()
                    .find(|x| &x.provider.id == id)
                    .cloned()
                {
                    detail::render_provider_detail(self, &pv, zh, cx).into_any_element()
                } else {
                    gateway::render_gateway_cockpit(self, zh, cx).into_any_element()
                }
            }
        };

        h_resizable("providers-main-split")
            .child(
                resizable_panel()
                    .size(px(276.0))
                    .size_range(px(220.0)..px(360.0))
                    .child(left_pane),
            )
            .child(
                resizable_panel().child(
                    div()
                        .size_full()
                        .bg(to_hsla(p.surface))
                        .child(right_content),
                ),
            )
    }
}
