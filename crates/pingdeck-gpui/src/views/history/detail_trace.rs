//! Trace & Timing waterfall and Checks & Run Children detail tabs for `HistoryView`.

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::model::ExecutionHistoryEntry;
use pingdeck_desktop::core::model::models::PhaseTiming;
use pingdeck_desktop::core::model::phase::PhaseValue;

use crate::ui::{
    Tone, badge, card, data_row, empty_inline, h_row, kv_line, list_row, meta_pill, method_badge,
    palette, progress_bar, section_header, to_hsla, v_col,
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
                view.t("传输与 TLS 链路", "Transport & TLS"),
                None,
                cx,
            ))
            .child(kv_line(
                view.t("协议", "Protocol"),
                chain.transport.protocol.clone(),
                140.0,
                cx,
            ));
        if let Some(addr) = &chain.transport.remote_addr {
            tr = tr.child(kv_line(
                view.t("远端地址", "Remote Addr"),
                addr.clone(),
                140.0,
                cx,
            ));
        }
        if let Some(proxy) = &chain.transport.proxy_url {
            tr = tr.child(kv_line(view.t("代理", "Proxy"), proxy.clone(), 140.0, cx));
        }
        if let Some(cert) = &chain.transport.client_cert_host {
            tr = tr.child(kv_line(
                view.t("客户端证书", "Client Cert"),
                cert.clone(),
                140.0,
                cx,
            ));
        }
        if chain.transport.custom_ca_count > 0 {
            tr = tr.child(kv_line(
                view.t("自定义 CA", "Custom CAs"),
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
            .child(section_header(view.t("鉴权溯源", "Auth Trace"), None, cx))
            .child(kv_line(
                view.t("类型", "Type"),
                chain.auth.auth_type.clone(),
                140.0,
                cx,
            ))
            .child(kv_line(
                view.t("来源", "Source"),
                chain.auth.source.clone(),
                140.0,
                cx,
            ))
            .child(kv_line(
                view.t("落点", "Applied To"),
                chain.auth.applied_to.clone(),
                140.0,
                cx,
            ));
        col = col.child(auth_card);

        if !chain.variables.is_empty() {
            let mut v_card = card(cx).p(px(12.0)).gap(px(5.0)).child(section_header(
                view.t("变量解析追踪", "Resolved Variables"),
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
                        .children(
                            var.secret
                                .then(|| badge(view.t("机密", "Secret"), Tone::Warning, cx)),
                        ),
                );
            }
            col = col.child(v_card);
        }
    } else if entry.timing.is_none() {
        col = col.child(empty_inline(
            view.t(
                "此记录无阶段耗时与调用链路信息",
                "No timing or call-chain trace recorded",
            ),
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
        PhaseValue::Reused => view.t("复用连接", "Reused").to_owned(),
        PhaseValue::NotNeeded => view.t("无需", "N/A").to_owned(),
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
            view.t("阶段耗时瀑布", "Phase Timing Waterfall"),
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
            view.t("等待首包 (TTFB)", "Request / TTFB"),
            format!("{} ms", timing.request_ms),
            Some(timing.request_ms as u64),
        ))
        .child(bar_row(
            "t-resp",
            view.t("接收正文", "Response Read"),
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
                    view.t("断言与脚本测试汇总", "Checks Summary"),
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
        let mut run_card = card(cx).p(px(12.0)).gap(px(5.0)).child(section_header(
            if view.is_zh() {
                format!("同批次运行记录 ({})", view.run_children.len())
            } else {
                format!("Sibling Run Steps ({})", view.run_children.len())
            },
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
        col = col.child(run_card);
    }

    if entry.check_summary.is_none() && view.run_children.is_empty() {
        col = col.child(empty_inline(
            view.t(
                "此记录没有断言校验或所属批次步骤",
                "No checks or parent run recorded",
            ),
            None,
            None,
            cx,
        ));
    }

    col.into_any_element()
}
