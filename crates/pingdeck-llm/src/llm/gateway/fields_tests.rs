//! Tests of the field classes: every row of the table, per inbound
//! protocol, toward every other outbound protocol, through the real codecs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};

use super::*;
use crate::llm::adapters::{EncodeCtx, InboundHint, codec};
use crate::llm::wire::Quirks;

const CLAUDE_CODE: &str =
    include_str!("../../../../../fixtures/gateway-requests/claude-code-messages.json");
const CODEX: &str = include_str!("../../../../../fixtures/gateway-requests/codex-responses.json");

const ALL: [Protocol; 4] = [CHAT, RESP, MSG, GEM];

/// What the pipeline decides for `raw` (a client request of `inbound`) toward `outbound`: the
/// feature it is refused for, or the dropped field names in header order.
fn decide(
    inbound: Protocol,
    outbound: Protocol,
    raw: &Value,
    beta: bool,
) -> Result<Vec<String>, String> {
    if let Some(feature) = refusal(inbound, raw) {
        return Err(feature);
    }
    let hint = InboundHint {
        model: Some("m".into()),
        stream: false,
    };
    let request = codec(inbound)
        .unwrap()
        .decode_request(raw, &hint)
        .map_err(|error| feature_of_error(error.message()))?;
    let mut encoded = codec(outbound)
        .unwrap()
        .encode_request(
            &request,
            &EncodeCtx {
                quirks: &Quirks::default(),
            },
        )
        .map_err(|error| feature_of_error(error.message()))?;
    conform(outbound, &mut encoded);
    let notes = drops(
        inbound,
        outbound,
        raw,
        beta && outbound != inbound,
        &request,
        &encoded,
    );
    Ok(dropped_header(&notes)
        .map(|names| names.split(',').map(str::to_owned).collect())
        .unwrap_or_default())
}

fn fixture(text: &str) -> (Value, bool) {
    let parsed: Value = serde_json::from_str(text).unwrap();
    let beta = parsed["headers"]
        .as_object()
        .is_some_and(|headers| headers.contains_key("anthropic-beta"));
    (parsed["body"].clone(), beta)
}

fn messages(extra: Value) -> Value {
    let mut body =
        json!({"model": "m", "max_tokens": 10, "messages": [{"role": "user", "content": "hi"}]});
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn chat(extra: Value) -> Value {
    let mut body = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn responses(extra: Value) -> Value {
    let mut body = json!({"model": "m", "input": "hi"});
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn gemini(extra: Value) -> Value {
    let mut body = json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]});
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

// ---------------------------------------------------------------------- the table itself

#[test]
fn the_safe_drop_list_is_the_closed_list_of_gw_034() {
    let listed: Vec<&str> = TABLE
        .iter()
        .filter(|row| row.class == Class::SafeDrop)
        .map(|row| row.field)
        .collect();
    for field in [
        "service_tier",
        "metadata",
        "user",
        "store",
        "seed",
        "logit_bias",
        "top_k",
        "stream_options",
        "parallel_tool_calls",
    ] {
        assert!(listed.contains(&field), "{field}");
    }
    assert_eq!(class_of(MSG, "cache_control"), Class::Cache);
}

#[test]
fn refuse_unlisted_and_reasoning_classes_match_each_protocol() {
    assert_eq!(class_of(MSG, "context_management"), Class::Refuse);
    assert_eq!(class_of(MSG, "mcp_servers"), Class::Refuse);
    assert_eq!(class_of(CHAT, "context_management"), Class::Unlisted);
    for protocol in ALL {
        assert_eq!(class_of(protocol, "x_custom"), Class::Unlisted);
    }
    assert_eq!(class_of(MSG, "thinking"), Class::Reasoning);
    assert_eq!(class_of(CHAT, "reasoning_effort"), Class::Reasoning);
    assert_eq!(class_of(RESP, "reasoning"), Class::Reasoning);
}

#[test]
fn model_messages_system_and_stream_map() {
    for (protocol, fields) in [
        (CHAT, vec!["model", "messages", "stream"]),
        (RESP, vec!["model", "input", "instructions", "stream"]),
        (MSG, vec!["model", "messages", "system", "stream"]),
        (GEM, vec!["contents", "systemInstruction"]),
    ] {
        for field in fields {
            assert_eq!(
                class_of(protocol, field),
                Class::Map,
                "{protocol:?} {field}"
            );
        }
    }
}

#[test]
fn labels_and_the_refusal_sentence_are_exact() {
    assert_eq!(
        refusal_message("context_management", MSG, CHAT, "Prov", "m"),
        "PingDeck cannot translate context_management from Messages to Chat Completions (Prov/m). \
         Use a target that speaks Messages, or remove context_management from the request."
    );
    assert_eq!(label(RESP), "Responses");
    assert_eq!(label(GEM), "Gemini");
}

// ----------------------------------------------------------- refuse class, every outbound

macro_rules! refused {
    ($name:ident, $inbound:expr, $body:expr, $feature:expr) => {
        #[test]
        fn $name() {
            for outbound in ALL.into_iter().filter(|p| *p != $inbound) {
                assert_eq!(
                    decide($inbound, outbound, &$body, false),
                    Err($feature.to_owned()),
                    "{outbound:?}"
                );
            }
        }
    };
}

refused!(
    context_management_is_refused_toward_every_other_protocol,
    MSG,
    messages(json!({"context_management": {"edits": []}})),
    "context_management"
);
refused!(
    the_mcp_connector_is_refused_toward_every_other_protocol,
    MSG,
    messages(json!({"mcp_servers": [{"type": "url", "url": "https://x", "name": "x"}]})),
    "MCP connector"
);
refused!(
    n_above_one_is_refused_from_chat,
    CHAT,
    chat(json!({"n": 2})),
    "n > 1"
);
refused!(
    candidate_count_above_one_is_refused_from_gemini,
    GEM,
    gemini(json!({"generationConfig": {"candidateCount": 3}})),
    "n > 1"
);

#[test]
fn n_of_one_is_not_a_refusal() {
    assert_eq!(refusal(CHAT, &chat(json!({"n": 1}))), None);
    assert_eq!(refusal(CHAT, &chat(json!({}))), None);
}

#[test]
fn safe_drops_and_unlisted_fields_by_target() {
    for (inbound, outbound, body, expected) in [
        (MSG, CHAT, messages(json!({"top_k": 5})), &["top_k"][..]),
        (MSG, GEM, messages(json!({"top_k": 5})), &[][..]),
        (CHAT, GEM, chat(json!({"user": "u"})), &["user"][..]),
        (
            CHAT,
            MSG,
            chat(json!({"seed": 7, "store": true})),
            &["store", "seed"][..],
        ),
        (
            RESP,
            CHAT,
            responses(json!({"store": false, "metadata": {"a": "b"}})),
            &["metadata", "store"][..],
        ),
        (
            GEM,
            CHAT,
            gemini(json!({"generationConfig": {"topK": 4}})),
            &["top_k"][..],
        ),
        (CHAT, MSG, chat(json!({"x_custom": null})), &[][..]),
    ] {
        assert_eq!(decide(inbound, outbound, &body, false), Ok(names(expected)));
    }
}

#[test]
fn unlisted_fields_and_conversation_references_are_refused() {
    for (inbound, body, feature) in [
        (CHAT, chat(json!({"x_custom": 1})), "x_custom"),
        (
            RESP,
            responses(json!({"previous_response_id": "r1"})),
            "previous_response_id",
        ),
        (
            RESP,
            responses(json!({"conversation": "c1"})),
            "conversation",
        ),
        (RESP, responses(json!({"prompt": {"id": "p"}})), "prompt"),
        (
            GEM,
            gemini(json!({"cachedContent": "cachedContents/x"})),
            "cachedContent",
        ),
        (
            CHAT,
            chat(json!({"functions": [{"name": "f"}]})),
            "functions",
        ),
    ] {
        for outbound in ALL.into_iter().filter(|p| *p != inbound) {
            assert_eq!(
                decide(inbound, outbound, &body, false),
                Err(feature.to_owned()),
                "{inbound:?} -> {outbound:?}"
            );
        }
    }
}

#[test]
fn cache_control_on_a_block_is_dropped_with_the_cache_note() {
    let body = messages(
        json!({"system": [{"type": "text", "text": "s", "cache_control": {"type": "ephemeral"}}]}),
    );
    for outbound in [CHAT, RESP, GEM] {
        assert_eq!(
            decide(MSG, outbound, &body, false),
            Ok(names(&["cache_control"]))
        );
    }
    let request = codec(MSG)
        .unwrap()
        .decode_request(&body, &InboundHint::default())
        .unwrap();
    let encoded = codec(CHAT)
        .unwrap()
        .encode_request(
            &request,
            &EncodeCtx {
                quirks: &Quirks::default(),
            },
        )
        .unwrap();
    let notes = drops(MSG, CHAT, &body, false, &request, &encoded);
    assert_eq!(notes.len(), 1);
    assert_eq!(
        notes[0].reason,
        "prompt cache not available on target, cost may differ"
    );
    assert_eq!(notes[0].kind, TranslationKind::Dropped);
}

#[test]
fn top_k_then_cache_control_keep_the_order_of_the_prd_example() {
    let body = messages(json!({
        "top_k": 3,
        "messages": [{"role": "user", "content": [{"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}]}]
    }));
    assert_eq!(
        decide(MSG, CHAT, &body, false),
        Ok(names(&["top_k", "cache_control"]))
    );
}

#[test]
fn the_beta_header_is_dropped_toward_other_protocols_only() {
    let body = messages(json!({}));
    for outbound in [CHAT, RESP, GEM] {
        assert_eq!(
            decide(MSG, outbound, &body, true),
            Ok(names(&["anthropic-beta"]))
        );
    }
    assert_eq!(decide(MSG, MSG, &body, true), Ok(Vec::new()));
    assert_eq!(decide(MSG, CHAT, &body, false), Ok(Vec::new()));
}

#[test]
fn reasoning_without_an_equivalent_is_dropped_with_its_note() {
    let body = messages(json!({"thinking": {"type": "enabled", "budget_tokens": 2000}}));
    let request = codec(MSG)
        .unwrap()
        .decode_request(&body, &InboundHint::default())
        .unwrap();
    let encoded = codec(CHAT)
        .unwrap()
        .encode_request(
            &request,
            &EncodeCtx {
                quirks: &Quirks::default(),
            },
        )
        .unwrap();
    let notes = drops(MSG, CHAT, &body, false, &request, &encoded);
    assert_eq!(notes.len(), 1);
    assert_eq!(
        (notes[0].field.as_str(), notes[0].reason.as_str()),
        ("thinking", "reasoning not available on target")
    );
}
#[test]
fn claude_code_toward_chat_drops_what_chat_cannot_carry() {
    let (body, beta) = fixture(CLAUDE_CODE);
    assert_eq!(
        decide(MSG, CHAT, &body, beta),
        Ok(names(&[
            "top_k",
            "cache_control",
            "thinking",
            "anthropic-beta"
        ]))
    );
}
#[test]
fn every_codex_and_claude_code_field_is_classified() {
    for (protocol, text) in [(RESP, CODEX), (MSG, CLAUDE_CODE)] {
        let (body, _) = fixture(text);
        let unlisted: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .filter(|key| class_of(protocol, key) == Class::Unlisted)
            .collect();
        assert!(unlisted.is_empty(), "{protocol:?}: {unlisted:?}");
    }
}

#[test]
fn codex_toward_messages_drops_store_and_the_cache_hints_visibly() {
    let (body, beta) = fixture(CODEX);
    let dropped = decide(RESP, MSG, &body, beta).unwrap();
    for field in ["store", "include", "prompt_cache_key"] {
        assert!(dropped.contains(&field.to_owned()), "{dropped:?}");
    }
    // Chat Completions takes the prompt cache key as it is.
    let to_chat = decide(RESP, CHAT, &body, beta).unwrap();
    assert!(
        !to_chat.contains(&"prompt_cache_key".to_owned()),
        "{to_chat:?}"
    );
}
#[test]
fn the_dropped_header_lists_each_name_once_in_order() {
    let note = |field: &str| TranslationNote {
        field: field.to_owned(),
        kind: TranslationKind::Dropped,
        reason: "r".to_owned(),
    };
    assert_eq!(dropped_header(&[]), None);
    assert_eq!(
        dropped_header(&[note("a"), note("b"), note("a")]),
        Some("a,b".to_owned())
    );
    let renamed = TranslationNote {
        field: "x".to_owned(),
        kind: TranslationKind::Renamed,
        reason: "r".to_owned(),
    };
    assert_eq!(dropped_header(&[renamed]), None);
}

#[test]
fn feature_names_come_from_the_codec_message() {
    assert_eq!(
        feature_of_error("anthropic: tool type `x_tool` is not supported"),
        "x_tool"
    );
    assert_eq!(
        feature_of_error("anthropic: does not accept audio input"),
        "audio"
    );
    assert_eq!(feature_of_error("a document inside"), "document");
    assert_eq!(feature_of_error("something else"), "something else");
}

#[test]
fn mapped_fields_a_target_leaves_out_are_dropped_visibly() {
    // Claude 5 refuses sampling and Responses has no stop sequences, so the header says so.
    let body = chat(json!({"temperature": 0.3, "top_p": 0.9, "stop": ["END"]}));
    let mut to_claude5 = body.clone();
    to_claude5["model"] = json!("claude-sonnet-5-5");
    let hint = InboundHint {
        model: None,
        stream: false,
    };
    let request = codec(CHAT)
        .unwrap()
        .decode_request(&to_claude5, &hint)
        .unwrap();
    let encoded = codec(MSG)
        .unwrap()
        .encode_request(
            &request,
            &EncodeCtx {
                quirks: &Quirks::default(),
            },
        )
        .unwrap();
    let notes = drops(CHAT, MSG, &to_claude5, false, &request, &encoded);
    assert_eq!(dropped_header(&notes).as_deref(), Some("temperature,top_p"));
    assert_eq!(decide(CHAT, RESP, &body, false), Ok(names(&["stop"])));
    // `xhigh` reaches a Responses target as `xhigh`, so nothing is dropped.
    let xhigh = responses(json!({"reasoning": {"effort": "xhigh"}}));
    let request = codec(RESP).unwrap().decode_request(&xhigh, &hint).unwrap();
    let quirks = Quirks {
        reasoning_request: Some(crate::llm::wire::ReasoningStyle::ReasoningEffort),
        ..Quirks::default()
    };
    let encoded = codec(CHAT)
        .unwrap()
        .encode_request(&request, &EncodeCtx { quirks: &quirks })
        .unwrap();
    assert_eq!(encoded["reasoning_effort"], "xhigh");
}
