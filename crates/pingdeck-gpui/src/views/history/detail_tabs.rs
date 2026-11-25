//! Detail inspector tabs for `HistoryView`: Body, Headers, Sent Wire Request, and Request Snapshot.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    div, px,
};
use pingdeck_desktop::core::app::{BodySource, response_format_body_impl};
use pingdeck_desktop::core::model::ExecutionHistoryEntry;
use pingdeck_desktop::core::response_viewer::ResponseBodyFormat;
use pingdeck_desktop::services::clipboard;

use crate::bridge::runtime;
use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_secondary, card, choice_chip,
    empty_inline, h_row, icon_xs, kv_line, palette, section_header, to_hsla, v_col,
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
    let p = palette(cx);
    let body_copy = entry.response_body_preview.clone();
    let mut col = v_col().w_full().gap(px(10.0));

    if let Some(msg) = &entry.error_message {
        col = col.child(alert_banner(
            Tone::Danger,
            format!(
                "{}: {msg}",
                entry
                    .error_kind
                    .as_deref()
                    .unwrap_or(view.t("执行错误", "Error"))
            ),
            None,
            cx,
        ));
    }

    if entry.response_body_truncated {
        col = col.child(alert_banner(
            Tone::Warning,
            if view.is_zh() {
                format!(
                    "仅保留响应正文前 {} 字节预览（原始响应共 {} 字节）。",
                    entry.response_body_preview.len(),
                    entry.body_size_bytes
                )
            } else {
                format!(
                    "Showing first {} bytes preview of {} total bytes.",
                    entry.response_body_preview.len(),
                    entry.body_size_bytes
                )
            },
            None,
            cx,
        ));
    }

    let display_body = if view.body_pretty && !entry.response_body_preview.is_empty() {
        let ct = entry
            .response_headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case("content-type"))
            .map(|h| h.value.to_ascii_lowercase())
            .unwrap_or_default();
        let trimmed = entry.response_body_preview.trim_start();
        let fmt = if ct.contains("xml") || trimmed.starts_with("<?xml") {
            ResponseBodyFormat::Xml
        } else if ct.contains("html")
            || trimmed.starts_with("<!DOCTYPE")
            || trimmed.starts_with("<html")
        {
            ResponseBodyFormat::Html
        } else if ct.contains("yaml") {
            ResponseBodyFormat::Yaml
        } else {
            ResponseBodyFormat::Json
        };
        response_format_body_impl(
            runtime(cx).state(),
            BodySource::Text(entry.response_body_preview.clone()),
            fmt,
        )
        .map(|f| f.text)
        .unwrap_or_else(|_| entry.response_body_preview.clone())
    } else {
        entry.response_body_preview.clone()
    };

    let body_card = card(cx)
        .p(px(12.0))
        .gap(px(8.0))
        .child(section_header(
            view.t("响应正文预览", "Response Body Preview"),
            Some(
                h_row()
                    .gap(px(6.0))
                    .child(
                        choice_chip(
                            "hist-body-pretty",
                            view.t("格式化", "Pretty"),
                            view.body_pretty,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.body_pretty = true;
                            cx.notify();
                        })),
                    )
                    .child(
                        choice_chip(
                            "hist-body-raw",
                            view.t("原文", "Raw"),
                            !view.body_pretty,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.body_pretty = false;
                            cx.notify();
                        })),
                    )
                    .child(
                        btn_secondary("hist-copy-body", view.t("复制正文", "Copy Body"))
                            .icon(icon_xs(UiIcon::Copy))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let _ = clipboard::set_text(&body_copy);
                                this.status_msg = Some((
                                    Tone::Success,
                                    this.t("已复制响应正文", "Copied response body").to_owned(),
                                ));
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            ),
            cx,
        ))
        .child(if entry.response_body_preview.is_empty() {
            empty_inline(
                view.t("响应正文为空", "Empty response body"),
                None,
                None,
                cx,
            )
        } else {
            div()
                .w_full()
                .p(px(10.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(display_body)
        });

    col.child(body_card).into_any_element()
}

fn render_headers_tab(
    view: &HistoryView,
    entry: &ExecutionHistoryEntry,
    cx: &mut Context<HistoryView>,
) -> AnyElement {
    let mut box_el = card(cx).p(px(12.0)).gap(px(5.0)).child(section_header(
        view.t("响应头（敏感值已脱敏）", "Response Headers (Redacted)"),
        Some(badge(entry.response_headers.len().to_string(), Tone::Neutral, cx).into_any_element()),
        cx,
    ));
    if entry.response_headers.is_empty() {
        box_el = box_el.child(empty_inline(
            view.t("未记录响应头", "No response headers recorded"),
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
    let p = palette(cx);
    let Some(sent) = &entry.sent_request else {
        return empty_inline(
            view.t(
                "此记录未包含实发请求快照",
                "No wire request snapshot stored",
            ),
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
                view.t(
                    "实际发送的请求（凭据已脱敏）",
                    "Sent Wire Request (Redacted)",
                ),
                None,
                cx,
            ))
            .child(kv_line(
                view.t("方法", "Method"),
                sent.method.clone(),
                120.0,
                cx,
            ))
            .child(kv_line("URL", sent.url.clone(), 120.0, cx)),
    );

    if !sent.headers.is_empty() {
        let mut h_card = card(cx).p(px(12.0)).gap(px(5.0)).child(section_header(
            view.t("请求头", "Request Headers"),
            None,
            cx,
        ));
        for h in &sent.headers {
            h_card = h_card.child(kv_line(h.key.clone(), h.value.clone(), 180.0, cx));
        }
        col = col.child(h_card);
    }

    if !sent.body_preview.is_empty() {
        col = col.child(
            card(cx)
                .p(px(12.0))
                .gap(px(6.0))
                .child(section_header(
                    view.t("请求正文预览", "Request Body Preview"),
                    None,
                    cx,
                ))
                .child(
                    div()
                        .w_full()
                        .p(px(10.0))
                        .rounded(px(6.0))
                        .bg(to_hsla(p.sunken))
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(sent.body_preview.clone()),
                ),
        );
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
            view.t(
                "此记录无请求定义快照",
                "No request definition snapshot stored",
            ),
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
            view.t("发送时的请求定义快照", "Request Definition Snapshot"),
            Some(
                btn_secondary(
                    "hist-snap-open",
                    view.t("载入到请求工作台", "Open in Requests"),
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
            view.t("名称", "Name"),
            snap.name.clone(),
            130.0,
            cx,
        ))
        .child(kv_line(
            view.t("方法", "Method"),
            format!("{:?}", snap.method).to_uppercase(),
            130.0,
            cx,
        ))
        .child(kv_line(
            view.t("原始模板 URL", "Template URL"),
            snap.url.clone(),
            130.0,
            cx,
        ))
        .child(kv_line(
            view.t("鉴权模式", "Auth"),
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
