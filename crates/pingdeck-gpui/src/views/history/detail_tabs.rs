//! Detail inspector tabs for `HistoryView`: Body, Headers, Sent Wire Request, and Request Snapshot.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    div, px,
};
use pingdeck_desktop::core::model::ExecutionHistoryEntry;

use crate::ui::{
    PayloadBlockOptions, ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_secondary,
    card, empty_inline, icon_xs, kv_line, payload_block, section_header, v_col,
};

use super::detail_trace::{render_checks_tab, render_trace_tab};
use super::state::{HistoryDetailTab, HistoryView};

pub fn render_detail_tab(
    view: &HistoryView,
    entry: &ExecutionHistoryEntry,
    cx: &mut Context<HistoryView>,
) -> AnyElement {
    let body: AnyElement = match view.detail_tab {
        HistoryDetailTab::Body => render_body_tab(view, entry, cx),
        HistoryDetailTab::Headers => render_headers_tab(view, entry, cx),
        HistoryDetailTab::Sent => render_sent_tab(view, entry, cx),
        HistoryDetailTab::Snapshot => render_snapshot_tab(view, entry, cx),
        HistoryDetailTab::Trace => render_trace_tab(view, entry, cx),
        HistoryDetailTab::Checks => render_checks_tab(view, entry, cx),
    };

    div()
        .id("hist-detail-body-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .p(px(16.0))
        .child(body)
        .into_any_element()
}

fn render_body_tab(
    view: &HistoryView,
    entry: &ExecutionHistoryEntry,
    cx: &mut Context<HistoryView>,
) -> AnyElement {
    let mut col = v_col().w_full().gap(px(10.0));

    if let Some(msg) = &entry.error_message {
        col = col.child(alert_banner(
            Tone::Danger,
            format!(
                "{}: {msg}",
                entry
                    .error_kind
                    .as_deref()
                    .unwrap_or(crate::ui::Tr(view.lang).history().detail_tabs_error())
            ),
            None,
            cx,
        ));
    }

    if entry.response_body_truncated {
        col = col.child(alert_banner(
            Tone::Warning,
            crate::ui::Tr(view.lang)
                .history()
                .detail_tabs_showing_first_bytes_preview_of(
                    entry.response_body_preview.len(),
                    entry.body_size_bytes,
                ),
            None,
            cx,
        ));
    }

    let body_card = card(cx)
        .p(px(12.0))
        .gap(px(8.0))
        .child(section_header(
            crate::ui::Tr(view.lang)
                .history()
                .detail_tabs_response_body_preview(),
            None,
            cx,
        ))
        .child(if entry.response_body_preview.is_empty() {
            empty_inline(
                crate::ui::Tr(view.lang)
                    .history()
                    .detail_tabs_empty_response_body(),
                None,
                None,
                cx,
            )
            .into_any_element()
        } else {
            div()
                .w_full()
                .h(px(460.0))
                .child(view.body_viewer.clone())
                .into_any_element()
        });

    col.child(body_card).into_any_element()
}

fn render_headers_tab(
    view: &HistoryView,
    entry: &ExecutionHistoryEntry,
    cx: &mut Context<HistoryView>,
) -> AnyElement {
    let mut box_el = card(cx).p(px(12.0)).gap(px(5.0)).child(section_header(
        crate::ui::Tr(view.lang)
            .history()
            .detail_tabs_response_headers_redacted(),
        Some(badge(entry.response_headers.len().to_string(), Tone::Neutral, cx).into_any_element()),
        cx,
    ));
    if entry.response_headers.is_empty() {
        box_el = box_el.child(empty_inline(
            crate::ui::Tr(view.lang)
                .history()
                .detail_tabs_no_response_headers_recorded(),
            None,
            None,
            cx,
        ));
    } else {
        for h in &entry.response_headers {
            box_el = box_el.child(kv_line(h.key.clone(), h.value.clone(), 180.0, cx));
        }
    }
    box_el.into_any_element()
}

fn render_sent_tab(
    view: &HistoryView,
    entry: &ExecutionHistoryEntry,
    cx: &mut Context<HistoryView>,
) -> AnyElement {
    let Some(sent) = &entry.sent_request else {
        return empty_inline(
            crate::ui::Tr(view.lang)
                .history()
                .detail_tabs_no_wire_request_snapshot_stored(),
            None,
            None,
            cx,
        )
        .into_any_element();
    };
    let mut col = v_col().w_full().gap(px(12.0)).child(
        card(cx)
            .p(px(12.0))
            .gap(px(6.0))
            .child(section_header(
                crate::ui::Tr(view.lang)
                    .history()
                    .detail_tabs_sent_wire_request_redacted(),
                None,
                cx,
            ))
            .child(kv_line(
                crate::ui::Tr(view.lang).history().filter_methods(),
                sent.method.clone(),
                120.0,
                cx,
            ))
            .child(kv_line("URL", sent.url.clone(), 120.0, cx)),
    );

    if !sent.headers.is_empty() {
        let mut h_card = card(cx).p(px(12.0)).gap(px(5.0)).child(section_header(
            crate::ui::Tr(view.lang)
                .history()
                .detail_tabs_request_headers(),
            None,
            cx,
        ));
        for h in &sent.headers {
            h_card = h_card.child(kv_line(h.key.clone(), h.value.clone(), 180.0, cx));
        }
        col = col.child(h_card);
    }

    if !sent.body_preview.is_empty() {
        let sent_ct = sent
            .headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case("content-type"))
            .map(|h| h.value.as_str());
        col = col.child(payload_block(
            "hist-sent-body",
            &sent.body_preview,
            PayloadBlockOptions {
                title: Some(
                    crate::ui::Tr(view.lang)
                        .history()
                        .detail_tabs_request_body_preview(),
                ),
                content_type: sent_ct,
                max_height_px: Some(320.0),
                ..Default::default()
            },
            cx,
        ));
    }
    col.into_any_element()
}

fn render_snapshot_tab(
    view: &HistoryView,
    entry: &ExecutionHistoryEntry,
    cx: &mut Context<HistoryView>,
) -> AnyElement {
    let Some(snap) = &entry.request_snapshot else {
        return empty_inline(
            crate::ui::Tr(view.lang)
                .history()
                .detail_tabs_no_request_definition_snapshot_stored(),
            None,
            None,
            cx,
        )
        .into_any_element();
    };
    let entry_clone = entry.clone();
    let mut box_el = card(cx)
        .p(px(12.0))
        .gap(px(6.0))
        .child(section_header(
            crate::ui::Tr(view.lang)
                .history()
                .detail_tabs_request_definition_snapshot(),
            Some(
                btn_secondary(
                    "hist-snap-open",
                    crate::ui::Tr(view.lang)
                        .history()
                        .detail_tabs_open_in_requests(),
                )
                .icon(icon_xs(UiIcon::Send))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_in_requests(&entry_clone, cx);
                }))
                .into_any_element(),
            ),
            cx,
        ))
        .child(kv_line(
            crate::ui::Tr(view.lang).shared().name(),
            snap.name.clone(),
            130.0,
            cx,
        ))
        .child(kv_line(
            crate::ui::Tr(view.lang).history().filter_methods(),
            format!("{:?}", snap.method).to_uppercase(),
            130.0,
            cx,
        ))
        .child(kv_line(
            crate::ui::Tr(view.lang)
                .history()
                .detail_tabs_template_url(),
            snap.url.clone(),
            130.0,
            cx,
        ))
        .child(kv_line(
            crate::ui::Tr(view.lang).history().detail_tabs_auth(),
            format!("{:?}", snap.auth),
            130.0,
            cx,
        ));

    for qp in &snap.query_params {
        box_el = box_el.child(kv_line(format!("?{}", qp.key), qp.value.clone(), 130.0, cx));
    }
    for h in &snap.headers {
        box_el = box_el.child(kv_line(format!("H: {}", h.key), h.value.clone(), 130.0, cx));
    }
    box_el.into_any_element()
}
