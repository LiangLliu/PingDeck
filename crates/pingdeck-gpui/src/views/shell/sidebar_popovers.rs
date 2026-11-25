//! Anchored sidebar popovers for `AppShell`:
//! - Workspace Switcher Popover (expanded pill & collapsed-rail colored avatar trigger)
//! - Running Work & Attention Popover (pending approvals, live mock ports, flows, provider health).

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::state::{NavModule, UtilityDialog};

use crate::ui::{
    Anchor, Popover, StatusShape, Tone, UiIcon, badge, btn_ghost, btn_primary, h_row, icon_xs,
    list_row, meta_pill, palette, pill_btn, popover, section_heading, status_indicator, text_input,
    to_hsla, v_col,
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
    zh: bool,
    cx: &mut Context<AppShell>,
) -> Popover {
    let workspaces_snapshot = shell.state.workspaces.clone();
    let active_ws_id = shell.state.active_workspace_id.clone();
    let new_ws_input = shell.new_workspace_input.clone();
    let ws_counts = shell.workspace_counts.clone();
    let shell_entity = cx.entity().clone();

    let trigger_el = if collapsed {
        pill_btn(
            "sidebar-workspace-rail",
            SharedString::from(workspace_avatar_char(&active_ws_name)),
            None,
            true,
        )
        .w(px(34.0))
        .justify_center()
    } else {
        pill_btn(
            "sidebar-workspace-trigger",
            SharedString::from(active_ws_name),
            Some(UiIcon::FolderOpen),
            false,
        )
        .w_full()
        .justify_between()
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
        let mut menu = v_col().w(px(252.0)).gap(px(4.0));
        menu = menu.child(div().px(px(6.0)).pb(px(2.0)).child(section_heading(
            if zh { "工作区" } else { "Workspaces" },
            pop_cx,
        )));

        for ws in &workspaces_snapshot {
            let ws_id = ws.id.as_str().to_owned();
            let selected = active_ws_id.as_deref() == Some(ws.id.as_str());
            let name = ws.name.clone();
            let count_sub = ws_counts.get(&ws_id).map(|(reqs, mocks)| {
                if zh {
                    format!("{reqs} 请求 · {mocks} Mock")
                } else {
                    format!("{reqs} req · {mocks} mock")
                }
            });
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
                        btn_primary("ws-create-ok", if zh { "建" } else { "OK" }).on_click(
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
        menu
    })
}

pub fn render_running_work_popover(
    shell: &AppShell,
    active_jobs: u32,
    work_text: String,
    zh: bool,
    cx: &mut Context<AppShell>,
) -> Popover {
    let approvals = shell.state.attention.approvals.clone();
    let mocks = shell.running_mock_summary.clone();
    let work = shell.state.running_work;
    let prov_attn = shell.provider_attention_count;
    let shell_ent = cx.entity().clone();

    let has_attention = !approvals.is_empty() || prov_attn > 0;
    let trigger_icon = if has_attention {
        UiIcon::Warning
    } else if active_jobs > 0 {
        UiIcon::Activity
    } else {
        UiIcon::Terminal
    };
    let trigger = btn_ghost("footer-running", SharedString::from(work_text))
        .icon(icon_xs(trigger_icon))
        .w_full()
        .justify_start();

    popover("footer-running-popover")
        .anchor(Anchor::BottomLeft)
        .trigger(trigger)
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut body = v_col().w(px(286.0)).gap(px(6.0)).child(section_heading(
                if zh {
                    "后台任务与待处理"
                } else {
                    "Running Work & Attention"
                },
                pop_cx,
            ));

            let mut any_row = false;

            // 1. Pending Chat approvals
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
                    .child(badge(
                        if zh { "待批" } else { "Gate" },
                        Tone::Warning,
                        pop_cx,
                    )),
                );
            }

            // 2. Running Mock servers
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

            // 3. Running Flows / Collection runs / Agent runs
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
                    .child(div().text_xs().child(if zh {
                        format!("运行中的流程 ({})", work.flow_runs)
                    } else {
                        format!("Running flows ({})", work.flow_runs)
                    }))
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
                    .child(div().text_xs().child(if zh {
                        format!("运行中的智能体对话 ({})", work.agent_runs)
                    } else {
                        format!("Active agent runs ({})", work.agent_runs)
                    }))
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
                    .child(div().text_xs().child(if zh {
                        format!("{prov_attn} 个 AI 服务商需关注")
                    } else {
                        format!("{prov_attn} provider(s) need attention")
                    }))
                    .child(badge(
                        if zh { "异常" } else { "Check" },
                        Tone::Danger,
                        pop_cx,
                    )),
                );
            }

            if !any_row {
                body = body.child(
                    div()
                        .px(px(6.0))
                        .py(px(8.0))
                        .text_xs()
                        .text_color(to_hsla(pop_p.muted))
                        .child(if zh {
                            "当前没有运行中的后台任务或待审批项。"
                        } else {
                            "No running tasks or pending approvals."
                        }),
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
                btn_ghost(
                    "rw-open-console",
                    if zh {
                        "打开诊断控制台 →"
                    } else {
                        "Open Diagnostics Console →"
                    },
                )
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
