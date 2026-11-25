//! Chat metrics formatting, cost/speed calculation, and inline `<think>` tag extraction
//! for AI Chat messages and live streaming turns.

use gpui_kit::{IntoElement as _, ParentElement as _, SharedString, Styled as _, px};
use pingdeck_desktop::core::agent::model::{AgentMessage, AgentStopReason, ReasoningEffort};

use crate::ui::{Tone, UiIcon, badge, h_row, kv_chip, metric_datum};

/// Extracts inline `<think>...</think>` or `<thinking>...</thinking>` blocks from text
/// (common on OpenAI-compatible relays that embed thinking inside `content`).
/// Returns `(reasoning_segments, cleaned_answer_text)`.
pub fn extract_inline_thinking(raw: &str, streaming: bool) -> (Vec<String>, String) {
    let mut reasoning = Vec::new();
    let mut answer = String::with_capacity(raw.len());
    let mut rest = raw;

    while !rest.is_empty() {
        let next_open = find_open_tag(rest);
        let Some((open_idx, open_len, close_tag)) = next_open else {
            answer.push_str(rest);
            break;
        };
        answer.push_str(&rest[..open_idx]);
        let after_open = &rest[open_idx + open_len..];
        if let Some(close_idx) = after_open.find(close_tag) {
            let inner = after_open[..close_idx].trim();
            if !inner.is_empty() {
                reasoning.push(inner.to_owned());
            }
            rest = &after_open[close_idx + close_tag.len()..];
        } else if streaming {
            let inner = after_open.trim_start();
            if !inner.is_empty() {
                reasoning.push(inner.to_owned());
            }
            break;
        } else {
            // Unclosed tag on a finished message: still preserve as answer so text is never lost.
            answer.push_str(&rest[open_idx..]);
            break;
        }
    }

    (reasoning, answer.trim().to_owned())
}

fn find_open_tag(s: &str) -> Option<(usize, usize, &'static str)> {
    let a = s.find("<think>").map(|i| (i, "<think>".len(), "</think>"));
    let b = s
        .find("<thinking>")
        .map(|i| (i, "<thinking>".len(), "</thinking>"));
    match (a, b) {
        (Some(x), Some(y)) => Some(if x.0 <= y.0 { x } else { y }),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

/// Formats millisecond durations cleanly: `<1000ms` as `420ms`, `>=1000ms` as `2.35s`.
#[must_use]
pub fn format_duration_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.2}s", ms as f64 / 1_000.0)
    } else {
        let secs = ms / 1_000;
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// Formats token counts compactly (`<1000` as exact integer, `>=1000` as `12.4K` / `1.2M`) so
/// long sessions never overflow the per-message statistics footer.
#[must_use]
pub fn format_tokens_compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else if n >= 1_000 {
        format!("{:.2}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Formats token counts with thousands separators (e.g. `7,199`).
#[must_use]
pub fn format_tokens_int(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(b as char);
    }
    out
}

/// Formats micro-USD (or micro-currency) cost cleanly.
#[must_use]
pub fn format_cost_micro(micro: i64, currency: Option<&str>) -> Option<String> {
    if micro <= 0 {
        return None;
    }
    let val = micro as f64 / 1_000_000.0;
    let sym = match currency {
        Some("CNY" | "RMB") => "¥",
        Some("EUR") => "€",
        Some("GBP") => "£",
        Some("JPY") => "¥",
        _ => "$",
    };
    if val < 0.0001 {
        Some(format!("{sym}{val:.6}"))
    } else if val < 0.01 {
        Some(format!("{sym}{val:.4}"))
    } else {
        Some(format!("{sym}{val:.3}"))
    }
}

#[must_use]
pub const fn format_effort_short(effort: ReasoningEffort, zh: bool) -> &'static str {
    match effort {
        ReasoningEffort::Minimal => {
            if zh {
                "推理: 极简"
            } else {
                "Effort: Min"
            }
        }
        ReasoningEffort::Low => {
            if zh {
                "推理: 低"
            } else {
                "Effort: Low"
            }
        }
        ReasoningEffort::Medium => {
            if zh {
                "推理: 中"
            } else {
                "Effort: Med"
            }
        }
        ReasoningEffort::High => {
            if zh {
                "推理: 高"
            } else {
                "Effort: High"
            }
        }
        ReasoningEffort::Max => {
            if zh {
                "推理: 最高"
            } else {
                "Effort: Max"
            }
        }
    }
}

/// Renders a concise, overflow-proof per-message statistics footer below an assistant reply
/// using monochrome theme-adaptive vector icons for the 6 essential metrics:
/// Input (`CornerDownRight`), Output (`CornerUpRight`), Context (`Database`),
/// Duration (`Timer`), Speed (`Zap`), and Cost (`Coins`), each with a full-detail hover tooltip.
pub fn render_message_footer(
    msg: &AgentMessage,
    context_limit_tokens: Option<u64>,
    fallback_currency: Option<&str>,
    zh: bool,
    cx: &gpui_kit::App,
) -> Option<gpui_kit::Div> {
    let mut chips: Vec<gpui_kit::AnyElement> = Vec::new();
    let mut datums: Vec<gpui_kit::AnyElement> = Vec::new();
    let mid = &msg.id;

    for src in msg.meta.sources.iter().take(4) {
        chips.push(
            kv_chip(format!("[{}]", src.source_id), src.title.clone(), cx).into_any_element(),
        );
    }

    let mut out_tok_val = 0u64;
    if let Some(u) = &msg.usage {
        let in_tok = u.input_tokens.unwrap_or(0);
        let out_tok = u.output_tokens.unwrap_or(0);
        out_tok_val = out_tok;

        if in_tok > 0 || out_tok > 0 {
            datums.push(
                metric_datum(
                    SharedString::from(format!("stat-in-{mid}")),
                    UiIcon::CornerDownRight,
                    format_tokens_compact(in_tok),
                    if zh {
                        format!("输入 Token: {}", format_tokens_int(in_tok))
                    } else {
                        format!("Input tokens: {}", format_tokens_int(in_tok))
                    },
                    Tone::Neutral,
                    cx,
                )
                .into_any_element(),
            );
            datums.push(
                metric_datum(
                    SharedString::from(format!("stat-out-{mid}")),
                    UiIcon::CornerUpRight,
                    format_tokens_compact(out_tok),
                    if zh {
                        format!("输出 Token: {}", format_tokens_int(out_tok))
                    } else {
                        format!("Output tokens: {}", format_tokens_int(out_tok))
                    },
                    Tone::Neutral,
                    cx,
                )
                .into_any_element(),
            );
        }

        // Context size sent on this turn (input tokens vs model window limit when known).
        // Escalates to theme-aware Warning (>=70%) and Danger (>=85%) tones as the window fills.
        if in_tok > 0 {
            let (ctx_val, ctx_tip, ctx_tone) = match context_limit_tokens.filter(|&lim| lim > 0) {
                Some(lim) => {
                    let pct = ((in_tok as f64 / lim as f64) * 100.0).min(100.0);
                    let tone = if pct >= 85.0 {
                        Tone::Danger
                    } else if pct >= 70.0 {
                        Tone::Warning
                    } else {
                        Tone::Neutral
                    };
                    let val = format!(
                        "{}/{} ({:.0}%)",
                        format_tokens_compact(in_tok),
                        format_tokens_compact(lim),
                        pct,
                    );
                    let tip = if zh {
                        format!(
                            "上下文窗口: {} / {} ({:.1}%)",
                            format_tokens_int(in_tok),
                            format_tokens_int(lim),
                            pct,
                        )
                    } else {
                        format!(
                            "Context window: {} / {} ({:.1}%)",
                            format_tokens_int(in_tok),
                            format_tokens_int(lim),
                            pct,
                        )
                    };
                    (val, tip, tone)
                }
                None => (
                    format_tokens_compact(in_tok),
                    if zh {
                        format!("上下文窗口: {} token", format_tokens_int(in_tok))
                    } else {
                        format!("Context window: {} tokens", format_tokens_int(in_tok))
                    },
                    Tone::Neutral,
                ),
            };
            datums.push(
                metric_datum(
                    SharedString::from(format!("stat-ctx-{mid}")),
                    UiIcon::Database,
                    ctx_val,
                    ctx_tip,
                    ctx_tone,
                    cx,
                )
                .into_any_element(),
            );
        }
    }

    let total_ms = match (msg.meta.ttft_ms, msg.meta.stream_ms) {
        (Some(ttft), Some(stream)) => Some(ttft.saturating_add(stream)),
        (Some(ttft), None) => Some(ttft),
        (None, Some(stream)) => Some(stream),
        (None, None) => None,
    };
    if let Some(ms) = total_ms.filter(|&v| v > 0) {
        let dur_val = format_duration_ms(ms);
        let dur_tip = match (msg.meta.ttft_ms, msg.meta.stream_ms) {
            (Some(ttft), Some(stream)) if ttft > 0 && stream > 0 => {
                if zh {
                    format!(
                        "总耗时: {dur_val}（首字 {} · 生成 {}）",
                        format_duration_ms(ttft),
                        format_duration_ms(stream),
                    )
                } else {
                    format!(
                        "Duration: {dur_val} (TTFT {} · stream {})",
                        format_duration_ms(ttft),
                        format_duration_ms(stream),
                    )
                }
            }
            _ => {
                if zh {
                    format!("总耗时: {dur_val}")
                } else {
                    format!("Duration: {dur_val}")
                }
            }
        };
        datums.push(
            metric_datum(
                SharedString::from(format!("stat-dur-{mid}")),
                UiIcon::Timer,
                dur_val,
                dur_tip,
                Tone::Neutral,
                cx,
            )
            .into_any_element(),
        );
    }

    if out_tok_val > 0
        && let Some(stream_ms) = msg.meta.stream_ms.filter(|&ms| ms > 0)
    {
        let tps = out_tok_val as f64 / (stream_ms as f64 / 1_000.0);
        datums.push(
            metric_datum(
                SharedString::from(format!("stat-tps-{mid}")),
                UiIcon::Zap,
                format!("{tps:.1}/s"),
                if zh {
                    format!("生成速度: {tps:.1} tok/s")
                } else {
                    format!("Output speed: {tps:.1} tok/s")
                },
                Tone::Neutral,
                cx,
            )
            .into_any_element(),
        );
    }

    let currency = msg.cost_currency.as_deref().or(fallback_currency);
    if let Some(cost_str) = format_cost_micro(msg.cost_micro_usd, currency) {
        datums.push(
            metric_datum(
                SharedString::from(format!("stat-cost-{mid}")),
                UiIcon::Coins,
                cost_str.clone(),
                if zh {
                    format!("估算费用: {cost_str}")
                } else {
                    format!("Estimated cost: {cost_str}")
                },
                Tone::Neutral,
                cx,
            )
            .into_any_element(),
        );
    }

    if let Some(stop) = msg.meta.stop_reason
        && !matches!(stop, AgentStopReason::Completed)
    {
        let label = match stop {
            AgentStopReason::MaxIterations => {
                if zh {
                    "达步数上限"
                } else {
                    "Max steps"
                }
            }
            AgentStopReason::CostLimit => {
                if zh {
                    "达费用上限"
                } else {
                    "Cost cap"
                }
            }
            AgentStopReason::OutputLimit => {
                if zh {
                    "达输出上限"
                } else {
                    "Output limit"
                }
            }
            AgentStopReason::ContextFull => {
                if zh {
                    "上下文已满"
                } else {
                    "Context full"
                }
            }
            AgentStopReason::NoProgress => {
                if zh {
                    "无进展停止"
                } else {
                    "No progress"
                }
            }
            AgentStopReason::Completed => "",
        };
        if !label.is_empty() {
            chips.push(badge(label, Tone::Warning, cx).into_any_element());
        }
    }

    if chips.is_empty() && datums.is_empty() {
        return None;
    }

    Some(
        h_row()
            .pl(px(30.0))
            .pt(px(2.0))
            .gap_x(px(12.0))
            .gap_y(px(4.0))
            .flex_wrap()
            .children(chips)
            .children(datums),
    )
}
