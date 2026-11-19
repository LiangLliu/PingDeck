//! The opt-in context-window check.
//!
//! Single responsibility: climb a ladder of ever larger prompts to find the longest input a
//! model accepts. Expensive: never part of a check set.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Ctx, Outcome, chat, elapsed_ms, fail, pass};
use crate::llm::failure::FailureKind;
use crate::llm::store::ProbeResult;
use crate::llm::upstream::AttemptError;
use pingdeck_base::cancel::CancelToken;

/// Input sizes (tokens) the context test climbs through.
const CONTEXT_LADDER: [u64; 6] = [8_000, 32_000, 128_000, 256_000, 512_000, 1_000_000];
/// Top of the ladder when the declared window is unknown.
const CONTEXT_UNDECLARED_CAP: u64 = 128_000;

/// What the UI shows before it lets the user enable the context test.
pub const CONTEXT_PROBE_WARNING: &str = "The context test sends very large prompts to find the longest input the model accepts: \
in steps from 8,000 tokens up to 1,000,000 tokens, about 2,000,000 input tokens in total on a model with a 1,000,000-token window. \
That can cost real money (a few dollars on a premium model) and each step can take minutes. \
Enable it only for keys you are willing to spend on.";

/// The structured warning of the context test: the webview shows these numbers and
/// the text, it never parses them out of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextProbeInfo {
    /// Most input tokens the whole run may send (the sum of its steps).
    pub total_tokens: u64,
    /// Size of the first step.
    pub start_tokens: u64,
    /// The warning text ([`CONTEXT_PROBE_WARNING`]).
    pub warning: String,
}

impl ContextProbeInfo {
    /// The warning for a model whose declared context window is `declared_context`.
    #[must_use]
    pub fn for_window(declared_context: Option<u64>) -> Self {
        let steps = context_steps(declared_context);
        Self {
            total_tokens: steps.iter().sum(),
            start_tokens: steps.first().copied().unwrap_or(CONTEXT_LADDER[0]),
            warning: CONTEXT_PROBE_WARNING.to_owned(),
        }
    }
}

/// The ladder steps a model with `declared` context window is tested at (a step may overshoot
/// the window by 5 %).
fn context_steps(declared: Option<u64>) -> Vec<u64> {
    let cap = declared.map_or(CONTEXT_UNDECLARED_CAP, |window| {
        window.saturating_add(window / 20)
    });
    CONTEXT_LADDER
        .into_iter()
        .filter(|step| *step <= cap)
        .collect()
}

/// Short, varied words; a fixed LCG picks them so the filler is the same on every run but is
/// not trivially compressible.
const FILLER_WORDS: [&str; 64] = [
    "amber", "river", "stone", "quiet", "lamp", "cloud", "iron", "maple", "tide", "north", "glass",
    "fox", "ember", "field", "dusk", "pine", "salt", "wind", "copper", "harbor", "moss", "arrow",
    "violet", "frost", "orchard", "ladder", "silk", "ridge", "comet", "basket", "cedar", "pearl",
    "anchor", "meadow", "falcon", "marble", "thread", "canyon", "willow", "ripple", "lantern",
    "granite", "puzzle", "velvet", "drift", "summit", "ocean", "bridge", "garden", "shadow",
    "silver", "timber", "yellow", "winter", "sparrow", "cobalt", "market", "island", "forest",
    "candle", "engine", "valley", "dune", "quartz",
];

pub(super) fn filler(chars: usize) -> String {
    let mut text = String::with_capacity(chars + 16);
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    while text.len() < chars {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        // The high bits of an LCG are the well-mixed ones.
        let index = usize::try_from(state >> 58).unwrap_or(0);
        text.push_str(FILLER_WORDS[index % FILLER_WORDS.len()]);
        text.push(' ');
    }
    text
}

pub(super) async fn test_context(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    if !ctx.options.allow_context_probe {
        fail(
            result,
            FailureKind::PolicyBlocked,
            "the context test is opt-in",
        );
        return Ok(());
    }
    let protocol = ctx.chat_proto()?;
    result.endpoint = protocol.as_str().to_owned();
    let declared = ctx.options.declared_context;
    let steps = context_steps(declared);
    if steps.is_empty() {
        fail(
            result,
            FailureKind::Unknown,
            format!(
                "the declared window ({} tokens) is below the smallest step ({} tokens)",
                declared.unwrap_or(0),
                CONTEXT_LADDER[0]
            ),
        );
        return Ok(());
    }

    let started = Instant::now();
    let mut tried: Vec<Value> = Vec::new();
    let mut max_ok = 0u64;
    let mut estimated_total = 0u64;
    let mut last_input: Option<u64> = None;
    let mut stopped: Option<(u64, AttemptError)> = None;
    for step in &steps {
        if ctx.cancel.is_some_and(CancelToken::is_cancelled) {
            stopped = Some((*step, AttemptError::Cancelled));
            break;
        }
        let chars = usize::try_from(step.saturating_mul(4)).unwrap_or(usize::MAX);
        let mut prompt = filler(chars);
        prompt.push_str("\n\nReply with the single word OK.");
        let request = ctx.request(prompt, 16);
        match chat(ctx, protocol, &request, ctx.timeouts.context_step).await {
            Ok((response, _)) => {
                tried.push(json!({"tokens": step, "ok": true, "status": 200}));
                max_ok = *step;
                last_input = response.usage.input_tokens.or(Some(*step));
                estimated_total = estimated_total.saturating_add(last_input.unwrap_or(*step));
            }
            Err(error) => {
                tried.push(json!({"tokens": step, "ok": false, "status": error.status()}));
                stopped = Some((*step, error));
                break;
            }
        }
    }
    result.latency_ms = Some(elapsed_ms(started));
    result.input_tokens = last_input;
    result.detail = json!({
        "max_ok_tokens": max_ok,
        "tried": tried,
        "declared_context": declared,
        "estimated_input_tokens_total": estimated_total,
    });

    let window = match declared {
        Some(window) if max_ok.saturating_mul(10) >= window.saturating_mul(9) => {
            format!(" (reached the declared {window}-token window)")
        }
        Some(window) => format!(" (below the declared {window}-token window)"),
        None => format!(" (no declared window; the test stops at {CONTEXT_UNDECLARED_CAP} tokens)"),
    };
    match stopped {
        Some((_, error)) if max_ok == 0 || matches!(error, AttemptError::Cancelled) => Err(error),
        Some((step, error)) => {
            result.status = error.status();
            pass(
                result,
                format!(
                    "accepted {max_ok} tokens{window}; the {step}-token step failed: {}",
                    error.message()
                ),
            );
            Ok(())
        }
        None => {
            result.status = Some(200);
            pass(result, format!("accepted {max_ok} tokens{window}"));
            Ok(())
        }
    }
}
