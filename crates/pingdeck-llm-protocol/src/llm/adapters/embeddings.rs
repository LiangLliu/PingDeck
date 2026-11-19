//! Embeddings probe helpers for the OpenAI-style and Gemini `embedContent` endpoints.
//!
//! Single responsibility: build the smallest valid request body and read the vector dimension and
//! token usage from the response. Pure functions; the HTTP call lives with the probe.

use serde_json::{Value, json};

use crate::llm::hub::HubUsage;

/// `POST /embeddings` body: `{"model", "input"}`.
#[must_use]
pub fn openai_probe_body(model: &str, input: &str) -> Value {
    json!({ "model": model, "input": input })
}

/// `POST /models/{model}:embedContent` body; Gemini wants the `models/` resource name in the body too.
#[must_use]
pub fn gemini_probe_body(model: &str, input: &str) -> Value {
    json!({
        "model": format!("models/{}", model.strip_prefix("models/").unwrap_or(model)),
        "content": { "parts": [{ "text": input }] },
    })
}

/// Length of `data[0].embedding` in an OpenAI embeddings response.
#[must_use]
pub fn openai_dimension(body: &Value) -> Option<usize> {
    body.pointer("/data/0/embedding")?.as_array().map(Vec::len)
}

/// Length of `embedding.values` in a Gemini `embedContent` response.
#[must_use]
pub fn gemini_dimension(body: &Value) -> Option<usize> {
    body.pointer("/embedding/values")?.as_array().map(Vec::len)
}

/// Token usage of an OpenAI embeddings response (`prompt_tokens`, falling back to `total_tokens`).
#[must_use]
pub fn usage(body: &Value) -> HubUsage {
    let tokens = |key: &str| body.get("usage")?.get(key)?.as_u64();
    HubUsage {
        input_tokens: tokens("prompt_tokens").or_else(|| tokens("total_tokens")),
        ..HubUsage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_bodies() {
        assert_eq!(
            openai_probe_body("text-embedding-3-small", "hi"),
            json!({"model": "text-embedding-3-small", "input": "hi"})
        );
        let expected = json!({
            "model": "models/gemini-embedding-001",
            "content": {"parts": [{"text": "hi"}]},
        });
        assert_eq!(gemini_probe_body("gemini-embedding-001", "hi"), expected);
        assert_eq!(
            gemini_probe_body("models/gemini-embedding-001", "hi"),
            expected
        );
    }

    #[test]
    fn dimensions() {
        let openai = json!({"data": [{"embedding": [0.1, 0.2, 0.3], "index": 0}]});
        assert_eq!(openai_dimension(&openai), Some(3));
        assert_eq!(openai_dimension(&json!({"data": []})), None);
        assert_eq!(
            openai_dimension(&json!({"data": [{"embedding": "b64"}]})),
            None
        );
        let gemini = json!({"embedding": {"values": [1.0, 2.0]}});
        assert_eq!(gemini_dimension(&gemini), Some(2));
        assert_eq!(gemini_dimension(&json!({"error": {}})), None);
    }

    #[test]
    fn usage_fields() {
        let body = json!({"usage": {"prompt_tokens": 5, "total_tokens": 5}});
        assert_eq!(usage(&body).input_tokens, Some(5));
        assert_eq!(
            usage(&json!({"usage": {"total_tokens": 7}})).input_tokens,
            Some(7)
        );
        assert_eq!(usage(&json!({})), HubUsage::default());
    }
}
