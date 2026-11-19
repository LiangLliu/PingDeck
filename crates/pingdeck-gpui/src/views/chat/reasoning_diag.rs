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
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let r_key = format!("reason-{}", msg.id);
    let headline = match info.reasoning_tokens {
        Some(tok) if zh => format!(
            "已深度思考 · 消耗 {} reasoning tok（上游隐藏思考原文）",
            format_tokens_int(tok)
        ),
        Some(tok) => format!(
            "Reasoned · {} reasoning tok (summary omitted by upstream)",
            format_tokens_int(tok)
        ),
        None if info.has_signature && zh => "已深度思考 · 含思考签名（上游未返回明文摘要）".into(),
        None if info.has_encrypted && zh => "已深度思考 · 加密思考块（上游隐藏原文）".into(),
        None if zh => "已深度思考（上游未返回明文摘要）".into(),
        None => "Reasoned (summary omitted by upstream)".into(),
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
            signals.push(if zh {
                "思考签名 (signature)"
            } else {
                "signature"
            });
        }
        if info.has_encrypted {
            signals.push(if zh {
                "加密块 (redacted_thinking)"
            } else {
                "encrypted payload"
            });
        }
        if info.has_item_id {
            signals.push(if zh {
                "推理项 ID (reasoning item)"
            } else {
                "reasoning item id"
            });
        }
        if info.reasoning_tokens.is_some() {
            signals.push(if zh {
                "reasoning_tokens 统计"
            } else {
                "reasoning_tokens usage"
            });
        }

        let proto_str = info
            .origin
            .map(|pr| format_protocol_label(pr, zh))
            .unwrap_or_else(|| if zh { "未知" } else { "Unknown" }.into());

        let mut detail = v_col()
            .gap(px(6.0))
            .px(px(12.0))
            .py(px(9.0))
            .border_t_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(detail_kv(
                if zh { "检测信号" } else { "Signals" },
                signals.join(" · "),
                cx,
            ))
            .child(detail_kv(
                if zh { "响应协议" } else { "Protocol" },
                proto_str,
                cx,
            ));

        if let Some(eff) = msg.meta.reasoning_effort {
            detail = detail.child(detail_kv(
                if zh { "请求强度" } else { "Effort" },
                format_effort_short(eff, zh).to_owned(),
                cx,
            ));
        }

        detail = detail.child(
            div()
                .pt(px(2.0))
                .text_size(px(11.0))
                .text_color(to_hsla(p.faint))
                .child(if zh {
                    "模型已执行内部推理，但上游接口仅返回了签名/加密块或 token 统计，未下发明文思考摘要（例如 Claude 4.7+/5.x 未返回 summarized display、或上游中转隐藏了思考文本）。"
                } else {
                    "The model performed internal reasoning, but the upstream API returned only a signature/encrypted block or token count without a plaintext summary."
                }),
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
    zh: bool,
    cx: &mut Context<ChatView>,
) -> Div {
    let p = palette(cx);
    let session_effort = view.session.as_ref().and_then(|s| s.reasoning_effort);
    let msg_effort = msg.meta.reasoning_effort;

    let proto_label = provider_meta
        .and_then(|m| m.primary_protocol)
        .map(|pr| format_protocol_label(pr, zh))
        .unwrap_or_else(|| {
            if zh {
                "OpenAI 兼容接口（默认）".into()
            } else {
                "OpenAI-compatible (default)".into()
            }
        });

    let missing_quirk = provider_meta.is_some_and(ProviderChatMeta::missing_reasoning_quirk);
    let dialect_label = match provider_meta {
        Some(m) if m.primary_protocol == Some(Protocol::AnthropicMessages) => {
            if zh {
                "Anthropic 原生 thinking / effort".into()
            } else {
                "Anthropic native thinking / effort".into()
            }
        }
        Some(m) if m.primary_protocol == Some(Protocol::OpenaiResponses) => {
            if zh {
                "OpenAI Responses 原生 reasoning".into()
            } else {
                "OpenAI Responses native reasoning".into()
            }
        }
        Some(m) if m.primary_protocol == Some(Protocol::Gemini) => {
            if zh {
                "Gemini 原生 thinkingConfig".into()
            } else {
                "Gemini native thinkingConfig".into()
            }
        }
        Some(m) => match m.reasoning_request {
            Some(style) => format_reasoning_style_label(style),
            None if m.preset_id.is_some() => {
                if zh {
                    "跟随服务商预设".into()
                } else {
                    "Preset default".into()
                }
            }
            None => {
                if zh {
                    "未配置（自定义兼容接口默认不发送推理参数）".into()
                } else {
                    "Not configured (custom compat omits reasoning param)".into()
                }
            }
        },
        None => if zh { "未知" } else { "Unknown" }.into(),
    };

    let effort_label = match msg_effort.or(session_effort) {
        Some(eff) => format_effort_short(eff, zh).to_owned(),
        None if zh => "自动（未主动请求思考）".to_owned(),
        None => "Auto (no explicit reasoning requested)".to_owned(),
    };

    let timing_label = match (msg.meta.ttft_ms, msg.meta.stream_ms) {
        (Some(ttft), Some(stream)) => {
            let total = ttft.saturating_add(stream);
            if zh {
                format!(
                    "首字 {} · 生成 {} · 总耗时 {}",
                    format_duration_ms(ttft),
                    format_duration_ms(stream),
                    format_duration_ms(total),
                )
            } else {
                format!(
                    "TTFT {} · Gen {} · Total {}",
                    format_duration_ms(ttft),
                    format_duration_ms(stream),
                    format_duration_ms(total),
                )
            }
        }
        (Some(ttft), None) => format!("TTFT {}", format_duration_ms(ttft)),
        (None, Some(stream)) => format_duration_ms(stream),
        (None, None) => "—".to_owned(),
    };

    let reason_hint = if missing_quirk {
        if zh {
            "原因定位：当前自定义兼容服务商尚未设置「推理参数格式」，PingDeck 不会向该中转发送 reasoning / thinking 参数。请前往「服务商 › 配置 › 推理参数格式」选择对应方言（或切换为 Anthropic Messages 协议），并在下方将「推理」设为中/高。"
        } else {
            "Cause: this custom OpenAI-compatible provider has no Reasoning Format configured, so PingDeck omits reasoning params. Configure it under Providers › Config (or use Anthropic Messages) and pick Medium/High effort below."
        }
    } else if session_effort.is_none() {
        if zh {
            "原因定位：当前会话推理强度为「自动」，Claude 等模型在不传 thinking 参数时默认直接回复、不输出思考过程。开启下方「推理: 中」或「推理: 高」后，下一轮对话即可展示完整思考过程。"
        } else {
            "Cause: session effort is set to Auto, so models like Claude answer directly without thinking. Switch effort to Medium or High below to see reasoning on the next turn."
        }
    } else if zh {
        "上游本轮未返回 thinking / reasoning_content / <think> 内容。常见原因：问题较简单模型跳过思考、上游中转过滤了思考字段，或该模型不支持返回思考摘要。"
    } else {
        "Upstream returned no thinking / reasoning_content / <think> data. The model may have skipped thinking for a trivial prompt, or the relay stripped reasoning fields."
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
                        .child(if zh {
                            "本轮思考与调用诊断"
                        } else {
                            "Turn thinking & call diagnostics"
                        }),
                )
                .child(badge(
                    if zh {
                        "未返回思考"
                    } else {
                        "No reasoning"
                    },
                    if missing_quirk || session_effort.is_none() {
                        Tone::Warning
                    } else {
                        Tone::Neutral
                    },
                    cx,
                )),
        )
        .child(detail_kv(
            if zh { "目标模型" } else { "Model" },
            model_display.to_owned(),
            cx,
        ))
        .child(detail_kv(
            if zh { "服务商协议" } else { "Protocol" },
            proto_label,
            cx,
        ))
        .child(detail_kv(
            if zh { "推理方言" } else { "Dialect" },
            dialect_label,
            cx,
        ))
        .child(detail_kv(
            if zh { "推理强度" } else { "Effort" },
            effort_label,
            cx,
        ))
        .child(detail_kv(
            if zh { "调用耗时" } else { "Timing" },
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
                        if zh {
                            "开启推理: 中"
                        } else {
                            "Enable Effort: Med"
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_reasoning_effort(Some(ReasoningEffort::Medium), cx);
                    })),
                )
                .child(
                    btn_secondary(
                        high_id,
                        if zh {
                            "开启推理: 高"
                        } else {
                            "Enable Effort: High"
                        },
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

fn format_protocol_label(proto: Protocol, zh: bool) -> String {
    match proto {
        Protocol::OpenaiChat => {
            if zh {
                "OpenAI 兼容 (/chat/completions)".into()
            } else {
                "OpenAI Chat (/chat/completions)".into()
            }
        }
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
