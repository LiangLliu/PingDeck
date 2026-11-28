//! Historical and live reasoning / thinking process extraction & borderless collapsible rows for AI Chat.
//!
//! Renders plaintext reasoning (`HubPart::Reasoning.text` and inline `<think>` blocks) as a single
//! borderless spine row (`● 思考了 Ns ›` / `◜ 思考中… ›`) with a trailing chevron and left-bordered
//! expandable Markdown body (`docs/design.md`, `.overhaul/chat-redesign-spec.md` §3.2 & §4).

use gpui_kit::{
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::model::{AgentMessage, AgentRole};
use spanbay_desktop::state::extract_message_reasoning_parts;

use crate::ui::{
    ScrollableElement as _, StatusShape, UiIcon, h_row, icon_xs, markdown_view, palette,
    status_indicator, streaming_markdown_view, to_hsla, v_col,
};

use super::metrics::format_duration_ms;

/// High-level reasoning availability for a stored assistant message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningKind {
    /// Model returned readable thinking text (`HubPart::Reasoning.text` or `<think>`).
    Plaintext,
    /// Model performed thinking (`signature` / `encrypted` / `id` / `reasoning_tokens`), but
    /// upstream omitted or redacted plaintext summary.
    Sealed,
    /// No reasoning content or metadata was returned by upstream for this turn.
    NoneReturned,
}

/// Extracted reasoning & cleaned answer text for one persisted assistant message.
#[derive(Debug, Clone)]
pub struct MessageReasoningInfo {
    pub kind: ReasoningKind,
    pub text: String,
    pub reasoning_tokens: Option<u64>,
    pub duration_ms: Option<u64>,
}

/// Extracts every reasoning signal alongside cleaned text parts by delegating to
/// [`extract_message_reasoning_parts`].
pub fn extract_message_reasoning(
    msg: &AgentMessage,
    duration_ms: Option<u64>,
) -> (MessageReasoningInfo, Vec<String>) {
    let (text, sealed, reasoning_tokens, text_chunks) = extract_message_reasoning_parts(msg);
    let kind = if !text.is_empty() {
        ReasoningKind::Plaintext
    } else if sealed {
        ReasoningKind::Sealed
    } else {
        ReasoningKind::NoneReturned
    };
    (
        MessageReasoningInfo {
            kind,
            text,
            reasoning_tokens,
            duration_ms,
        },
        text_chunks,
    )
}

/// Returns true if a message carries collapsible plaintext thinking (eligible for bulk toggle).
#[must_use]
pub fn message_has_reasoning(msg: &AgentMessage) -> bool {
    msg.role == AgentRole::Assistant
        && extract_message_reasoning(msg, None).0.kind == ReasoningKind::Plaintext
}

/// Renders a single-line borderless sealed-reasoning row (`● 模型已思考（上游未返回明文摘要）`).
pub fn render_sealed_reasoning_row(key: &str, cx: &gpui_kit::App) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    h_row()
        .id(SharedString::from(format!("hdr-{key}")))
        .w_full()
        .gap(px(8.0))
        .px(px(6.0))
        .py(px(3.0))
        .rounded(px(5.0))
        .text_xs()
        .text_color(to_hsla(p.muted))
        .child(status_indicator(StatusShape::Ok, cx))
        .child(SharedString::from(
            ch.metrics_reasoning_sealed_no_tok().to_owned(),
        ))
}

/// Borderless single-line collapsible reasoning row (`● 思考了 6s ›` / `◜ 思考中… 3s ›`)
/// expanding into a left-bordered Markdown block without heavy card chrome or truncated previews.
#[allow(clippy::too_many_arguments)]
pub fn render_reasoning_block(
    key: String,
    open: bool,
    is_live: bool,
    text: String,
    duration_ms: Option<u64>,
    on_toggle: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
    cx: &gpui_kit::App,
) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let label = if is_live {
        match duration_ms.filter(|&ms| ms > 0) {
            Some(ms) => format!("{} {}", ch.thread_thinking(), format_duration_ms(ms)),
            None => ch.thread_thinking().to_owned(),
        }
    } else if let Some(ms) = duration_ms.filter(|&ms| ms > 0) {
        ch.reason_thought(format_duration_ms(ms))
    } else {
        ch.graph_inspector_reasoning().to_owned()
    };

    let toggle_tip = if open {
        ch.reasoning_hide()
    } else {
        ch.reasoning_view()
    };

    let header = div()
        .id(SharedString::from(format!("hdr-{key}")))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .px(px(6.0))
        .py(px(3.0))
        .rounded(px(5.0))
        .cursor_pointer()
        .text_xs()
        .text_color(if is_live {
            to_hsla(p.accent_ink)
        } else {
            to_hsla(p.muted)
        })
        .hover(|s| {
            s.bg(to_hsla(p.hover.composite_over(p.surface)))
                .text_color(to_hsla(p.ink))
        })
        .tooltip(crate::ui::text_tooltip(toggle_tip))
        .on_click(on_toggle)
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(7.0))
                .child(status_indicator(
                    if is_live {
                        StatusShape::Run
                    } else {
                        StatusShape::Ok
                    },
                    cx,
                ))
                .child(div().truncate().child(SharedString::from(label))),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_color(to_hsla(p.faint))
                .child(icon_xs(if open {
                    UiIcon::ChevronDown
                } else {
                    UiIcon::ChevronRight
                })),
        );

    let mut col = v_col().w_full().gap(px(2.0)).child(header);

    if open && !text.trim().is_empty() {
        let md_id = SharedString::from(format!("reason-md-{key}"));
        let content_el: gpui_kit::AnyElement = if is_live {
            streaming_markdown_view(md_id, text).into_any_element()
        } else {
            markdown_view(md_id, text).into_any_element()
        };
        col = col.child(
            div()
                .id(SharedString::from(format!("reason-body-{key}")))
                .w_full()
                .max_h(px(320.0))
                .overflow_y_scrollbar()
                .ml(px(9.0))
                .pl(px(10.0))
                .py(px(4.0))
                .border_l_2()
                .border_color(to_hsla(if is_live {
                    p.accent_line.composite_over(p.surface)
                } else {
                    p.line_strong.composite_over(p.surface)
                }))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(content_el),
        );
    }

    col
}
