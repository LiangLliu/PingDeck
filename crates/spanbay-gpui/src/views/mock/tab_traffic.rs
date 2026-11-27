//! Live Traffic sub-tab for `MockView`: filterable request log stream, detail inspector, and route creation from logs.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use spanbay_desktop::core::model::items::{MockLogOutcome, MockTransactionLog};

use crate::ui::{
    PaginationBar, PayloadBlockOptions, ScrollableElement as _, Tone, UiIcon, badge, btn_ghost,
    btn_primary, btn_secondary, card, choice_chip, empty_inline, empty_state, h_row, icon_xs,
    kv_line, list_row, meta_pill, method_badge, page_slice, pagination_bar, palette, payload_block,
    search_input, section_header, to_hsla, toolbar_strip, v_col,
};

use super::state::{MockView, TrafficFilter};

pub fn render_traffic_tab(view: &MockView, cx: &mut Context<MockView>) -> AnyElement {
    let p = palette(cx);
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

    let m_tr = crate::ui::Tr(view.lang).mock();
    let filters = [
        (TrafficFilter::All, "all", m_tr.traffic_filter_all()),
        (TrafficFilter::Route, "route", m_tr.traffic_filter_route()),
        (TrafficFilter::Proxy, "proxy", m_tr.traffic_filter_proxy()),
        (
            TrafficFilter::NoMatch,
            "nomatch",
            m_tr.traffic_filter_no_match(),
        ),
        (TrafficFilter::Error, "error", m_tr.traffic_filter_error()),
    ];

    let mut bar = toolbar_strip(cx).child(
        div()
            .w(px(200.0))
            .child(search_input(&view.traffic_search_input, cx)),
    );
    for (f, tag, lbl) in filters {
        bar = bar.child(
            choice_chip(
                SharedString::from(format!("mock-tf-{tag}")),
                lbl,
                view.traffic_filter == f,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.traffic_filter = f;
                this.traffic_page = 0;
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
                    crate::ui::Tr(view.lang).mock().tab_traffic_stop_rec()
                } else {
                    crate::ui::Tr(view.lang).mock().tab_traffic_record()
                },
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle_recording(cx))),
        );
        if rec_count > 0 {
            bar = bar.child(
                btn_primary(
                    "mock-tf-rec-save",
                    crate::ui::tr(cx)
                        .mock()
                        .tab_traffic_save_recorded(rec_count),
                )
                .on_click(cx.listener(|this, _, _, cx| this.save_recorded_routes(cx))),
            );
        }
    }
    if running {
        bar = bar.child(
            btn_ghost(
                "mock-tf-clear",
                crate::ui::Tr(view.lang).mock().tab_traffic_clear(),
            )
            .icon(icon_xs(UiIcon::Trash))
            .on_click(cx.listener(|this, _, _, cx| this.clear_logs(cx))),
        );
    }

    // Left list of log entries (bounded 50/page to keep live traffic smooth)
    const TRAFFIC_PAGE_SIZE: usize = 50;
    let (page_rows, clamped_page, _) = page_slice(&filtered, view.traffic_page, TRAFFIC_PAGE_SIZE);
    let mut rows_col = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if filtered.is_empty() {
        rows_col = rows_col.child(empty_inline(
            if running {
                crate::ui::Tr(view.lang)
                    .mock()
                    .tab_traffic_waiting_for_incoming_traffic()
            } else {
                crate::ui::Tr(view.lang)
                    .mock()
                    .tab_traffic_server_is_stopped()
            },
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .mock()
                    .tab_traffic_send_requests_to_the_local(),
            )),
            None,
            cx,
        ));
    } else {
        for row in page_rows {
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
        .or_else(|| page_rows.first().copied());

    let detail = if let Some(log) = selected_log {
        render_log_inspector(view, log, cx)
    } else {
        empty_state(
            UiIcon::Activity,
            crate::ui::Tr(view.lang)
                .mock()
                .tab_traffic_select_a_traffic_entry(),
            crate::ui::Tr(view.lang)
                .mock()
                .tab_traffic_inspect_headers_bodies_branch_selection(),
            cx,
        )
        .into_any_element()
    };

    let mut left_pane = v_col()
        .w(px(340.0))
        .flex_shrink_0()
        .h_full()
        .bg(to_hsla(p.pane))
        .border_r_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            div()
                .id("mock-traffic-list-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(rows_col),
        );
    if filtered.len() > TRAFFIC_PAGE_SIZE {
        left_pane = left_pane.child(
            div()
                .flex_shrink_0()
                .px(px(10.0))
                .py(px(7.0))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(pagination_bar(
                    PaginationBar {
                        id_prefix: "mock-tf-pg",
                        page: clamped_page,
                        page_size: TRAFFIC_PAGE_SIZE,
                        total: filtered.len(),
                        page_sizes: &[],
                        loading: false,
                        compact: true,
                    },
                    |this: &mut MockView, next_page, cx| {
                        this.traffic_page = next_page;
                        cx.notify();
                    },
                    None::<fn(&mut MockView, usize, &mut Context<MockView>)>,
                    cx,
                )),
        );
    }

    v_col()
        .size_full()
        .min_h_0()
        .child(bar)
        .child(
            h_row()
                .flex_1()
                .min_h_0()
                .child(left_pane)
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
                        btn_secondary(
                            "mock-log-to-route",
                            crate::ui::Tr(view.lang).mock().tab_traffic_save_as_route(),
                        )
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
            crate::ui::Tr(view.lang).mock().tab_traffic_selected_by(),
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
                crate::ui::Tr(view.lang).mock().tab_traffic_phase_timing(),
                parts.join(" · "),
                110.0,
                cx,
            ));
        }
    }

    // Request headers & body
    let mut req_card = card(cx).p(px(12.0)).gap(px(6.0)).child(section_header(
        crate::ui::Tr(view.lang).mock().tab_traffic_request(),
        None,
        cx,
    ));
    for h in &log.request_headers {
        req_card = req_card.child(kv_line(h.key.clone(), h.value.clone(), 160.0, cx));
    }
    if !log.request_body.is_empty() {
        let req_ct = log
            .request_headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case("content-type"))
            .map(|h| h.value.as_str());
        req_card = req_card.child(payload_block(
            format!("mock-log-req-{}", log.id),
            &log.request_body,
            PayloadBlockOptions {
                content_type: req_ct,
                max_height_px: Some(260.0),
                ..Default::default()
            },
            cx,
        ));
    }
    content = content.child(req_card);

    // Response headers & body
    let mut resp_card = card(cx).p(px(12.0)).gap(px(6.0)).child(section_header(
        crate::ui::Tr(view.lang).mock().tab_traffic_response(),
        None,
        cx,
    ));
    for h in &log.response_headers {
        resp_card = resp_card.child(kv_line(h.key.clone(), h.value.clone(), 160.0, cx));
    }
    if !log.response_body.is_empty() {
        let resp_ct = log
            .response_headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case("content-type"))
            .map(|h| h.value.as_str());
        resp_card = resp_card.child(payload_block(
            format!("mock-log-resp-{}", log.id),
            &log.response_body,
            PayloadBlockOptions {
                content_type: resp_ct,
                max_height_px: Some(320.0),
                ..Default::default()
            },
            cx,
        ));
    }
    content = content.child(resp_card);

    // Callbacks delivered
    if !log.callbacks.is_empty() {
        let mut cb_card = card(cx).p(px(12.0)).gap(px(6.0)).child(section_header(
            crate::ui::Tr(view.lang)
                .mock()
                .tab_traffic_callbacks_delivered(),
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
