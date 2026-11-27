//! Decision-API probe helpers (TypeSafe `/v1/systemone`, OpenAI `/v1/decisions`).
//!
//! Single responsibility: build a one-question probe body and summarise the answer. Pure
//! functions; the HTTP call lives with the probe.

use serde_json::{Value, json};

use crate::llm::hub::HubUsage;
use crate::llm::wire::Protocol;
use spanbay_base::error::AppError;

const PROBE_TEXT: &str = "The invoice was paid on time and the customer thanked us.";
const PROBE_QUESTION: &str = "Does the text say that a payment was made?";
const PROBE_NAME: &str = "paid";

/// The probe body: OpenAI's `questions` array for [`Protocol::DecisionOpenai`], the systemone
/// `questions` object for every other protocol.
#[must_use]
pub fn probe_body(protocol: Protocol, model: &str) -> Value {
    if protocol == Protocol::DecisionOpenai {
        json!({
            "model": model,
            "input": PROBE_TEXT,
            "questions": [{ "type": "predicate", "name": PROBE_NAME, "instructions": PROBE_QUESTION }],
        })
    } else {
        json!({
            "model": model,
            "state": PROBE_TEXT,
            "questions": { PROBE_NAME: { "type": "noul", "instructions": PROBE_QUESTION } },
        })
    }
}

/// What a decision response says, reduced to what the probe reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionSummary {
    /// How many answers the response holds.
    pub answers: usize,
    /// The model the response names, trimmed; `None` when absent or blank.
    pub model: Option<String>,
    /// The first answer's headline as a one-key object (`noul`, `choice` or `score`).
    pub headline: Option<Value>,
    /// Token usage from `usage`.
    pub usage: HubUsage,
}

/// The answer to the probe question, else the first answer.
fn first_answer(body: &Value) -> Option<&Value> {
    match body.get("answers")? {
        Value::Object(map) => map.get(PROBE_NAME).or_else(|| map.values().next()),
        Value::Array(items) => items.first(),
        _ => None,
    }
}

/// The headline of one answer: a numeric `noul` (OpenAI's `probability` is reported under the same
/// key), else a non-empty `choice`, else a numeric `score`.
fn headline_of(answer: &Value) -> Option<Value> {
    if let Some(noul) = answer
        .get("noul")
        .or_else(|| answer.get("probability"))
        .filter(|v| v.is_number())
    {
        return Some(json!({ "noul": noul }));
    }
    if let Some(choice) = answer
        .get("choice")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
    {
        return Some(json!({ "choice": choice }));
    }
    answer
        .get("score")
        .filter(|v| v.is_number())
        .map(|score| json!({ "score": score }))
}

/// Summarises a decision response.
///
/// # Errors
///
/// A network error when the body has no non-empty `answers` object or array.
pub fn read_response(body: &Value) -> Result<DecisionSummary, AppError> {
    let answers = match body.get("answers") {
        Some(Value::Object(map)) => map.len(),
        Some(Value::Array(items)) => items.len(),
        _ => 0,
    };
    if answers == 0 {
        return Err(AppError::network(
            "Decision response has no `answers` object or array",
        ));
    }
    let tokens = |pointer: &str| body.pointer(pointer).and_then(Value::as_u64);
    Ok(DecisionSummary {
        answers,
        model: body
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_owned),
        headline: first_answer(body).and_then(headline_of),
        usage: HubUsage {
            input_tokens: tokens("/usage/input_tokens"),
            output_tokens: tokens("/usage/output_tokens"),
            cached_input_tokens: tokens("/usage/input_tokens_details/cached_tokens"),
            cache_write_tokens: tokens("/usage/input_tokens_details/cache_write_tokens"),
            cache_write_long_tokens: None,
            reasoning_tokens: None,
        },
    })
}

/// The summary as a flat JSON object: `answers`, `model` when known, and the headline's key.
#[must_use]
pub fn summary_detail(summary: &DecisionSummary) -> Value {
    let mut detail = serde_json::Map::new();
    detail.insert("answers".to_owned(), json!(summary.answers));
    if let Some(model) = &summary.model {
        detail.insert("model".to_owned(), Value::String(model.clone()));
    }
    if let Some(Value::Object(headline)) = &summary.headline {
        for (key, value) in headline {
            detail.insert(key.clone(), value.clone());
        }
    }
    Value::Object(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_bodies_follow_protocol_shape() {
        let sys = probe_body(Protocol::DecisionSystemone, "jev-latest");
        assert_eq!(sys["questions"]["paid"]["type"], "noul");
        let oai = probe_body(Protocol::DecisionOpenai, "gpt-6-luna");
        assert_eq!(oai["questions"][0]["type"], "predicate");
    }

    #[test]
    fn systemone_answers_summarise_noul() {
        let ts_resp = json!({
            "model": " jev-1.14.0 ",
            "answers": {
                "paid": {"type": "noul", "noul": 0.98},
                "tone": {"type": "choice", "choice": "calm"}
            },
            "usage": {"input_tokens": 120, "output_tokens": 12}
        });
        let summary = read_response(&ts_resp).unwrap();
        assert_eq!(
            summary_detail(&summary),
            json!({"answers": 2, "model": "jev-1.14.0", "noul": 0.98})
        );
    }

    #[test]
    fn openai_probability_normalises_to_noul() {
        let oai_resp = json!({
            "answers": [{"type": "predicate", "name": "paid", "probability": 0.92}],
            "usage": {"input_tokens": 40, "output_tokens": 3,
                      "input_tokens_details": {"cached_tokens": 16, "cache_write_tokens": 0}}
        });
        let summary = read_response(&oai_resp).unwrap();
        assert_eq!(
            summary_detail(&summary),
            json!({"answers": 1, "noul": 0.92})
        );
        assert_eq!(summary.usage.cached_input_tokens, Some(16));
    }

    #[test]
    fn headline_falls_back_to_choice_then_score() {
        for (ans, expected) in [
            (
                json!({"paid": {"type": "choice", "choice": "calm"}}),
                Some(json!({"choice": "calm"})),
            ),
            (
                json!({"paid": {"type": "score", "score": 0.7}}),
                Some(json!({"score": 0.7})),
            ),
            (json!({"paid": {"type": "noul"}}), None),
        ] {
            assert_eq!(
                read_response(&json!({"answers": ans})).unwrap().headline,
                expected
            );
        }
    }

    #[test]
    fn empty_answers_are_rejected() {
        assert!(read_response(&json!({"answers": {}})).is_err());
    }
}
