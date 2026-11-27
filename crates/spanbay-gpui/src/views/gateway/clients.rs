//! Gateway Clients tab matching `docs/design/prototype/js/data-subtabs.js`:
//! - Compact strip ONLY when installed-but-unconnected local coding tools are detected
//! - Single unified `Connected Clients & Tokens` table combining managed coding tools and
//!   custom client tokens (config path, live status, token hint, model scope, actions)
//! - Collapsible backup ledger at the bottom when SpanBay has managed config backups.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::app::VerifyOutcome;
use spanbay_desktop::core::client_config::{ClientApp, ClientStatus};
use spanbay_desktop::services::clipboard;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_danger, btn_ghost, btn_icon, btn_primary, btn_secondary,
    card, empty_state_with_actions, h_row, icon_xs, palette, section_heading, status_indicator,
    table_header, text_input, to_hsla, v_col,
};
use crate::views::providers::format::{format_client_status, format_relative_time};

use super::GatewayView;

pub fn render_clients_tab(view: &GatewayView, cx: &mut Context<GatewayView>) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let cm = crate::ui::tr(cx).common();

    let mut root = v_col().gap(px(14.0));

    // 1. One-time revealed token banner (after creating or rotating a non-managed client token).
    if let Some((name, token)) = &view.revealed_client_token {
        let copy_tok = token.clone();
        let tok_hdr = h_row()
            .justify_between()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ok))
                    .child(SharedString::from(
                        pr.prov_gateway_clients_token_minted_for_shown_once(name),
                    )),
            )
            .child(
                btn_icon("gw-hide-new-tok", UiIcon::Close).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.revealed_client_token = None;
                        cx.notify();
                    },
                )),
            );
        let tok_box = div()
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
            .child(SharedString::from(token.clone()));
        let copy_btn = btn_primary("gw-copy-new-tok", pr.prov_gateway_clients_copy_token())
            .icon(icon_xs(UiIcon::Copy))
            .on_click(cx.listener(move |this, _, _, cx| {
                let _ = clipboard::set_text(&copy_tok);
                let msg = crate::ui::tr(cx)
                    .providers()
                    .prov_gateway_clients_token_copied();
                this.status_msg = Some((Tone::Success, msg.into()));
                cx.notify();
            }));
        let gw_port = view
            .overview
            .as_ref()
            .map(|o| o.status.port)
            .filter(|&p| p > 0)
            .unwrap_or(view.gateway_settings.port);
        let usage_hint = div()
            .text_size(px(11.0))
            .text_color(to_hsla(p.muted))
            .truncate()
            .child(SharedString::from(format!(
                "Base URL: http://127.0.0.1:{gw_port}/v1  ·  Authorization: Bearer <token>"
            )));
        root = root.child(
            card(cx)
                .p(px(12.0))
                .bg(to_hsla(p.ok_soft))
                .border_color(to_hsla(p.ok_line.composite_over(p.ok_soft)))
                .child(
                    v_col()
                        .gap(px(6.0))
                        .child(tok_hdr)
                        .child(h_row().gap(px(8.0)).child(tok_box).child(copy_btn))
                        .child(usage_hint),
                ),
        );
    }

    // 2. Installed local tools not yet connected to SpanBay (subtle auxiliary links in empty state).
    let unconnected_installed: Vec<ClientApp> = view
        .detected_clients
        .iter()
        .filter(|det| {
            det.installed
                && !matches!(
                    view.inspections.get(&det.app).map(|i| &i.status),
                    Some(
                        ClientStatus::Configured
                            | ClientStatus::Verified { .. }
                            | ClientStatus::WaitingForFirstRequest
                            | ClientStatus::Drifted
                    )
                )
        })
        .map(|det| det.app)
        .collect();

    // 3. Single Unified Card: Client Tokens & Connected Tools (token creation is primary).
    let active_count = view
        .gateway_clients
        .iter()
        .filter(|c| c.is_active())
        .count();
    let has_clients = !view.gateway_clients.is_empty();
    let mut header_row =
        h_row()
            .justify_between()
            .gap(px(10.0))
            .flex_wrap()
            .child(section_heading(
                pr.prov_gateway_clients_unified_title(active_count),
                cx,
            ));
    if has_clients {
        header_row = header_row.child(
            h_row()
                .gap(px(6.0))
                .child(
                    btn_secondary("gw-clients-connect", pr.prov_gateway_connect_client())
                        .icon(icon_xs(UiIcon::Terminal))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_connect_sheet(None, window, cx);
                        })),
                )
                .child(
                    btn_secondary(
                        "gw-toggle-custom-tok",
                        pr.prov_gateway_clients_custom_token_btn(),
                    )
                    .icon(icon_xs(UiIcon::Key))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.custom_token_open {
                            this.custom_token_open = false;
                            cx.notify();
                        } else {
                            this.open_create_token(window, cx);
                        }
                    })),
                ),
        );
    }
    let mut main_card = v_col().gap(px(8.0)).child(header_row);

    if view.custom_token_open {
        main_card = main_card.child(
            h_row()
                .w_full()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.pane))
                .border_1()
                .border_color(to_hsla(p.accent_line.composite_over(p.pane)))
                .child(
                    div()
                        .flex_1()
                        .max_w(px(360.0))
                        .child(text_input(&view.new_client_name_input)),
                )
                .child(
                    btn_primary("gw-create-client", pr.prov_gateway_clients_mint_token())
                        .icon(icon_xs(UiIcon::Key))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.create_gateway_client(window, cx);
                        })),
                )
                .child(
                    btn_ghost("gw-cancel-custom-tok", cm.cancel()).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.custom_token_open = false;
                            cx.notify();
                        },
                    )),
                ),
        );
    }

    if !has_clients {
        let empty_actions = (!view.custom_token_open).then(|| {
            let main_btns = h_row()
                .justify_center()
                .gap(px(8.0))
                .child(
                    btn_primary(
                        "gw-empty-custom",
                        pr.prov_gateway_clients_custom_token_btn(),
                    )
                    .icon(icon_xs(UiIcon::Key))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_create_token(window, cx);
                    })),
                )
                .child(
                    btn_secondary("gw-empty-connect", pr.prov_gateway_connect_client())
                        .icon(icon_xs(UiIcon::Terminal))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_connect_sheet(None, window, cx);
                        })),
                );
            let mut col = v_col().items_center().gap(px(12.0)).child(main_btns);
            if !unconnected_installed.is_empty() {
                let mut quick_links = h_row().justify_center().gap(px(4.0)).flex_wrap().child(
                    h_row()
                        .gap(px(5.0))
                        .pr(px(4.0))
                        .child(status_indicator(StatusShape::Ok, cx))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(pr.prov_gateway_clients_detected_banner()),
                        ),
                );
                for app in unconnected_installed {
                    quick_links = quick_links.child(
                        btn_ghost(
                            SharedString::from(format!("gw-quick-conn-{}", app.as_str())),
                            format!(
                                "{} {}",
                                app.display_name(),
                                pr.prov_gateway_clients_connect()
                            ),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.open_connect_sheet(Some(app), window, cx);
                            },
                        )),
                    );
                }
                col = col.child(quick_links);
            }
            col.into_any_element()
        });
        main_card = main_card.child(empty_state_with_actions(
            UiIcon::Key,
            pr.prov_gateway_clients_empty_title(),
            pr.prov_gateway_clients_empty_desc(),
            empty_actions,
            cx,
        ));
    } else {
        main_card = main_card.child(table_header(
            &[
                (pr.prov_gateway_clients_col_client(), Some(176.0)),
                (pr.prov_gateway_clients_col_config(), None),
                (pr.prov_gateway_clients_col_token(), Some(134.0)),
                (pr.prov_gateway_clients_col_scope(), Some(166.0)),
                (pr.prov_gateway_clients_col_actions(), Some(244.0)),
            ],
            cx,
        ));

        for client in &view.gateway_clients {
            main_card = main_card.child(render_client_row(view, client, cx));
        }
    }

    root = root.child(card(cx).p(px(14.0)).child(main_card));
    if let Some(ledger) = super::backups::render_managed_backups(view, cx) {
        root = root.child(ledger);
    }
    root
}

fn render_client_row(
    view: &GatewayView,
    client: &spanbay_desktop::core::llm::store::GatewayClient,
    cx: &mut Context<GatewayView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let cm = crate::ui::tr(cx).common();
    let lang = crate::ui::active_language(cx);

    let cid = client.id.clone();
    let active = client.is_active();
    let hint = format!("{}••••{}", client.token_prefix, client.token_last4);
    let today_reqs = view
        .overview
        .as_ref()
        .and_then(|o| o.clients.iter().find(|r| r.id == client.id))
        .map_or(0, |r| r.requests_today);
    let editing_policy = view.editing_client_policy.as_deref() == Some(&client.id);

    // Match this client token to any managed coding tool (config file + status + backup).
    let matched_managed = view
        .managed_tools
        .iter()
        .find(|t| t.files.iter().any(|f| f.client_id == client.id));
    let matched_app: Option<ClientApp> = matched_managed.map(|t| t.app).or_else(|| {
        view.inspections
            .iter()
            .find(|(_, ins)| ins.client_id.as_deref() == Some(client.id.as_str()))
            .map(|(app, _)| *app)
    });
    let inspect = matched_app.and_then(|app| view.inspections.get(&app));
    let config_path: String = matched_managed
        .and_then(|t| t.files.first().map(|f| f.path.clone()))
        .or_else(|| {
            matched_app.and_then(|app| {
                view.detected_clients
                    .iter()
                    .find(|d| d.app == app)
                    .and_then(|d| d.config_paths.first().cloned())
            })
        })
        .unwrap_or_else(|| pr.prov_gateway_clients_manual_token_type().to_owned());

    let (shape, status_pill) = if !active {
        (
            StatusShape::Idle,
            Some((pr.gw_clients_revoked(), Tone::Danger)),
        )
    } else if let Some(ins) = inspect {
        let (lbl, tone) = format_client_status(&ins.status, &ins.inspection.drift, lang);
        let sh = match &ins.status {
            ClientStatus::Configured | ClientStatus::Verified { .. } => StatusShape::Ok,
            ClientStatus::Drifted
            | ClientStatus::Overridden { .. }
            | ClientStatus::ModelUnavailable => StatusShape::Warn,
            ClientStatus::WaitingForFirstRequest => StatusShape::Run,
            ClientStatus::Rejected { .. } => StatusShape::Bad,
            _ => StatusShape::Idle,
        };
        (sh, Some((lbl, tone)))
    } else {
        (StatusShape::Ok, None)
    };

    let last_active_txt = client.last_used_ms.map_or_else(
        || pr.gw_clients_never().to_owned(),
        |ms| format_relative_time(ms, lang),
    );
    let scope_txt = if client.allow_all {
        pr.gw_clients_all().to_owned()
    } else if client.allowed_models.len() == 1 {
        client
            .allowed_models
            .first()
            .map(|m| view.format_model_ref(m))
            .unwrap_or_else(|| pr.prov_gateway_overview_none().to_owned())
    } else {
        format!(
            "{} {}",
            client.allowed_models.len(),
            pr.prov_gateway_clients_models_2()
        )
    };

    let cid_rot = cid.clone();
    let cid_pol = cid.clone();
    let cid_rev = cid.clone();
    let cid_del = cid.clone();

    let mut actions = h_row()
        .w(px(244.0))
        .flex_shrink_0()
        .justify_end()
        .gap(px(4.0));
    if active {
        if let Some(app) = matched_app {
            let cid_v = cid.clone();
            actions = actions.child(
                btn_ghost(
                    SharedString::from(format!("cl-test-{cid}")),
                    pr.prov_gateway_clients_test(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.verify_client_tool(cid_v.clone(), app, cx);
                })),
            );
        }
        actions = actions
            .child(
                btn_secondary(
                    SharedString::from(format!("cl-rot-{cid}")),
                    pr.prov_gateway_clients_rotate(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.rotate_gateway_client(cid_rot.clone(), cx);
                })),
            )
            .child(
                btn_ghost(
                    SharedString::from(format!("cl-pol-{cid}")),
                    pr.prov_gateway_clients_policy(),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.toggle_edit_client_policy(cid_pol.clone(), window, cx);
                })),
            );
        if let Some(tool) = matched_managed {
            let app = tool.app;
            actions = actions.child(
                btn_ghost(
                    SharedString::from(format!("cl-rst-{cid}")),
                    pr.prov_gateway_clients_restore(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.restore_client_tool(app, false, cx);
                })),
            );
        }
        let confirming = view.confirm_revoke_client.as_deref() == Some(cid.as_str());
        if confirming {
            actions = actions.child(
                btn_danger(
                    SharedString::from(format!("cl-rev-ok-{cid}")),
                    pr.prov_gateway_clients_confirm_revoke(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm_revoke_client = None;
                    this.revoke_gateway_client(cid_rev.clone(), cx);
                })),
            );
        } else {
            let cid_arm = cid.clone();
            actions = actions.child(
                btn_ghost(
                    SharedString::from(format!("cl-rev-{cid}")),
                    pr.gwa_gateway_revoke_action(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm_revoke_client = Some(cid_arm.clone());
                    cx.notify();
                })),
            );
        }
    } else {
        actions = actions.child(
            btn_danger(SharedString::from(format!("cl-del-{cid}")), cm.delete()).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.delete_gateway_client(cid_del.clone(), cx);
                }),
            ),
        );
    }

    let verify_banner = matched_app
        .and_then(|app| view.client_verify_results.get(&app))
        .map(|vr| {
            let (v_tone, v_text) = match vr.outcome {
                VerifyOutcome::Served => (
                    Tone::Success,
                    format!(
                        "{} · {} · {} ms",
                        pr.prov_gateway_clients_test_passed(),
                        vr.target.as_deref().unwrap_or(&vr.alias),
                        vr.ttft_ms.unwrap_or(vr.latency_ms)
                    ),
                ),
                other => (
                    Tone::Danger,
                    format!("{other:?}: {}", vr.message.as_deref().unwrap_or("failed")),
                ),
            };
            h_row()
                .px(px(10.0))
                .pb(px(6.0))
                .child(badge(v_text, v_tone, cx))
        });

    let mut row_wrap = v_col()
        .w_full()
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .w_full()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(8.0))
                .child(
                    h_row()
                        .w(px(176.0))
                        .flex_shrink_0()
                        .gap(px(7.0))
                        .min_w_0()
                        .child(status_indicator(shape, cx))
                        .child(
                            v_col()
                                .flex_1()
                                .min_w_0()
                                .gap(px(2.0))
                                .child(
                                    h_row()
                                        .gap(px(5.0))
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                .text_color(to_hsla(if active {
                                                    p.ink
                                                } else {
                                                    p.muted
                                                }))
                                                .truncate()
                                                .child(SharedString::from(client.name.clone())),
                                        )
                                        .children(
                                            status_pill.map(|(lbl, tone)| badge(lbl, tone, cx)),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(to_hsla(p.faint))
                                        .truncate()
                                        .child(SharedString::from(last_active_txt)),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .truncate()
                        .child(SharedString::from(config_path)),
                )
                .child(
                    v_col()
                        .w(px(134.0))
                        .flex_shrink_0()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(hint)),
                        )
                        .child(
                            h_row()
                                .gap(px(5.0))
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(to_hsla(p.faint))
                                        .child(SharedString::from(
                                            pr.prov_gateway_clients_today(today_reqs),
                                        )),
                                )
                                .children(
                                    client
                                        .limits
                                        .rpm
                                        .map(|rpm| badge(format!("{rpm} RPM"), Tone::Neutral, cx)),
                                ),
                        ),
                )
                .child(div().w(px(166.0)).flex_shrink_0().min_w_0().child(badge(
                    SharedString::from(scope_txt),
                    Tone::Accent,
                    cx,
                )))
                .child(actions),
        )
        .children(verify_banner);

    if editing_policy {
        row_wrap = row_wrap.child(super::backups::render_client_policy_editor(
            view, client, cx,
        ));
    }
    row_wrap
}
