//! Reasoning controls: the dialects Chat Completions servers accept, both directions.

use serde_json::{Map, Value, json};

use crate::llm::adapters::{effort_str, parse_effort, to_u32};
use crate::llm::hub::{HubEffort, HubReasoning};
use crate::llm::wire::ReasoningStyle;

/// Writes the reasoning request in the provider's `style`; nothing when no control is set.
pub(super) fn encode_reasoning(
    body: &mut Map<String, Value>,
    style: ReasoningStyle,
    r: &HubReasoning,
) {
    if r.enabled.is_none() && r.effort.is_none() && r.budget_tokens.is_none() {
        return;
    }
    let enabled = r.enabled.unwrap_or(true);
    let effort = r.effort.map(|effort| effort_for(style, effort));
    match style {
        ReasoningStyle::ReasoningEffort => {
            if !enabled {
                body.insert("reasoning_effort".into(), json!("none"));
            } else if let Some(effort) = effort {
                body.insert("reasoning_effort".into(), json!(effort));
            }
        }
        ReasoningStyle::ReasoningObject => {
            let value = if !enabled {
                json!({"enabled": false})
            } else if let Some(effort) = effort {
                json!({"effort": effort})
            } else if let Some(budget) = r.budget_tokens {
                json!({"max_tokens": budget})
            } else {
                json!({"enabled": true})
            };
            body.insert("reasoning".into(), value);
        }
        ReasoningStyle::ThinkingType => {
            let kind = if enabled { "enabled" } else { "disabled" };
            body.insert("thinking".into(), json!({"type": kind}));
            if let (true, Some(effort)) = (enabled, effort) {
                body.insert("reasoning_effort".into(), json!(effort));
            }
        }
        ReasoningStyle::EnableThinking => {
            body.insert("enable_thinking".into(), json!(enabled));
            if let (true, Some(budget)) = (enabled, r.budget_tokens) {
                body.insert("thinking_budget".into(), json!(budget));
            }
        }
        ReasoningStyle::ChatTemplateKwargs => {
            body.insert(
                "chat_template_kwargs".into(),
                json!({"enable_thinking": enabled}),
            );
        }
        ReasoningStyle::AnthropicAdaptive
        | ReasoningStyle::GeminiThinkingLevel
        | ReasoningStyle::None => {}
    }
}

/// The effort word for `style`: only OpenAI's own `reasoning_effort` takes `xhigh`, the other
/// styles get `high`.
fn effort_for(style: ReasoningStyle, effort: HubEffort) -> &'static str {
    if effort == HubEffort::XHigh && style != ReasoningStyle::ReasoningEffort {
        "high"
    } else {
        effort_str(effort)
    }
}

/// The reasoning controls of a client request, read from the `reasoning_effort`, `reasoning`
/// object and `thinking` dialects only (`enable_thinking`/`chat_template_kwargs` are not read):
/// `reasoning_effort` wins over `reasoning.effort` (`"none"` disables), then `reasoning.enabled`
/// and after it `thinking.type` set the enabled flag, and `reasoning.max_tokens` sets the budget.
pub(super) fn decode_reasoning(body: &Value) -> Option<HubReasoning> {
    let mut reasoning = HubReasoning::default();
    if let Some(effort) = body["reasoning_effort"]
        .as_str()
        .or_else(|| body["reasoning"]["effort"].as_str())
    {
        if effort == "none" {
            reasoning.enabled = Some(false);
        } else {
            reasoning.effort = parse_effort(effort);
        }
    }
    if let Some(enabled) = body["reasoning"]["enabled"].as_bool() {
        reasoning.enabled = Some(enabled);
    }
    if let Some(budget) = body["reasoning"]["max_tokens"].as_u64() {
        reasoning.budget_tokens = Some(to_u32(budget));
    }
    match body["thinking"]["type"].as_str() {
        Some("enabled") => reasoning.enabled = Some(true),
        Some("disabled") => reasoning.enabled = Some(false),
        _ => {}
    }
    (reasoning != HubReasoning::default()).then_some(reasoning)
}
