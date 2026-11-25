//! Gateway Clients tab: detected coding tools (inspect/connect/verify/restore),
//! issued client tokens (create/rotate/policy/revoke/delete), and managed config backups.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
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

pub fn render_clients_tab(view: &ProvidersView, cx: &mut Context<ProvidersView>) -> gpui_kit::Div {
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
                                        .child(SharedString::from(
                                            crate::ui::tr(cx)
                                                .providers()
                                                .prov_gateway_clients_token_minted_for_shown_once(
                                                    name,
                                                ),
                                        )),
                                )
                                .child(btn_icon("gw-hide-new-tok", UiIcon::Close).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.revealed_client_token = None;
                                        cx.notify();
                                    }),
                                )),
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
                                        .border_color(to_hsla(
                                            p.line_strong.composite_over(p.surface),
                                        ))
                                        .text_xs()
                                        .text_color(to_hsla(p.ink))
                                        .truncate()
                                        .child(SharedString::from(token.clone())),
                                )
                                .child(
                                    btn_primary(
                                        "gw-copy-new-tok",
                                        crate::ui::tr(cx)
                                            .providers()
                                            .prov_gateway_clients_copy_token(),
                                    )
                                    .icon(icon_xs(UiIcon::Copy))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            let _ = clipboard::set_text(&copy_tok);
                                            this.status_msg = Some((
                                                Tone::Success,
                                                crate::ui::tr(cx)
                                                    .providers()
                                                    .prov_gateway_clients_token_copied()
                                                    .into(),
                                            ));
                                            cx.notify();
                                        },
                                    )),
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
                crate::ui::tr(cx)
                    .providers()
                    .prov_gateway_clients_local_ai_coding_tools_managed(),
                cx,
            ))
            .child(
                btn_primary(
                    "gw-clients-connect",
                    crate::ui::tr(cx)
                        .providers()
                        .prov_gateway_clients_connect_tool(),
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
                    (
                        crate::ui::tr(cx)
                            .providers()
                            .prov_gateway_clients_installed(),
                        Tone::Neutral,
                    )
                } else {
                    (
                        crate::ui::tr(cx)
                            .providers()
                            .gwa_gateway_writer_not_installed(),
                        Tone::Neutral,
                    )
                }
            },
            |ins| {
                format_client_status(
                    &ins.status,
                    &ins.inspection.drift,
                    crate::ui::active_language(cx),
                )
            },
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
                    crate::ui::tr(cx).providers().prov_gateway_clients_update()
                } else {
                    crate::ui::tr(cx).providers().prov_gateway_clients_connect()
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
                    crate::ui::tr(cx).providers().prov_gateway_clients_test(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.verify_client_tool(cid.clone(), app, cx);
                })),
            );
        }
        if view.managed_tools.iter().any(|m| m.app == app) {
            row_actions = row_actions.child(
                btn_ghost(
                    SharedString::from(format!("tool-restore-{}", app.as_str())),
                    crate::ui::tr(cx).providers().prov_gateway_clients_restore(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.restore_client_tool(app, false, cx);
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
                        crate::ui::tr(cx)
                            .providers()
                            .prov_gateway_clients_test_passed(),
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

    if !view.snippet_clients.is_empty() {
        let mut sn_row = h_row().gap(px(6.0)).flex_wrap();
        for sc in &view.snippet_clients {
            let tool_kind = super::connect_sheet::ConnectToolKind::Snippet(sc.client);
            sn_row = sn_row.child(
                btn_secondary(
                    SharedString::from(format!("snip-cl-{:?}", sc.client)),
                    format!(
                        "{}{}",
                        tool_kind.label(crate::ui::active_language(cx)),
                        if sc.installed { " ✓" } else { "" }
                    ),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_connect_sheet_kind(tool_kind, window, cx);
                })),
            );
        }
        tools_box = tools_box.child(
            card(cx).p(px(10.0)).child(
                v_col()
                    .gap(px(6.0))
                    .child(section_heading(
                        crate::ui::tr(cx)
                            .providers()
                            .prov_gateway_clients_snippet_clients_zed_cursor_cherry(),
                        cx,
                    ))
                    .child(sn_row),
            ),
        );
    }

    root = root.child(tools_box);

    // 2. Issued Gateway Client Tokens table + inline create form.
    let mut tokens_box = v_col().gap(px(8.0)).child(
        h_row()
            .justify_between()
            .flex_wrap()
            .gap(px(8.0))
            .child(section_heading(
                crate::ui::tr(cx)
                    .providers()
                    .prov_gateway_clients_gateway_client_tokens(view.gateway_clients.len()),
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
                            crate::ui::tr(cx)
                                .providers()
                                .prov_gateway_clients_mint_token(),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.create_gateway_client(window, cx);
                            },
                        )),
                    ),
            ),
    );

    if view.gateway_clients.is_empty() {
        tokens_box = tokens_box.child(
            card(cx).p(px(14.0)).child(
                div().text_xs().text_color(to_hsla(p.muted)).child(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_gateway_clients_no_client_tokens_yet_connect(),
                ),
            ),
        );
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
                                            crate::ui::tr(cx).providers().gw_clients_active()
                                        } else {
                                            crate::ui::tr(cx).providers().gw_clients_revoked()
                                        },
                                        if active { Tone::Success } else { Tone::Danger },
                                        cx,
                                    ))
                                    .child(badge(
                                        if client.allow_all {
                                            crate::ui::tr(cx).providers().gw_clients_all().into()
                                        } else {
                                            format!(
                                                "{} {}",
                                                client.allowed_models.len(),
                                                crate::ui::tr(cx)
                                                    .providers()
                                                    .prov_gateway_clients_models_2()
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
                                        crate::ui::tr(cx).providers().prov_gateway_clients_today(),
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
                                                        crate::ui::tr(cx)
                                                            .providers()
                                                            .prov_gateway_clients_last_used_never()
                                                            .into()
                                                    },
                                                    |ms| {
                                                        let lang = crate::ui::active_language(cx);
                                                        crate::ui::tr(cx)
                                                            .providers()
                                                            .prov_gw_last_used_at(
                                                                format_relative_time(ms, lang),
                                                            )
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
                                    crate::ui::tr(cx).providers().prov_gateway_clients_rotate(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.rotate_gateway_client(cid_rot.clone(), cx);
                                    },
                                ))
                            }))
                            .children(active.then(|| {
                                btn_ghost(
                                    SharedString::from(format!("cl-pol-{cid}")),
                                    crate::ui::tr(cx).providers().prov_gateway_clients_policy(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.toggle_edit_client_policy(cid_pol.clone(), window, cx);
                                    },
                                ))
                            }))
                            .children(active.then(|| {
                                let confirming =
                                    view.confirm_revoke_client.as_deref() == Some(cid.as_str());
                                if confirming {
                                    btn_danger(
                                        SharedString::from(format!("cl-rev-ok-{cid}")),
                                        crate::ui::tr(cx)
                                            .providers()
                                            .prov_gateway_clients_confirm_revoke(),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.confirm_revoke_client = None;
                                        this.revoke_gateway_client(cid_rev.clone(), cx);
                                    }))
                                    .into_any_element()
                                } else {
                                    let cid_arm = cid.clone();
                                    btn_ghost(
                                        SharedString::from(format!("cl-rev-{cid}")),
                                        crate::ui::tr(cx).providers().gwa_gateway_revoke_action(),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.confirm_revoke_client = Some(cid_arm.clone());
                                        cx.notify();
                                    }))
                                    .into_any_element()
                                }
                            }))
                            .children((!active).then(|| {
                                btn_danger(
                                    SharedString::from(format!("cl-del-{cid}")),
                                    crate::ui::tr(cx).common().delete(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.delete_gateway_client(cid_del.clone(), cx);
                                    },
                                ))
                            })),
                    ),
            );

            if editing_policy {
                client_card = client_card.child(
                    super::gateway_backups::render_client_policy_editor(view, client, cx),
                );
            }

            tokens_box = tokens_box.child(card(cx).p(px(11.0)).child(client_card));
        }
    }

    root = root.child(tokens_box);
    if let Some(ledger) = super::gateway_backups::render_managed_backups(view, cx) {
        root = root.child(ledger);
    }
    root
}
