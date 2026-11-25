//! "Models & Pricing" (`SettingsSection::Catalog`) page view & data controller.
//!
//! Connects directly to `pingdeck-core`'s model catalog, models.dev sync engine, provider pricing
//! rules, per-model pricing overrides, user-defined models, and unlisted model aliases.

pub mod drawer;
pub mod drawer_forms;
pub mod drawer_forms_extra;
pub mod drawer_save;
pub mod filters;
pub mod format;
pub mod models_table;
pub mod sync_section;

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Task, Window, px,
};
use pingdeck_desktop::core::app::{
    ProviderPricingView, UnlistedModel, current_time_ms, llm_model_catalog_config_save_impl,
    llm_model_catalog_list_impl, llm_model_catalog_status_impl, llm_model_catalog_sync_impl,
    llm_model_unlisted_impl, llm_provider_pricing_list_impl, llm_provider_pricing_save_impl,
    settings_get_impl, settings_update_impl,
};
use pingdeck_desktop::core::model::model_catalog::{
    CatalogKind, CatalogSource, ModelCatalogPage, ModelCatalogQuery, ModelCatalogRow,
    ModelCatalogStatus, ModelCatalogSyncReport, PricingFilter,
};
use pingdeck_desktop::core::model::settings::{Language, Settings, SettingsSection as CoreSection};

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into};
use crate::ui::{InputEvent, InputState, Tone, WindowExt as _, v_col};

use drawer::CatalogDrawer;

const PAGE_SIZE: u32 = 80;

pub struct CatalogSettingsView {
    pub status: Option<ModelCatalogStatus>,
    pub syncing: bool,
    pub sync_cancel_token: Option<String>,
    pub last_sync_report: Option<ModelCatalogSyncReport>,
    pub sync_error: Option<String>,
    pub provider_pricing: Vec<ProviderPricingView>,
    pub unlisted: Vec<UnlistedModel>,
    pub page: Option<ModelCatalogPage>,
    pub loading_page: bool,
    pub loading_more: bool,
    pub search_input: Entity<InputState>,
    pub publisher: Option<String>,
    pub kind: Option<CatalogKind>,
    pub source: Option<CatalogSource>,
    pub mode: Option<PricingFilter>,
    pub in_use: bool,
    pub distributor_entries: bool,
    pub flash_msg: Option<(Tone, String)>,
    pub auto_sync: bool,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl CatalogSettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = settings_get_impl(runtime(cx).state());
        let zh = settings.general.language == Language::ZhCn;
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if zh {
                "按名称、ID、厂商或分发商搜索…"
            } else {
                "Search by name, ID, publisher or distributor…"
            })
        });
        let search_sub = cx.subscribe(&search_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.load_page(false, cx);
            }
        });

        let mut view = Self {
            status: None,
            syncing: false,
            sync_cancel_token: None,
            last_sync_report: None,
            sync_error: None,
            provider_pricing: Vec::new(),
            unlisted: Vec::new(),
            page: None,
            loading_page: true,
            loading_more: false,
            search_input,
            publisher: None,
            kind: None,
            source: None,
            mode: None,
            in_use: false,
            distributor_entries: false,
            flash_msg: None,
            auto_sync: settings.ai.providers.catalog_auto_refresh,
            _subscriptions: vec![search_sub],
            _tasks: Vec::new(),
        };
        view.reload_all(cx);
        view
    }

    pub fn sync_from_settings(&mut self, settings: &Settings, cx: &mut Context<Self>) {
        if self.auto_sync != settings.ai.providers.catalog_auto_refresh {
            self.auto_sync = settings.ai.providers.catalog_auto_refresh;
            cx.notify();
        }
    }

    pub fn has_active_filters(&self, cx: &gpui_kit::App) -> bool {
        !self.search_input.read(cx).value().trim().is_empty()
            || self.publisher.is_some()
            || self.kind.is_some()
            || self.source.is_some()
            || self.mode.is_some()
            || self.in_use
            || self.distributor_entries
    }

    pub fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.publisher = None;
        self.kind = None;
        self.source = None;
        self.mode = None;
        self.in_use = false;
        self.distributor_entries = false;
        self.load_page(false, cx);
    }

    pub fn set_flash(&mut self, tone: Tone, msg: String, cx: &mut Context<Self>) {
        self.flash_msg = Some((tone, msg));
        cx.notify();
    }

    /// Reloads status, provider pricing, unlisted models, and the filtered first catalog page.
    pub fn reload_all(&mut self, cx: &mut Context<Self>) {
        let status_task = spawn_blocking_into(
            cx,
            |state| {
                let status = llm_model_catalog_status_impl(state);
                let auto = state.settings().ai.providers.catalog_auto_refresh;
                (status, auto)
            },
            |this, (res, auto), cx| {
                this.auto_sync = auto;
                if let Ok(st) = res {
                    this.status = Some(st);
                }
                cx.notify();
            },
        );
        self._tasks.push(status_task);

        let aux_task = spawn_async_into(
            cx,
            |state| async move {
                let pp = llm_provider_pricing_list_impl(&state).await;
                let unlisted = llm_model_unlisted_impl(&state).await;
                (pp, unlisted)
            },
            |this, (pp, unlisted), cx| {
                if let Ok(rows) = pp {
                    this.provider_pricing = rows;
                }
                if let Ok(rows) = unlisted {
                    this.unlisted = rows;
                }
                cx.notify();
            },
        );
        self._tasks.push(aux_task);

        self.load_page(false, cx);
    }

    fn build_query(&self, offset: u32, cx: &gpui_kit::App) -> ModelCatalogQuery {
        let search_raw = self.search_input.read(cx).value().trim().to_owned();
        ModelCatalogQuery {
            search: (!search_raw.is_empty()).then_some(search_raw),
            family: None,
            kind: self.kind,
            source: self.source,
            mode: self.mode,
            publisher: self.publisher.clone(),
            model_key: None,
            in_use: self.in_use.then_some(true),
            distributor_entries: self.distributor_entries.then_some(true),
            offset: Some(offset),
            limit: Some(PAGE_SIZE),
        }
    }

    pub fn load_page(&mut self, append: bool, cx: &mut Context<Self>) {
        let offset = if append {
            self.page.as_ref().map_or(0, |p| p.rows.len() as u32)
        } else {
            0
        };
        if append {
            self.loading_more = true;
        } else {
            self.loading_page = true;
        }
        let query = self.build_query(offset, cx);
        let task = spawn_async_into(
            cx,
            move |state| async move { llm_model_catalog_list_impl(&state, query).await },
            move |this, res, cx| {
                this.loading_page = false;
                this.loading_more = false;
                match res {
                    Ok(mut next_page) => {
                        if append && let Some(prev) = &mut this.page {
                            prev.rows.append(&mut next_page.rows);
                            prev.total = next_page.total;
                            prev.families = next_page.families;
                            prev.publishers = next_page.publishers;
                        } else {
                            this.page = Some(next_page);
                        }
                    }
                    Err(err) => {
                        this.flash_msg = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
        cx.notify();
    }

    pub fn toggle_auto_sync(&mut self, checked: bool, cx: &mut Context<Self>) {
        self.auto_sync = checked;
        let patch = serde_json::json!({
            "providers": { "catalog_auto_refresh": checked }
        });
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let _ = settings_update_impl(state, CoreSection::Ai, &patch)?;
                llm_model_catalog_status_impl(state)
            },
            |this, res, cx| {
                match res {
                    Ok(st) => {
                        this.auto_sync = st.auto_sync;
                        this.status = Some(st);
                    }
                    Err(err) => {
                        this.flash_msg = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
        cx.notify();
    }

    pub fn set_sync_interval(&mut self, days: u16, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_model_catalog_config_save_impl(state, days),
            |this, res, cx| {
                match res {
                    Ok(st) => {
                        this.status = Some(st);
                    }
                    Err(err) => {
                        this.flash_msg = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn start_sync(&mut self, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        let token = format!("catalog-sync-{}", current_time_ms());
        self.syncing = true;
        self.sync_cancel_token = Some(token.clone());
        self.sync_error = None;
        self.flash_msg = None;
        cx.notify();

        let task = spawn_async_into(
            cx,
            move |state| async move { llm_model_catalog_sync_impl(&state, Some(token)).await },
            |this, res, cx| {
                this.syncing = false;
                this.sync_cancel_token = None;
                match res {
                    Ok(report) => {
                        this.last_sync_report = Some(report);
                        this.sync_error = None;
                        this.reload_all(cx);
                    }
                    Err(err) => {
                        if !err.is_cancelled() {
                            this.sync_error = Some(err.message().to_owned());
                        }
                        this.reload_all(cx);
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn cancel_sync(&mut self, cx: &mut Context<Self>) {
        if let Some(token) = self.sync_cancel_token.clone() {
            runtime(cx).state().cancellations.cancel(&token);
        }
    }

    pub fn save_provider_pricing(
        &mut self,
        provider_id: String,
        provider_name: String,
        catalog_family: Option<String>,
        zh: bool,
        cx: &mut Context<Self>,
    ) {
        let task = spawn_async_into(
            cx,
            move |state| async move {
                llm_provider_pricing_save_impl(&state, provider_id, catalog_family).await
            },
            move |this, res, cx| {
                match res {
                    Ok(views) => {
                        this.provider_pricing = views;
                        let msg = if zh {
                            format!("已保存 {provider_name} 的计价")
                        } else {
                            format!("Saved pricing for {provider_name}")
                        };
                        this.flash_msg = Some((Tone::Success, msg));
                        this.load_page(false, cx);
                    }
                    Err(err) => {
                        this.flash_msg = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn open_model_drawer(
        &mut self,
        row: ModelCatalogRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent = cx.entity().clone();
        let drawer = cx.new(|cx| CatalogDrawer::for_model(parent, row, window, cx));
        Self::present_sheet(drawer, window, cx);
    }

    pub fn open_add_user_model_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = cx.entity().clone();
        let drawer = cx.new(|cx| CatalogDrawer::for_add_user_model(parent, window, cx));
        Self::present_sheet(drawer, window, cx);
    }

    pub fn open_unlisted_drawer(
        &mut self,
        item: UnlistedModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent = cx.entity().clone();
        let drawer = cx.new(|cx| CatalogDrawer::for_unlisted(parent, item, window, cx));
        Self::present_sheet(drawer, window, cx);
    }

    fn present_sheet(drawer: Entity<CatalogDrawer>, window: &mut Window, cx: &mut Context<Self>) {
        window.open_sheet(cx, move |sheet, _window, cx| {
            let title = drawer.read(cx).sheet_title(cx);
            sheet
                .size(px(560.0))
                .resizable(true)
                .title(title)
                .child(drawer.clone())
        });
    }
}

impl Render for CatalogSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let zh = settings_get_impl(runtime(cx).state()).general.language == Language::ZhCn;

        let mut root = v_col().w_full().gap(px(18.0)).pb(px(24.0));

        if let Some((tone, msg)) = &self.flash_msg {
            root = root.child(sync_section::render_flash_banner(*tone, msg, cx));
        }

        root = root.child(sync_section::render_sync_section(self, zh, cx));

        if !self.provider_pricing.is_empty() {
            root = root.child(sync_section::render_provider_pricing_section(self, zh, cx));
        }

        root = root.child(models_table::render_models_section(self, zh, cx));

        if !self.unlisted.is_empty() {
            root = root.child(models_table::render_unlisted_section(self, zh, cx));
        }

        root
    }
}
