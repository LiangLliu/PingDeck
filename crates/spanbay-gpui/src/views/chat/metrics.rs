//! Chat metrics formatting, cost/speed calculation, inline `<think>` extraction, and the
//! hover-only per-turn metrics pill + 7-item breakdown popover (`.overhaul/chat-redesign-spec.md` §6.3).

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::agent::model::{AgentStopReason, ReasoningEffort};
use spanbay_desktop::core::model::settings::Language;
use spanbay_desktop::state::{AssistantTurnView, extract_inline_thinking_text};

use crate::ui::{h_row, mono_font_family, palette, popover, popover_trigger_btn, to_hsla, v_col};

use super::ChatView;

/// Extracts inline `<think>...</think>` or `<thinking>...</thinking>` blocks from text.
#[must_use]
pub fn extract_inline_thinking(raw: &str, streaming: bool) -> (Vec<String>, String) {
    extract_inline_thinking_text(raw, streaming)
}

/// Formats millisecond durations cleanly: `<1000ms` as `420ms`, `>=1000ms` as `2.3s`.
#[must_use]
pub fn format_duration_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        let secs = ms / 1_000;
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// Formats token counts compactly (`<1000` as exact integer, `>=1000` as `12.4K` / `1.2M`).
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

/// Formats micro-currency amounts (`US$0.52`, `¥0.04`) shared by footers, cost gates and popovers.
#[must_use]
pub fn format_money(micro: i64, currency: &str) -> String {
    let val = micro.max(0) as f64 / 1_000_000.0;
    let sym = match currency {
        "USD" | "" => "US$",
        "CNY" | "RMB" => "¥",
        "EUR" => "€",
        "GBP" => "£",
        "JPY" => "¥",
        other => return format!("{other} {val:.2}"),
    };
    if val == 0.0 {
        format!("{sym}0.00")
    } else if val < 0.0001 {
        format!("{sym}{val:.6}")
    } else if val < 0.01 {
        format!("{sym}{val:.4}")
    } else if val < 0.1 && (val * 1_000.0).round() as i64 % 10 != 0 {
        format!("{sym}{val:.3}")
    } else {
        format!("{sym}{val:.2}")
    }
}

/// Formats micro-USD (or micro-currency) cost cleanly when positive.
#[must_use]
pub fn format_cost_micro(micro: i64, currency: Option<&str>) -> Option<String> {
    let cur = currency.map(str::trim).filter(|c| !c.is_empty())?;
    (micro > 0).then(|| format_money(micro, cur))
}

#[must_use]
pub const fn format_effort_short(effort: ReasoningEffort, lang: Language) -> &'static str {
    match effort {
        ReasoningEffort::Minimal => crate::ui::Tr(lang).chat().metrics_effort_min(),
        ReasoningEffort::Low => crate::ui::Tr(lang).chat().composer_popovers_effort_low(),
        ReasoningEffort::Medium => crate::ui::Tr(lang).chat().metrics_effort_med(),
        ReasoningEffort::High => crate::ui::Tr(lang).chat().composer_popovers_effort_high(),
        ReasoningEffort::Max => crate::ui::Tr(lang).chat().composer_popovers_effort_max(),
    }
}

/// Formats the compact hover pill text (`11.4s · 19.5K token · US$0.018 ⓘ`) for a turn.
#[must_use]
pub fn format_turn_metrics_pill(
    turn: &AssistantTurnView,
    fallback_currency: Option<&str>,
    cx: &gpui_kit::App,
) -> Option<String> {
    let t = &turn.totals;
    let mut parts: Vec<String> = Vec::with_capacity(3);
    if t.duration_ms > 0 {
        parts.push(format_duration_ms(t.duration_ms));
    }
    let total_tok = t.total_tokens();
    if total_tok > 0 {
        parts.push(
            crate::ui::tr(cx)
                .chat()
                .context_used(format_tokens_compact(total_tok)),
        );
    }
    let cur = t.cost_currency.as_deref().or(fallback_currency);
    if let Some(c) = format_cost_micro(t.cost_micro_usd, cur) {
        parts.push(c);
    }
    if parts.is_empty() && !t.sealed_reasoning {
        return None;
    }
    if parts.is_empty() {
        parts.push("—".to_owned());
    }
    Some(format!("{} \u{24D8}", parts.join(" · ")))
}

/// Renders the hover-only metrics pill and its 7-item breakdown popover for an [`AssistantTurnView`].
pub fn render_turn_metrics_pill(
    view: &ChatView,
    turn: &AssistantTurnView,
    fallback_currency: Option<&str>,
    cx: &mut Context<ChatView>,
) -> Option<gpui_kit::AnyElement> {
    let pill_text = format_turn_metrics_pill(turn, fallback_currency, cx)?;
    let p = palette(cx);
    let mono = mono_font_family(cx);
    let ch = crate::ui::tr(cx).chat();
    let t = &turn.totals;

    let route_val = if turn.models_used.is_empty() {
        ch.thread_assistant().to_owned()
    } else {
        turn.models_used
            .iter()
            .map(|m| view.format_raw_model_label(m))
            .collect::<Vec<_>>()
            .join(" → ")
    };
    let steps_val = if t.hidden_todo_count > 0 {
        ch.turn_metrics_steps_hidden(
            t.step_count.max(1),
            t.visible_tool_count + t.hidden_todo_count,
            t.hidden_todo_count,
        )
    } else {
        ch.turn_metrics_steps_val(t.step_count.max(1), t.visible_tool_count)
    };
    let in_out_val = format!(
        "{} · {}",
        ch.metrics_input_tokens(format_tokens_int(t.input_tokens)),
        ch.metrics_output_tokens(format_tokens_int(t.output_tokens)),
    );
    let reason_part = if t.reasoning_tokens > 0 {
        if t.sealed_reasoning {
            ch.metrics_reasoning_tokens_sealed(format_tokens_int(t.reasoning_tokens))
        } else {
            ch.metrics_reasoning_tokens(format_tokens_int(t.reasoning_tokens))
        }
    } else if t.sealed_reasoning {
        ch.metrics_reasoning_sealed_no_tok().to_owned()
    } else {
        ch.metrics_reasoning_tokens("0")
    };
    let reason_cache_val = format!(
        "{reason_part} · {} {}",
        ch.inspector_tabs_cached_input(),
        format_tokens_int(t.cached_input_tokens),
    );
    let ctx_val = match t.context_limit_tokens.filter(|&lim| lim > 0) {
        Some(lim) => {
            let pct = ((t.peak_context_tokens as f64 / lim as f64) * 100.0).min(100.0);
            ch.metrics_context_window(
                format_tokens_int(t.peak_context_tokens),
                format_tokens_int(lim),
                format!("{pct:.0}"),
            )
        }
        None => ch.metrics_context_window_tokens(format_tokens_int(t.peak_context_tokens)),
    };
    let dur_val = format_duration_ms(t.duration_ms);
    let dur_part = match t.ttft_ms.filter(|&ms| ms > 0) {
        Some(ttft) if t.stream_ms > 0 => ch.metrics_duration_ttft_stream(
            &dur_val,
            format_duration_ms(ttft),
            format_duration_ms(t.stream_ms),
        ),
        _ => ch.metrics_duration(&dur_val),
    };
    let speed_val = match t.tokens_per_sec() {
        Some(tps) => format!(
            "{dur_part} · {}",
            ch.metrics_output_speed_tok_s(format!("{tps:.1}"))
        ),
        None => dur_part,
    };
    let cur = t.cost_currency.as_deref().or(fallback_currency);
    let cost_val = format_cost_micro(t.cost_micro_usd, cur)
        .map(|c| ch.metrics_estimated_cost(c))
        .unwrap_or_else(|| "—".to_owned());

    let rows: [(&'static str, String); 7] = [
        (ch.turn_metrics_route(), route_val),
        (ch.turn_metrics_steps(), steps_val),
        (ch.turn_metrics_in_out(), in_out_val),
        (ch.turn_metrics_reason_cache(), reason_cache_val),
        (ch.turn_metrics_ctx(), ctx_val),
        (ch.turn_metrics_speed(), speed_val),
        (ch.turn_metrics_cost(), cost_val),
    ];
    let title = ch.turn_metrics_title().to_owned();
    let tid = turn.turn_id.clone();

    let trigger = popover_trigger_btn(
        SharedString::from(format!("turn-metrics-pill-{tid}")),
        div()
            .px(px(6.0))
            .py(px(2.0))
            .font_family(mono)
            .text_size(px(11.0))
            .text_color(to_hsla(p.muted))
            .child(SharedString::from(pill_text)),
    );

    Some(
        popover(SharedString::from(format!("turn-metrics-pop-{tid}")))
            .trigger(trigger)
            .content(move |_, _, cx| {
                let p = palette(cx);
                let mono = mono_font_family(cx);
                let mut col = v_col()
                    .w(px(360.0))
                    .p(px(10.0))
                    .gap(px(6.0))
                    .text_xs()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .pb(px(2.0))
                            .border_b_1()
                            .border_color(to_hsla(p.line.composite_over(p.overlay)))
                            .child(SharedString::from(title.clone())),
                    );
                for (k, v) in &rows {
                    col = col.child(
                        h_row()
                            .w_full()
                            .justify_between()
                            .gap(px(10.0))
                            .child(div().flex_shrink_0().text_color(to_hsla(p.muted)).child(*k))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(mono.clone())
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(v.clone())),
                            ),
                    );
                }
                col
            })
            .into_any_element(),
    )
}

/// Why a run stopped early, in words; `None` for a normal completion.
#[must_use]
pub fn stop_reason_label(stop: AgentStopReason, cx: &gpui_kit::App) -> Option<&'static str> {
    let ch = crate::ui::tr(cx).chat();
    match stop {
        AgentStopReason::Completed => None,
        AgentStopReason::MaxIterations => Some(ch.metrics_max_steps()),
        AgentStopReason::CostLimit => Some(ch.metrics_cost_cap()),
        AgentStopReason::OutputLimit => Some(ch.metrics_output_limit()),
        AgentStopReason::ContextFull => Some(ch.metrics_context_full()),
        AgentStopReason::NoProgress => Some(ch.metrics_no_progress()),
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_inline_thinking, format_cost_micro};

    #[test]
    fn extract_inline_thinking_preserves_fenced_and_inline_code_tags() {
        let raw = "<think>step 1</think>\nUse `<think>inline</think>` or:\n```xml\n<think>fenced</think>\n```\nDone.";
        let (reasons, answer) = extract_inline_thinking(raw, false);
        assert_eq!(reasons, ["step 1"]);
        assert!(answer.contains("`<think>inline</think>`"));
        assert!(answer.contains("```xml\n<think>fenced</think>\n```"));
    }

    #[test]
    fn format_cost_micro_requires_explicit_currency() {
        assert_eq!(format_cost_micro(15_000, None), None);
        assert_eq!(format_cost_micro(15_000, Some("")), None);
        assert_eq!(
            format_cost_micro(15_000, Some("USD")).as_deref(),
            Some("US$0.015")
        );
    }
}
