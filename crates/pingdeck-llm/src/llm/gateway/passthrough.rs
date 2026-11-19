//! Byte-exact forwarding of a same-protocol request.
//!
//! Single responsibility: replace the value of the top-level `"model"` key (and an output cap above
//! the target's) inside the client's JSON text and touch nothing else, so key order, spacing,
//! number spelling, unknown fields and `cache_control` blocks reach the upstream exactly as the
//! client sent them. A parsed [`serde_json::Value`] cannot do that (it reorders keys), and prompt
//! caching depends on the bytes of the prefix. A body that echoes a signature of another protocol
//! (the `pdk1.` tag) loses only those blocks ([`strip_signature_tags`]) and is re-serialised.

use serde_json::Value;

use crate::llm::hub::unwrap_signature;
use crate::llm::wire::Protocol;

/// `raw` with every top-level string value of `"model"` replaced by `model`, or `None` when there
/// is none to replace (a Gemini body carries no model; the URL does) or `raw` is not a JSON object.
#[must_use]
pub(super) fn with_model(raw: &[u8], model: &str) -> Option<Vec<u8>> {
    let replacement = serde_json::to_vec(model).ok()?;
    edit_top_level(raw, |key, value| {
        (key == "model" && value.first() == Some(&b'"')).then(|| replacement.clone())
    })
}

/// The top-level output-cap fields of a body of `protocol` (Gemini's sits inside
/// `generationConfig`, see [`cap_output`]).
const fn output_fields(protocol: Protocol) -> &'static [&'static str] {
    match protocol {
        Protocol::OpenaiChat => &["max_tokens", "max_completion_tokens"],
        Protocol::OpenaiResponses => &["max_output_tokens"],
        Protocol::AnthropicMessages => &["max_tokens"],
        _ => &[],
    }
}

/// `raw` (a body of `protocol`) with every output cap above `cap` lowered to `cap`, and the field
/// and the value the client asked for; `None` when nothing is above it. Only the number changes:
/// every other byte stays (a Gemini body, whose cap is nested, is re-serialised).
#[must_use]
pub(super) fn cap_output(
    protocol: Protocol,
    raw: &[u8],
    json: &Value,
    cap: u32,
) -> Option<(Vec<u8>, &'static str, u64)> {
    if protocol == Protocol::Gemini {
        let asked = ["generationConfig", "generation_config"]
            .into_iter()
            .find_map(|config| {
                let config = json.get(config)?;
                ["maxOutputTokens", "max_output_tokens"]
                    .into_iter()
                    .find_map(|field| Some((config.get(field)?.as_u64()?, field)))
            })
            .filter(|(asked, _)| *asked > u64::from(cap));
        let (asked, _) = asked?;
        let mut json = json.clone();
        for config in ["generationConfig", "generation_config"] {
            if let Some(config) = json.get_mut(config).and_then(Value::as_object_mut) {
                for field in ["maxOutputTokens", "max_output_tokens"] {
                    if config.contains_key(field) {
                        config.insert(field.to_owned(), Value::from(cap));
                    }
                }
            }
        }
        return Some((serde_json::to_vec(&json).ok()?, "maxOutputTokens", asked));
    }
    let fields = output_fields(protocol);
    let mut found: Option<(&'static str, u64)> = None;
    let replacement = cap.to_string().into_bytes();
    let edited = edit_top_level(raw, |key, value| {
        let field = fields.iter().find(|field| **field == key)?;
        let asked: u64 = std::str::from_utf8(value).ok()?.parse().ok()?;
        (asked > u64::from(cap)).then(|| {
            found.get_or_insert((*field, asked));
            replacement.clone()
        })
    })?;
    found.map(|(field, asked)| (edited, field, asked))
}

/// Removes, in place, what a client echoed back that another protocol produced: a reasoning block
/// whose signature carries the `pdk1.` tag (Messages), a tagged `thoughtSignature` (Gemini). The
/// upstream of the client's own protocol would refuse it; every other field, `cache_control`
/// included, stays. Returns whether anything was removed.
pub(super) fn strip_signature_tags(protocol: Protocol, json: &mut Value) -> bool {
    let foreign = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .is_some_and(|raw| unwrap_signature(raw, protocol).0 != protocol)
    };
    let mut removed = false;
    match protocol {
        Protocol::AnthropicMessages => {
            let messages = json.get_mut("messages").and_then(Value::as_array_mut);
            for message in messages.into_iter().flatten() {
                if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) {
                    let before = blocks.len();
                    blocks.retain(|block| {
                        !(foreign(block.get("signature")) || foreign(block.get("data")))
                    });
                    removed |= blocks.len() != before;
                }
            }
        }
        Protocol::Gemini => {
            let contents = json.get_mut("contents").and_then(Value::as_array_mut);
            for content in contents.into_iter().flatten() {
                let parts = content.get_mut("parts").and_then(Value::as_array_mut);
                for part in parts.into_iter().flatten().filter_map(Value::as_object_mut) {
                    for key in ["thoughtSignature", "thought_signature"] {
                        if foreign(part.get(key)) {
                            part.remove(key);
                            removed = true;
                        }
                    }
                }
            }
        }
        _ => {}
    }
    removed
}

/// `raw` with the values of top-level keys replaced where `edit` (given the key and the raw value
/// bytes) answers new bytes; `None` when nothing changed or `raw` is not a JSON object.
fn edit_top_level(
    raw: &[u8],
    mut edit: impl FnMut(&str, &[u8]) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(raw.len() + 16);
    let mut copied = 0;
    let mut changed = false;
    let mut at = skip_space(raw, 0);
    if raw.get(at) != Some(&b'{') {
        return None;
    }
    at = skip_space(raw, at + 1);
    while raw.get(at)? != &b'}' {
        let key_end = skip_string(raw, at)?;
        let key: String = serde_json::from_slice(&raw[at..key_end]).ok()?;
        at = skip_space(raw, key_end);
        if raw.get(at)? != &b':' {
            return None;
        }
        at = skip_space(raw, at + 1);
        let value_end = skip_value(raw, at)?;
        if let Some(replacement) = edit(&key, &raw[at..value_end]) {
            out.extend_from_slice(&raw[copied..at]);
            out.extend_from_slice(&replacement);
            copied = value_end;
            changed = true;
        }
        at = skip_space(raw, value_end);
        match raw.get(at)? {
            b',' => at = skip_space(raw, at + 1),
            b'}' => {}
            _ => return None,
        }
    }
    if !changed {
        return None;
    }
    out.extend_from_slice(&raw[copied..]);
    Some(out)
}

fn skip_space(raw: &[u8], mut at: usize) -> usize {
    while raw.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// The index after the string that starts at `at` (a `"`).
fn skip_string(raw: &[u8], at: usize) -> Option<usize> {
    if raw.get(at)? != &b'"' {
        return None;
    }
    let mut i = at + 1;
    loop {
        match raw.get(i)? {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
}

/// The index after the JSON value that starts at `at`.
fn skip_value(raw: &[u8], at: usize) -> Option<usize> {
    match raw.get(at)? {
        b'"' => skip_string(raw, at),
        b'{' | b'[' => {
            let mut depth = 0_usize;
            let mut i = at;
            loop {
                match raw.get(i)? {
                    b'"' => {
                        i = skip_string(raw, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        _ => {
            let mut i = at;
            while raw.get(i).is_some_and(|byte| {
                !matches!(byte, b',' | b'}' | b']') && !byte.is_ascii_whitespace()
            }) {
                i += 1;
            }
            (i > at).then_some(i)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewritten(raw: &str, model: &str) -> Option<String> {
        with_model(raw.as_bytes(), model).map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn only_the_model_value_changes_and_every_other_byte_stays() {
        let raw = r#" { "z": 1.0 ,"model" : "claude-code", "x_custom":{"model":"inner","a":[1, 2 ]},"messages":[{"role":"user","content":"hi\"} {"}] , "cache_control":{"type":"ephemeral"}} "#;
        let out = rewritten(raw, "deepseek-v4").unwrap();
        assert_eq!(out, raw.replace("\"claude-code\"", "\"deepseek-v4\""));
    }

    #[test]
    fn the_new_model_is_escaped() {
        let out = rewritten(r#"{"model":"a"}"#, "we\"ird\\name").unwrap();
        assert_eq!(out, r#"{"model":"we\"ird\\name"}"#);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap()["model"],
            "we\"ird\\name"
        );
    }

    #[test]
    fn an_escaped_key_and_a_model_after_nested_values_are_found() {
        let out = rewritten(
            r#"{"a":{"model":"x"},"mod\u0065l":"m","b":[{"model":1}]}"#,
            "n",
        )
        .unwrap();
        assert_eq!(
            out,
            r#"{"a":{"model":"x"},"mod\u0065l":"n","b":[{"model":1}]}"#
        );
    }

    #[test]
    fn nothing_to_replace_is_none() {
        assert_eq!(rewritten(r#"{"contents":[]}"#, "m"), None);
        assert_eq!(rewritten(r#"{"model":7}"#, "m"), None);
        assert_eq!(rewritten(r#"{"a":{"model":"x"}}"#, "m"), None);
        assert_eq!(rewritten("[1]", "m"), None);
        assert_eq!(rewritten(r#"{"model":"a""#, "m"), None);
        assert_eq!(rewritten("", "m"), None);
    }

    #[test]
    fn an_output_cap_above_the_target_is_lowered_and_nothing_else_moves() {
        let raw = r#"{"max_tokens" : 64000,"model":"m","messages":[{"max_tokens":9}]}"#;
        let json: Value = serde_json::from_str(raw).unwrap();
        let (out, field, asked) =
            cap_output(Protocol::AnthropicMessages, raw.as_bytes(), &json, 8192).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            raw.replace("64000", "8192")
        );
        assert_eq!((field, asked), ("max_tokens", 64000));
        assert!(cap_output(Protocol::AnthropicMessages, raw.as_bytes(), &json, 64000).is_none());
        let gemini = serde_json::json!({"generationConfig": {"maxOutputTokens": 99999}});
        let raw = serde_json::to_vec(&gemini).unwrap();
        let (out, _, asked) = cap_output(Protocol::Gemini, &raw, &gemini, 100).unwrap();
        let out: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(out["generationConfig"]["maxOutputTokens"], 100);
        assert_eq!(asked, 99999);
    }

    #[test]
    fn foreign_signatures_are_stripped_and_cache_control_stays() {
        let mut body = serde_json::json!({"messages": [{"role": "assistant", "content": [
            {"type": "thinking", "thinking": "t", "signature": "pdk1.gemini.abc"},
            {"type": "thinking", "thinking": "u", "signature": "own-sig"},
            {"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}
        ]}]});
        assert!(strip_signature_tags(Protocol::AnthropicMessages, &mut body));
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["signature"], "own-sig");
        assert_eq!(blocks[1]["cache_control"]["type"], "ephemeral");
        assert!(!strip_signature_tags(
            Protocol::AnthropicMessages,
            &mut body
        ));
        let mut gemini = serde_json::json!({"contents": [{"parts": [
            {"functionCall": {"name": "f"}, "thoughtSignature": "pdk1.anthropic_messages.x"}
        ]}]});
        assert!(strip_signature_tags(Protocol::Gemini, &mut gemini));
        assert!(
            gemini["contents"][0]["parts"][0]
                .get("thoughtSignature")
                .is_none()
        );
        assert_eq!(
            gemini["contents"][0]["parts"][0]["functionCall"]["name"],
            "f"
        );
    }

    #[test]
    fn literals_and_numbers_are_skipped_whole() {
        let out = rewritten(r#"{"n":-1.5e3,"t":true,"z":null,"model":"a","e":{}}"#, "b").unwrap();
        assert_eq!(out, r#"{"n":-1.5e3,"t":true,"z":null,"model":"b","e":{}}"#);
    }
}
