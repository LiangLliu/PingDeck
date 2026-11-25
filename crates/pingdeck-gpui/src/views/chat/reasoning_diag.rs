//! Sealed-reasoning and no-reasoning diagnostic cards for completed AI Chat turns.
//!
//! Companion to `reasoning.rs` so every file stays concise and focused.

use gpui_kit::{
    Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::model::{AgentMessage, ReasoningEffort};
use pingdeck_desktop::core::llm::wire::{Protocol, ReasoningStyle};

use crate::ui::{Tone, UiIcon, badge, btn_secondary, h_row, icon_xs, palette, to_hsla, v_col};

use super::metrics::{format_duration_ms, format_effort_short, format_tokens_int};
use super::reasoning::MessageReasoningInfo;
use super::{ChatView, ProviderChatMeta};

/// Collapsible card for completed turns where the model performed thinking (signature /
/// encrypted block / Responses item id / reasoning_tokens) without plaintext summary text.
pub fn render_sealed_reasoning_card(
    msg: &AgentMessage,
    info: &MessageReasoningInfo,
    open: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let r_key = format!("reason-{}", msg.id);
    let ch = crate::ui::tr(cx).chat();
    let headline = match info.reasoning_tokens {
        Some(tok) => ch.diag_sealed_tok(format_tokens_int(tok)),
        None if info.has_signature => ch.diag_sealed_sig().into(),
        None if info.has_encrypted => ch.diag_sealed_redacted().into(),
        None => ch.diag_sealed_generic().into(),
    };

    let header = div()
        .id(SharedString::from(format!("hdr-sealed-{}", msg.id)))
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
        .text_color(to_hsla(p.muted))
        .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_reasoning(r_key.clone(), cx)))
        .child(
            h_row()
                .gap(px(6.0))
                .child(icon_xs(UiIcon::Brain))
                .child(SharedString::from(headline)),
        )
        .child(icon_xs(if open {
            UiIcon::ChevronDown
        } else {
            UiIcon::ChevronRight
        }));

    let mut box_el = v_col()
        .ml(px(30.0))
        .rounded(px(9.0))
        .overflow_hidden()
        .bg(to_hsla(p.pane))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(header);

    if open {
        let mut signals: Vec<&str> = Vec::new();
        if info.has_signature {
            signals.push(crate::ui::tr(cx).chat().reasoning_diag_signature());
        }
        let lang = crate::ui::active_language(cx);
        if info.has_encrypted {
            signals.push(ch.diag_redacted_payload());
        }
        if info.has_item_id {
            signals.push(ch.reasoning_diag_reasoning_item_id());
        }
        if info.reasoning_tokens.is_some() {
            signals.push(ch.reasoning_diag_reasoning_tokens_usage());
        }

        let proto_str = info
            .origin
            .map(|pr| format_protocol_label(pr, lang))
            .unwrap_or_else(|| ch.panel_mcp_unknown().into());

        let mut detail = v_col()
            .gap(px(6.0))
            .px(px(12.0))
            .py(px(9.0))
            .border_t_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(detail_kv(
                ch.reasoning_diag_signals(),
                signals.join(" · "),
                cx,
            ))
            .child(detail_kv(ch.reasoning_diag_protocol(), proto_str, cx));

        if let Some(eff) = msg.meta.reasoning_effort {
            detail = detail.child(detail_kv(
                ch.reasoning_diag_effort(),
                format_effort_short(eff, lang).to_owned(),
                cx,
            ));
        }

        detail = detail.child(
            div()
                .pt(px(2.0))
                .text_size(px(11.0))
                .text_color(to_hsla(p.faint))
                .child(ch.diag_sealed_explain()),
        );
        box_el = box_el.child(detail);
    }

    box_el
}

/// Inline diagnostic panel shown when the user clicks "无思考过程" on a completed turn that
/// received no reasoning parts from upstream. Explains the exact request/provider state and
/// offers one-click effort shortcuts.
pub fn render_no_reasoning_panel(
    view: &ChatView,
    msg: &AgentMessage,
    model_display: &str,
    provider_meta: Option<&ProviderChatMeta>,
    cx: &mut Context<ChatView>,
) -> Div {
    let p = palette(cx);
    let session_effort = view.session.as_ref().and_then(|s| s.reasoning_effort);
    let msg_effort = msg.meta.reasoning_effort;

    let lang = crate::ui::active_language(cx);
    let proto_label = provider_meta
        .and_then(|m| m.primary_protocol)
        .map(|pr| format_protocol_label(pr, lang))
        .unwrap_or_else(|| {
            crate::ui::tr(cx)
                .chat()
                .reasoning_diag_openai_compatible_default()
                .into()
        });

    let missing_quirk = provider_meta.is_some_and(ProviderChatMeta::missing_reasoning_quirk);
    let dialect_label = match provider_meta {
        Some(m) if m.primary_protocol == Some(Protocol::AnthropicMessages) => crate::ui::tr(cx)
            .chat()
            .reasoning_diag_anthropic_native_thinking_effort()
            .into(),
        Some(m) if m.primary_protocol == Some(Protocol::OpenaiResponses) => crate::ui::tr(cx)
            .chat()
            .reasoning_diag_openai_responses_native_reasoning()
            .into(),
        Some(m) if m.primary_protocol == Some(Protocol::Gemini) => crate::ui::tr(cx)
            .chat()
            .reasoning_diag_gemini_native_thinkingconfig()
            .into(),
        Some(m) => match m.reasoning_request {
            Some(style) => format_reasoning_style_label(style),
            None if m.preset_id.is_some() => crate::ui::tr(cx)
                .chat()
                .reasoning_diag_preset_default()
                .into(),
            None => crate::ui::tr(cx)
                .chat()
                .reasoning_diag_not_configured_custom_compat_omits()
                .into(),
        },
        None => crate::ui::tr(cx).chat().panel_mcp_unknown().into(),
    };

    let effort_label = match msg_effort.or(session_effort) {
        Some(eff) => format_effort_short(eff, lang).to_owned(),
        None => crate::ui::tr(cx).chat().diag_effort_auto().to_owned(),
    };

    let timing_label = match (msg.meta.ttft_ms, msg.meta.stream_ms) {
        (Some(ttft), Some(stream)) => {
            let total = ttft.saturating_add(stream);
            crate::ui::tr(cx).chat().reasoning_diag_ttft_gen_total(
                format_duration_ms(ttft),
                format_duration_ms(stream),
                format_duration_ms(total),
            )
        }
        (Some(ttft), None) => format!("TTFT {}", format_duration_ms(ttft)),
        (None, Some(stream)) => format_duration_ms(stream),
        (None, None) => "—".to_owned(),
    };

    let reason_hint = if missing_quirk {
        crate::ui::tr(cx)
            .chat()
            .reasoning_diag_cause_this_custom_openai_compatible()
    } else if session_effort.is_none() {
        crate::ui::tr(cx)
            .chat()
            .reasoning_diag_cause_session_effort_is_set()
    } else {
        crate::ui::tr(cx)
            .chat()
            .reasoning_diag_upstream_returned_no_thinking_reasoning()
    };

    let mut card = v_col()
        .ml(px(30.0))
        .rounded(px(9.0))
        .bg(to_hsla(p.pane))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .px(px(12.0))
        .py(px(10.0))
        .gap(px(6.0))
        .child(
            h_row()
                .justify_between()
                .gap(px(6.0))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink_2))
                        .child(icon_xs(UiIcon::Brain))
                        .child(
                            crate::ui::tr(cx)
                                .chat()
                                .reasoning_diag_turn_thinking_call_diagnostics(),
                        ),
                )
                .child(badge(
                    crate::ui::tr(cx).chat().reasoning_diag_no_reasoning(),
                    if missing_quirk || session_effort.is_none() {
                        Tone::Warning
                    } else {
                        Tone::Neutral
                    },
                    cx,
                )),
        )
        .child(detail_kv(
            crate::ui::tr(cx).chat().reasoning_diag_model(),
            model_display.to_owned(),
            cx,
        ))
        .child(detail_kv(
            crate::ui::tr(cx).chat().reasoning_diag_protocol_2(),
            proto_label,
            cx,
        ))
        .child(detail_kv(
            crate::ui::tr(cx).chat().reasoning_diag_dialect(),
            dialect_label,
            cx,
        ))
        .child(detail_kv(
            crate::ui::tr(cx).chat().model_effort(),
            effort_label,
            cx,
        ))
        .child(detail_kv(
            crate::ui::tr(cx).chat().reasoning_diag_timing(),
            timing_label,
            cx,
        ))
        .child(
            div()
                .pt(px(4.0))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .child(reason_hint),
        );

    if session_effort.is_none() {
        let mid_id = SharedString::from(format!("reason-enable-med-{}", msg.id));
        let high_id = SharedString::from(format!("reason-enable-high-{}", msg.id));
        card = card.child(
            h_row()
                .pt(px(2.0))
                .gap(px(6.0))
                .child(
                    btn_secondary(
                        mid_id,
                        crate::ui::tr(cx).chat().reasoning_diag_enable_effort_med(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_reasoning_effort(Some(ReasoningEffort::Medium), cx);
                    })),
                )
                .child(
                    btn_secondary(
                        high_id,
                        crate::ui::tr(cx).chat().reasoning_diag_enable_effort_high(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_reasoning_effort(Some(ReasoningEffort::High), cx);
                    })),
                ),
        );
    }

    card
}

fn detail_kv(label: &'static str, value: String, cx: &gpui_kit::App) -> Div {
    let p = palette(cx);
    h_row()
        .gap(px(10.0))
        .text_size(px(11.0))
        .child(
            div()
                .w(px(68.0))
                .flex_shrink_0()
                .text_color(to_hsla(p.faint))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(value)),
        )
}

fn format_protocol_label(
    proto: Protocol,
    lang: pingdeck_desktop::core::model::settings::Language,
) -> String {
    match proto {
        Protocol::OpenaiChat => crate::ui::Tr(lang)
            .chat()
            .reasoning_diag_openai_chat_chat_completions()
            .into(),
        Protocol::OpenaiResponses => "OpenAI Responses (/responses)".into(),
        Protocol::AnthropicMessages => "Anthropic Messages (/v1/messages)".into(),
        Protocol::Gemini => "Google Gemini".into(),
        other => other.as_str().to_owned(),
    }
}

fn format_reasoning_style_label(style: ReasoningStyle) -> String {
    match style {
        ReasoningStyle::ReasoningEffort => "reasoning_effort".into(),
        ReasoningStyle::ReasoningObject => "reasoning { effort }".into(),
        ReasoningStyle::ThinkingType => "thinking { type: enabled }".into(),
        ReasoningStyle::EnableThinking => "enable_thinking".into(),
        ReasoningStyle::ChatTemplateKwargs => "chat_template_kwargs".into(),
        ReasoningStyle::AnthropicAdaptive => "Anthropic adaptive".into(),
        ReasoningStyle::GeminiThinkingLevel => "Gemini thinkingLevel".into(),
        ReasoningStyle::None => "None (disabled)".into(),
    }
}
