//! Historical and live reasoning / thinking process extraction & collapsible cards for AI Chat.
//!
//! Handles plaintext reasoning (`HubPart::Reasoning.text` and inline `<think>` blocks), sealed
//! reasoning (`signature` / `encrypted` / `id` / `reasoning_tokens`), and clickable header chips
//! so users can inspect thinking on any completed or live turn. Sealed & no-reasoning diagnostic
//! cards live in [`super::reasoning_diag`].

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::model::AgentMessage;
use pingdeck_desktop::core::llm::wire::Protocol;
use pingdeck_desktop::core::model::hub::HubPart;

use crate::ui::{
    ScrollableElement as _, StatusShape, UiIcon, h_row, icon_xs, markdown_view, palette,
    status_indicator, streaming_markdown_view, to_hsla, v_col,
};

use super::ChatView;
use super::metrics::{extract_inline_thinking, format_duration_ms, format_tokens_int};

pub use super::reasoning_diag::{render_no_reasoning_panel, render_sealed_reasoning_card};

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
    pub has_signature: bool,
    pub has_encrypted: bool,
    pub has_item_id: bool,
    pub origin: Option<Protocol>,
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
    let mut has_signature = false;
    let mut has_encrypted = false;
    let mut has_item_id = false;
    let mut origin: Option<Protocol> = None;

    for part in &msg.parts {
        match part {
            HubPart::Reasoning {
                text,
                signature,
                encrypted,
                id,
                origin: part_origin,
            } => {
                if origin.is_none() {
                    origin = *part_origin;
                }
                if !text.trim().is_empty() {
                    reasoning_chunks.push(text.clone());
                }
                if signature.as_deref().is_some_and(|s| !s.is_empty()) {
                    has_signature = true;
                }
                if encrypted.as_deref().is_some_and(|s| !s.is_empty()) {
                    has_encrypted = true;
                }
                if id.as_deref().is_some_and(|s| !s.is_empty()) {
                    has_item_id = true;
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
    } else if has_signature || has_encrypted || has_item_id || reasoning_tokens.is_some() {
        ReasoningKind::Sealed
    } else {
        ReasoningKind::NoneReturned
    };

    (
        MessageReasoningInfo {
            kind,
            text,
            has_signature,
            has_encrypted,
            has_item_id,
            origin,
            reasoning_tokens,
            duration_ms,
        },
        text_chunks,
    )
}

/// Returns true if a message carries plaintext or sealed reasoning (eligible for bulk toggle).
#[must_use]
pub fn message_has_reasoning(msg: &AgentMessage) -> bool {
    extract_message_reasoning(msg, None).0.kind != ReasoningKind::NoneReturned
}

/// Compact clickable chip placed in the assistant message header row so the user can toggle the
/// thinking / process view of any completed turn directly from the header.
pub fn render_reasoning_header_chip(
    msg_id: &str,
    info: &MessageReasoningInfo,
    open: bool,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let r_key = format!("reason-{msg_id}");
    let label = match info.kind {
        ReasoningKind::Plaintext => {
            let chars = info.text.chars().count();
            match info.duration_ms {
                Some(ms) if zh => format!("思考了 {} · {chars} 字", format_duration_ms(ms)),
                Some(ms) => format!("Thought {} · {chars} chars", format_duration_ms(ms)),
                None if zh => format!("思考过程 · {chars} 字"),
                None => format!("Thinking · {chars} chars"),
            }
        }
        ReasoningKind::Sealed => match info.reasoning_tokens {
            Some(tok) if zh => format!("已思考 · {} tok", format_tokens_int(tok)),
            Some(tok) => format!("Reasoned · {} tok", format_tokens_int(tok)),
            None if zh => "已思考 · 摘要隐藏".to_owned(),
            None => "Reasoned (hidden)".to_owned(),
        },
        ReasoningKind::NoneReturned => {
            if zh {
                "无思考过程".to_owned()
            } else {
                "No thinking".to_owned()
            }
        }
    };

    let is_present = info.kind != ReasoningKind::NoneReturned;
    let bg = if open {
        to_hsla(p.accent_soft)
    } else if is_present {
        to_hsla(p.pane)
    } else {
        gpui_kit::transparent_black()
    };
    let fg = if open {
        to_hsla(p.accent_ink)
    } else if is_present {
        to_hsla(p.muted)
    } else {
        to_hsla(p.faint)
    };
    let border = if open {
        to_hsla(p.accent_line.composite_over(p.accent_soft))
    } else if is_present {
        to_hsla(p.line.composite_over(p.pane))
    } else {
        gpui_kit::transparent_black()
    };

    div()
        .id(SharedString::from(format!("hdr-chip-reason-{msg_id}")))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .px(px(7.0))
        .py(px(2.0))
        .rounded(px(6.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .cursor_pointer()
        .text_size(px(11.0))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(fg)
        .hover(|s| {
            s.bg(to_hsla(p.hover.composite_over(p.pane)))
                .text_color(to_hsla(p.ink_2))
        })
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_reasoning(r_key.clone(), cx)))
        .child(icon_xs(UiIcon::Brain))
        .child(SharedString::from(label))
        .child(icon_xs(if open {
            UiIcon::ChevronDown
        } else {
            UiIcon::ChevronRight
        }))
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
    zh: bool,
    on_toggle: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
    cx: &gpui_kit::App,
) -> impl IntoElement {
    let p = palette(cx);
    let char_count = text.chars().count();
    let tok_suffix =
        reasoning_tokens.map_or_else(String::new, |t| format!(" · {} tok", format_tokens_int(t)));
    let label = match (is_live, duration_ms) {
        (true, Some(ms)) if zh => {
            format!(
                "正在深度思考 · {} · {char_count} 字{tok_suffix}",
                format_duration_ms(ms)
            )
        }
        (true, Some(ms)) => {
            format!(
                "Thinking · {} · {char_count} chars{tok_suffix}",
                format_duration_ms(ms)
            )
        }
        (true, None) if zh => format!("正在深度思考 · {char_count} 字{tok_suffix}"),
        (true, None) => format!("Thinking · {char_count} chars{tok_suffix}"),
        (false, Some(ms)) if zh => {
            format!(
                "思考了 {} · {char_count} 字{tok_suffix}",
                format_duration_ms(ms)
            )
        }
        (false, Some(ms)) => {
            format!(
                "Thought for {} · {char_count} chars{tok_suffix}",
                format_duration_ms(ms)
            )
        }
        (false, None) if zh => format!("思考过程 · {char_count} 字{tok_suffix}"),
        (false, None) => format!("Reasoning · {char_count} chars{tok_suffix}"),
    };

    let preview = (!open).then(|| collapse_preview(&text, 96));
    let toggle_hint = if open {
        if zh { "收起" } else { "Hide" }
    } else if zh {
        "展开"
    } else {
        "View"
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
