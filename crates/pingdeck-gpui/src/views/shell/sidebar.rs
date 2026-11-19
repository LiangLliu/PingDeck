//! Collapsible navigation sidebar aligned with the PingDeck desktop prototype:
//! integrated macOS traffic-light header, native anchored workspace popover, clean 2-pillar
//! navigation (API Workspace + Shared AI), geometric status indicators, and compact footer.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, WindowControlArea, div, px,
};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::state::{NavModule, UtilityDialog};

use crate::ui::{
    Anchor, StatusShape, Tone, UiIcon, app_icon, badge, btn_ghost, btn_icon, btn_primary, h_row,
    icon, icon_xs, kbd_chip, list_row, palette, pill_btn, popover, section_heading,
    status_indicator, text_input, to_hsla, v_col,
};

use super::AppShell;

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
        px(54.0)
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

    // 1. Top macOS traffic-light integrated header + window drag area.
    let top_header = if collapsed {
        v_col()
            .items_center()
            .pt(if cfg!(target_os = "macos") {
                px(36.0)
            } else {
                px(10.0)
            })
            .pb(px(6.0))
            .gap(px(6.0))
            .window_control_area(WindowControlArea::Drag)
            .child(app_icon(24.0))
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
        v_col().child(
            h_row()
                .h(px(40.0))
                .pl(if cfg!(target_os = "macos") {
                    px(76.0)
                } else {
                    px(12.0)
                })
                .pr(px(8.0))
                .justify_between()
                .window_control_area(WindowControlArea::Drag)
                .child(
                    h_row().gap(px(7.0)).child(app_icon(20.0)).child(
                        div()
                            .text_xs()
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
                ),
        )
    };

    // 2. Workspace switcher (native anchored Popover) + ⌘K quick jump trigger.
    let mut top = v_col().gap(px(6.0)).px(px(8.0)).pb(px(4.0));
    if !collapsed {
        let workspaces_snapshot = shell.state.workspaces.clone();
        let active_ws_id = shell.state.active_workspace_id.clone();
        let new_ws_input = shell.new_workspace_input.clone();
        let shell_entity = cx.entity().clone();

        top = top
            .child(
                popover("sidebar-workspace-popover")
                    .anchor(Anchor::TopLeft)
                    .trigger(
                        pill_btn(
                            "sidebar-workspace-trigger",
                            SharedString::from(active_ws_name),
                            Some(UiIcon::FolderOpen),
                            false,
                        )
                        .w_full()
                        .justify_between(),
                    )
                    .content(move |pop_state, _window, pop_cx| {
                        let pop_p = palette(pop_cx);
                        let mut menu = v_col().w(px(236.0)).gap(px(4.0));
                        menu = menu.child(div().px(px(6.0)).pb(px(2.0)).child(section_heading(
                            if zh { "工作区" } else { "Workspaces" },
                            pop_cx,
                        )));

                        for ws in &workspaces_snapshot {
                            let ws_id = ws.id.as_str().to_owned();
                            let selected = active_ws_id.as_deref() == Some(ws.id.as_str());
                            let name = ws.name.clone();
                            let shell_ref = shell_entity.clone();
                            let pop_handle = pop_cx.entity().clone();
                            menu = menu.child(
                                list_row(
                                    SharedString::from(format!("ws-pick-{ws_id}")),
                                    selected,
                                    pop_cx,
                                    move |_, window, cx| {
                                        shell_ref.update(cx, |s, cx| {
                                            s.switch_workspace(ws_id.clone(), cx);
                                        });
                                        pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                                    },
                                )
                                .justify_between()
                                .child(div().text_xs().truncate().child(SharedString::from(name)))
                                .children(selected.then(|| {
                                    div()
                                        .text_color(to_hsla(pop_p.accent))
                                        .child(icon_xs(UiIcon::Check))
                                })),
                            );
                        }

                        menu = menu.child(
                            div()
                                .my(px(2.0))
                                .h(px(1.0))
                                .bg(to_hsla(pop_p.line.composite_over(pop_p.overlay))),
                        );

                        if let Some(input) = &new_ws_input {
                            let shell_ok = shell_entity.clone();
                            let pop_handle = pop_cx.entity().clone();
                            menu = menu.child(
                                h_row()
                                    .gap(px(4.0))
                                    .child(div().flex_1().child(text_input(input)))
                                    .child(
                                        btn_primary("ws-create-ok", if zh { "建" } else { "OK" })
                                            .on_click(move |_, window, cx| {
                                                shell_ok.update(cx, |s, cx| {
                                                    s.submit_create_workspace(cx);
                                                });
                                                pop_handle
                                                    .update(cx, |st, cx| st.dismiss(window, cx));
                                            }),
                                    ),
                            );
                        } else {
                            let shell_new = shell_entity.clone();
                            menu = menu.child(
                                btn_ghost(
                                    "ws-new-trigger",
                                    if zh {
                                        "+ 新建工作区"
                                    } else {
                                        "+ New workspace"
                                    },
                                )
                                .w_full()
                                .justify_start()
                                .on_click(move |_, window, cx| {
                                    shell_new.update(cx, |s, cx| {
                                        s.start_create_workspace(window, cx);
                                    });
                                }),
                            );
                        }

                        let shell_imp = shell_entity.clone();
                        let pop_handle = pop_cx.entity().clone();
                        menu = menu.child(
                            btn_ghost(
                                "ws-import-export",
                                if zh {
                                    "导入 / 导出…"
                                } else {
                                    "Import / Export…"
                                },
                            )
                            .icon(icon_xs(UiIcon::Upload))
                            .w_full()
                            .justify_start()
                            .on_click(move |_, window, cx| {
                                shell_imp.update(cx, |s, cx| {
                                    s.open_utility(UtilityDialog::ImportExport, window, cx);
                                });
                                pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                            }),
                        );
                        let _ = pop_state;
                        menu
                    }),
            )
            .child(
                div()
                    .id("sidebar-palette-btn")
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(px(9.0))
                    .py(px(5.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.surface))
                    .border_1()
                    .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
                    .cursor_pointer()
                    .hover(|s| s.border_color(to_hsla(p.accent_line.composite_over(p.surface))))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_palette(window, cx)))
                    .child(
                        h_row()
                            .gap(px(6.0))
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
        top = top.items_center().child(
            btn_icon("sidebar-palette-rail", UiIcon::Search)
                .tooltip("⌘K")
                .on_click(cx.listener(|this, _, window, cx| this.toggle_palette(window, cx))),
        );
    }

    // 3. Clean 2-pillar navigation: Workspace API modules + Shared AI modules.
    let mut nav = v_col().flex_1().gap(px(2.0)).px(px(8.0)).pt(px(6.0));
    for module in NavModule::WORKSPACE_MODULES {
        nav = nav.child(render_nav_item(shell, module, collapsed, zh, cx));
    }

    if !collapsed {
        nav = nav.child(
            div()
                .px(px(10.0))
                .pt(px(16.0))
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
                .my(px(8.0))
                .mx(px(6.0))
                .h(px(1.0))
                .bg(to_hsla(p.line.composite_over(p.canvas))),
        );
    }
    for module in NavModule::AI_MODULES {
        nav = nav.child(render_nav_item(shell, module, collapsed, zh, cx));
    }

    // 4. Footer: Running jobs + Gateway status + Settings & quick theme/console toggles.
    let work = &shell.state.running_work;
    let active_jobs = work.agent_runs
        + work.subagents
        + work.flow_runs
        + work.collection_runs
        + work.mock_servers;
    let gw_running = shell.state.gateway_port.is_some();
    let gw_text = match (shell.state.gateway_port, zh) {
        (Some(port), true) => format!("网关 :{port}"),
        (Some(port), false) => format!("Gateway :{port}"),
        (None, true) => "网关已停止".into(),
        (None, false) => "Gateway off".into(),
    };
    let work_text = if active_jobs == 0 {
        if zh {
            "无后台任务".into()
        } else {
            "Idle".into()
        }
    } else if zh {
        format!("{active_jobs} 项运行中")
    } else {
        format!("{active_jobs} running")
    };

    let mut footer = v_col()
        .gap(px(2.0))
        .px(px(8.0))
        .py(px(8.0))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.canvas)));

    if !collapsed {
        footer = footer
            .child(
                div()
                    .id("footer-running")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.canvas))))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_utility(UtilityDialog::Console, window, cx);
                    }))
                    .child(status_indicator(
                        if active_jobs > 0 {
                            StatusShape::Run
                        } else {
                            StatusShape::Idle
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(work_text)),
                    ),
            )
            .child(
                div()
                    .id("footer-gateway")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.canvas))))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.navigate(NavModule::Providers, cx);
                    }))
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
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(SharedString::from(gw_text)),
                    ),
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
                            .tooltip(if zh { "控制台" } else { "Console" })
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
        footer =
            footer
                .items_center()
                .child(render_nav_item(shell, NavModule::Settings, true, zh, cx));
    }

    v_col()
        .w(width)
        .h_full()
        .flex_shrink_0()
        .bg(to_hsla(p.canvas))
        .child(top_header)
        .child(top)
        .child(nav)
        .child(footer)
}

fn render_nav_item(
    shell: &AppShell,
    module: NavModule,
    collapsed: bool,
    zh: bool,
    cx: &mut Context<AppShell>,
) -> impl IntoElement {
    let selected = shell.state.active_module == module;
    let (zh_label, en_label) = module.label_pair();
    let label = if zh { zh_label } else { en_label };
    let waiting = if module == NavModule::Chat {
        shell.state.attention.waiting_approval
    } else {
        0
    };

    let mut row = list_row(
        SharedString::from(format!("nav-{}", module.shortcut_digit())),
        selected,
        cx,
        cx.listener(move |this, _, _, cx| {
            this.navigate(module, cx);
        }),
    )
    .child(icon(module_icon(module)));

    if !collapsed {
        row = row
            .justify_between()
            .child(div().flex_1().text_xs().truncate().child(label));
        if waiting > 0 {
            row = row.child(badge(waiting.to_string(), Tone::Warning, cx));
        }
    } else {
        row = row.justify_center().px(px(6.0));
    }
    row
}
