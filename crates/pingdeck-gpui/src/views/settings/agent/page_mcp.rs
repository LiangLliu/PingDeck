//! Settings › 智能体 › MCP 服务器 (`SettingsSection::Mcp`, prototype `ai/mcp`).

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div, px,
};
use pingdeck_desktop::core::agent::mcp::{
    AgentMcpServerView, McpServerStatus, McpTransportConfig, McpTrustState,
};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::i18n::{t, t_args};

use crate::ui::{
    Disableable as _, ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_danger,
    btn_ghost, btn_icon, btn_primary, btn_secondary, card, empty_state, h_row, icon_xs,
    markdown_view, palette, status_indicator, to_hsla, toggle_switch, v_col,
};

use super::AgentSettingsView;
use super::widgets::section_head;

pub(super) fn render_mcp(
    view: &AgentSettingsView,
    lang: Language,
    zh: bool,
    _window: &mut Window,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let servers = view
        .config
        .as_ref()
        .map(|c| c.mcp_servers.as_slice())
        .unwrap_or(&[]);

    let toolbar = h_row()
        .gap(px(8.0))
        .flex_wrap()
        .child(
            btn_primary(
                "mcp-add-btn",
                if zh {
                    "+ 添加 MCP 服务器"
                } else {
                    "+ Add MCP server"
                },
            )
            .on_click(cx.listener(|this, _, window, cx| this.open_mcp_add_sheet(window, cx))),
        )
        .child(
            btn_secondary("mcp-json-btn", if zh { "编辑 JSON…" } else { "Edit JSON…" })
                .icon(icon_xs(UiIcon::FileCode))
                .on_click(cx.listener(|this, _, window, cx| this.open_mcp_json_sheet(window, cx))),
        )
        .child(
            btn_ghost("mcp-refresh-all", if zh { "刷新" } else { "Refresh" })
                .icon(icon_xs(UiIcon::Refresh))
                .on_click(cx.listener(|this, _, _, cx| this.reload_all(cx))),
        );

    let header = section_head(
        if zh {
            "外部工具服务器"
        } else {
            "External tool servers"
        },
        None,
        cx,
    );
    let desc = div()
        .text_xs()
        .text_color(to_hsla(p.muted))
        .child(t(lang, "set.sec.mcp.mcp.d").to_owned());

    if servers.is_empty() {
        return v_col()
            .gap(px(12.0))
            .child(header)
            .child(desc)
            .child(toolbar)
            .child(card(cx).h(px(190.0)).child(empty_state(
                UiIcon::Server,
                t(lang, "set.tools.mcp.empty").to_owned(),
                t(lang, "set.tools.mcp.empty.d").to_owned(),
                cx,
            )));
    }

    let table_head = h_row()
        .px(px(12.0))
        .py(px(8.0))
        .gap(px(10.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .text_xs()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(to_hsla(p.muted))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(if zh { "服务器" } else { "Server" }),
        )
        .child(
            div()
                .w(px(76.0))
                .flex_shrink_0()
                .child(if zh { "传输" } else { "Transport" }),
        )
        .child(
            div()
                .w(px(180.0))
                .flex_shrink_0()
                .child(if zh { "状态" } else { "Status" }),
        )
        .child(
            div()
                .w(px(172.0))
                .flex_shrink_0()
                .text_right()
                .child(if zh {
                    "启用 / 操作"
                } else {
                    "Enable / Actions"
                }),
        );

    let mut rows = v_col().child(table_head);
    let total = servers.len();
    for (idx, srv_view) in servers.iter().enumerate() {
        rows = rows.child(render_mcp_row(
            view,
            srv_view,
            idx,
            idx + 1 < total,
            lang,
            zh,
            cx,
        ));
    }

    v_col()
        .gap(px(12.0))
        .child(header)
        .child(desc)
        .child(toolbar)
        .child(card(cx).overflow_hidden().child(rows))
}

fn render_mcp_row(
    view: &AgentSettingsView,
    sv: &AgentMcpServerView,
    idx: usize,
    bottom_border: bool,
    lang: Language,
    zh: bool,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let srv = &sv.server;
    let sid = srv.id.clone();

    let (transport_tag, transport_target, is_stdio) = match &srv.transport {
        McpTransportConfig::Stdio { command, args, .. } => {
            let mut full = command.clone();
            if !args.is_empty() {
                full.push(' ');
                full.push_str(&args.join(" "));
            }
            ("stdio", full, true)
        }
        McpTransportConfig::Http { url, .. } => ("HTTP", url.clone(), false),
    };

    let (st_shape, st_text, st_fg) = format_mcp_status(sv, lang, zh, &p);
    let restarting = view.mcp_restarting_id.as_deref() == Some(sid.as_str());
    let confirming_start = view.mcp_confirm_start_id.as_deref() == Some(sid.as_str());
    let confirming_delete = view.mcp_confirm_delete_id.as_deref() == Some(sid.as_str());
    let log_open = view.mcp_log_open_id.as_deref() == Some(sid.as_str());

    let trust_badge = match sv.trust_state {
        Some(McpTrustState::Trusted) => badge(
            t(lang, "set.tools.mcp.trusted").to_owned(),
            Tone::Success,
            cx,
        ),
        Some(McpTrustState::NeedsReview) => badge(
            t(lang, "set.tools.mcp.drifted").to_owned(),
            Tone::Warning,
            cx,
        ),
        _ => badge(
            t(lang, "set.tools.mcp.untrusted").to_owned(),
            Tone::Neutral,
            cx,
        ),
    };

    let name_col = v_col()
        .flex_1()
        .min_w_0()
        .gap(px(2.0))
        .child(
            h_row()
                .gap(px(6.0))
                .flex_wrap()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(srv.name.clone())),
                )
                .child(trust_badge)
                .children(is_stdio.then(|| {
                    badge(
                        t(lang, "set.tools.mcp.notSandboxed").to_owned(),
                        Tone::Neutral,
                        cx,
                    )
                })),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.faint))
                .truncate()
                .child(SharedString::from(transport_target.clone())),
        );

    let status_col = h_row()
        .w(px(180.0))
        .gap(px(5.0))
        .child(status_indicator(
            if restarting {
                StatusShape::Run
            } else {
                st_shape
            },
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(st_fg)
                .child(SharedString::from(st_text)),
        );

    let srv_toggle = srv.clone();
    let srv_edit = srv.clone();
    let sid_restart = sid.clone();
    let sid_review = sid.clone();
    let sid_log = sid.clone();
    let sid_del = sid.clone();

    let needs_trust_btn = matches!(
        sv.trust_state,
        None | Some(McpTrustState::Untrusted | McpTrustState::NeedsReview)
    );
    let trust_btn_label = if sv.trust_state == Some(McpTrustState::NeedsReview) {
        t(lang, "set.tools.mcp.review").to_owned()
    } else {
        t(lang, "set.tools.mcp.trust").to_owned()
    };

    let actions_col = h_row()
        .w(px(172.0))
        .justify_end()
        .gap(px(4.0))
        .children(needs_trust_btn.then(|| {
            btn_ghost(
                SharedString::from(format!("mcp-trust-{idx}")),
                trust_btn_label,
            )
            .on_click(
                cx.listener(move |this, _, _, cx| this.review_mcp_server(sid_review.clone(), cx)),
            )
        }))
        .children(srv.enabled.then(|| {
            btn_icon(SharedString::from(format!("mcp-re-{idx}")), UiIcon::Refresh)
                .tooltip(if zh {
                    "重启服务器"
                } else {
                    "Restart server"
                })
                .disabled(restarting)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.restart_mcp_server(sid_restart.clone(), cx);
                }))
        }))
        .children(is_stdio.then(|| {
            btn_icon(
                SharedString::from(format!("mcp-log-{idx}")),
                UiIcon::Terminal,
            )
            .tooltip(if zh {
                "查看标准错误日志"
            } else {
                "View stderr log"
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_mcp_log(sid_log.clone(), cx);
            }))
        }))
        .child(
            btn_icon(
                SharedString::from(format!("mcp-edit-{idx}")),
                UiIcon::Pencil,
            )
            .tooltip(if zh { "编辑配置" } else { "Edit server" })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_mcp_edit_sheet(srv_edit.clone(), window, cx);
            })),
        )
        .child(
            btn_icon(SharedString::from(format!("mcp-del-{idx}")), UiIcon::Trash)
                .tooltip(if zh {
                    "删除服务器"
                } else {
                    "Delete server"
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.mcp_confirm_delete_id = Some(sid_del.clone());
                    cx.notify();
                })),
        )
        .child(
            toggle_switch(SharedString::from(format!("mcp-en-{idx}")), srv.enabled).on_change(
                cx.listener(move |this, checked: &bool, _, cx| {
                    this.toggle_mcp_enabled(srv_toggle.clone(), *checked, false, cx);
                }),
            ),
        );

    let mut wrapper = v_col().child(
        h_row()
            .px(px(12.0))
            .py(px(10.0))
            .gap(px(10.0))
            .child(name_col)
            .child(
                div()
                    .w(px(76.0))
                    .flex_shrink_0()
                    .child(badge(transport_tag, Tone::Neutral, cx)),
            )
            .child(status_col)
            .child(actions_col),
    );

    if confirming_start {
        let srv_confirm = srv.clone();
        wrapper = wrapper.child(
            h_row()
                .justify_between()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(8.0))
                .bg(to_hsla(p.warn_soft))
                .border_t_1()
                .border_color(to_hsla(p.warn_line.composite_over(p.warn_soft)))
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.warn))
                                .child(SharedString::from(t_args(
                                    lang,
                                    "set.tools.mcp.start.title",
                                    &[("name", srv.name.as_str())],
                                ))),
                        )
                        .child(div().text_xs().text_color(to_hsla(p.ink_2)).child(
                            SharedString::from(format!(
                                "{} ({transport_target})",
                                t(lang, "set.tools.mcp.start.body")
                            )),
                        )),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            btn_ghost(
                                SharedString::from(format!("mcp-start-cancel-{idx}")),
                                if zh { "取消" } else { "Cancel" },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mcp_confirm_start_id = None;
                                cx.notify();
                            })),
                        )
                        .child(
                            btn_primary(
                                SharedString::from(format!("mcp-start-go-{idx}")),
                                t(lang, "set.tools.mcp.start.confirm").to_owned(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.toggle_mcp_enabled(srv_confirm.clone(), true, true, cx);
                                },
                            )),
                        ),
                ),
        );
    }

    if confirming_delete {
        let sid_confirm_del = sid.clone();
        wrapper = wrapper.child(
            h_row()
                .justify_between()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(8.0))
                .bg(to_hsla(p.bad_soft))
                .border_t_1()
                .border_color(to_hsla(p.bad_line.composite_over(p.bad_soft)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.bad))
                        .child(SharedString::from(format!(
                            "{} — {}",
                            t_args(
                                lang,
                                "set.tools.mcp.delete.title",
                                &[("name", srv.name.as_str())]
                            ),
                            t(lang, "set.tools.mcp.delete.body")
                        ))),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            btn_ghost(
                                SharedString::from(format!("mcp-del-cancel-{idx}")),
                                if zh { "取消" } else { "Cancel" },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mcp_confirm_delete_id = None;
                                cx.notify();
                            })),
                        )
                        .child(
                            btn_danger(
                                SharedString::from(format!("mcp-del-go-{idx}")),
                                t(lang, "set.tools.mcp.delete").to_owned(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.delete_mcp_server(sid_confirm_del.clone(), cx);
                                },
                            )),
                        ),
                ),
        );
    }

    if log_open {
        let lines = view.mcp_logs.get(&sid).cloned().unwrap_or_default();
        let sid_ref = sid;
        let body = if lines.is_empty() {
            div()
                .p(px(10.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(t(lang, "set.tools.mcp.log.empty").to_owned())
                .into_any_element()
        } else {
            let joined = format!("````text\n{}\n````", lines.join("\n"));
            div()
                .id(SharedString::from(format!("mcp-log-scroll-{idx}")))
                .w_full()
                .max_h(px(200.0))
                .overflow_y_scrollbar()
                .px(px(10.0))
                .py(px(6.0))
                .child(markdown_view(
                    SharedString::from(format!("mcp-log-md-{idx}")),
                    joined,
                ))
                .into_any_element()
        };
        wrapper = wrapper.child(
            v_col()
                .bg(to_hsla(p.sunken))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.sunken)))
                .child(
                    h_row()
                        .justify_between()
                        .px(px(10.0))
                        .py(px(5.0))
                        .border_b_1()
                        .border_color(to_hsla(p.line.composite_over(p.sunken)))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(t(lang, "set.tools.mcp.log.d").to_owned()),
                        )
                        .child(
                            btn_ghost(
                                SharedString::from(format!("mcp-log-ref-{idx}")),
                                t(lang, "set.tools.mcp.log.refresh").to_owned(),
                            )
                            .icon(icon_xs(UiIcon::Refresh))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.refresh_mcp_log(sid_ref.clone(), cx);
                                },
                            )),
                        ),
                )
                .child(body),
        );
    }

    if bottom_border {
        wrapper = wrapper
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)));
    }
    wrapper
}

fn format_mcp_status(
    sv: &AgentMcpServerView,
    lang: Language,
    zh: bool,
    p: &pingdeck_desktop::theme::ResolvedPalette,
) -> (StatusShape, String, gpui_kit::Hsla) {
    match &sv.status {
        McpServerStatus::Running => {
            let txt = match sv.tool_count {
                Some(n) if zh => format!("已连接 · {n} 个工具"),
                Some(n) => format!("Connected · {n} tool(s)"),
                None => t(lang, "set.tools.mcp.state.running").to_owned(),
            };
            (StatusShape::Ok, txt, to_hsla(p.ink_2))
        }
        McpServerStatus::Starting => (
            StatusShape::Run,
            t(lang, "set.tools.mcp.state.starting").to_owned(),
            to_hsla(p.muted),
        ),
        McpServerStatus::NeedsAuth => (
            StatusShape::Warn,
            if zh {
                "需要处理：缺少令牌"
            } else {
                "Needs auth token"
            }
            .to_owned(),
            to_hsla(p.warn),
        ),
        McpServerStatus::NeedsReview => (
            StatusShape::Warn,
            if zh {
                "配置已变，待保存复核"
            } else {
                "Config changed — review"
            }
            .to_owned(),
            to_hsla(p.warn),
        ),
        McpServerStatus::Error { message } => (
            StatusShape::Bad,
            if message.is_empty() {
                t(lang, "set.tools.mcp.state.error").to_owned()
            } else {
                message.clone()
            },
            to_hsla(p.bad),
        ),
        McpServerStatus::Disabled => (
            StatusShape::Idle,
            t(lang, "set.tools.mcp.state.disabled").to_owned(),
            to_hsla(p.muted),
        ),
    }
}
