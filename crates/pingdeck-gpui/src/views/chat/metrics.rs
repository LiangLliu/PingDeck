//! Chat metrics formatting, cost/speed calculation, and inline `<think>` tag extraction
//! for AI Chat messages and live streaming turns.

use gpui_kit::{IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::model::{AgentMessage, AgentStopReason, ReasoningEffort};

use crate::ui::{Tone, badge, h_row, kv_chip, palette, to_hsla};

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

/// Renders the complete per-request statistics footer below an assistant message:
/// sources, token breakdown (input/output/cached/cache-write/reasoning/total),
/// timing (TTFT / stream / total / tok/s), cost, and non-normal stop reasons.
pub fn render_message_footer(
    msg: &AgentMessage,
    fallback_currency: Option<&str>,
    zh: bool,
    cx: &gpui_kit::App,
) -> Option<gpui_kit::Div> {
    let p = palette(cx);
    let mut chips: Vec<gpui_kit::AnyElement> = Vec::new();
    let mut stat_parts: Vec<String> = Vec::new();

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
        let total = in_tok + out_tok;
        if total > 0 {
            if zh {
                stat_parts.push(format!(
                    "输入 {} · 输出 {} · 共 {} tok",
                    format_tokens_int(in_tok),
                    format_tokens_int(out_tok),
                    format_tokens_int(total),
                ));
            } else {
                stat_parts.push(format!(
                    "In {} · Out {} · {} tok",
                    format_tokens_int(in_tok),
                    format_tokens_int(out_tok),
                    format_tokens_int(total),
                ));
            }
        }
        if let Some(cached) = u.cached_input_tokens.filter(|&n| n > 0) {
            stat_parts.push(if zh {
                format!("缓存读 {}", format_tokens_int(cached))
            } else {
                format!("Cached {}", format_tokens_int(cached))
            });
        }
        if let Some(cw) = u.cache_write_tokens.filter(|&n| n > 0) {
            stat_parts.push(if zh {
                format!("缓存写 {}", format_tokens_int(cw))
            } else {
                format!("Cache write {}", format_tokens_int(cw))
            });
        }
        if let Some(rtok) = u.reasoning_tokens.filter(|&n| n > 0) {
            stat_parts.push(if zh {
                format!("思考 {} tok", format_tokens_int(rtok))
            } else {
                format!("Reasoning {} tok", format_tokens_int(rtok))
            });
        }
    }

    match (msg.meta.ttft_ms, msg.meta.stream_ms) {
        (Some(ttft), Some(stream)) => {
            let total_ms = ttft.saturating_add(stream);
            stat_parts.push(if zh {
                format!(
                    "首字 {} · 生成 {} · 总耗时 {}",
                    format_duration_ms(ttft),
                    format_duration_ms(stream),
                    format_duration_ms(total_ms),
                )
            } else {
                format!(
                    "TTFT {} · Gen {} · Total {}",
                    format_duration_ms(ttft),
                    format_duration_ms(stream),
                    format_duration_ms(total_ms),
                )
            });
        }
        (Some(ttft), None) => {
            stat_parts.push(if zh {
                format!("首字 {}", format_duration_ms(ttft))
            } else {
                format!("TTFT {}", format_duration_ms(ttft))
            });
        }
        (None, Some(stream)) => {
            stat_parts.push(if zh {
                format!("耗时 {}", format_duration_ms(stream))
            } else {
                format!("Duration {}", format_duration_ms(stream))
            });
        }
        (None, None) => {}
    }

    if out_tok_val > 0
        && let Some(stream_ms) = msg.meta.stream_ms.filter(|&ms| ms > 0)
    {
        let tps = out_tok_val as f64 / (stream_ms as f64 / 1_000.0);
        stat_parts.push(format!("{tps:.1} tok/s"));
    }

    let currency = msg.cost_currency.as_deref().or(fallback_currency);
    if let Some(cost_str) = format_cost_micro(msg.cost_micro_usd, currency) {
        stat_parts.push(cost_str);
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

    if chips.is_empty() && stat_parts.is_empty() {
        return None;
    }

    let mut row = h_row()
        .pl(px(30.0))
        .pt(px(2.0))
        .gap(px(8.0))
        .flex_wrap()
        .children(chips);
    if !stat_parts.is_empty() {
        row = row.child(
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.faint))
                .child(SharedString::from(stat_parts.join("  ·  "))),
        );
    }
    Some(row)
}
