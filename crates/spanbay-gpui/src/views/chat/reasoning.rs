//! Historical and live reasoning / thinking process extraction & collapsible cards for AI Chat.
//!
//! Handles plaintext reasoning (`HubPart::Reasoning.text` and inline `<think>` blocks) as a single
//! collapsible card per turn, and detects sealed reasoning (`signature` / `encrypted` / `id` /
//! `reasoning_tokens`) so the message statistics footer can surface it compactly without cluttering
//! the thread stream.

use gpui_kit::{
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::model::AgentMessage;
use spanbay_desktop::core::model::hub::HubPart;

use crate::ui::{
    ScrollableElement as _, StatusShape, UiIcon, h_row, icon_xs, markdown_view, palette,
    status_indicator, streaming_markdown_view, to_hsla, v_col,
};

use super::metrics::{extract_inline_thinking, format_duration_ms, format_tokens_int};

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

/// Extracts every reasoning signal (plaintext, inline `<think>`, signature, encrypted payload,
/// Responses item id, and `usage.reasoning_tokens`) alongside cleaned text parts.
pub fn extract_message_reasoning(
    msg: &AgentMessage,
    duration_ms: Option<u64>,
) -> (MessageReasoningInfo, Vec<String>) {
    let mut reasoning_chunks: Vec<String> = Vec::new();
    let mut text_chunks: Vec<String> = Vec::new();
    let mut has_sealed_signal = false;

    for part in &msg.parts {
        match part {
            HubPart::Reasoning {
                text,
                signature,
                encrypted,
                id,
                ..
            } => {
                if !text.trim().is_empty() {
                    reasoning_chunks.push(text.clone());
                }
                if signature.as_deref().is_some_and(|s| !s.is_empty())
                    || encrypted.as_deref().is_some_and(|s| !s.is_empty())
                    || id.as_deref().is_some_and(|s| !s.is_empty())
                {
                    has_sealed_signal = true;
                }
            }
            HubPart::Text { text, .. } if !text.is_empty() => {
                let (inline_r, cleaned) = extract_inline_thinking(text, false);
                reasoning_chunks.extend(inline_r);
                if !cleaned.is_empty() {
                    text_chunks.push(cleaned);
                }
            }
            _ => {}
        }
    }

    let reasoning_tokens = msg
        .usage
        .as_ref()
        .and_then(|u| u.reasoning_tokens)
        .filter(|&n| n > 0);
    let text = reasoning_chunks.join("\n\n");
    let kind = if !text.is_empty() {
        ReasoningKind::Plaintext
    } else if has_sealed_signal || reasoning_tokens.is_some() {
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
    extract_message_reasoning(msg, None).0.kind == ReasoningKind::Plaintext
}

/// Collapsible reasoning card for live streaming and completed plaintext thinking.
#[allow(clippy::too_many_arguments)]
pub fn render_reasoning_block(
    key: String,
    open: bool,
    is_live: bool,
    text: String,
    duration_ms: Option<u64>,
    reasoning_tokens: Option<u64>,
    on_toggle: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
    cx: &gpui_kit::App,
) -> impl IntoElement {
    let p = palette(cx);
    let char_count = text.chars().count();
    let tok_suffix =
        reasoning_tokens.map_or_else(String::new, |t| format!(" · {} tok", format_tokens_int(t)));
    let ch = crate::ui::tr(cx).chat();
    let label = match (is_live, duration_ms) {
        (true, Some(ms)) => ch.reason_live_ms(format_duration_ms(ms), char_count, &tok_suffix),
        (true, None) => ch.reason_live(char_count, &tok_suffix),
        (false, Some(ms)) => ch.reason_done_ms(format_duration_ms(ms), char_count, &tok_suffix),
        (false, None) => ch.reason_done(char_count, &tok_suffix),
    };

    let preview = (!open).then(|| collapse_preview(&text, 96));
    let toggle_hint = if open {
        crate::ui::tr(cx).chat().reasoning_hide()
    } else {
        crate::ui::tr(cx).chat().reasoning_view()
    };

    let header = div()
        .id(SharedString::from(format!("hdr-{key}")))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(6.0))
        .cursor_pointer()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(if is_live {
            to_hsla(p.accent_ink)
        } else {
            to_hsla(p.ink_2)
        })
        .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
        .on_click(on_toggle)
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(6.0))
                .child(if is_live {
                    status_indicator(StatusShape::Run, cx)
                } else {
                    div()
                        .text_color(to_hsla(p.muted))
                        .child(icon_xs(UiIcon::Brain))
                        .into_any_element()
                })
                .child(SharedString::from(label))
                .children(preview.filter(|s| !s.is_empty()).map(|excerpt| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl(px(4.0))
                        .text_size(px(11.0))
                        .font_weight(gpui_kit::FontWeight::NORMAL)
                        .text_color(to_hsla(p.faint))
                        .truncate()
                        .child(SharedString::from(excerpt))
                })),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(3.0))
                .text_size(px(11.0))
                .font_weight(gpui_kit::FontWeight::NORMAL)
                .text_color(to_hsla(p.faint))
                .child(toggle_hint)
                .child(icon_xs(if open {
                    UiIcon::ChevronDown
                } else {
                    UiIcon::ChevronRight
                })),
        );

    let mut box_el = v_col()
        .ml(px(30.0))
        .rounded(px(9.0))
        .overflow_hidden()
        .bg(if is_live {
            to_hsla(p.accent_soft)
        } else {
            to_hsla(p.pane)
        })
        .border_1()
        .border_color(if is_live {
            to_hsla(p.accent_line.composite_over(p.accent_soft))
        } else {
            to_hsla(p.line.composite_over(p.pane))
        })
        .child(header);

    if open {
        let md_id = SharedString::from(format!("reason-md-{key}"));
        let content_el: gpui_kit::AnyElement = if is_live {
            streaming_markdown_view(md_id, text).into_any_element()
        } else {
            markdown_view(md_id, text).into_any_element()
        };
        box_el = box_el.child(
            div()
                .w_full()
                .max_h(px(360.0))
                .overflow_y_scrollbar()
                .id(SharedString::from(format!("reason-body-{key}")))
                .px(px(12.0))
                .py(px(8.0))
                .border_t_1()
                .border_color(if is_live {
                    to_hsla(p.accent_line.composite_over(p.accent_soft))
                } else {
                    to_hsla(p.line.composite_over(p.pane))
                })
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(content_el),
        );
    }

    box_el
}

fn collapse_preview(text: &str, max_chars: usize) -> String {
    let compact: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}
