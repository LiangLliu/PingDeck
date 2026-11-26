//! Collapsible navigation sidebar sitting cleanly beneath the unified 38px top bar:
//! native anchored workspace popover, `⌘K` quick jump, 2-pillar navigation (API Workspace +
//! Shared AI) with live status badges, and Running Work / Gateway / Settings footer.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::state::NavModule;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_icon, h_row, icon, icon_xs, kbd_chip, list_row, palette,
    section_heading, status_indicator, to_hsla, tr, v_col,
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
        NavModule::Gateway => UiIcon::Zap,
        NavModule::Usage => UiIcon::Usage,
        NavModule::Settings => UiIcon::Settings,
    }
}

pub fn render_sidebar(shell: &AppShell, cx: &mut Context<AppShell>) -> impl IntoElement {
    let p = palette(cx);
    let sh = tr(cx).shell();
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
        .unwrap_or_else(|| sh.workspace_default_name().to_owned());

    let mut top = v_col().w_full().gap(px(6.0)).pb(px(6.0)).flex_shrink_0();
    if !collapsed {
        top = top
            .child(render_workspace_popover(shell, false, active_ws_name, cx))
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
                            .child(sh.sidebar_search_placeholder()),
                    )
                    .child(kbd_chip("⌘K", cx)),
            );
    } else {
        top = top
            .items_center()
            .child(render_workspace_popover(shell, true, active_ws_name, cx))
            .child(
                btn_icon("sidebar-palette-rail", UiIcon::Search)
                    .tooltip(sh.sidebar_search_tooltip())
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_palette(window, cx))),
            );
    }

    let mut nav = v_col().flex_1().min_h_0().w_full().gap(px(2.0)).pt(px(2.0));
    if collapsed {
        nav = nav.items_center();
    }
    for module in NavModule::WORKSPACE_MODULES {
        nav = nav.child(render_nav_item(shell, module, collapsed, cx));
    }

    if !collapsed {
        nav = nav.child(
            div()
                .mt(px(8.0))
                .pt(px(10.0))
                .pb(px(4.0))
                .px(px(10.0))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.canvas)))
                .child(section_heading(sh.sidebar_ai_shared(), cx)),
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
        nav = nav.child(render_nav_item(shell, module, collapsed, cx));
    }

    let work = &shell.state.running_work;
    let active_jobs = work.agent_runs
        + work.subagents
        + work.flow_runs
        + work.collection_runs
        + work.mock_servers;
    let work_text = if shell.state.attention.waiting_approval > 0 {
        sh.sidebar_work_waiting(shell.state.attention.waiting_approval, active_jobs)
    } else if active_jobs == 0 {
        sh.sidebar_work_idle().to_owned()
    } else {
        sh.sidebar_work_running(active_jobs)
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
                cx,
            ))
            .child(div().w_full().pt(px(2.0)).child(render_nav_item(
                shell,
                NavModule::Settings,
                false,
                cx,
            )));
    } else {
        footer = footer
            .items_center()
            .gap(px(4.0))
            .child(render_running_work_popover(
                shell,
                true,
                active_jobs,
                work_text,
                cx,
            ))
            .child(render_nav_item(shell, NavModule::Settings, true, cx));
    }

    v_col()
        .w(width)
        .h_full()
        .flex_shrink_0()
        .bg(to_hsla(p.canvas))
        .px(px(8.0))
        .pb(px(8.0))
        .child(top)
        .child(nav)
        .child(footer)
}

fn render_nav_item(
    shell: &AppShell,
    module: NavModule,
    collapsed: bool,
    cx: &mut Context<AppShell>,
) -> impl IntoElement {
    let p = palette(cx);
    let t = tr(cx);
    let selected = shell.state.active_module == module;
    let label = module.label(t.lang());
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
    let gw_port = if module == NavModule::Gateway {
        shell.state.gateway_port
    } else {
        None
    };

    let id = SharedString::from(format!("nav-{}", module.id_slug()));
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
        } else if live_mocks > 0 || live_flows > 0 || gw_port.is_some() {
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
    .child(
        div()
            .flex_shrink_0()
            .text_color(to_hsla(if selected { p.accent_ink } else { p.muted }))
            .child(icon(module_icon(module))),
    )
    .child(
        div()
            .flex_1()
            .min_w_0()
            .text_size(px(13.0))
            .font_weight(if selected {
                gpui_kit::FontWeight::SEMIBOLD
            } else {
                gpui_kit::FontWeight::MEDIUM
            })
            .truncate()
            .child(label),
    );

    if waiting > 0 {
        row = row.child(badge(
            t.shell().sidebar_badge_waiting(waiting),
            Tone::Warning,
            cx,
        ));
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
    } else if let Some(port) = gw_port {
        row = row.child(
            h_row()
                .gap(px(4.0))
                .child(status_indicator(StatusShape::Live, cx))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.ok))
                        .child(format!(":{port}")),
                ),
        );
    }
    row
}
