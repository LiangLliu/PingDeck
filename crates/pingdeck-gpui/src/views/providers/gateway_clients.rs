//! Gateway Clients tab: detected coding tools (inspect/connect/verify/restore),
//! issued client tokens (create/rotate/policy/revoke/delete), and managed config backups.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::VerifyOutcome;
use pingdeck_desktop::core::client_config::ClientStatus;
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_danger, btn_ghost, btn_icon, btn_primary, btn_secondary,
    card, h_row, icon_xs, kv_chip, palette, section_heading, status_indicator, text_input, to_hsla,
    v_col,
};

use super::ProvidersView;
use super::format::{format_client_status, format_relative_time};

pub fn render_clients_tab(
    view: &ProvidersView,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);

    let mut root = v_col().gap(px(16.0));

    // One-time revealed token banner (after creating or rotating a client token).
    if let Some((name, token)) = &view.revealed_client_token {
        let copy_tok = token.clone();
        root = root.child(
            card(cx)
                .p(px(12.0))
                .bg(to_hsla(p.ok_soft))
                .border_color(to_hsla(p.ok_line.composite_over(p.ok_soft)))
                .child(
                    v_col()
                        .gap(px(6.0))
                        .child(
                            h_row()
                                .justify_between()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ok))
                                        .child(SharedString::from(if zh {
                                            format!("已签发客户端「{name}」令牌（仅显示一次，请立即复制）")
                                        } else {
                                            format!("Token minted for \"{name}\" (shown once — copy now)")
                                        })),
                                )
                                .child(
                                    btn_icon("gw-hide-new-tok", UiIcon::Close).on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.revealed_client_token = None;
                                            cx.notify();
                                        }),
                                    ),
                                ),
                        )
                        .child(
                            h_row()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .px(px(8.0))
                                        .py(px(5.0))
                                        .rounded(px(6.0))
                                        .bg(to_hsla(p.surface))
                                        .border_1()
                                        .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
                                        .text_xs()
                                        .text_color(to_hsla(p.ink))
                                        .truncate()
                                        .child(SharedString::from(token.clone())),
                                )
                                .child(
                                    btn_primary("gw-copy-new-tok", if zh { "复制令牌" } else { "Copy token" })
                                        .icon(icon_xs(UiIcon::Copy))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let _ = clipboard::set_text(&copy_tok);
                                            this.status_msg = Some((
                                                Tone::Success,
                                                if zh {
                                                    "令牌已复制到剪贴板".into()
                                                } else {
                                                    "Token copied".into()
                                                },
                                            ));
                                            cx.notify();
                                        })),
                                ),
                        ),
                ),
        );
    }

    // 1. Detected local coding tools (file writers).
    let mut tools_box = v_col().gap(px(8.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(
                if zh {
                    "本机 AI 编码助手（自动写入配置与备份）"
                } else {
                    "Local AI Coding Tools (Managed Configs)"
                },
                cx,
            ))
            .child(
                btn_primary(
                    "gw-clients-connect",
                    if zh {
                        "+ 接入编码工具…"
                    } else {
                        "+ Connect tool…"
                    },
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_connect_sheet(None, window, cx);
                })),
            ),
    );

    for det in &view.detected_clients {
        let app = det.app;
        let inspect = view.inspections.get(&app);
        let (st_lbl, st_tone) = inspect.map_or_else(
            || {
                if det.installed {
                    (if zh { "已安装" } else { "Installed" }, Tone::Neutral)
                } else {
                    (if zh { "未安装" } else { "Not installed" }, Tone::Neutral)
                }
            },
            |ins| format_client_status(&ins.status, &ins.inspection.drift, zh),
        );
        let shape = match inspect.map(|i| &i.status) {
            Some(ClientStatus::Configured | ClientStatus::Verified { .. }) => StatusShape::Ok,
            Some(
                ClientStatus::Drifted
                | ClientStatus::Overridden { .. }
                | ClientStatus::ModelUnavailable,
            ) => StatusShape::Warn,
            Some(ClientStatus::WaitingForFirstRequest) => StatusShape::Run,
            Some(ClientStatus::Rejected { .. }) => StatusShape::Bad,
            _ => StatusShape::Idle,
        };
        let path_preview = det
            .config_paths
            .first()
            .cloned()
            .unwrap_or_else(|| "—".into());
        let alias_info = inspect.and_then(|i| i.inspection.alias.clone());
        let client_id = inspect.and_then(|i| i.client_id.clone());
        let verify_res = view.client_verify_results.get(&app);

        let mut row_actions = h_row().gap(px(5.0)).flex_wrap();
        row_actions = row_actions.child(
            btn_secondary(
                SharedString::from(format!("tool-connect-{}", app.as_str())),
                if matches!(
                    inspect.map(|i| &i.status),
                    Some(
                        ClientStatus::Configured
                            | ClientStatus::Verified { .. }
                            | ClientStatus::Drifted
                            | ClientStatus::WaitingForFirstRequest
                    )
                ) {
                    if zh { "更新配置…" } else { "Update…" }
                } else if zh {
                    "连接…"
                } else {
                    "Connect…"
                },
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_connect_sheet(Some(app), window, cx);
            })),
        );
        if let Some(cid) = client_id {
            row_actions = row_actions.child(
                btn_ghost(
                    SharedString::from(format!("tool-verify-{}", app.as_str())),
                    if zh { "测试请求" } else { "Test" },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.verify_client_tool(cid.clone(), app, zh, cx);
                })),
            );
        }
        if view.managed_tools.iter().any(|m| m.app == app) {
            row_actions = row_actions.child(
                btn_ghost(
                    SharedString::from(format!("tool-restore-{}", app.as_str())),
                    if zh { "还原" } else { "Restore" },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.restore_client_tool(app, false, zh, cx);
                })),
            );
        }

        let mut tool_col = v_col().gap(px(5.0)).child(
            h_row()
                .justify_between()
                .gap(px(10.0))
                .child(
                    h_row()
                        .gap(px(8.0))
                        .flex_1()
                        .min_w_0()
                        .child(status_indicator(shape, cx))
                        .child(
                            v_col()
                                .flex_1()
                                .min_w_0()
                                .gap(px(2.0))
                                .child(
                                    h_row()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                .text_color(to_hsla(p.ink))
                                                .child(app.display_name()),
                                        )
                                        .child(badge(st_lbl, st_tone, cx))
                                        .children(alias_info.map(|a| {
                                            badge(SharedString::from(a), Tone::Accent, cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(to_hsla(p.muted))
                                        .truncate()
                                        .child(SharedString::from(path_preview)),
                                ),
                        ),
                )
                .child(row_actions),
        );

        if let Some(vr) = verify_res {
            let (v_tone, v_text) = match vr.outcome {
                VerifyOutcome::Served => (
                    Tone::Success,
                    format!(
                        "{} · {} · {} ms",
                        if zh { "测试通过" } else { "Test passed" },
                        vr.target.as_deref().unwrap_or(&vr.alias),
                        vr.ttft_ms.unwrap_or(vr.latency_ms)
                    ),
                ),
                other => (
                    Tone::Danger,
                    format!("{:?}: {}", other, vr.message.as_deref().unwrap_or("failed")),
                ),
            };
            tool_col = tool_col.child(
                h_row()
                    .pl(px(16.0))
                    .gap(px(6.0))
                    .child(badge(v_text, v_tone, cx)),
            );
        }

        tools_box = tools_box.child(card(cx).p(px(11.0)).child(tool_col));
    }

    root = root.child(tools_box);

    // 2. Issued Gateway Client Tokens table + inline create form.
    let mut tokens_box = v_col().gap(px(8.0)).child(
        h_row()
            .justify_between()
            .flex_wrap()
            .gap(px(8.0))
            .child(section_heading(
                if zh {
                    format!("网关客户端令牌（{}）", view.gateway_clients.len())
                } else {
                    format!("Gateway Client Tokens ({})", view.gateway_clients.len())
                },
                cx,
            ))
            .child(
                h_row()
                    .gap(px(6.0))
                    .child(
                        div()
                            .w(px(180.0))
                            .child(text_input(&view.new_client_name_input)),
                    )
                    .child(
                        btn_secondary(
                            "gw-create-client",
                            if zh { "+ 创建令牌" } else { "+ Mint token" },
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.create_gateway_client(zh, window, cx);
                            },
                        )),
                    ),
            ),
    );

    if view.gateway_clients.is_empty() {
        tokens_box = tokens_box.child(card(cx).p(px(14.0)).child(
            div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                "还没有客户端令牌。接入编码工具或输入名称创建令牌后，外部请求才能通过网关鉴权。"
            } else {
                "No client tokens yet. Connect a tool or mint a token above."
            }),
        ));
    } else {
        for client in &view.gateway_clients {
            let cid = client.id.clone();
            let cid_rot = cid.clone();
            let cid_rev = cid.clone();
            let cid_del = cid.clone();
            let cid_pol = cid.clone();
            let active = client.is_active();
            let hint = format!("{}••••{}", client.token_prefix, client.token_last4);
            let today_reqs = view
                .overview
                .as_ref()
                .and_then(|o| o.clients.iter().find(|r| r.id == client.id))
                .map_or(0, |r| r.requests_today);
            let editing_policy = view.editing_client_policy.as_deref() == Some(&client.id);

            let mut client_card = v_col().gap(px(8.0)).child(
                h_row()
                    .justify_between()
                    .gap(px(10.0))
                    .flex_wrap()
                    .child(
                        v_col()
                            .gap(px(3.0))
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .flex_wrap()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .text_color(to_hsla(if active {
                                                p.ink
                                            } else {
                                                p.muted
                                            }))
                                            .child(SharedString::from(client.name.clone())),
                                    )
                                    .child(badge(SharedString::from(hint), Tone::Neutral, cx))
                                    .child(badge(
                                        if active {
                                            if zh { "有效" } else { "Active" }
                                        } else if zh {
                                            "已吊销"
                                        } else {
                                            "Revoked"
                                        },
                                        if active { Tone::Success } else { Tone::Danger },
                                        cx,
                                    ))
                                    .child(badge(
                                        if client.allow_all {
                                            if zh {
                                                "全部模型".into()
                                            } else {
                                                "All models".into()
                                            }
                                        } else {
                                            format!(
                                                "{} {}",
                                                client.allowed_models.len(),
                                                if zh { "个允许模型" } else { "models" }
                                            )
                                        },
                                        Tone::Accent,
                                        cx,
                                    )),
                            )
                            .child(
                                h_row()
                                    .gap(px(8.0))
                                    .child(kv_chip(
                                        if zh { "今日" } else { "today" },
                                        today_reqs.to_string(),
                                        cx,
                                    ))
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(to_hsla(p.faint))
                                            .child(SharedString::from(
                                                client.last_used_ms.map_or_else(
                                                    || {
                                                        if zh {
                                                            "上次使用：从未".into()
                                                        } else {
                                                            "Last used: never".into()
                                                        }
                                                    },
                                                    |ms| {
                                                        if zh {
                                                            format!(
                                                                "上次使用：{}",
                                                                format_relative_time(ms, true)
                                                            )
                                                        } else {
                                                            format!(
                                                                "Last used: {}",
                                                                format_relative_time(ms, false)
                                                            )
                                                        }
                                                    },
                                                ),
                                            )),
                                    )
                                    .children(
                                        client.limits.rpm.map(|rpm| {
                                            badge(format!("{rpm} RPM"), Tone::Neutral, cx)
                                        }),
                                    ),
                            ),
                    )
                    .child(
                        h_row()
                            .gap(px(4.0))
                            .children(active.then(|| {
                                btn_secondary(
                                    SharedString::from(format!("cl-rot-{cid}")),
                                    if zh { "轮换令牌" } else { "Rotate" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.rotate_gateway_client(cid_rot.clone(), zh, cx);
                                    },
                                ))
                            }))
                            .children(active.then(|| {
                                btn_ghost(
                                    SharedString::from(format!("cl-pol-{cid}")),
                                    if zh { "策略" } else { "Policy" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.toggle_edit_client_policy(cid_pol.clone(), window, cx);
                                    },
                                ))
                            }))
                            .children(active.then(|| {
                                btn_ghost(
                                    SharedString::from(format!("cl-rev-{cid}")),
                                    if zh { "吊销" } else { "Revoke" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.revoke_gateway_client(cid_rev.clone(), zh, cx);
                                    },
                                ))
                            }))
                            .child(
                                btn_danger(
                                    SharedString::from(format!("cl-del-{cid}")),
                                    if zh { "删除" } else { "Delete" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.delete_gateway_client(cid_del.clone(), zh, cx);
                                    },
                                )),
                            ),
                    ),
            );

            if editing_policy {
                client_card = client_card.child(
                    super::gateway_backups::render_client_policy_editor(view, client, zh, cx),
                );
            }

            tokens_box = tokens_box.child(card(cx).p(px(11.0)).child(client_card));
        }
    }

    root = root.child(tokens_box);
    if let Some(ledger) = super::gateway_backups::render_managed_backups(view, zh, cx) {
        root = root.child(ledger);
    }
    root
}
