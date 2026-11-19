//! The thinking request and effort word, whose shape depends on the Claude generation.

use serde_json::{Map, Value, json};

use super::DEFAULT_MAX_TOKENS;
use crate::llm::adapters::EncodeCtx;
use crate::llm::adapters::generation::{self, Thinking};
use crate::llm::hub::{HubEffort, HubReasoning, HubRequest};
use crate::llm::wire::ReasoningStyle;

/// What [`encode_thinking`] decided.
pub(super) struct ThinkingPlan {
    /// Thinking is on, which forbids sampling parameters and (on some models) prefill.
    pub(super) on: bool,
    /// The `max_tokens` to send, raised above the thinking budget when needed.
    pub(super) max_tokens: u32,
}

/// Writes `thinking` into `body` and `effort` into `output_config` for the request's reasoning.
///
/// The model generation decides the shape (see [`generation::claude`]); an id it does not know
/// keeps what the request asked for. A provider with [`ReasoningStyle::None`] gets no thinking.
pub(super) fn encode_thinking(
    req: &HubRequest,
    ctx: &EncodeCtx<'_>,
    body: &mut Map<String, Value>,
    output_config: &mut Map<String, Value>,
) -> ThinkingPlan {
    let mut plan = ThinkingPlan {
        on: false,
        max_tokens: req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
    };
    let Some(reasoning) = req
        .reasoning
        .as_ref()
        .filter(|_| ctx.quirks.reasoning_request != Some(ReasoningStyle::None))
    else {
        return plan;
    };
    if reasoning.enabled == Some(false) {
        // Only the adaptive dialect has an explicit off switch; elsewhere omitting it is off.
        if ctx.quirks.reasoning_request == Some(ReasoningStyle::AnthropicAdaptive) {
            body.insert("thinking".into(), json!({"type": "disabled"}));
        }
        return plan;
    }
    let asked = reasoning.enabled == Some(true) || reasoning.effort.is_some();
    let generation = generation::claude(&req.model);
    if let Some(budget) = thinking_budget(generation, reasoning, asked) {
        plan.on = true;
        body.insert(
            "thinking".into(),
            json!({"type": "enabled", "budget_tokens": budget}),
        );
        // Anthropic requires `max_tokens` to exceed `thinking.budget_tokens`.
        if plan.max_tokens <= budget {
            plan.max_tokens = budget.saturating_add(plan.max_tokens.max(1));
        }
    } else if asked {
        plan.on = true;
        // 4.7 and later omit the thinking text unless asked for a summary.
        let thinking = if generation == Thinking::Adaptive {
            json!({"type": "adaptive", "display": "summarized"})
        } else {
            json!({"type": "adaptive"})
        };
        body.insert("thinking".into(), thinking);
    }
    if let Some(effort) = reasoning.effort.filter(|_| generation != Thinking::Budget) {
        output_config.insert(
            "effort".into(),
            json!(effort_name(effort, generation == Thinking::Adaptive)),
        );
    }
    plan
}

/// The budget to send: none for an adaptive-only generation; for a budget-only one the request's
/// budget, else (when thinking was asked for) the effort's; otherwise only an explicit budget.
fn thinking_budget(generation: Thinking, reasoning: &HubReasoning, asked: bool) -> Option<u32> {
    match generation {
        Thinking::Adaptive => None,
        Thinking::Budget => reasoning
            .budget_tokens
            .or_else(|| asked.then(|| effort_budget(reasoning.effort))),
        Thinking::Either => reasoning.budget_tokens,
    }
}

/// The thinking budget an effort stands for on a model that takes only budgets.
fn effort_budget(effort: Option<HubEffort>) -> u32 {
    match effort {
        Some(HubEffort::Minimal | HubEffort::Low) => 1_024,
        Some(HubEffort::Medium) | None => 4_096,
        Some(HubEffort::High) => 16_000,
        Some(HubEffort::XHigh) => 24_000,
        Some(HubEffort::Max) => 32_000,
    }
}

/// The `output_config.effort` word; `xhigh` only where the generation has it (4.7 and later),
/// `high` elsewhere.
fn effort_name(effort: HubEffort, has_xhigh: bool) -> &'static str {
    match effort {
        HubEffort::Minimal | HubEffort::Low => "low",
        HubEffort::Medium => "medium",
        HubEffort::XHigh if has_xhigh => "xhigh",
        HubEffort::High | HubEffort::XHigh => "high",
        HubEffort::Max => "max",
    }
}
