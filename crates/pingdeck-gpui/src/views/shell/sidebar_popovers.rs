//! Anchored sidebar popovers for `AppShell`:
//! - Workspace Switcher Popover (expanded pill & collapsed-rail colored avatar trigger)
//! - Running Work & Attention Popover (pending approvals, live mock ports, flows, provider health).

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::state::{NavModule, UtilityDialog};

use crate::ui::{
    Anchor, Popover, StatusShape, Tone, UiIcon, badge, btn_ghost, btn_primary, h_row, icon_xs,
    list_row, meta_pill, palette, popover, popover_trigger_btn, section_heading, status_indicator,
    text_input, to_hsla, tr, v_col,
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
    let bg = if active { p.accent } else { p.accent_soft };
    let fg = if active { p.on_accent } else { p.accent_ink };
    div()
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
        .child(workspace_avatar_char(name))
}

pub fn render_workspace_popover(
    shell: &AppShell,
    collapsed: bool,
    active_ws_name: String,
    cx: &mut Context<AppShell>,
) -> Popover {
    let workspaces_snapshot = shell.state.workspaces.clone();
    let active_ws_id = shell.state.active_workspace_id.clone();
    let new_ws_input = shell.new_workspace_input.clone();
    let ws_counts = shell.workspace_counts.clone();
    let shell_entity = cx.entity().clone();

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

    popover(if collapsed {
        "sidebar-workspace-popover-rail"
    } else {
        "sidebar-workspace-popover"
    })
    .anchor(Anchor::TopLeft)
    .trigger(trigger_el)
    .content(move |_pop_state, _window, pop_cx| {
        let pop_p = palette(pop_cx);
        let sh = tr(pop_cx).shell();
        let mut menu = v_col().w(px(252.0)).gap(px(4.0));
        menu = menu.child(
            div()
                .px(px(6.0))
                .pb(px(2.0))
                .child(section_heading(sh.workspace_list(), pop_cx)),
        );

        for ws in &workspaces_snapshot {
            let ws_id = ws.id.as_str().to_owned();
            let selected = active_ws_id.as_deref() == Some(ws.id.as_str());
            let name = ws.name.clone();
            let count_sub = ws_counts
                .get(&ws_id)
                .map(|(reqs, mocks)| sh.workspace_counts(reqs, mocks));
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
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .truncate()
                                        .child(SharedString::from(name)),
                                )
                                .children(count_sub.map(|sub| {
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(to_hsla(pop_p.muted))
                                        .truncate()
                                        .child(sub)
                                })),
                        ),
                )
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
                        btn_primary("ws-create-ok", sh.workspace_create_short()).on_click(
                            move |_, window, cx| {
                                shell_ok.update(cx, |s, cx| {
                                    s.submit_create_workspace(cx);
                                });
                                pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                            },
                        ),
                    ),
            );
        } else {
            let shell_new = shell_entity.clone();
            menu = menu.child(
                btn_ghost("ws-new-trigger", sh.workspace_new_btn())
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
            btn_ghost("ws-import-export", sh.workspace_import_export())
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
        menu
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

    popover(if collapsed {
        "footer-running-popover-rail"
    } else {
        "footer-running-popover"
    })
    .anchor(Anchor::BottomLeft)
    .trigger(trigger)
    .content(move |_, _, pop_cx| {
        let pop_p = palette(pop_cx);
        let sh = tr(pop_cx).shell();
        let mut body = v_col()
            .w(px(286.0))
            .gap(px(6.0))
            .child(section_heading(sh.running_heading(), pop_cx));

        let mut any_row = false;

        for appr in &approvals {
            any_row = true;
            let sid = appr.session_id.clone();
            let s_ent = shell_ent.clone();
            let ph = pop_cx.entity().clone();
            body = body.child(
                list_row(
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
                .child(badge(sh.running_gate_badge(), Tone::Warning, pop_cx)),
            );
        }

        for (mid, mname, port) in &mocks {
            any_row = true;
            let mid_go = mid.clone();
            let s_ent = shell_ent.clone();
            let ph = pop_cx.entity().clone();
            body = body.child(
                list_row(
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
                list_row("rw-flows", false, pop_cx, move |_, window, cx| {
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
                list_row("rw-agents", false, pop_cx, move |_, window, cx| {
                    s_ent.update(cx, |s, cx| s.navigate(NavModule::Chat, cx));
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
                list_row("rw-prov-attn", false, pop_cx, move |_, window, cx| {
                    s_ent.update(cx, |s, cx| s.navigate(NavModule::Providers, cx));
                    ph.update(cx, |st, cx| st.dismiss(window, cx));
                })
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .child(sh.running_providers_attention(prov_attn)),
                )
                .child(badge(sh.running_check_badge(), Tone::Danger, pop_cx)),
            );
        }

        if !any_row {
            body = body.child(
                div()
                    .px(px(6.0))
                    .py(px(8.0))
                    .text_xs()
                    .text_color(to_hsla(pop_p.muted))
                    .child(sh.running_empty()),
            );
        }

        let s_con = shell_ent.clone();
        let ph_con = pop_cx.entity().clone();
        body.child(
            div()
                .my(px(2.0))
                .h(px(1.0))
                .bg(to_hsla(pop_p.line.composite_over(pop_p.overlay))),
        )
        .child(
            btn_ghost("rw-open-console", sh.running_open_console())
                .icon(icon_xs(UiIcon::Terminal))
                .w_full()
                .justify_start()
                .on_click(move |_, window, cx| {
                    s_con.update(cx, |s, cx| {
                        s.open_utility(UtilityDialog::Console, window, cx);
                    });
                    ph_con.update(cx, |st, cx| st.dismiss(window, cx));
                }),
        )
    })
}
