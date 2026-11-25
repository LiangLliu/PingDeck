//! Collapsible navigation sidebar aligned with the PingDeck desktop prototype:
//! integrated macOS traffic-light header, native anchored workspace popover, clean 2-pillar
//! navigation (API Workspace + Shared AI) with live status badges, and Running Work popover footer.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, WindowControlArea, div, px,
};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::state::{NavModule, UtilityDialog};

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_icon, h_row, icon, icon_xs, kbd_chip, list_row, palette,
    pingmark, section_heading, status_indicator, to_hsla, v_col,
};

use super::AppShell;
use super::sidebar_popovers::{render_running_work_popover, render_workspace_popover};

fn module_icon(module: NavModule) -> UiIcon {
    match module {
        NavModule::Requests => UiIcon::Send,
        NavModule::Mock => UiIcon::Server,
        NavModule::Flows => UiIcon::Workflow,
        NavModule::History => UiIcon::History,
        NavModule::Environments => UiIcon::Layers,
        NavModule::Chat => UiIcon::Chat,
        NavModule::Providers => UiIcon::Cpu,
        NavModule::Usage => UiIcon::Usage,
        NavModule::Settings => UiIcon::Settings,
    }
}

pub fn render_sidebar(shell: &AppShell, cx: &mut Context<AppShell>) -> impl IntoElement {
    let p = palette(cx);
    let zh = shell.language() == Language::ZhCn;
    let collapsed = shell.state.sidebar_collapsed;
    let width = if collapsed {
        px(52.0)
    } else {
        px(shell.state.sidebar_width.clamp(196.0, 260.0))
    };

    let active_ws_name = shell
        .state
        .active_workspace_id
        .as_deref()
        .and_then(|id| shell.state.workspaces.iter().find(|w| w.id.as_str() == id))
        .map(|w| w.name.clone())
        .unwrap_or_else(|| {
            if zh {
                "默认工作区".into()
            } else {
                "Default workspace".into()
            }
        });

    // 1. Dedicated top window-drag band clearing macOS traffic lights cleanly in both expanded
    //    and collapsed rail modes (matching prototype `.sidebar-top`).
    let top_drag = div()
        .w_full()
        .h(px(if cfg!(target_os = "macos") { 38.0 } else { 8.0 }))
        .flex_shrink_0()
        .window_control_area(WindowControlArea::Drag);

    // 2. Brand + sidebar collapse/expand row at a fixed 32px height so collapsing causes zero
    //    vertical layout shift across Workspace, Search, and Nav rows.
    let brand_row = if collapsed {
        h_row()
            .w_full()
            .h(px(32.0))
            .flex_shrink_0()
            .justify_center()
            .mb(px(6.0))
            .child(
                btn_icon("sb-toggle-rail", UiIcon::PanelLeft)
                    .tooltip(if zh {
                        "展开侧栏 (⌘B)"
                    } else {
                        "Expand sidebar (⌘B)"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
            )
    } else {
        h_row()
            .w_full()
            .h(px(32.0))
            .flex_shrink_0()
            .pl(px(10.0))
            .pr(px(2.0))
            .mb(px(6.0))
            .justify_between()
            .window_control_area(WindowControlArea::Drag)
            .child(
                h_row().gap(px(8.0)).child(pingmark(15.0, cx)).child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child("PingDeck"),
                ),
            )
            .child(
                btn_icon("sb-toggle", UiIcon::PanelLeft)
                    .tooltip(if zh {
                        "收起侧栏 (⌘B)"
                    } else {
                        "Collapse sidebar (⌘B)"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
            )
    };

    // 3. Workspace switcher (expanded + collapsed rail) & ⌘K quick jump trigger.
    let mut top = v_col().w_full().gap(px(6.0)).pb(px(6.0)).flex_shrink_0();
    if !collapsed {
        top = top
            .child(render_workspace_popover(
                shell,
                false,
                active_ws_name,
                zh,
                cx,
            ))
            .child(
                div()
                    .id("sidebar-palette-btn")
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(30.0))
                    .px(px(10.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.surface))
                    .border_1()
                    .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
                    .cursor_pointer()
                    .hover(|s| s.border_color(to_hsla(p.accent_line.composite_over(p.surface))))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_palette(window, cx)))
                    .child(
                        h_row()
                            .gap(px(7.0))
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(icon_xs(UiIcon::Search))
                            .child(if zh {
                                "搜索或跳转"
                            } else {
                                "Search or jump"
                            }),
                    )
                    .child(kbd_chip("⌘K", cx)),
            );
    } else {
        top = top
            .items_center()
            .child(render_workspace_popover(
                shell,
                true,
                active_ws_name,
                zh,
                cx,
            ))
            .child(
                btn_icon("sidebar-palette-rail", UiIcon::Search)
                    .tooltip(if zh {
                        "搜索或跳转 (⌘K)"
                    } else {
                        "Search or jump (⌘K)"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_palette(window, cx))),
            );
    }

    // 4. Clean 2-pillar navigation: Workspace API modules + Shared AI modules.
    let mut nav = v_col().flex_1().min_h_0().w_full().gap(px(2.0)).pt(px(2.0));
    if collapsed {
        nav = nav.items_center();
    }
    for module in NavModule::WORKSPACE_MODULES {
        nav = nav.child(render_nav_item(shell, module, collapsed, zh, cx));
    }

    if !collapsed {
        nav = nav.child(
            div()
                .px(px(10.0))
                .pt(px(14.0))
                .pb(px(4.0))
                .child(section_heading(
                    if zh {
                        "AI · 所有工作区共用"
                    } else {
                        "AI · Shared"
                    },
                    cx,
                )),
        );
    } else {
        nav = nav.child(
            div()
                .my(px(6.0))
                .w(px(20.0))
                .h(px(1.0))
                .bg(to_hsla(p.line.composite_over(p.canvas))),
        );
    }
    for module in NavModule::AI_MODULES {
        nav = nav.child(render_nav_item(shell, module, collapsed, zh, cx));
    }

    // 5. Footer: Running Work popover + Gateway status + Settings & quick theme/console toggles.
    let work = &shell.state.running_work;
    let active_jobs = work.agent_runs
        + work.subagents
        + work.flow_runs
        + work.collection_runs
        + work.mock_servers;
    let gw_running = shell.state.gateway_port.is_some();
    let gw_port_label = shell.state.gateway_port.map(|port| format!(":{port}"));
    let gw_text = match (gw_running, zh) {
        (true, true) => "网关运行中",
        (true, false) => "Gateway live",
        (false, true) => "网关已停止",
        (false, false) => "Gateway off",
    };
    let work_text = if shell.state.attention.waiting_approval > 0 {
        if zh {
            format!(
                "{} 项待批 · {active_jobs} 运行",
                shell.state.attention.waiting_approval
            )
        } else {
            format!(
                "{} waiting · {active_jobs} run",
                shell.state.attention.waiting_approval
            )
        }
    } else if active_jobs == 0 {
        if zh { "无后台任务" } else { "Idle" }.into()
    } else if zh {
        format!("{active_jobs} 项运行中")
    } else {
        format!("{active_jobs} running")
    };

    let mut footer = v_col()
        .w_full()
        .flex_shrink_0()
        .gap(px(2.0))
        .pt(px(8.0))
        .mt(px(6.0))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.canvas)));

    if !collapsed {
        footer = footer
            .child(render_running_work_popover(
                shell,
                false,
                active_jobs,
                work_text,
                zh,
                cx,
            ))
            .child(
                div()
                    .id("footer-gateway")
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(28.0))
                    .gap(px(8.0))
                    .px(px(10.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.canvas))))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.navigate(NavModule::Providers, cx);
                        this.modules.open_providers_gateway(window, cx);
                    }))
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .flex_1()
                            .min_w_0()
                            .child(status_indicator(
                                if gw_running {
                                    StatusShape::Live
                                } else {
                                    StatusShape::Idle
                                },
                                cx,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(to_hsla(p.ink_2))
                                    .truncate()
                                    .child(gw_text),
                            ),
                    )
                    .children(gw_port_label.map(|port| {
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.muted))
                            .child(port)
                    })),
            )
            .child(
                h_row()
                    .gap(px(2.0))
                    .pt(px(2.0))
                    .child(div().flex_1().min_w_0().child(render_nav_item(
                        shell,
                        NavModule::Settings,
                        false,
                        zh,
                        cx,
                    )))
                    .child(
                        btn_icon("sb-console", UiIcon::Terminal)
                            .tooltip(if zh { "诊断控制台" } else { "Console" })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_utility(UtilityDialog::Console, window, cx);
                            })),
                    )
                    .child(
                        btn_icon(
                            "sb-theme-quick",
                            if p.is_dark { UiIcon::Sun } else { UiIcon::Moon },
                        )
                        .tooltip(if zh {
                            "切换浅/深色"
                        } else {
                            "Toggle Light / Dark"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_theme_mode(cx))),
                    ),
            );
    } else {
        footer = footer
            .items_center()
            .gap(px(4.0))
            .child(render_running_work_popover(
                shell,
                true,
                active_jobs,
                work_text,
                zh,
                cx,
            ))
            .child(
                div()
                    .id("footer-gateway-rail")
                    .size(px(34.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.canvas))))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.navigate(NavModule::Providers, cx);
                        this.modules.open_providers_gateway(window, cx);
                    }))
                    .child(status_indicator(
                        if gw_running {
                            StatusShape::Live
                        } else {
                            StatusShape::Idle
                        },
                        cx,
                    )),
            )
            .child(
                btn_icon(
                    "sb-theme-rail",
                    if p.is_dark { UiIcon::Sun } else { UiIcon::Moon },
                )
                .tooltip(if zh {
                    "切换浅/深色"
                } else {
                    "Toggle theme"
                })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_theme_mode(cx))),
            )
            .child(render_nav_item(shell, NavModule::Settings, true, zh, cx));
    }

    v_col()
        .w(width)
        .h_full()
        .flex_shrink_0()
        .bg(to_hsla(p.canvas))
        .child(top_drag)
        .child(
            v_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .px(px(8.0))
                .pb(px(8.0))
                .child(brand_row)
                .child(top)
                .child(nav)
                .child(footer),
        )
}

fn render_nav_item(
    shell: &AppShell,
    module: NavModule,
    collapsed: bool,
    zh: bool,
    cx: &mut Context<AppShell>,
) -> impl IntoElement {
    let p = palette(cx);
    let selected = shell.state.active_module == module;
    let (zh_label, en_label) = module.label_pair();
    let label = if zh { zh_label } else { en_label };
    let waiting = if module == NavModule::Chat {
        shell.state.attention.waiting_approval
    } else {
        0
    };
    let live_mocks = if module == NavModule::Mock {
        shell.state.running_work.mock_servers
    } else {
        0
    };
    let live_flows = if module == NavModule::Flows {
        shell.state.running_work.flow_runs
    } else {
        0
    };
    let prov_attn = if module == NavModule::Providers {
        shell.provider_attention_count
    } else {
        0
    };
    let gw_live = module == NavModule::Providers && shell.state.gateway_port.is_some();

    let id = SharedString::from(format!("nav-{}", module.shortcut_digit()));
    if collapsed {
        let hover_bg = to_hsla(p.hover.composite_over(p.canvas));
        let mut tile = div()
            .id(id)
            .relative()
            .size(px(34.0))
            .rounded(px(7.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.navigate(module, cx);
            }));
        if selected {
            tile = tile
                .bg(to_hsla(p.selected))
                .border_1()
                .border_color(to_hsla(p.line_strong.composite_over(p.selected)))
                .text_color(to_hsla(p.accent_ink))
                .child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .top(px(7.0))
                        .bottom(px(7.0))
                        .w(px(2.5))
                        .rounded(px(2.0))
                        .bg(to_hsla(p.accent)),
                );
        } else {
            let hover_fg = to_hsla(p.ink);
            tile = tile
                .text_color(to_hsla(p.ink_2))
                .hover(move |s| s.bg(hover_bg).text_color(hover_fg));
        }
        tile = tile.child(icon(module_icon(module)));
        let mini_color = if waiting > 0 {
            Some(p.warn)
        } else if prov_attn > 0 {
            Some(p.bad)
        } else if live_mocks > 0 || live_flows > 0 || gw_live {
            Some(p.ok)
        } else {
            None
        };
        if let Some(col) = mini_color {
            tile = tile.child(
                div()
                    .absolute()
                    .top(px(4.0))
                    .right(px(4.0))
                    .size(px(6.0))
                    .rounded(px(99.0))
                    .bg(to_hsla(col)),
            );
        }
        return tile;
    }

    let mut row = list_row(
        id,
        selected,
        cx,
        cx.listener(move |this, _, _, cx| {
            this.navigate(module, cx);
        }),
    )
    .w_full()
    .justify_between()
    .child(icon(module_icon(module)))
    .child(div().flex_1().min_w_0().text_xs().truncate().child(label));

    if waiting > 0 {
        let txt = if zh {
            format!("{waiting} 待批")
        } else {
            waiting.to_string()
        };
        row = row.child(badge(txt, Tone::Warning, cx));
    } else if live_mocks > 0 {
        row = row.child(
            h_row()
                .gap(px(4.0))
                .child(status_indicator(StatusShape::Live, cx))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.ok))
                        .child(live_mocks.to_string()),
                ),
        );
    } else if live_flows > 0 {
        row = row.child(status_indicator(StatusShape::Run, cx));
    } else if prov_attn > 0 {
        row = row.child(status_indicator(StatusShape::Bad, cx));
    } else if gw_live {
        row = row.child(status_indicator(StatusShape::Live, cx));
    }
    row
}
