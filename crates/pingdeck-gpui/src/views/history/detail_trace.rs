//! Trace & Timing waterfall and Checks & Run Children detail tabs for `HistoryView`.

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::model::ExecutionHistoryEntry;
use pingdeck_desktop::core::model::models::PhaseTiming;
use pingdeck_desktop::core::model::phase::PhaseValue;

use crate::ui::{
    Disableable as _, Tone, badge, btn_secondary, card, data_row, empty_inline, h_row, kv_line,
    list_row, meta_pill, method_badge, palette, progress_bar, section_header, to_hsla, v_col,
};

use super::state::HistoryView;

pub fn render_trace_tab(
    view: &HistoryView,
    entry: &ExecutionHistoryEntry,
    cx: &mut Context<HistoryView>,
) -> AnyElement {
    let mut col = v_col().w_full().gap(px(12.0));

    if let Some(t) = &entry.timing {
        col = col.child(render_timing_card(view, t, cx));
    }

    if let Some(chain) = &entry.call_chain {
        let mut tr = card(cx)
            .p(px(12.0))
            .gap(px(5.0))
            .child(section_header(
                crate::ui::Tr(view.lang)
                    .history()
                    .detail_trace_transport_tls(),
                None,
                cx,
            ))
            .child(kv_line(
                crate::ui::Tr(view.lang).history().detail_trace_protocol(),
                chain.transport.protocol.clone(),
                140.0,
                cx,
            ));
        if let Some(addr) = &chain.transport.remote_addr {
            tr = tr.child(kv_line(
                crate::ui::Tr(view.lang)
                    .history()
                    .detail_trace_remote_addr(),
                addr.clone(),
                140.0,
                cx,
            ));
        }
        if let Some(proxy) = &chain.transport.proxy_url {
            tr = tr.child(kv_line(
                crate::ui::Tr(view.lang).history().detail_trace_proxy(),
                proxy.clone(),
                140.0,
                cx,
            ));
        }
        if let Some(cert) = &chain.transport.client_cert_host {
            tr = tr.child(kv_line(
                crate::ui::Tr(view.lang)
                    .history()
                    .detail_trace_client_cert(),
                cert.clone(),
                140.0,
                cx,
            ));
        }
        if chain.transport.custom_ca_count > 0 {
            tr = tr.child(kv_line(
                crate::ui::Tr(view.lang).history().detail_trace_custom_cas(),
                chain.transport.custom_ca_count.to_string(),
                140.0,
                cx,
            ));
        }
        for hop in &chain.transport.redirect_hops {
            tr = tr.child(kv_line(
                format!("Redirect #{}", hop.hop),
                format!(
                    "{} {} -> {} ({} ms)",
                    hop.method, hop.url, hop.status_code, hop.duration_ms
                ),
                140.0,
                cx,
            ));
        }
        col = col.child(tr);

        let auth_card = card(cx)
            .p(px(12.0))
            .gap(px(5.0))
            .child(section_header(
                crate::ui::Tr(view.lang).history().detail_trace_auth_trace(),
                None,
                cx,
            ))
            .child(kv_line(
                crate::ui::Tr(view.lang).history().detail_trace_type(),
                chain.auth.auth_type.clone(),
                140.0,
                cx,
            ))
            .child(kv_line(
                crate::ui::Tr(view.lang).history().filter_sources(),
                chain.auth.source.clone(),
                140.0,
                cx,
            ))
            .child(kv_line(
                crate::ui::Tr(view.lang).history().detail_trace_applied_to(),
                chain.auth.applied_to.clone(),
                140.0,
                cx,
            ));
        col = col.child(auth_card);

        if !chain.variables.is_empty() {
            let mut v_card = card(cx).p(px(12.0)).gap(px(5.0)).child(section_header(
                crate::ui::Tr(view.lang)
                    .history()
                    .detail_trace_resolved_variables(),
                None,
                cx,
            ));
            for var in &chain.variables {
                v_card = v_card.child(
                    data_row(cx)
                        .child(meta_pill(format!("{:?}", var.source), cx))
                        .child(
                            div()
                                .w(px(140.0))
                                .text_xs()
                                .text_color(to_hsla(palette(cx).ink))
                                .truncate()
                                .child(var.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(to_hsla(palette(cx).ink_2))
                                .truncate()
                                .child(var.resolved_value.clone()),
                        )
                        .children(var.secret.then(|| {
                            badge(
                                crate::ui::Tr(view.lang).history().detail_trace_secret(),
                                Tone::Warning,
                                cx,
                            )
                        })),
                );
            }
            col = col.child(v_card);
        }
    } else if entry.timing.is_none() {
        col = col.child(empty_inline(
            crate::ui::Tr(view.lang)
                .history()
                .detail_trace_no_timing_or_call_chain(),
            None,
            None,
            cx,
        ));
    }

    col.into_any_element()
}

fn render_timing_card(view: &HistoryView, timing: &PhaseTiming, cx: &gpui_kit::App) -> AnyElement {
    let total = timing.total_ms.max(1) as f32;
    let phase_label = |v: PhaseValue| match v {
        PhaseValue::Ms(ms) => format!("{ms} ms"),
        PhaseValue::Reused => crate::ui::Tr(view.lang)
            .history()
            .detail_trace_reused()
            .to_owned(),
        PhaseValue::NotNeeded => crate::ui::Tr(view.lang)
            .history()
            .detail_trace_n_a()
            .to_owned(),
        PhaseValue::NotMeasured => "—".to_owned(),
    };
    let bar_row = |id: &'static str, label: &'static str, val_text: String, ms: Option<u64>| {
        let pct = ms.map_or(0.0, |m| (m as f32 / total * 100.0).clamp(1.0, 100.0));
        h_row()
            .w_full()
            .gap(px(10.0))
            .child(
                div()
                    .w(px(110.0))
                    .text_xs()
                    .text_color(to_hsla(palette(cx).muted))
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(progress_bar(id, pct)))
            .child(
                div()
                    .w(px(80.0))
                    .text_xs()
                    .text_color(to_hsla(palette(cx).ink))
                    .child(val_text),
            )
    };

    let mut c = card(cx)
        .p(px(12.0))
        .gap(px(7.0))
        .child(section_header(
            crate::ui::Tr(view.lang)
                .history()
                .detail_trace_phase_timing_waterfall(),
            Some(meta_pill(format!("Total {} ms", timing.total_ms), cx).into_any_element()),
            cx,
        ))
        .child(bar_row(
            "t-dns",
            "DNS",
            phase_label(timing.dns_ms),
            timing.dns_ms.ms(),
        ))
        .child(bar_row(
            "t-conn",
            if timing.connect_includes_tls {
                "TCP + TLS"
            } else {
                "TCP Connect"
            },
            phase_label(timing.connect_ms),
            timing.connect_ms.ms(),
        ))
        .child(bar_row(
            "t-req",
            crate::ui::Tr(view.lang)
                .history()
                .detail_trace_request_ttfb(),
            format!("{} ms", timing.request_ms),
            Some(timing.request_ms as u64),
        ))
        .child(bar_row(
            "t-resp",
            crate::ui::Tr(view.lang)
                .history()
                .detail_trace_response_read(),
            format!("{} ms", timing.response_ms),
            Some(timing.response_ms as u64),
        ));
    if let Some(ttft) = timing.first_token_ms {
        c = c.child(bar_row(
            "t-ttft",
            "AI TTFT",
            format!("{ttft} ms"),
            Some(ttft),
        ));
    }
    c.into_any_element()
}

pub fn render_checks_tab(
    view: &HistoryView,
    entry: &ExecutionHistoryEntry,
    cx: &mut Context<HistoryView>,
) -> AnyElement {
    let p = palette(cx);
    let mut col = v_col().w_full().gap(px(12.0));

    if let Some(chk) = &entry.check_summary {
        col = col.child(
            card(cx)
                .p(px(12.0))
                .gap(px(6.0))
                .child(section_header(
                    crate::ui::Tr(view.lang)
                        .history()
                        .detail_trace_checks_summary(),
                    None,
                    cx,
                ))
                .child(
                    h_row()
                        .gap(px(8.0))
                        .flex_wrap()
                        .child(badge(
                            format!("{} passed", chk.passed),
                            if chk.passed > 0 {
                                Tone::Success
                            } else {
                                Tone::Neutral
                            },
                            cx,
                        ))
                        .child(badge(
                            format!("{} failed", chk.failed),
                            if chk.failed > 0 {
                                Tone::Danger
                            } else {
                                Tone::Neutral
                            },
                            cx,
                        ))
                        .child(meta_pill(format!("total {}", chk.passed + chk.failed), cx)),
                ),
        );
    }

    if !view.run_children.is_empty() {
        let total_steps = view
            .run_children_total
            .map_or(view.run_children.len(), |t| t as usize);
        let mut run_card = card(cx).p(px(12.0)).gap(px(5.0)).child(section_header(
            crate::ui::Tr(view.lang)
                .history()
                .detail_trace_sibling_run_steps(total_steps),
            (!entry.source_name.is_empty())
                .then(|| meta_pill(entry.source_name.clone(), cx).into_any_element()),
            cx,
        ));
        for child in &view.run_children {
            let cid = child.id.as_str().to_owned();
            let active = entry.id == child.id;
            let method_s = format!("{:?}", child.method).to_uppercase();
            run_card = run_card.child(
                list_row(
                    SharedString::from(format!("hist-child-{}", child.id.as_str())),
                    active,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.select_entry(cid.clone(), cx);
                    }),
                )
                .child(method_badge(&method_s, cx))
                .child(badge(
                    child.status_code.to_string(),
                    if child.status_code > 0 && child.status_code < 400 {
                        Tone::Success
                    } else {
                        Tone::Danger
                    },
                    cx,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(child.request_name.clone()),
                )
                .child(meta_pill(format!("{} ms", child.elapsed_ms), cx)),
            );
        }
        if view.run_children_next_cursor.is_some() {
            run_card = run_card.child(
                h_row().w_full().justify_center().pt(px(4.0)).child(
                    btn_secondary(
                        "hist-children-more",
                        if view.run_children_loading {
                            crate::ui::Tr(view.lang).common().loading()
                        } else {
                            crate::ui::Tr(view.lang).history().sidebar_load_more()
                        },
                    )
                    .disabled(view.run_children_loading)
                    .on_click(cx.listener(|this, _, _, cx| this.load_more_run_children(cx))),
                ),
            );
        }
        col = col.child(run_card);
    }

    if entry.check_summary.is_none() && view.run_children.is_empty() {
        col = col.child(empty_inline(
            crate::ui::Tr(view.lang)
                .history()
                .detail_trace_no_checks_or_parent_run(),
            None,
            None,
            cx,
        ));
    }

    col.into_any_element()
}
