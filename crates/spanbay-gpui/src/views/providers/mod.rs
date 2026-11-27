//! AI Providers, Keys, Models, Pricing & Model Test Bench (`NavModule::Providers`).
//!
//! Unified master-detail shell (`h_resizable`):
//! - Left pane (`sidebar.rs`): live-searchable configured Providers + pinned Model Test Bench tool
//! - Right pane:
//!   - Provider Detail (`detail.rs`, `detail_keys.rs`, `detail_side.rs`, `detail_models.rs`,
//!     `detail_models_table.rs`, `detail_config.rs`, `detail_config_extra.rs`, `detail_config_pricing.rs`)
//!   - Model Test Bench (`bench.rs`, `bench_table.rs`, `bench_panels.rs`)
//!   - Full-width Provider Onboarding Hero when no providers exist yet
//! - Native Slide-Over Sheet (`sniff_sheet.rs`) via `WindowExt::open_sheet`.

pub mod actions_models;
pub mod actions_provider;
pub mod bench;
pub mod bench_panels;
pub mod bench_table;
pub mod detail;
pub mod detail_config;
pub mod detail_config_extra;
pub mod detail_config_pricing;
pub mod detail_keys;
pub mod detail_models;
pub mod detail_models_table;
pub mod detail_side;
pub mod format;
pub mod loader;
pub mod sidebar;
pub mod sniff_sheet;
pub mod sniff_sheet_actions;
pub mod sniff_sheet_steps;

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Task, Window, div, px,
};
use spanbay_desktop::core::app::{
    ProviderHealthDto, ProviderModelView, ProviderPricingView, ProviderView,
};
use spanbay_desktop::core::llm::presets::ProviderPreset;
use spanbay_desktop::core::llm::status::ProviderStatusDto;
use spanbay_desktop::core::llm::store::{ProbeResult, ProviderKey, RequestLogEntry};
use spanbay_desktop::core::model::model_catalog::ModelCatalogFamily;

use crate::ui::{InputEvent, InputState, Tone, h_resizable, palette, resizable_panel, to_hsla};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LeftSelection {
    #[default]
    Empty,
    Provider(String),
    Bench,
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

#[derive(Debug, Clone)]
pub enum ProvidersHandoff {
    OpenUsageProvider(Option<String>),
    OpenUsageCall(i64),
}

pub struct ProvidersView {
    pub selection: LeftSelection,
    pub provider_tab: ProviderTab,
    pub providers: Vec<ProviderView>,
    pub statuses: HashMap<String, ProviderStatusDto>,
    pub keys: Vec<ProviderKey>,
    pub models: Vec<ProviderModelView>,
    pub presets: Vec<ProviderPreset>,
    pub probe_results: Vec<ProbeResult>,
    pub provider_health: Option<ProviderHealthDto>,
    pub provider_pricing: HashMap<String, ProviderPricingView>,
    pub catalog_families: Vec<ModelCatalogFamily>,
    pub recent_logs: Vec<RequestLogEntry>,

    // Interactive state & inputs.
    pub search_input: Entity<InputState>,
    pub quick_url_input: Entity<InputState>,
    pub model_search_input: Entity<InputState>,
    pub new_key_name_input: Entity<InputState>,
    pub new_key_secret_input: Entity<InputState>,
    pub new_key_group_input: Entity<InputState>,
    pub new_key_user_id_input: Entity<InputState>,
    pub edit_key_name_input: Entity<InputState>,
    pub edit_key_group_input: Entity<InputState>,
    pub edit_key_notes_input: Entity<InputState>,
    pub edit_key_secret_input: Entity<InputState>,
    pub new_model_id_input: Entity<InputState>,
    pub new_model_ctx_input: Entity<InputState>,

    pub adding_key_open: bool,
    pub editing_key_id: Option<String>,
    pub confirm_delete_key: Option<String>,
    pub revealed_provider_key: Option<(String, String)>,
    pub evidence_open: bool,
    pub refreshing_balance: bool,
    pub refreshing_models: bool,
    pub adding_model_open: bool,
    pub model_filter: ModelFilterKind,
    pub models_page: usize,
    pub models_page_size: usize,
    pub selected_models: BTreeSet<String>,
    pub last_bulk_undo: Option<(String, String)>,

    pub config_form: Option<Entity<detail_config::ProviderConfigForm>>,
    pub bench_view: Option<Entity<bench::BenchView>>,
    pub status_msg: Option<(Tone, String)>,
    pub pending_handoff: Cell<Option<ProvidersHandoff>>,
    pub(super) llm_sub_id: Option<u64>,
    pub(super) llm_stream_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl ProvidersView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mk = |ph: &'static str, masked: bool, w: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut st = InputState::new(w, cx).placeholder(ph);
                if masked {
                    st = st.masked(true);
                }
                st
            })
        };

        let pr = crate::ui::tr(cx).providers();
        let search_input = mk(pr.prov_pane_search(), false, window, cx);
        let quick_url_input = mk("https://api.deepseek.com", false, window, cx);
        let model_search_input = mk(pr.prov_providers_filter_models(), false, window, cx);
        let new_key_name_input = mk(pr.prov_providers_key_name(), false, window, cx);
        let new_key_secret_input = mk(pr.prov_providers_api_secret_sk(), true, window, cx);
        let new_key_group_input = mk(pr.prov_providers_group_optional(), false, window, cx);
        let new_key_user_id_input = mk("New-Api-User (opt)", false, window, cx);
        let edit_key_name_input = mk(pr.cfg_keys_name(), false, window, cx);
        let edit_key_group_input = mk(pr.prov_providers_group(), false, window, cx);
        let edit_key_notes_input = mk(pr.prov_providers_notes(), false, window, cx);
        let edit_key_secret_input = mk(pr.prov_providers_new_secret_empty_keep(), true, window, cx);
        let new_model_id_input = mk(pr.prov_providers_model_id(), false, window, cx);
        let new_model_ctx_input = mk(pr.prov_providers_context_opt(), false, window, cx);

        let sub_s = cx.subscribe(&search_input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        let sub_m = cx.subscribe(&model_search_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.models_page = 0;
                cx.notify();
            }
        });

        let mut view = Self {
            selection: LeftSelection::Empty,
            provider_tab: ProviderTab::Overview,
            providers: Vec::new(),
            statuses: HashMap::new(),
            keys: Vec::new(),
            models: Vec::new(),
            presets: Vec::new(),
            probe_results: Vec::new(),
            provider_health: None,
            provider_pricing: HashMap::new(),
            catalog_families: Vec::new(),
            recent_logs: Vec::new(),
            search_input,
            quick_url_input,
            model_search_input,
            new_key_name_input,
            new_key_secret_input,
            new_key_group_input,
            new_key_user_id_input,
            edit_key_name_input,
            edit_key_group_input,
            edit_key_notes_input,
            edit_key_secret_input,
            new_model_id_input,
            new_model_ctx_input,
            adding_key_open: false,
            editing_key_id: None,
            confirm_delete_key: None,
            revealed_provider_key: None,
            evidence_open: false,
            refreshing_balance: false,
            refreshing_models: false,
            adding_model_open: false,
            model_filter: ModelFilterKind::All,
            models_page: 0,
            models_page_size: 50,
            selected_models: BTreeSet::new(),
            last_bulk_undo: None,
            config_form: None,
            bench_view: None,
            status_msg: None,
            pending_handoff: Cell::new(None),
            llm_sub_id: None,
            llm_stream_task: None,
            _subscriptions: vec![sub_s, sub_m],
            _tasks: Vec::new(),
        };
        view.ensure_llm_subscribed(cx);
        view.reload_all(cx);
        view
    }

    #[must_use]
    pub fn take_pending_handoff(&self) -> Option<ProvidersHandoff> {
        self.pending_handoff.take()
    }

    pub fn dismiss_modal_if_open(&mut self, cx: &mut Context<Self>) -> bool {
        let had = self.confirm_delete_key.is_some()
            || self.adding_key_open
            || self.adding_model_open
            || self.editing_key_id.is_some();
        if had {
            self.confirm_delete_key = None;
            self.adding_key_open = false;
            self.adding_model_open = false;
            self.editing_key_id = None;
            cx.notify();
        }
        had
    }

    #[must_use]
    pub fn pricing_of(&self, provider_id: &str) -> Option<&ProviderPricingView> {
        self.provider_pricing.get(provider_id)
    }
}

impl Render for ProvidersView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);

        if let LeftSelection::Provider(id) = self.selection.clone()
            && self.provider_tab == ProviderTab::Config
        {
            self.ensure_config_form(&id, window, cx);
        }

        let left_pane = sidebar::render_sidebar(self, cx);
        let right_content: gpui_kit::AnyElement = match &self.selection {
            LeftSelection::Bench => {
                if let Some(bv) = &self.bench_view {
                    bv.clone().into_any_element()
                } else {
                    sidebar::render_onboarding_hero(self, cx).into_any_element()
                }
            }
            LeftSelection::Provider(id) => {
                if let Some(pv) = self
                    .providers
                    .iter()
                    .find(|x| &x.provider.id == id)
                    .cloned()
                {
                    detail::render_provider_detail(self, &pv, cx).into_any_element()
                } else {
                    sidebar::render_onboarding_hero(self, cx).into_any_element()
                }
            }
            LeftSelection::Empty => sidebar::render_onboarding_hero(self, cx).into_any_element(),
        };

        h_resizable("providers-main-split")
            .child(
                resizable_panel()
                    .size(px(268.0))
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
