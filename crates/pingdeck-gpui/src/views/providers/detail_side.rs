//! Provider Overview supporting cards: Protocol Endpoints & Sniff Evidence, Balance,
//! 24h Health & Circuits, and Recent Calls.

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::llm::store::{Provider, ProviderKey};

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_ghost, card, collapsible_box, h_row, icon_xs, kv_chip,
    palette, progress_bar, section_heading, status_indicator, to_hsla, v_col,
};

use super::ProvidersView;
use super::format::{format_auth, format_balance_report, format_protocol, format_relative_time};

pub fn render_endpoints_card(
    view: &ProvidersView,
    prov: &Provider,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let enabled_eps = prov.endpoints.iter().filter(|e| e.enabled).count();
    let mut ep_rows = v_col().gap(px(5.0));

    for ep in &prov.endpoints {
        let is_primary = prov.primary_protocol == Some(ep.protocol);
        let prefix = if ep.path_prefix.is_empty() {
            "/"
        } else {
            &ep.path_prefix
        };
        let auth_str = ep.auth.map_or_else(
            || format_auth(prov.auth, crate::ui::active_language(cx)),
            |a| format_auth(a, crate::ui::active_language(cx)),
        );
        ep_rows = ep_rows.child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.pane))
                .child(
                    h_row()
                        .gap(px(7.0))
                        .min_w_0()
                        .child(status_indicator(
                            if ep.enabled {
                                StatusShape::Ok
                            } else {
                                StatusShape::Idle
                            },
                            cx,
                        ))
                        .child(badge(
                            format_protocol(ep.protocol, crate::ui::active_language(cx)),
                            if ep.enabled {
                                Tone::Accent
                            } else {
                                Tone::Neutral
                            },
                            cx,
                        ))
                        .children(is_primary.then(|| {
                            badge(
                                crate::ui::tr(cx).providers().sniff_proto_primary(),
                                Tone::Success,
                                cx,
                            )
                        }))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .truncate()
                                .child(SharedString::from(format!(
                                    "{prefix}{}",
                                    ep.protocol.suffix()
                                ))),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(badge(auth_str, Tone::Neutral, cx))
                        .child(badge(
                            if ep.enabled {
                                crate::ui::tr(cx).common().kv_enabled()
                            } else {
                                crate::ui::tr(cx).providers().prov_detail_side_disabled()
                            },
                            if ep.enabled {
                                Tone::Success
                            } else {
                                Tone::Neutral
                            },
                            cx,
                        )),
                ),
        );
    }

    let mut ev_rows = v_col().gap(px(4.0)).pt(px(6.0));
    for ev in prov.sniff_evidence.iter().take(16) {
        ev_rows = ev_rows.child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(5.0))
                .bg(to_hsla(p.sunken))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .min_w_0()
                        .flex_1()
                        .child(badge(
                            SharedString::from(ev.phase.clone()),
                            Tone::Neutral,
                            cx,
                        ))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.ink_2))
                                .truncate()
                                .child(SharedString::from(format!("{} {}", ev.method, ev.url))),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(badge(
                            ev.status.map_or_else(|| "—".into(), |s| s.to_string()),
                            if ev.status.is_some_and(|s| s < 400) {
                                Tone::Success
                            } else {
                                Tone::Neutral
                            },
                            cx,
                        ))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.faint))
                                .child(SharedString::from(format!("{}ms", ev.latency_ms))),
                        ),
                ),
        );
    }

    let ev_open = view.evidence_open;
    let ev_block = (!prov.sniff_evidence.is_empty()).then(|| {
        v_col()
            .gap(px(4.0))
            .pt(px(4.0))
            .child(
                btn_ghost(
                    "prov-toggle-ev",
                    crate::ui::tr(cx)
                        .providers()
                        .prov_detail_side_sniff_evidence_requests(
                            if ev_open { "▾" } else { "▸" },
                            prov.sniff_evidence.len(),
                        ),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.evidence_open = !this.evidence_open;
                    cx.notify();
                })),
            )
            .child(collapsible_box(ev_open, "prov-ev-col").content(ev_rows))
    });

    card(cx).p(px(14.0)).child(
        v_col()
            .gap(px(8.0))
            .child(
                h_row()
                    .justify_between()
                    .child(section_heading(
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_side_protocol_endpoints_evidence(),
                        cx,
                    ))
                    .child(badge(
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_side_enabled(enabled_eps, prov.endpoints.len()),
                        Tone::Accent,
                        cx,
                    )),
            )
            .child(ep_rows)
            .children(ev_block),
    )
}

pub fn render_balance_card(
    view: &ProvidersView,
    prov: &Provider,
    keys: &[ProviderKey],
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pid = prov.id.clone();
    let best = keys
        .iter()
        .filter(|k| k.enabled)
        .filter_map(|k| k.balance.as_ref())
        .find(|b| b.last_good.as_ref().is_some_and(|r| r.supported));

    let mut content = v_col().gap(px(7.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(
                crate::ui::tr(cx).providers().prov_detail_side_balance(),
                cx,
            ))
            .child(
                btn_ghost(
                    "prov-bal-refresh",
                    if view.refreshing_balance {
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_models_refreshing()
                    } else {
                        crate::ui::tr(cx).common().refresh()
                    },
                )
                .icon(icon_xs(UiIcon::Refresh))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.refreshing_balance {
                        this.refresh_provider_balance(pid.clone(), cx);
                    }
                })),
            ),
    );

    if prov.category == "local" {
        content = content.child(
            div()
                .text_base()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ok))
                .child(crate::ui::tr(cx).providers().prov_detail_side_local_free()),
        );
    } else if let Some(stored) = best
        && let Some(rep) = &stored.last_good
    {
        content = content.child(
            div()
                .text_lg()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(if rep.exhausted { p.bad } else { p.ink }))
                .child(SharedString::from(format_balance_report(
                    rep,
                    crate::ui::active_language(cx),
                ))),
        );
        if let (Some(rem), Some(tot)) = (rep.remaining, rep.total)
            && tot > 0.0
        {
            let used = (tot - rem).max(0.0);
            content = content
                .child(progress_bar(
                    "prov-bal-prog",
                    ((rem / tot) * 100.0).clamp(0.0, 100.0) as f32,
                ))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .child(crate::ui::tr(cx).providers().prov_detail_side_total_used(
                            format!("{:.2}", tot),
                            format!("{:.2}", used),
                        )),
                );
        }
        if let Some(src) = &rep.source_endpoint {
            content = content.child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.faint))
                    .truncate()
                    .child(SharedString::from(
                        crate::ui::tr(cx).providers().prov_side_balance_from(
                            src,
                            format_relative_time(rep.checked_at_ms, crate::ui::active_language(cx)),
                        ),
                    )),
            );
        }
        if let Some(err) = &stored.error {
            content = content.child(badge(
                SharedString::from(err.message.clone()),
                Tone::Warning,
                cx,
            ));
        }
    } else if let Some(err) = keys
        .iter()
        .filter_map(|k| k.balance.as_ref().and_then(|b| b.error.as_ref()))
        .next()
    {
        content = content.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.warn))
                .child(SharedString::from(err.message.clone())),
        );
    } else {
        content = content.child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_side_not_checked_yet_or_no(),
            ),
        );
    }

    card(cx).p(px(14.0)).child(content)
}

pub fn render_health_card(
    view: &ProvidersView,
    prov: &Provider,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut col = v_col().gap(px(7.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_side_health_circuits_24h(),
                cx,
            ))
            .children(view.provider_health.as_ref().map(|h| {
                badge(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_detail_side_reqs(h.requests),
                    Tone::Neutral,
                    cx,
                )
            })),
    );

    if let Some(h) = &view.provider_health {
        if h.enough_requests {
            let sr = h
                .success_rate
                .map_or_else(|| "—".into(), |r| format!("{:.1}%", r * 100.0));
            let p50 = h
                .ttft_p50_ms
                .map_or_else(|| "—".into(), |m| format!("{m} ms"));
            let p95 = h
                .ttft_p95_ms
                .map_or_else(|| "—".into(), |m| format!("{m} ms"));
            col = col.child(
                h_row()
                    .flex_wrap()
                    .gap(px(5.0))
                    .child(kv_chip(
                        crate::ui::tr(cx).providers().prov_detail_side_ok(),
                        sr,
                        cx,
                    ))
                    .child(kv_chip("p50", p50, cx))
                    .child(kv_chip("p95", p95, cx)),
            );
        } else {
            col = col.child(
                div().text_xs().text_color(to_hsla(p.muted)).child(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_detail_side_not_enough_requests_10_in(),
                ),
            );
        }

        if !h.open_circuits.is_empty() {
            let pid_reset = prov.id.clone();
            col = col.child(
                h_row()
                    .justify_between()
                    .pt(px(4.0))
                    .child(badge(
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_side_open_circuit_s(h.open_circuits.len()),
                        Tone::Danger,
                        cx,
                    ))
                    .child(
                        btn_ghost(
                            "prov-reset-cb",
                            crate::ui::tr(cx).providers().prov_detail_side_reset(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reset_provider_circuits(pid_reset.clone(), cx);
                        })),
                    ),
            );
            for oc in h.open_circuits.iter().take(4) {
                col = col.child(
                    h_row()
                        .justify_between()
                        .px(px(7.0))
                        .py(px(4.0))
                        .rounded(px(5.0))
                        .bg(to_hsla(p.bad_soft))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.bad))
                                .truncate()
                                .child(SharedString::from(oc.model_id.clone())),
                        )
                        .child(
                            div().text_size(px(10.5)).text_color(to_hsla(p.bad)).child(
                                SharedString::from(
                                    oc.retry_in_s
                                        .map_or_else(|| "half-open".into(), |s| format!("{s}s")),
                                ),
                            ),
                        ),
                );
            }
        }
    } else {
        col = col.child(
            div().text_xs().text_color(to_hsla(p.faint)).child(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_side_loading_health(),
            ),
        );
    }

    card(cx).p(px(14.0)).child(col)
}

pub fn render_recent_calls_card(
    view: &ProvidersView,
    prov_id: &str,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pid = prov_id.to_owned();
    let mut col = v_col().gap(px(6.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_side_recent_calls(),
                cx,
            ))
            .child(
                btn_ghost(
                    "prov-calls-all",
                    crate::ui::tr(cx).providers().prov_detail_side_all(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pending_handoff
                        .set(Some(super::ProvidersHandoff::OpenUsageProvider(Some(
                            pid.clone(),
                        ))));
                    cx.notify();
                })),
            ),
    );

    if view.recent_logs.is_empty() {
        col = col.child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_side_no_recent_calls_yet(),
            ),
        );
    } else {
        for (i, log) in view.recent_logs.iter().take(6).enumerate() {
            let tone = if (200..300).contains(&log.status) {
                Tone::Success
            } else if log.status == 429 {
                Tone::Warning
            } else {
                Tone::Danger
            };
            let ms = log.ttft_ms.unwrap_or(log.latency_ms);
            let call_id = log.id;
            col = col.child(
                h_row()
                    .id(SharedString::from(format!("prov-rc-{i}")))
                    .cursor_pointer()
                    .justify_between()
                    .gap(px(6.0))
                    .px(px(4.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .hover(|s| s.bg(to_hsla(p.pane)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pending_handoff
                            .set(Some(super::ProvidersHandoff::OpenUsageCall(call_id)));
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(SharedString::from(
                                log.upstream_model
                                    .clone()
                                    .unwrap_or_else(|| log.requested_model.clone()),
                            )),
                    )
                    .child(badge(format!("{} · {ms}ms", log.status), tone, cx)),
            );
        }
    }

    card(cx).p(px(14.0)).child(col)
}
