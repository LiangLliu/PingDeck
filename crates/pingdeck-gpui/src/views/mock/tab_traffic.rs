//! Live Traffic sub-tab for `MockView`: filterable request log stream, detail inspector, and route creation from logs.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::model::items::{MockLogOutcome, MockTransactionLog};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_primary, btn_secondary, card,
    choice_chip, empty_inline, empty_state, h_row, icon_xs, kv_line, list_row, meta_pill,
    method_badge, palette, search_input, section_header, to_hsla, toolbar_strip, v_col,
};

use super::state::{MockView, TrafficFilter};

pub fn render_traffic_tab(view: &MockView, cx: &mut Context<MockView>) -> AnyElement {
    let p = palette(cx);
    let zh = view.is_zh();
    let Some(srv) = &view.draft else {
        return div().into_any_element();
    };
    let running = view.is_running(&srv.id);
    let recording = view.status_of(&srv.id).is_some_and(|s| s.recording);
    let rec_count = view.status_of(&srv.id).map_or(0, |s| s.recorded_count);
    let q = view
        .traffic_search_input
        .read(cx)
        .value()
        .trim()
        .to_lowercase();

    let filtered: Vec<&MockTransactionLog> = view
        .logs
        .iter()
        .filter(|row| {
            let match_q = q.is_empty()
                || row.path.to_lowercase().contains(&q)
                || row.method.to_lowercase().contains(&q)
                || row.status_code.to_string().contains(&q);
            let match_f = match view.traffic_filter {
                TrafficFilter::All => true,
                TrafficFilter::Route => row.outcome == Some(MockLogOutcome::Route),
                TrafficFilter::Proxy => row.proxied || row.outcome == Some(MockLogOutcome::Proxy),
                TrafficFilter::NoMatch => row.outcome == Some(MockLogOutcome::NoMatch),
                TrafficFilter::Error => {
                    row.status_code >= 400
                        || matches!(
                            row.outcome,
                            Some(
                                MockLogOutcome::ProxyFailed
                                    | MockLogOutcome::RejectedHost
                                    | MockLogOutcome::RejectedOrigin
                                    | MockLogOutcome::Busy
                            )
                        )
                }
            };
            match_q && match_f
        })
        .collect();

    let filters = [
        (TrafficFilter::All, "全部", "All"),
        (TrafficFilter::Route, "路由命中", "Route"),
        (TrafficFilter::Proxy, "代理回源", "Proxy"),
        (TrafficFilter::NoMatch, "未匹配", "No Match"),
        (TrafficFilter::Error, "错误", "Errors"),
    ];

    let mut bar = toolbar_strip(cx).child(
        div()
            .w(px(200.0))
            .child(search_input(&view.traffic_search_input, cx)),
    );
    for (f, zh_l, en_l) in filters {
        bar = bar.child(
            choice_chip(
                SharedString::from(format!("mock-tf-{en_l}")),
                if zh { zh_l } else { en_l },
                view.traffic_filter == f,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.traffic_filter = f;
                cx.notify();
            })),
        );
    }
    bar = bar.child(div().flex_1());
    if running && srv.proxy_enabled {
        bar = bar.child(
            btn_secondary(
                "mock-tf-rec",
                if recording {
                    view.t("停止录制", "Stop Rec")
                } else {
                    view.t("录制回源", "Record")
                },
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle_recording(cx))),
        );
        if rec_count > 0 {
            bar = bar.child(
                btn_primary(
                    "mock-tf-rec-save",
                    if zh {
                        format!("保存录制 ({rec_count})")
                    } else {
                        format!("Save Recorded ({rec_count})")
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| this.save_recorded_routes(cx))),
            );
        }
    }
    if running {
        bar = bar.child(
            btn_ghost("mock-tf-clear", view.t("清空", "Clear"))
                .icon(icon_xs(UiIcon::Trash))
                .on_click(cx.listener(|this, _, _, cx| this.clear_logs(cx))),
        );
    }

    // Left list of log entries
    let mut rows_col = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if filtered.is_empty() {
        rows_col = rows_col.child(empty_inline(
            if running {
                view.t("等待传入流量…", "Waiting for incoming traffic…")
            } else {
                view.t("服务未启动，暂无实时日志", "Server is stopped")
            },
            Some(SharedString::from(view.t(
                "向本地端口发送请求后，此处将实时显示匹配结果、耗时和回调状态。",
                "Send requests to the local port to inspect matches, timings, and callbacks live.",
            ))),
            None,
            cx,
        ));
    } else {
        for row in &filtered {
            let lid = row.id.clone();
            let active = view.selected_log_id.as_deref() == Some(row.id.as_str());
            let tone = if row.status_code >= 500 {
                Tone::Danger
            } else if row.status_code >= 400 {
                Tone::Warning
            } else {
                Tone::Success
            };
            rows_col = rows_col.child(
                list_row(
                    SharedString::from(format!("mock-log-{}", row.id)),
                    active,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.selected_log_id = Some(lid.clone());
                        cx.notify();
                    }),
                )
                .py(px(6.0))
                .child(method_badge(&row.method, cx))
                .child(badge(row.status_code.to_string(), tone, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(row.path.clone()),
                )
                .child(meta_pill(format!("{} ms", row.elapsed_ms), cx)),
            );
        }
    }

    let selected_log = view
        .selected_log_id
        .as_deref()
        .and_then(|id| view.logs.iter().find(|l| l.id == id))
        .or_else(|| filtered.first().copied());

    let detail = if let Some(log) = selected_log {
        render_log_inspector(view, log, cx)
    } else {
        empty_state(
            UiIcon::Activity,
            view.t("选择一条流量记录", "Select a traffic entry"),
            view.t(
                "查看完整请求/响应头、正文、分支匹配原因及 Webhook 回调结果。",
                "Inspect headers, bodies, branch selection reason, and callbacks.",
            ),
            cx,
        )
        .into_any_element()
    };

    v_col()
        .size_full()
        .min_h_0()
        .child(bar)
        .child(
            h_row()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("mock-traffic-list-scroll")
                        .w(px(340.0))
                        .flex_shrink_0()
                        .h_full()
                        .bg(to_hsla(p.pane))
                        .border_r_1()
                        .border_color(to_hsla(p.line.composite_over(p.pane)))
                        .overflow_y_scrollbar()
                        .child(rows_col),
                )
                .child(div().flex_1().min_w_0().h_full().child(detail)),
        )
        .into_any_element()
}

fn render_log_inspector(
    view: &MockView,
    log: &MockTransactionLog,
    cx: &mut Context<MockView>,
) -> AnyElement {
    let p = palette(cx);
    let log_for_route = log.clone();
    let full_path = if log.query.is_empty() {
        log.path.clone()
    } else {
        format!("{}?{}", log.path, log.query)
    };

    let mut content = v_col().w_full().p(px(16.0)).gap(px(12.0)).child(
        card(cx)
            .p(px(14.0))
            .gap(px(8.0))
            .child(
                h_row()
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.0))
                            .child(method_badge(&log.method, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(full_path),
                            ),
                    )
                    .child(
                        btn_secondary("mock-log-to-route", view.t("保存为路由", "Save as Route"))
                            .icon(icon_xs(UiIcon::Plus))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.create_route_from_log(log_for_route.clone(), cx);
                            })),
                    ),
            )
            .child(
                h_row()
                    .gap(px(6.0))
                    .flex_wrap()
                    .child(badge(
                        format!("HTTP {}", log.status_code),
                        if log.status_code < 400 {
                            Tone::Success
                        } else {
                            Tone::Danger
                        },
                        cx,
                    ))
                    .child(meta_pill(format!("{} ms", log.elapsed_ms), cx))
                    .children(log.outcome.map(|o| meta_pill(format!("{o:?}"), cx)))
                    .children(log.route_label.as_ref().map(|r| meta_pill(r.clone(), cx)))
                    .children(
                        log.response_label
                            .as_ref()
                            .map(|r| meta_pill(r.clone(), cx)),
                    ),
            ),
    );

    if let Some(sel) = &log.selected_by {
        let detail_txt = match (&sel.rule_number, &sel.received) {
            (Some(n), Some(recv)) => format!("{:?} · rule #{n} · received={recv}", sel.kind),
            (Some(n), None) => format!("{:?} · rule #{n}", sel.kind),
            _ => format!("{:?}", sel.kind),
        };
        content = content.child(kv_line(
            view.t("命中原因", "Selected By"),
            detail_txt,
            110.0,
            cx,
        ));
    }

    if let Some(t) = &log.timing {
        let mut parts = Vec::new();
        if let Some(v) = t.read_ms {
            parts.push(format!("read {v}ms"));
        }
        if let Some(v) = t.route_ms {
            parts.push(format!("route {v}ms"));
        }
        if let Some(v) = t.latency_ms {
            parts.push(format!("latency {v}ms"));
        }
        if let Some(v) = t.proxy_ms {
            parts.push(format!("proxy {v}ms"));
        }
        if !parts.is_empty() {
            content = content.child(kv_line(
                view.t("阶段耗时", "Phase Timing"),
                parts.join(" · "),
                110.0,
                cx,
            ));
        }
    }

    // Request headers & body
    let mut req_card = card(cx).p(px(12.0)).gap(px(6.0)).child(section_header(
        view.t("请求详情", "Request"),
        None,
        cx,
    ));
    for h in &log.request_headers {
        req_card = req_card.child(kv_line(h.key.clone(), h.value.clone(), 160.0, cx));
    }
    if !log.request_body.is_empty() {
        req_card = req_card.child(
            div()
                .w_full()
                .p(px(10.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(log.request_body.clone()),
        );
    }
    content = content.child(req_card);

    // Response headers & body
    let mut resp_card = card(cx).p(px(12.0)).gap(px(6.0)).child(section_header(
        view.t("响应详情", "Response"),
        None,
        cx,
    ));
    for h in &log.response_headers {
        resp_card = resp_card.child(kv_line(h.key.clone(), h.value.clone(), 160.0, cx));
    }
    if !log.response_body.is_empty() {
        resp_card = resp_card.child(
            div()
                .w_full()
                .p(px(10.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(log.response_body.clone()),
        );
    }
    content = content.child(resp_card);

    // Callbacks delivered
    if !log.callbacks.is_empty() {
        let mut cb_card = card(cx).p(px(12.0)).gap(px(6.0)).child(section_header(
            view.t("触发的 Webhook 回调", "Callbacks Delivered"),
            None,
            cx,
        ));
        for cb in &log.callbacks {
            let status_txt = cb
                .status
                .map(|s| format!("HTTP {s}"))
                .or_else(|| cb.error.clone())
                .unwrap_or_else(|| "—".to_owned());
            cb_card = cb_card.child(kv_line(
                cb.name.clone(),
                format!("{} · {} ({} ms)", cb.url, status_txt, cb.elapsed_ms),
                140.0,
                cx,
            ));
        }
        content = content.child(cb_card);
    }

    div()
        .id("mock-log-inspector-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(content)
        .into_any_element()
}
