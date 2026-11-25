//! Tab body renderers for `CallDetailSheet` (Overview, Merged Reply, Payloads, SSE Frames).

use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _, div, px};
use pingdeck_desktop::core::llm::capture::MergedReply;
use pingdeck_desktop::time::format_local_datetime;

use crate::ui::{
    Tone, alert_banner, badge, card, empty_inline, kv_line, meta_pill, palette, section_header,
    to_hsla, v_col,
};

use super::call_drawer::CallDetailSheet;
use super::state::{fmt_compact, fmt_cost_micro, fmt_latency};

pub fn render_overview_section(
    sheet: &CallDetailSheet,
    cx: &Context<CallDetailSheet>,
) -> AnyElement {
    let e = &sheet.detail.entry;
    let cost = e
        .cost_micro_usd
        .map(|m| fmt_cost_micro(m, &e.cost_currency))
        .unwrap_or_else(|| "—".to_owned());
    let mut col = v_col().w_full().gap(px(10.0));
    if let Some(err) = &e.error {
        col = col.child(alert_banner(Tone::Danger, err.clone(), None, cx));
    }
    col = col.child(
        card(cx)
            .p(px(12.0))
            .gap(px(5.0))
            .child(section_header(
                crate::ui::Tr(sheet.lang)
                    .usage()
                    .usage_call_drawer_tabs_summary(),
                None,
                cx,
            ))
            .child(kv_line(
                crate::ui::Tr(sheet.lang).usage().log_col_time(),
                format_local_datetime(e.ts_ms),
                120.0,
                cx,
            ))
            .child(kv_line(
                crate::ui::Tr(sheet.lang)
                    .usage()
                    .usage_call_drawer_tabs_provider_upstream(),
                format!(
                    "{} · {}",
                    e.provider_name.as_deref().unwrap_or("—"),
                    e.upstream_model.as_deref().unwrap_or(&e.requested_model)
                ),
                120.0,
                cx,
            ))
            .child(kv_line(
                crate::ui::Tr(sheet.lang).usage().log_more_protocol(),
                format!(
                    "{} → {}{}",
                    e.protocol_in,
                    e.protocol_out.as_deref().unwrap_or(&e.protocol_in),
                    if e.translated { " (transcoded)" } else { "" }
                ),
                120.0,
                cx,
            ))
            .child(kv_line(
                crate::ui::Tr(sheet.lang).usage().log_col_latency(),
                format!(
                    "TTFT {} · Total {}",
                    e.ttft_ms.map_or_else(|| "—".to_owned(), fmt_latency),
                    fmt_latency(e.latency_ms)
                ),
                120.0,
                cx,
            ))
            .child(kv_line(
                "Token",
                format!(
                    "in {} / out {} / cached {} / reasoning {}",
                    fmt_compact(e.input_tokens.unwrap_or(0)),
                    fmt_compact(e.output_tokens.unwrap_or(0)),
                    fmt_compact(e.cached_tokens.unwrap_or(0)),
                    fmt_compact(e.reasoning_tokens.unwrap_or(0)),
                ),
                120.0,
                cx,
            ))
            .child(kv_line(
                crate::ui::Tr(sheet.lang).usage().usage_breakdown_cost(),
                format!(
                    "{cost} ({})",
                    e.price_basis.as_deref().unwrap_or(&e.cost_currency)
                ),
                120.0,
                cx,
            )),
    );
    if let Some(arr) = e.attempts.as_array()
        && !arr.is_empty()
    {
        let mut att_card = card(cx).p(px(12.0)).gap(px(5.0)).child(section_header(
            crate::ui::Tr(sheet.lang)
                .usage()
                .usage_call_drawer_tabs_attempts_chain(),
            Some(badge(arr.len().to_string(), Tone::Neutral, cx).into_any_element()),
            cx,
        ));
        for (i, a) in arr.iter().enumerate() {
            att_card = att_card.child(kv_line(format!("#{}", i + 1), a.to_string(), 60.0, cx));
        }
        col = col.child(att_card);
    }
    col.into_any_element()
}

pub fn render_reply_section(sheet: &CallDetailSheet, cx: &Context<CallDetailSheet>) -> AnyElement {
    let Some(reply) = &sheet.detail.merged_reply else {
        return empty_inline(
            crate::ui::Tr(sheet.lang)
                .usage()
                .usage_call_drawer_tabs_no_captured_reply_for_this(),
            None,
            None,
            cx,
        )
        .into_any_element();
    };
    render_merged_reply_card(
        sheet,
        crate::ui::Tr(sheet.lang)
            .usage()
            .usage_call_drawer_tabs_client_reply(),
        reply,
        cx,
    )
}

fn render_merged_reply_card(
    sheet: &CallDetailSheet,
    title: &'static str,
    reply: &MergedReply,
    cx: &Context<CallDetailSheet>,
) -> AnyElement {
    let p = palette(cx);
    let mut c = card(cx)
        .p(px(12.0))
        .gap(px(8.0))
        .child(section_header(title, None, cx));
    if !reply.reasoning.is_empty() {
        c = c.child(
            v_col()
                .gap(px(4.0))
                .child(
                    div().text_xs().text_color(to_hsla(p.muted)).child(
                        crate::ui::Tr(sheet.lang)
                            .usage()
                            .usage_call_drawer_tabs_reasoning(),
                    ),
                )
                .child(
                    div()
                        .p(px(8.0))
                        .rounded(px(6.0))
                        .bg(to_hsla(p.sunken))
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(reply.reasoning.clone()),
                ),
        );
    }
    if !reply.text.is_empty() {
        c = c.child(
            div()
                .p(px(10.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.pane))
                .text_xs()
                .text_color(to_hsla(p.ink))
                .child(reply.text.clone()),
        );
    }
    for tc in &reply.tool_calls {
        c = c.child(kv_line(
            format!("tool: {}", tc.name),
            tc.arguments.clone(),
            140.0,
            cx,
        ));
    }
    c.into_any_element()
}

pub fn render_payloads_section(
    sheet: &CallDetailSheet,
    cx: &Context<CallDetailSheet>,
) -> AnyElement {
    let p = palette(cx);
    let Some(cap) = &sheet.detail.capture else {
        return empty_inline(
            crate::ui::Tr(sheet.lang)
                .usage()
                .usage_call_drawer_tabs_capture_was_off_or_deleted(),
            None,
            None,
            cx,
        )
        .into_any_element();
    };
    let mut col = v_col().w_full().gap(px(10.0)).child(kv_line(
        crate::ui::Tr(sheet.lang)
            .usage()
            .usage_call_drawer_tabs_upstream_url(),
        cap.upstream_url.clone(),
        110.0,
        cx,
    ));
    if !cap.translation_notes.is_empty() {
        let mut n_card = card(cx).p(px(10.0)).gap(px(4.0)).child(section_header(
            crate::ui::Tr(sheet.lang)
                .usage()
                .usage_call_drawer_tabs_translation_notes(),
            None,
            cx,
        ));
        for n in &cap.translation_notes {
            n_card = n_card.child(kv_line(
                format!("{:?} · {}", n.kind, n.field),
                n.reason.clone(),
                150.0,
                cx,
            ));
        }
        col = col.child(n_card);
    }
    for (title, body_opt) in [
        (
            crate::ui::Tr(sheet.lang)
                .usage()
                .usage_call_drawer_tabs_client_request_body(),
            &cap.client_req_body,
        ),
        (
            crate::ui::Tr(sheet.lang)
                .usage()
                .usage_call_drawer_tabs_upstream_request_body(),
            &cap.upstream_req_body,
        ),
        (
            crate::ui::Tr(sheet.lang)
                .usage()
                .usage_call_drawer_tabs_response_body(),
            &cap.client_resp_body,
        ),
    ] {
        if let Some(body) = body_opt
            && let Some(text) = &body.text
        {
            let preview: String = text.chars().take(4000).collect();
            col = col.child(
                card(cx)
                    .p(px(10.0))
                    .gap(px(6.0))
                    .child(section_header(title, None, cx))
                    .child(
                        div()
                            .w_full()
                            .p(px(8.0))
                            .rounded(px(6.0))
                            .bg(to_hsla(p.sunken))
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .child(preview),
                    ),
            );
        }
    }
    col.into_any_element()
}

pub fn render_frames_section(sheet: &CallDetailSheet, cx: &Context<CallDetailSheet>) -> AnyElement {
    let frames = sheet
        .detail
        .capture
        .as_ref()
        .and_then(|c| c.client_frames.as_ref().or(c.upstream_frames.as_ref()));
    let Some(frames) = frames else {
        return empty_inline(
            crate::ui::Tr(sheet.lang)
                .usage()
                .usage_call_drawer_tabs_no_sse_frames_captured(),
            None,
            None,
            cx,
        )
        .into_any_element();
    };
    let mut col = v_col().w_full().gap(px(4.0)).child(meta_pill(
        format!("{} / {} frames", frames.frames.len(), frames.total_frames),
        cx,
    ));
    for f in frames.frames.iter().take(120) {
        col = col.child(kv_line(
            format!("#{} +{}ms {}", f.index, f.t_ms, f.event),
            f.data.chars().take(300).collect::<String>(),
            150.0,
            cx,
        ));
    }
    col.into_any_element()
}
