//! "Models & Pricing" (`SettingsSection::Catalog`) page view & data controller.
//!
//! Global Price Book organized as a compact source sync bar above a two-tab workspace:
//! - Tab 1 (`CatalogTab::Catalog`): synced catalog (models.dev / future sources) & custom user models
//! - Tab 2 (`CatalogTab::Unlisted`): enabled provider models not yet matched to a catalog row.

pub mod drawer;
pub mod drawer_forms;
pub mod drawer_forms_extra;
pub mod drawer_save;
pub mod filters;
pub mod format;
pub mod sync_section;
pub mod tab_catalog;
pub mod tab_unlisted;

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    UnlistedModel, current_time_ms, llm_model_catalog_config_save_impl,
    llm_model_catalog_list_impl, llm_model_catalog_status_impl, llm_model_catalog_sync_impl,
    llm_model_unlisted_impl, llm_subscribe_impl, settings_get_impl, settings_update_impl,
};
use pingdeck_desktop::core::llm::events::LlmEvent;
use pingdeck_desktop::core::model::events::Sequenced;
use pingdeck_desktop::core::model::model_catalog::{
    CatalogKind, CatalogSource, ModelCatalogPage, ModelCatalogQuery, ModelCatalogRow,
    ModelCatalogStatus, ModelCatalogSyncReport, PricingFilter,
};
use pingdeck_desktop::core::model::settings::{Settings, SettingsSection as CoreSection};
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};
use crate::ui::{
    InputEvent, InputState, Tone, WindowExt as _, badge, btn_primary, card, h_row, palette,
    tab_bar_underline, tab_item, to_hsla, v_col,
};

use drawer::CatalogDrawer;

const DEFAULT_PAGE_SIZE: u32 = 50;

/// Primary sub-tabs of the Models & Pricing global price book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CatalogTab {
    #[default]
    Catalog,
    Unlisted,
}

impl CatalogTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Catalog => 0,
            Self::Unlisted => 1,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Unlisted,
            _ => Self::Catalog,
        }
    }
}

/// Pricing status filter inside the Unlisted Models tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnlistedPriceFilter {
    #[default]
    All,
    Unpriced,
    Priced,
}

pub struct CatalogSettingsView {
    pub active_tab: CatalogTab,
    pub status: Option<ModelCatalogStatus>,
    pub syncing: bool,
    pub sync_cancel_token: Option<String>,
    pub last_sync_report: Option<ModelCatalogSyncReport>,
    pub sync_error: Option<String>,
    pub unlisted: Vec<UnlistedModel>,
    pub unlisted_search_input: Entity<InputState>,
    pub unlisted_provider: Option<String>,
    pub unlisted_price_filter: UnlistedPriceFilter,
    pub unlisted_page: usize,
    pub unlisted_page_size: usize,
    pub page: Option<ModelCatalogPage>,
    pub unfiltered_catalog_total: Option<usize>,
    pub page_index: usize,
    pub page_size: u32,
    pub loading_page: bool,
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
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                crate::ui::tr(cx)
                    .settings()
                    .catalog_search_by_name_id_publisher(),
            )
        });
        let unlisted_search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                crate::ui::tr(cx)
                    .settings()
                    .catalog_unlisted_search_placeholder(),
            )
        });
        let search_sub = cx.subscribe(&search_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.load_page(false, cx);
            }
        });
        let unlisted_sub = cx.subscribe(&unlisted_search_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.unlisted_page = 0;
                cx.notify();
            }
        });

        let mut view = Self {
            active_tab: CatalogTab::Catalog,
            status: None,
            syncing: false,
            sync_cancel_token: None,
            last_sync_report: None,
            sync_error: None,
            unlisted: Vec::new(),
            unlisted_search_input,
            unlisted_provider: None,
            unlisted_price_filter: UnlistedPriceFilter::All,
            unlisted_page: 0,
            unlisted_page_size: DEFAULT_PAGE_SIZE as usize,
            page: None,
            unfiltered_catalog_total: None,
            page_index: 0,
            page_size: DEFAULT_PAGE_SIZE,
            loading_page: true,
            search_input,
            publisher: None,
            kind: None,
            source: None,
            mode: None,
            in_use: false,
            distributor_entries: false,
            flash_msg: None,
            auto_sync: settings.ai.providers.catalog_auto_refresh,
            _subscriptions: vec![search_sub, unlisted_sub],
            _tasks: Vec::new(),
        };
        let (llm_sink, llm_rx) = channel_sink::<Sequenced<LlmEvent>>();
        if llm_subscribe_impl(runtime(cx).state(), llm_sink, None).is_ok() {
            let sub_task = subscribe_stream_into(cx, llm_rx, 16, |this, batch, cx| {
                let relevant = batch.iter().any(|ev| {
                    matches!(
                        ev.event,
                        LlmEvent::CatalogChanged
                            | LlmEvent::ProvidersChanged
                            | LlmEvent::ModelsChanged { .. }
                            | LlmEvent::Gap { .. }
                    )
                });
                if relevant {
                    this.reload_all(cx);
                }
            });
            view._tasks.push(sub_task);
        }
        view.reload_all(cx);
        view
    }

    pub fn sync_from_settings(&mut self, settings: &Settings, cx: &mut Context<Self>) {
        if self.auto_sync != settings.ai.providers.catalog_auto_refresh {
            self.auto_sync = settings.ai.providers.catalog_auto_refresh;
            cx.notify();
        }
    }

    pub fn set_tab(&mut self, tab: CatalogTab, cx: &mut Context<Self>) {
        if self.active_tab != tab {
            self.active_tab = tab;
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

    pub fn unlisted_has_active_filters(&self, cx: &gpui_kit::App) -> bool {
        !self
            .unlisted_search_input
            .read(cx)
            .value()
            .trim()
            .is_empty()
            || self.unlisted_provider.is_some()
            || self.unlisted_price_filter != UnlistedPriceFilter::All
    }

    pub fn clear_unlisted_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.unlisted_search_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.unlisted_provider = None;
        self.unlisted_price_filter = UnlistedPriceFilter::All;
        self.unlisted_page = 0;
        cx.notify();
    }

    pub fn set_flash(&mut self, tone: Tone, msg: String, cx: &mut Context<Self>) {
        self.flash_msg = Some((tone, msg));
        cx.notify();
    }

    /// Reloads catalog sync status, unlisted models, and the filtered catalog page.
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

        let unlisted_task = spawn_async_into(
            cx,
            |state| async move { llm_model_unlisted_impl(&state).await },
            |this, unlisted, cx| {
                if let Ok(rows) = unlisted {
                    if let Some(pid) = &this.unlisted_provider
                        && !rows.iter().any(|r| &r.provider_id == pid)
                    {
                        this.unlisted_provider = None;
                    }
                    this.unlisted = rows;
                }
                cx.notify();
            },
        );
        self._tasks.push(unlisted_task);

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
            limit: Some(self.page_size),
        }
    }

    pub fn set_page(&mut self, page: usize, cx: &mut Context<Self>) {
        if self.page_index == page && !self.loading_page {
            return;
        }
        self.page_index = page;
        self.load_page(true, cx);
    }

    pub fn set_page_size(&mut self, size: usize, cx: &mut Context<Self>) {
        let clamped = (size as u32).clamp(10, 500);
        if self.page_size == clamped {
            return;
        }
        self.page_size = clamped;
        self.page_index = 0;
        self.load_page(false, cx);
    }

    pub fn load_page(&mut self, keep_page: bool, cx: &mut Context<Self>) {
        if !keep_page {
            self.page_index = 0;
        }
        let offset = (self.page_index as u32).saturating_mul(self.page_size);
        self.loading_page = true;
        let query = self.build_query(offset, cx);
        let task = spawn_async_into(
            cx,
            move |state| async move { llm_model_catalog_list_impl(&state, query).await },
            move |this, res, cx| {
                this.loading_page = false;
                match res {
                    Ok(next_page) => {
                        let total_pages = (next_page.total as usize)
                            .div_ceil(this.page_size.max(1) as usize)
                            .max(1);
                        if this.page_index >= total_pages {
                            this.page_index = total_pages - 1;
                            this.load_page(true, cx);
                            return;
                        }
                        if !this.has_active_filters(cx) {
                            this.unfiltered_catalog_total = Some(next_page.total as usize);
                        }
                        this.page = Some(next_page);
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
        window.open_sheet(cx, move |sheet, w, cx| {
            let title = drawer.read(cx).sheet_title(cx);
            let sheet_w =
                crate::ui::responsive_sheet_width(560.0, f32::from(w.viewport_size().width));
            sheet
                .size(px(sheet_w))
                .resizable(true)
                .title(title)
                .child(drawer.clone())
        });
    }
}

impl Render for CatalogSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let s = crate::ui::tr(cx).settings();
        let catalog_total = self
            .unfiltered_catalog_total
            .or_else(|| self.page.as_ref().map(|pg| pg.total as usize))
            .unwrap_or(0);
        let unlisted_total = self.unlisted.len();
        let unpriced_count = self
            .unlisted
            .iter()
            .filter(|u| u.effective.is_none())
            .count();

        let tab_suffix = h_row()
            .flex_shrink_0()
            .gap(px(8.0))
            .pb(px(5.0))
            .children((unpriced_count > 0).then(|| {
                badge(
                    s.catalog_unlisted_unpriced_badge(unpriced_count),
                    Tone::Warning,
                    cx,
                )
            }))
            .child(
                btn_primary("cat-add-user-model", s.models_table_add_custom_model())
                    .h(px(28.0))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_add_user_model_drawer(window, cx);
                    })),
            );

        let tab_header = tab_bar_underline("cat-main-tabs")
            .w_full()
            .px(px(14.0))
            .pt(px(4.0))
            .selected_index(self.active_tab.index())
            .child(tab_item(SharedString::from(
                s.catalog_tab_catalog(catalog_total),
            )))
            .child(tab_item(SharedString::from(
                s.catalog_tab_unlisted(unlisted_total),
            )))
            .suffix(tab_suffix)
            .on_click(cx.listener(|this, idx: &usize, _, cx| {
                this.set_tab(CatalogTab::from_index(*idx), cx);
            }));

        let body = match self.active_tab {
            CatalogTab::Catalog => tab_catalog::render_catalog_tab(self, cx),
            CatalogTab::Unlisted => tab_unlisted::render_unlisted_tab(self, cx),
        };
        let footnote = match self.active_tab {
            CatalogTab::Catalog => s.models_table_prices_are_per_1m_tokens(),
            CatalogTab::Unlisted => s.models_table_models_your_providers_serve_that(),
        };

        let workspace_card = v_col()
            .w_full()
            .gap(px(8.0))
            .child(
                card(cx)
                    .w_full()
                    .overflow_hidden()
                    .child(tab_header)
                    .child(body),
            )
            .child(
                div()
                    .px(px(4.0))
                    .text_xs()
                    .text_color(to_hsla(p.faint))
                    .child(footnote),
            );

        let mut root = v_col().w_full().gap(px(16.0)).pb(px(24.0));
        if let Some((tone, msg)) = &self.flash_msg {
            root = root.child(sync_section::render_flash_banner(*tone, msg, cx));
        }
        root.child(sync_section::render_sync_section(self, cx))
            .child(workspace_card)
    }
}
