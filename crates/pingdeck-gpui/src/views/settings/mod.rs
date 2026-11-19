//! Settings module (`NavModule::Settings`) hosting all 20 pages across 5 categories
//! with live search, instant OKLCH eye-care palette preview, and direct `pingdeck-core` saves.

pub mod appearance;
pub mod catalog;
pub mod sections_ai;
pub mod sections_core;

use std::cell::Cell;

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div, px,
};
use pingdeck_desktop::core::agent::mcp::AgentMcpServerView;
use pingdeck_desktop::core::agent::model::{AgentFolderTrust, AgentSkill};
use pingdeck_desktop::core::app::{
    PermissionRuleView, agent_folder_trust_list_impl, agent_mcp_server_list_impl,
    agent_permission_rule_views_impl, agent_skill_list_impl, settings_get_impl,
    settings_reset_all_impl, settings_reset_section_impl, settings_update_impl,
};
use pingdeck_desktop::core::model::settings::{Language, Settings, SettingsSection as CoreSection};
use pingdeck_desktop::state::{SettingsGroup, SettingsSection};
use pingdeck_desktop::theme::PaletteId;

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::{
    InputState, ScrollableElement as _, SliderEvent, SliderState, UiIcon, h_row, icon_xs, list_row,
    palette, search_input, to_hsla, v_col,
};

pub struct SettingsView {
    pub active: SettingsSection,
    pub settings: Settings,
    pub active_palette: PaletteId,
    pub mcp_servers: Vec<AgentMcpServerView>,
    pub skills: Vec<AgentSkill>,
    pub trusted_folders: Vec<AgentFolderTrust>,
    pub permission_rules: Vec<PermissionRuleView>,
    pub saved_flash: Option<String>,
    pub ui_font_slider: Entity<SliderState>,
    catalog_view: Option<Entity<catalog::CatalogSettingsView>>,
    search_input: Entity<InputState>,
    pending_palette: Cell<Option<PaletteId>>,
    pending_back: Cell<bool>,
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
        let settings = settings_get_impl(runtime(cx).state());
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索设置 / Search…"));
        let initial_font = f32::from(settings.appearance.ui_font_size.clamp(11, 18));
        let ui_font_slider = cx.new(|_| {
            SliderState::new()
                .min(11.0)
                .max(18.0)
                .step(1.0)
                .default_value(initial_font)
        });
        let slider_sub = cx.subscribe(&ui_font_slider, |this, _, ev: &SliderEvent, cx| {
            let val = match ev {
                SliderEvent::Release(v) | SliderEvent::Change(v) => v.end().round() as u8,
            };
            if matches!(ev, SliderEvent::Release(_)) && val != this.settings.appearance.ui_font_size
            {
                this.patch_section(
                    CoreSection::Appearance,
                    serde_json::json!({ "ui_font_size": val }),
                    cx,
                );
            }
        });
        let mut view = Self {
            active: initial_section,
            settings,
            active_palette,
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            trusted_folders: Vec::new(),
            permission_rules: Vec::new(),
            saved_flash: None,
            ui_font_slider,
            catalog_view: None,
            search_input,
            pending_palette: Cell::new(None),
            pending_back: Cell::new(false),
            _subscriptions: vec![slider_sub],
            _tasks: Vec::new(),
        };
        view.reload_aux(cx);
        view
    }

    pub fn reload_aux(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            |state| {
                let settings = settings_get_impl(state);
                let mcp = agent_mcp_server_list_impl(state).unwrap_or_default();
                let skills = agent_skill_list_impl(state, None).unwrap_or_default();
                let trust = agent_folder_trust_list_impl(state).unwrap_or_default();
                let rules = agent_permission_rule_views_impl(state, None, None).unwrap_or_default();
                (settings, mcp, skills, trust, rules)
            },
            |this, (settings, mcp, skills, trust, rules), cx| {
                this.settings = settings;
                this.mcp_servers = mcp;
                this.skills = skills;
                this.trusted_folders = trust;
                this.permission_rules = rules;
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn set_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.active = section;
        cx.notify();
    }

    pub fn take_pending_palette(&self) -> Option<PaletteId> {
        self.pending_palette.take()
    }

    pub fn take_pending_back(&self) -> bool {
        self.pending_back.replace(false)
    }

    pub fn choose_palette(&mut self, pid: PaletteId, cx: &mut Context<Self>) {
        self.active_palette = pid;
        self.pending_palette.set(Some(pid));
        cx.notify();
    }

    pub fn patch_section(
        &mut self,
        section: CoreSection,
        patch: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let task = spawn_blocking_into(
            cx,
            move |state| settings_update_impl(state, section, &patch),
            |this, res, cx| {
                match res {
                    Ok(next) => {
                        this.settings = next;
                        this.saved_flash = Some("Saved".into());
                    }
                    Err(err) => {
                        this.saved_flash = Some(err.message().to_owned());
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn reset_section(&mut self, section: CoreSection, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| settings_reset_section_impl(state, section, None),
            |this, res, cx| {
                if let Ok(next) = res {
                    this.settings = next;
                    this.saved_flash = Some("Section reset".into());
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn reset_all(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(cx, settings_reset_all_impl, |this, res, cx| {
            if let Ok(next) = res {
                this.settings = next;
                this.saved_flash = Some("All settings reset".into());
            }
            cx.notify();
        });
        self._tasks.push(task);
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let zh = self.settings.general.language == Language::ZhCn;
        let query = self.search_input.read(cx).value().trim().to_lowercase();

        let mut nav = v_col()
            .flex_1()
            .gap(px(2.0))
            .p(px(10.0))
            .overflow_y_scrollbar();
        for group in SettingsGroup::ALL {
            let matching: Vec<SettingsSection> = SettingsSection::ALL
                .into_iter()
                .filter(|s| s.group() == group)
                .filter(|s| {
                    if query.is_empty() {
                        return true;
                    }
                    let (zl, el) = s.label_pair();
                    zl.to_lowercase().contains(&query) || el.to_lowercase().contains(&query)
                })
                .collect();
            if matching.is_empty() {
                continue;
            }
            let (gz, ge) = group.label_pair();
            nav = nav.child(
                div()
                    .px(px(10.0))
                    .pt(px(12.0))
                    .pb(px(3.0))
                    .text_size(px(11.0))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.faint))
                    .child(if zh { gz } else { ge }),
            );
            for sec in matching {
                let (zl, el) = sec.label_pair();
                nav = nav.child(
                    list_row(
                        SharedString::from(format!("set-nav-{el}")),
                        self.active == sec,
                        cx,
                        cx.listener(move |this, _, _, cx| {
                            this.active = sec;
                            cx.notify();
                        }),
                    )
                    .child(div().text_xs().child(if zh { zl } else { el })),
                );
            }
        }

        let left = v_col()
            .w(px(230.0))
            .h_full()
            .bg(to_hsla(p.pane))
            .border_r_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(
                v_col()
                    .p(px(10.0))
                    .gap(px(8.0))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.pane)))
                    .child(
                        list_row(
                            "settings-back",
                            false,
                            cx,
                            cx.listener(|this, _, _, cx| {
                                this.pending_back.set(true);
                                cx.notify();
                            }),
                        )
                        .gap(px(6.0))
                        .text_color(to_hsla(p.ink_2))
                        .child(icon_xs(UiIcon::ArrowLeft))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(if zh {
                                    "返回工作区"
                                } else {
                                    "Back to workspace"
                                }),
                        ),
                    )
                    .child(search_input(&self.search_input, cx)),
            )
            .child(nav);

        let (gz, ge) = self.active.group().label_pair();
        let (sz, se) = self.active.label_pair();
        let breadcrumb = if zh {
            format!("{gz}  ›  {sz}")
        } else {
            format!("{ge}  ›  {se}")
        };

        let is_wide_page = self.active == SettingsSection::Catalog;
        let page_body = match self.active {
            SettingsSection::Catalog => self
                .catalog_view
                .get_or_insert_with(|| cx.new(|cx| catalog::CatalogSettingsView::new(window, cx)))
                .clone()
                .into_any_element(),
            SettingsSection::General
            | SettingsSection::Appearance
            | SettingsSection::Shortcuts
            | SettingsSection::Requests
            | SettingsSection::Network
            | SettingsSection::Mock
            | SettingsSection::Data
            | SettingsSection::Advanced
            | SettingsSection::About => {
                sections_core::render_core_section(self, cx).into_any_element()
            }
            _ => sections_ai::render_ai_section(self, cx).into_any_element(),
        };

        let right = v_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .bg(to_hsla(p.surface))
            .child(
                h_row()
                    .justify_between()
                    .px(px(24.0))
                    .py(px(12.0))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(SharedString::from(breadcrumb)),
                    )
                    .child(
                        h_row()
                            .gap(px(5.0))
                            .px(px(9.0))
                            .py(px(3.0))
                            .rounded(px(999.0))
                            .bg(to_hsla(p.ok_soft))
                            .border_1()
                            .border_color(to_hsla(p.ok_line.composite_over(p.ok_soft)))
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ok))
                            .child(icon_xs(UiIcon::Check))
                            .child(SharedString::from(
                                self.saved_flash
                                    .clone()
                                    .unwrap_or_else(|| if zh { "已保存" } else { "Saved" }.into()),
                            )),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(28.0))
                    .py(px(18.0))
                    .overflow_y_scrollbar()
                    .child(if is_wide_page {
                        div().w_full().child(page_body)
                    } else {
                        div().max_w(px(760.0)).child(page_body)
                    }),
            );

        h_row().size_full().items_start().child(left).child(right)
    }
}
