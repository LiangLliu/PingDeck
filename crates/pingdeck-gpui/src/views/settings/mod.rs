//! Settings module (`NavModule::Settings`) hosting all 20 pages across 5 categories with
//! bilingual keyword search, live `SettingsEvent` sync, instant OKLCH palette preview,
//! full validation & undoable reset, and direct `pingdeck-core` services.

pub mod about;
pub mod actions_core;
pub mod actions_data;
pub mod actions_sync;
pub mod agent;
pub mod appearance;
pub mod catalog;
mod dispatch;
pub mod modals;
mod nav;
pub mod page_advanced;
pub mod page_data;
pub mod page_general;
pub mod page_mock;
pub mod page_network;
pub mod page_requests;
pub mod page_routing;
pub mod page_shortcuts;
pub mod page_usage;
pub mod state_inputs;

use std::cell::Cell;

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Task, Window, div, px,
};
use pingdeck_desktop::core::agent::mcp::AgentMcpServerView;
use pingdeck_desktop::core::app::{
    CaBundleInfoDto, ProxyTestDto, StorageUsageDto, SyncFolderInfoDto, agent_mcp_server_list_impl,
    settings_get_impl, settings_registry_get_impl, settings_subscribe_impl,
};
use pingdeck_desktop::core::llm::gateway::GatewayStatus;
use pingdeck_desktop::core::model::settings::registry::SettingsRegistryDto;
use pingdeck_desktop::core::model::settings::{
    Settings, SettingsEvent, SettingsSection as CoreSection,
};
use pingdeck_desktop::core::sync::SyncStatus;
use pingdeck_desktop::state::{NavModule, SettingsGroup, SettingsSection};
use pingdeck_desktop::streams::channel_sink;
use pingdeck_desktop::theme::PaletteId;

use crate::bridge::{runtime, spawn_blocking_into, subscribe_stream_into};
use crate::ui::{
    InputEvent, InputState, ScrollableElement as _, SliderEvent, StatusShape, Tone, UiIcon,
    btn_secondary, dismissible_banner, h_row, icon_xs, palette, status_indicator, to_hsla, v_col,
};

use state_inputs::{ProxyModeChoice, SettingsInputs, SettingsModal};

pub struct SettingsView {
    pub active: SettingsSection,
    pub settings: Settings,
    pub active_palette: PaletteId,
    pub mcp_servers: Vec<AgentMcpServerView>,
    pub flash: Option<(Tone, String)>,
    pub last_save_error: bool,
    pub can_undo_reset_all: bool,
    pub inputs: SettingsInputs,
    pub modal: Option<SettingsModal>,
    pub proxy_mode: ProxyModeChoice,
    pub proxy_testing: bool,
    pub proxy_test_result: Option<ProxyTestDto>,
    pub ca_testing: bool,
    pub ca_test_result: Option<CaBundleInfoDto>,
    pub editing_shortcut: Option<&'static str>,
    pub storage_usage: Option<StorageUsageDto>,
    pub data_dir_path: Option<String>,
    pub storage_loading: bool,
    pub data_busy: bool,
    pub chat_retention_preview: Option<u64>,
    pub usage_retention_preview: Option<(u64, u64)>,
    pub sync_status: Option<SyncStatus>,
    pub sync_folder_info: Option<SyncFolderInfoDto>,
    pub sync_busy: bool,
    pub gateway_status: Option<GatewayStatus>,
    pub registry: SettingsRegistryDto,
    pub(crate) pending_sync_inputs: bool,
    pub(crate) about_view: Option<Entity<about::AboutSettingsView>>,
    pub(crate) agent_view: Option<Entity<agent::AgentSettingsView>>,
    pub(crate) catalog_view: Option<Entity<catalog::CatalogSettingsView>>,
    pub(super) search_input: Entity<InputState>,
    pending_palette: Cell<Option<PaletteId>>,
    pub(super) pending_back: Cell<bool>,
    pending_navigate: Cell<Option<NavModule>>,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl SettingsView {
    pub fn new(
        initial_section: SettingsSection,
        active_palette: PaletteId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let rt = runtime(cx);
        let settings = settings_get_impl(rt.state());
        let registry = settings_registry_get_impl();
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::ui::tr(cx).settings().nav_search_placeholder())
        });
        let inputs = SettingsInputs::new(&settings, window, cx);
        let proxy_mode = ProxyModeChoice::from_proxy_url(&settings.network.proxy_url);

        let search_sub = cx.subscribe(&search_input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        let sc_filter_sub = cx.subscribe(&inputs.shortcut_filter, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        let ui_slider_sub =
            cx.subscribe(&inputs.ui_font_slider, |this, _, ev: &SliderEvent, cx| {
                let val = match ev {
                    SliderEvent::Release(v) | SliderEvent::Change(v) => v.end().round() as u8,
                };
                if matches!(ev, SliderEvent::Change(_)) {
                    cx.notify();
                } else if val != this.settings.appearance.ui_font_size {
                    this.patch_section(
                        CoreSection::Appearance,
                        serde_json::json!({ "ui_font_size": val }),
                        cx,
                    );
                }
            });
        let mono_slider_sub =
            cx.subscribe(&inputs.mono_font_slider, |this, _, ev: &SliderEvent, cx| {
                let val = match ev {
                    SliderEvent::Release(v) | SliderEvent::Change(v) => v.end().round() as u8,
                };
                if matches!(ev, SliderEvent::Change(_)) {
                    cx.notify();
                } else if val != this.settings.appearance.mono_font_size {
                    this.patch_section(
                        CoreSection::Appearance,
                        serde_json::json!({ "mono_font_size": val }),
                        cx,
                    );
                }
            });

        // Subscribe to core SettingsEvent stream so edits from Agent, Catalog, Command Palette,
        // or Sync Import keep every page and input synchronized.
        let (set_sink, set_rx) = channel_sink::<SettingsEvent>();
        let _ = settings_subscribe_impl(rt.state(), set_sink);
        let settings_task = subscribe_stream_into(cx, set_rx, 8, |this, _, cx| {
            let latest = settings_get_impl(runtime(cx).state());
            if latest != this.settings {
                this.apply_updated_settings(latest, cx);
                cx.notify();
            }
        });

        let mut view = Self {
            active: initial_section,
            settings,
            active_palette,
            mcp_servers: Vec::new(),
            flash: None,
            last_save_error: false,
            can_undo_reset_all: false,
            inputs,
            modal: None,
            proxy_mode,
            proxy_testing: false,
            proxy_test_result: None,
            ca_testing: false,
            ca_test_result: None,
            editing_shortcut: None,
            storage_usage: None,
            data_dir_path: None,
            storage_loading: false,
            data_busy: false,
            chat_retention_preview: None,
            usage_retention_preview: None,
            sync_status: None,
            sync_folder_info: None,
            sync_busy: false,
            gateway_status: None,
            registry,
            pending_sync_inputs: false,
            about_view: None,
            agent_view: None,
            catalog_view: None,
            search_input,
            pending_palette: Cell::new(None),
            pending_back: Cell::new(false),
            pending_navigate: Cell::new(None),
            _subscriptions: vec![search_sub, sc_filter_sub, ui_slider_sub, mono_slider_sub],
            _tasks: vec![settings_task],
        };
        view.reload_aux(cx);
        view.refresh_data_and_sync(cx);
        view
    }

    pub fn reload_aux(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            |state| agent_mcp_server_list_impl(state).unwrap_or_default(),
            |this, mcp, cx| {
                let latest = settings_get_impl(runtime(cx).state());
                if latest != this.settings {
                    this.apply_updated_settings(latest, cx);
                }
                this.mcp_servers = mcp;
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn set_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.active = section;
        self.editing_shortcut = None;
        if section == SettingsSection::About
            && let Some(about) = &self.about_view
        {
            about.update(cx, |v, cx| v.refresh(cx));
        }
        if section == SettingsSection::Catalog
            && let Some(cat) = &self.catalog_view
        {
            cat.update(cx, |v, cx| v.reload_all(cx));
        }
        if section.group() == SettingsGroup::Agent
            && let Some(ag) = &self.agent_view
        {
            ag.update(cx, |v, cx| v.set_section(section, cx));
        }
        if matches!(
            section,
            SettingsSection::Data | SettingsSection::Routing | SettingsSection::Usage
        ) {
            self.refresh_data_and_sync(cx);
        }
        cx.notify();
    }

    pub fn has_pending_shell_signal(&self) -> bool {
        self.pending_palette.get().is_some()
            || self.pending_back.get()
            || self.pending_navigate.get().is_some()
    }

    pub fn take_pending_palette(&self) -> Option<PaletteId> {
        self.pending_palette.take()
    }

    pub fn take_pending_back(&self) -> bool {
        self.pending_back.replace(false)
    }

    pub fn take_pending_navigate(&self) -> Option<NavModule> {
        self.pending_navigate.take()
    }

    pub fn jump_to_module(&mut self, target: NavModule, cx: &mut Context<Self>) {
        self.pending_navigate.set(Some(target));
        cx.notify();
    }

    pub fn dismiss_modal_if_open(&mut self, cx: &mut Context<Self>) -> bool {
        if self.modal.take().is_some() {
            cx.notify();
            return true;
        }
        if self.editing_shortcut.take().is_some() {
            cx.notify();
            return true;
        }
        false
    }

    pub fn choose_palette(&mut self, pid: PaletteId, cx: &mut Context<Self>) {
        self.active_palette = pid;
        self.pending_palette.set(Some(pid));
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_sync_inputs {
            self.pending_sync_inputs = false;
            let snapshot = self.settings.clone();
            self.inputs.sync_from_settings(&snapshot, window, cx);
        }

        let p = palette(cx);
        let lang = crate::ui::active_language(cx);
        let left = nav::render_settings_sidebar(self, cx);

        let group_lbl = self.active.group().label(lang);
        let page_lbl = self.active.label(lang);
        let show_group_crumb = group_lbl != page_lbl;
        let page_sub = dispatch::page_subtitle(self.active, lang);

        let breadcrumb_row = h_row()
            .gap(px(6.0))
            .children(show_group_crumb.then(|| {
                h_row()
                    .gap(px(6.0))
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(group_lbl)
                    .child("›")
            }))
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink))
                    .child(page_lbl),
            );

        let status_pill = (self.active != SettingsSection::About).then(|| {
            let (bg, border, fg, icon_el, text) = if self.last_save_error {
                (
                    to_hsla(p.bad_soft),
                    to_hsla(p.bad_line.composite_over(p.bad_soft)),
                    to_hsla(p.bad),
                    status_indicator(StatusShape::Bad, cx),
                    crate::ui::tr(cx).settings().settings_save_error(),
                )
            } else {
                (
                    to_hsla(p.ok_soft),
                    to_hsla(p.ok_line.composite_over(p.ok_soft)),
                    to_hsla(p.ok),
                    icon_xs(UiIcon::Check).into_any_element(),
                    crate::ui::tr(cx).settings().settings_auto_saved(),
                )
            };
            h_row()
                .gap(px(5.0))
                .px(px(9.0))
                .py(px(3.0))
                .rounded(px(999.0))
                .bg(bg)
                .border_1()
                .border_color(border)
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(fg)
                .child(icon_el)
                .child(text)
        });

        let page_body = dispatch::render_page_body(self, window, cx);

        let flash_banner = self.flash.clone().map(|(tone, msg)| {
            h_row()
                .w_full()
                .gap(px(8.0))
                .pb(px(12.0))
                .child(div().flex_1().min_w_0().child(dismissible_banner(
                    "settings-flash-close",
                    tone,
                    msg,
                    cx.listener(|this, _, _, cx| {
                        this.flash = None;
                        this.last_save_error = false;
                        cx.notify();
                    }),
                    cx,
                )))
                .children(self.can_undo_reset_all.then(|| {
                    btn_secondary("settings-flash-undo", crate::ui::tr(cx).common().undo())
                        .icon(icon_xs(UiIcon::RotateCcw))
                        .on_click(cx.listener(|this, _, _, cx| this.undo_last_reset(cx)))
                }))
        });

        let page_hero = (self.active != SettingsSection::About).then(|| {
            v_col()
                .w_full()
                .gap(px(3.0))
                .px(px(2.0))
                .pb(px(16.0))
                .child(
                    div()
                        .text_size(px(18.0))
                        .font_weight(gpui_kit::FontWeight::BOLD)
                        .text_color(to_hsla(p.ink))
                        .child(page_lbl),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(to_hsla(p.muted))
                        .child(page_sub),
                )
        });

        let right = v_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .bg(to_hsla(p.pane))
            .child(
                h_row()
                    .h(px(44.0))
                    .flex_shrink_0()
                    .justify_between()
                    .px(px(24.0))
                    .bg(to_hsla(p.surface))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)))
                    .child(breadcrumb_row)
                    .children(status_pill),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .px(px(24.0))
                    .pt(px(18.0))
                    .pb(px(36.0))
                    .overflow_y_scrollbar()
                    .child(
                        v_col()
                            .w_full()
                            .children(flash_banner)
                            .children(page_hero)
                            .child(div().w_full().child(page_body)),
                    ),
            );

        h_row()
            .size_full()
            .items_start()
            .child(left)
            .child(right)
            .children(modals::render_settings_modal(self, window, cx))
    }
}
