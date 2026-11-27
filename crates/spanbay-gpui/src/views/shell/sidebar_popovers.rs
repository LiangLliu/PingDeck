//! Anchored sidebar popovers for `AppShell`:
//! - Workspace Switcher Popover (expanded pill & collapsed-rail colored avatar trigger)
//! - Running Work & Attention Popover (pending approvals, live mock ports, flows, provider health).

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::state::{NavModule, UtilityDialog};

use crate::ui::{
    Anchor, Popover, ScrollableElement as _, StatusShape, Tone, UiIcon, badge, h_row, icon_xs,
    menu_divider, menu_item, menu_option, meta_pill, palette, popover, popover_full_trigger,
    popover_trigger_btn, section_heading, status_indicator, to_hsla, tr, v_col,
};

use super::AppShell;

fn workspace_avatar_char(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "W".to_owned())
}

fn render_ws_avatar(name: &str, size: f32, active: bool, cx: &gpui_kit::App) -> gpui_kit::Div {
    let p = palette(cx);
    let (bg, fg) = if active {
        (p.accent, p.on_accent)
    } else {
        (p.sunken, p.ink_2)
    };
    let mut avatar = div()
        .size(px(size))
        .rounded(px(6.0))
        .bg(to_hsla(bg))
        .text_color(to_hsla(fg))
        .text_size(px(11.0))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .child(workspace_avatar_char(name));
    if !active {
        avatar = avatar
            .border_1()
            .border_color(to_hsla(p.line_strong.composite_over(p.sunken)));
    }
    avatar
}

pub fn render_workspace_popover(
    shell: &AppShell,
    collapsed: bool,
    active_ws_name: String,
    cx: &mut Context<AppShell>,
) -> Popover {
    let workspaces_snapshot = shell.state.workspaces.clone();
    let active_ws_id = shell.state.active_workspace_id.clone();
    let ws_counts = shell.workspace_counts.clone();
    let shell_entity = cx.entity().clone();
    let sidebar_w = shell.state.sidebar_width.clamp(196.0, 260.0);
    // Keep expanded popover flush with the sidebar trigger pill (no overhang into adjacent panes).
    let content_w = if collapsed {
        228.0
    } else {
        (sidebar_w - 16.0).max(208.0) - 12.0
    };

    let p = palette(cx);
    let sh = tr(cx).shell();
    let trigger_el = if collapsed {
        popover_trigger_btn(
            "sidebar-workspace-rail",
            render_ws_avatar(&active_ws_name, 22.0, true, cx),
        )
        .size(px(34.0))
        .p(px(0.0))
        .tooltip(SharedString::from(sh.workspace_tooltip(&active_ws_name)))
    } else {
        popover_trigger_btn(
            "sidebar-workspace-trigger",
            h_row()
                .w_full()
                .gap(px(8.0))
                .child(render_ws_avatar(&active_ws_name, 20.0, true, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(SharedString::from(active_ws_name)),
                )
                .child(
                    div()
                        .text_color(to_hsla(p.faint))
                        .flex_shrink_0()
                        .child(icon_xs(UiIcon::ChevronDown)),
                ),
        )
        .w_full()
        .h(px(32.0))
        .px(px(8.0))
    };

    let pop = if collapsed {
        popover("sidebar-workspace-popover-rail")
    } else {
        popover_full_trigger("sidebar-workspace-popover")
    };

    pop.p(px(6.0))
        .rounded(px(10.0))
        .anchor(Anchor::TopLeft)
        .trigger(trigger_el)
        .content(move |_pop_state, _window, pop_cx| {
            let pop_p = palette(pop_cx);
            let sh = tr(pop_cx).shell();

            let mut ws_list = v_col()
                .w_full()
                .gap(px(2.0))
                .max_h(px(220.0))
                .overflow_y_scrollbar();

            for ws in &workspaces_snapshot {
                let ws_id = ws.id.as_str().to_owned();
                let selected = active_ws_id.as_deref() == Some(ws.id.as_str());
                let name = ws.name.clone();
                let (reqs, mocks) = ws_counts.get(&ws_id).copied().unwrap_or((0, 0));
                let count_sub = sh.workspace_counts(reqs, mocks);
                let shell_ref = shell_entity.clone();
                let pop_handle = pop_cx.entity().clone();
                ws_list = ws_list.child(
                    menu_option(
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
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .min_w_0()
                            .flex_1()
                            .child(render_ws_avatar(&name, 22.0, selected, pop_cx))
                            .child(
                                v_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(1.0))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(if selected {
                                                gpui_kit::FontWeight::SEMIBOLD
                                            } else {
                                                gpui_kit::FontWeight::MEDIUM
                                            })
                                            .text_color(to_hsla(if selected {
                                                pop_p.accent_ink
                                            } else {
                                                pop_p.ink
                                            }))
                                            .truncate()
                                            .child(SharedString::from(name)),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .font_weight(gpui_kit::FontWeight::NORMAL)
                                            .text_color(to_hsla(pop_p.muted))
                                            .truncate()
                                            .child(count_sub),
                                    ),
                            ),
                    )
                    .children(selected.then(|| {
                        div()
                            .flex_shrink_0()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }

            let shell_new = shell_entity.clone();
            let pop_new = pop_cx.entity().clone();
            let shell_imp = shell_entity.clone();
            let pop_imp = pop_cx.entity().clone();

            v_col()
                .w(px(content_w))
                .gap(px(2.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .px(px(8.0))
                        .pt(px(3.0))
                        .pb(px(5.0))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(pop_p.muted))
                                .child(sh.workspace_list()),
                        )
                        .child(meta_pill(sh.workspace_local_tag(), pop_cx)),
                )
                .child(ws_list)
                .child(menu_divider(pop_cx))
                .child(menu_item(
                    "ws-new-trigger",
                    UiIcon::Plus,
                    sh.workspace_new_btn(),
                    false,
                    pop_cx,
                    move |_, window, cx| {
                        pop_new.update(cx, |st, cx| st.dismiss(window, cx));
                        shell_new.update(cx, |s, cx| s.open_new_workspace_modal(window, cx));
                    },
                ))
                .child(menu_item(
                    "ws-import-export",
                    UiIcon::Upload,
                    sh.workspace_import_export(),
                    false,
                    pop_cx,
                    move |_, window, cx| {
                        pop_imp.update(cx, |st, cx| st.dismiss(window, cx));
                        shell_imp.update(cx, |s, cx| {
                            s.open_utility(UtilityDialog::ImportExport, window, cx);
                        });
                    },
                ))
        })
}

pub fn render_running_work_popover(
    shell: &AppShell,
    collapsed: bool,
    active_jobs: u32,
    work_text: String,
    cx: &mut Context<AppShell>,
) -> Popover {
    let p = palette(cx);
    let approvals = shell.state.attention.approvals.clone();
    let mocks = shell.running_mock_summary.clone();
    let gw_port = shell.state.gateway_port;
    let work = shell.state.running_work;
    let prov_attn = shell.provider_attention_count;
    let shell_ent = cx.entity().clone();

    let has_attention = !approvals.is_empty() || prov_attn > 0;
    let trigger_shape = if has_attention {
        StatusShape::Warn
    } else if active_jobs > 0 {
        StatusShape::Run
    } else {
        StatusShape::Idle
    };
    let trigger = if collapsed {
        popover_trigger_btn("footer-running-rail", status_indicator(trigger_shape, cx))
            .size(px(34.0))
            .p(px(0.0))
            .tooltip(SharedString::from(work_text))
    } else {
        let count_meta = (active_jobs > 0).then(|| active_jobs.to_string());
        popover_trigger_btn(
            "footer-running",
            h_row()
                .w_full()
                .gap(px(8.0))
                .child(status_indicator(trigger_shape, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(if has_attention { p.warn } else { p.ink_2 }))
                        .truncate()
                        .child(SharedString::from(work_text)),
                )
                .children(count_meta.map(|m| {
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .child(m)
                })),
        )
        .w_full()
        .h(px(28.0))
        .px(px(10.0))
    };

    let pop = if collapsed {
        popover("footer-running-popover-rail")
    } else {
        popover_full_trigger("footer-running-popover")
    };

    pop.p(px(6.0))
        .rounded(px(10.0))
        .anchor(Anchor::BottomLeft)
        .trigger(trigger)
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let sh = tr(pop_cx).shell();
            let mut body = v_col().w(px(286.0)).gap(px(3.0)).child(
                div()
                    .px(px(8.0))
                    .pt(px(3.0))
                    .pb(px(4.0))
                    .child(section_heading(sh.running_heading(), pop_cx)),
            );

            let mut any_row = false;

            for appr in &approvals {
                any_row = true;
                let sid = appr.session_id.clone();
                let s_ent = shell_ent.clone();
                let ph = pop_cx.entity().clone();
                body = body.child(
                    menu_option(
                        SharedString::from(format!("rw-appr-{}", appr.call_id)),
                        false,
                        pop_cx,
                        move |_, window, cx| {
                            s_ent.update(cx, |s, cx| {
                                s.navigate(NavModule::Chat, cx);
                                s.modules.open_chat_session_mounted(sid.clone(), window, cx);
                            });
                            ph.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .child(appr.summary.clone()),
                    )
                    .child(badge(
                        sh.running_gate_badge(),
                        Tone::Warning,
                        pop_cx,
                    )),
                );
            }

            if let Some(port) = gw_port {
                any_row = true;
                let s_ent = shell_ent.clone();
                let ph = pop_cx.entity().clone();
                body = body.child(
                    menu_option("rw-gateway", false, pop_cx, move |_, window, cx| {
                        s_ent.update(cx, |s, cx| s.navigate(NavModule::Gateway, cx));
                        ph.update(cx, |st, cx| st.dismiss(window, cx));
                    })
                    .justify_between()
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .min_w_0()
                            .flex_1()
                            .child(status_indicator(StatusShape::Live, pop_cx))
                            .child(div().text_xs().truncate().child(sh.running_gateway_live())),
                    )
                    .child(meta_pill(format!(":{port}"), pop_cx)),
                );
            }

            for (mid, mname, port) in &mocks {
                any_row = true;
                let mid_go = mid.clone();
                let s_ent = shell_ent.clone();
                let ph = pop_cx.entity().clone();
                body = body.child(
                    menu_option(
                        SharedString::from(format!("rw-mock-{mid}")),
                        false,
                        pop_cx,
                        move |_, window, cx| {
                            s_ent.update(cx, |s, cx| {
                                let ws = s.state.active_workspace_id.clone();
                                s.navigate(NavModule::Mock, cx);
                                s.modules.open_mock_server(ws, mid_go.clone(), window, cx);
                            });
                            ph.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .min_w_0()
                            .flex_1()
                            .child(status_indicator(StatusShape::Live, pop_cx))
                            .child(div().text_xs().truncate().child(mname.clone())),
                    )
                    .child(meta_pill(format!(":{port}"), pop_cx)),
                );
            }

            if work.flow_runs > 0 {
                any_row = true;
                let s_ent = shell_ent.clone();
                let ph = pop_cx.entity().clone();
                body = body.child(
                    menu_option("rw-flows", false, pop_cx, move |_, window, cx| {
                        s_ent.update(cx, |s, cx| s.navigate(NavModule::Flows, cx));
                        ph.update(cx, |st, cx| st.dismiss(window, cx));
                    })
                    .justify_between()
                    .child(div().text_xs().child(sh.running_flows(work.flow_runs)))
                    .child(status_indicator(StatusShape::Run, pop_cx)),
                );
            }
            if work.agent_runs > 0 {
                any_row = true;
                let s_ent = shell_ent.clone();
                let ph = pop_cx.entity().clone();
                body = body.child(
                    menu_option("rw-agents", false, pop_cx, move |_, window, cx| {
                        s_ent.update(cx, |s, cx| s.navigate(NavModule::Runs, cx));
                        ph.update(cx, |st, cx| st.dismiss(window, cx));
                    })
                    .justify_between()
                    .child(div().text_xs().child(sh.running_agents(work.agent_runs)))
                    .child(status_indicator(StatusShape::Run, pop_cx)),
                );
            }
            if prov_attn > 0 {
                any_row = true;
                let s_ent = shell_ent.clone();
                let ph = pop_cx.entity().clone();
                body = body.child(
                    menu_option("rw-prov-attn", false, pop_cx, move |_, window, cx| {
                        s_ent.update(cx, |s, cx| s.navigate(NavModule::Providers, cx));
                        ph.update(cx, |st, cx| st.dismiss(window, cx));
                    })
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .child(sh.running_providers_attention(prov_attn)),
                    )
                    .child(badge(
                        sh.running_check_badge(),
                        Tone::Danger,
                        pop_cx,
                    )),
                );
            }

            if !any_row {
                body = body.child(
                    div()
                        .px(px(8.0))
                        .py(px(8.0))
                        .text_xs()
                        .text_color(to_hsla(pop_p.muted))
                        .child(sh.running_empty()),
                );
            }

            let s_con = shell_ent.clone();
            let ph_con = pop_cx.entity().clone();
            body.child(menu_divider(pop_cx)).child(menu_item(
                "rw-open-console",
                UiIcon::Terminal,
                sh.running_open_console(),
                false,
                pop_cx,
                move |_, window, cx| {
                    s_con.update(cx, |s, cx| {
                        s.open_diagnostics(window, cx);
                    });
                    ph_con.update(cx, |st, cx| st.dismiss(window, cx));
                },
            ))
        })
}
