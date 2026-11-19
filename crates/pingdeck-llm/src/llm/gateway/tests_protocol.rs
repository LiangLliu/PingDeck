//! End-to-end tests of the protocol rules of the gateway through a real listener: output caps,
//! refused unlisted fields, prompt-cache breakpoints, signature tags, broken streams, the
//! unknown-name mapping, token counts and the passthrough routes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;

use super::tests::{Canned, Fake, Harness, Reply, TOKEN, canned_json, canned_sse, sse_bytes};
use super::tests_pipeline::{request_of, send, upstream};
use crate::llm::store::{self, ModelSource, ProviderModel};
use crate::llm::wire::Protocol;

const CHAT: Protocol = Protocol::OpenaiChat;
const RESP: Protocol = Protocol::OpenaiResponses;
const MSG: Protocol = Protocol::AnthropicMessages;
const GEM: Protocol = Protocol::Gemini;

fn capped_model(h: &Harness, provider: &str, model: &str, cap: u64) {
    store::save_model(
        &h.db,
        &ProviderModel {
            provider_id: provider.into(),
            model_id: model.into(),
            source: ModelSource::Manual,
            enabled: true,
            max_output: Some(cap),
            ..Default::default()
        },
    )
    .unwrap();
    h.llm.touch();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_output_cap_above_the_target_is_lowered_in_both_paths() {
    let msg = upstream(MSG, "ok").await;
    let chat = upstream(CHAT, "ok").await;
    let h = Harness::start(|_| {});
    h.add_provider("m", msg.port, &[MSG], &["km"], &[]);
    h.add_provider("c", chat.port, &[CHAT], &["kc"], &[]);
    capped_model(&h, "m", "small", 1000);
    capped_model(&h, "c", "small", 1000);
    // Same protocol: only the number changes.
    let sent = r#"{"max_tokens": 64000, "model": "m/small", "messages": [{"role": "user", "content": "hi"}]}"#;
    let bearer = format!("Bearer {TOKEN}");
    let r = h
        .send_raw(
            "POST",
            "/v1/messages",
            &[("authorization", &bearer)],
            Some(sent.as_bytes().to_vec()),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        String::from_utf8_lossy(&msg.requests()[0].raw),
        sent.replace("64000", "1000").replace("m/small", "small")
    );
    // Through the hub format.
    let mut body = request_of(MSG, "c/small", false);
    body["max_tokens"] = json!(64000);
    let r = send(&h, MSG, "c/small", &body, false).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(chat.requests()[0].body["max_tokens"], 1000);
}

#[tokio::test(flavor = "multi_thread")]
async fn unlisted_fields_and_conversation_references_are_refused_toward_other_protocols() {
    let chat = upstream(CHAT, "ok").await;
    let h = Harness::start(|_| {});
    h.add_provider("c", chat.port, &[CHAT], &["kc"], &["gpt"]);
    let mut stateful = request_of(RESP, "c/gpt", false);
    stateful["previous_response_id"] = json!("resp_1");
    let r = send(&h, RESP, "c/gpt", &stateful, false).await;
    assert_eq!(r.status, 400, "{}", r.text());
    assert!(r.text().contains("previous_response_id"), "{}", r.text());
    let mut custom = request_of(MSG, "c/gpt", false);
    custom["x_custom"] = json!(1);
    let r = send(&h, MSG, "c/gpt", &custom, false).await;
    assert_eq!(r.status, 400, "{}", r.text());
    assert_eq!(chat.count(), 0, "no upstream call for a refused request");
    // The same field toward the client's own protocol passes untouched.
    let mut own = request_of(CHAT, "c/gpt", false);
    own["x_custom"] = json!(1);
    assert_eq!(send(&h, CHAT, "c/gpt", &own, false).await.status, 200);
    assert_eq!(chat.requests()[0].body["x_custom"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_messages_target_gets_cache_breakpoints_and_tagged_bodies_keep_theirs() {
    let msg = upstream(MSG, "ok").await;
    let h = Harness::start(|_| {});
    h.add_provider("m", msg.port, &[MSG], &["km"], &["claude"]);
    let mut chat = request_of(CHAT, "m/claude", false);
    chat["messages"] = json!([
        {"role": "system", "content": "be brief"},
        {"role": "user", "content": "hi"}
    ]);
    assert_eq!(send(&h, CHAT, "m/claude", &chat, false).await.status, 200);
    let seen = msg.requests()[0].body.clone();
    assert_eq!(seen["system"][0]["cache_control"]["type"], "ephemeral");
    let last = seen["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["content"][0]["cache_control"]["type"], "ephemeral");
    // A Messages body that echoes a foreign signature stays a passthrough with its own markers.
    let tagged = json!({"model": "m/claude", "max_tokens": 16, "messages": [
        {"role": "user", "content": [{"type": "text", "text": "q", "cache_control": {"type": "ephemeral"}}]},
        {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "t", "signature": "pdk1.gemini.sig"},
            {"type": "text", "text": "a"}
        ]},
        {"role": "user", "content": "more"}
    ]});
    assert_eq!(send(&h, MSG, "m/claude", &tagged, false).await.status, 200);
    let seen = msg.requests()[1].body.clone();
    assert_eq!(
        seen["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(seen["messages"][1]["content"].as_array().unwrap().len(), 1);
    assert!(!seen.to_string().contains("pdk1."));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_without_its_end_marker_ends_with_an_error_frame() {
    for (client, upstream_protocol) in [(CHAT, CHAT), (MSG, CHAT)] {
        // The upstream closes cleanly after the first text, without a finish or `[DONE]`.
        let mut steps = canned_sse(CHAT, "up", &Canned::text(&["half"]));
        steps.truncate(2);
        let forwarded = sse_bytes(&steps);
        let up = Fake::start(move |_, _| Reply::Sse(steps.clone())).await;
        let h = Harness::start(|_| {});
        h.add_provider("a", up.port, &[upstream_protocol], &["ka"], &["ma"]);
        let r = send(&h, client, "a/ma", &request_of(client, "a/ma", true), true).await;
        assert_eq!(r.status, 200);
        let text = r.text();
        if client == upstream_protocol {
            assert!(text.starts_with(&forwarded), "{text}");
        }
        assert!(text.contains("error"), "{client:?}: {text}");
        let row = &h.logs(1).await[0];
        assert_eq!(row.outcome, "failure");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mapped_unknown_name_is_logged_as_asked_and_named_in_a_header() {
    let up = upstream(MSG, "ok").await;
    let h = Harness::start(|_| {});
    h.add_provider("a", up.port, &[MSG], &["ka"], &["claude"]);
    let grant = store::ModelGrant {
        default_model: Some("a/claude".into()),
        extend: Vec::new(),
        unknown_model_to_default: Some(true),
    };
    store::grant_model(&h.db, "gc_base", &grant).unwrap();
    h.llm.touch();
    let sent = r#"{"model":"claude-haiku-old","max_tokens":5,"messages":[{"role":"user","content":"hi"}]}"#;
    let bearer = format!("Bearer {TOKEN}");
    let r = h
        .send_raw(
            "POST",
            "/v1/messages",
            &[("authorization", &bearer)],
            Some(sent.as_bytes().to_vec()),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        r.headers["x-pingdeck-model-mapped"],
        "claude-haiku-old -> a/claude"
    );
    assert_eq!(
        String::from_utf8_lossy(&up.requests()[0].raw),
        sent.replace("claude-haiku-old", "claude")
    );
    let row = &h.logs(1).await[0];
    assert_eq!(row.requested_model, "claude-haiku-old");
    assert_eq!(row.upstream_model.as_deref(), Some("claude"));
}

#[tokio::test(flavor = "multi_thread")]
async fn token_counts_are_forwarded_to_messages_targets_and_marked_when_estimated() {
    let msg = Fake::start(|_, req| {
        if req.path.ends_with("/messages/count_tokens") {
            Reply::Json(200, json!({"input_tokens": 42}))
        } else {
            Reply::Json(200, canned_json(MSG, "up", &Canned::text(&["ok"])))
        }
    })
    .await;
    let chat = upstream(CHAT, "ok").await;
    let h = Harness::start(|_| {});
    h.add_provider("m", msg.port, &[MSG], &["km"], &["claude"]);
    h.add_provider("c", chat.port, &[CHAT], &["kc"], &["gpt"]);
    let bearer = format!("Bearer {TOKEN}");
    let count = |model: &str| json!({"model": model, "messages": [{"role": "user", "content": "hello there"}]});
    let r = h
        .send(
            "POST",
            "/v1/messages/count_tokens",
            &[("authorization", &bearer)],
            Some(&count("m/claude")),
        )
        .await;
    assert_eq!(r.json()["input_tokens"], 42);
    assert!(!r.headers.contains_key("x-pingdeck-token-estimate"));
    assert_eq!(msg.requests()[0].body["model"], "claude");
    let r = h
        .send(
            "POST",
            "/v1/messages/count_tokens",
            &[("authorization", &bearer)],
            Some(&count("c/gpt")),
        )
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.headers["x-pingdeck-token-estimate"], "1");
    assert_eq!(chat.count(), 0);
    let r = h
        .send(
            "POST",
            "/v1beta/models/c%2Fgpt:countTokens",
            &[("authorization", &bearer)],
            Some(&request_of(GEM, "c/gpt", false)),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.json()["totalTokens"].as_u64().is_some());
    assert_eq!(r.headers["x-pingdeck-token-estimate"], "1");
}

#[tokio::test(flavor = "multi_thread")]
async fn completions_rerank_and_gemini_embeddings_pass_through() {
    let up = Fake::start(|_, req| match req.path.as_str() {
        "/v1/completions" => Reply::Json(
            200,
            json!({"id": "c1", "object": "text_completion", "choices": [{"text": "fn", "index": 0}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 1}}),
        ),
        "/v1/rerank" => Reply::Json(
            200,
            json!({"results": [{"index": 0, "relevance_score": 0.9}]}),
        ),
        path if path.contains(":embedContent") => {
            Reply::Json(200, json!({"embedding": {"values": [0.1, 0.2]}}))
        }
        _ => Reply::Json(404, json!({"error": {"message": "no"}})),
    })
    .await;
    let h = Harness::start(|_| {});
    h.add_provider(
        "o",
        up.port,
        &[CHAT, Protocol::Embeddings],
        &["ko"],
        &["code", "rr"],
    );
    h.add_provider("g", up.port, &[GEM], &["kg"], &["emb"]);
    let bearer = format!("Bearer {TOKEN}");
    let auth = [("authorization", bearer.as_str())];
    let fim = json!({"model": "o/code", "prompt": "def", "suffix": "end", "max_tokens": 4});
    let r = h.send("POST", "/v1/completions", &auth, Some(&fim)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["choices"][0]["text"], "fn");
    assert_eq!(up.requests()[0].body["suffix"], "end");
    assert_eq!(up.requests()[0].body["model"], "code");
    let streamed = json!({"model": "o/code", "prompt": "def", "stream": true});
    assert_eq!(
        h.send("POST", "/v1/completions", &auth, Some(&streamed))
            .await
            .status,
        400
    );
    let rerank = json!({"model": "o/rr", "query": "q", "documents": ["a"]});
    let r = h.send("POST", "/v1/rerank", &auth, Some(&rerank)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(up.requests()[1].path, "/v1/rerank");
    let embed = json!({"content": {"parts": [{"text": "hi"}]}});
    let r = h
        .send(
            "POST",
            "/v1beta/models/g%2Femb:embedContent",
            &auth,
            Some(&embed),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["embedding"]["values"][1], 0.2);
    assert!(up.requests()[2].path.contains("/models/emb:embedContent"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exhausted_rate_limited_walk_tells_the_client_when_to_come_back() {
    let limited = Fake::always(Reply::WithHeaders(
        vec![("retry-after", "30".to_owned())],
        Box::new(Reply::Json(
            429,
            json!({"error": {"message": "rate limit reached"}}),
        )),
    ))
    .await;
    let h = Harness::start(|_| {});
    h.add_provider("a", limited.port, &[CHAT], &["ka"], &["ma", "mb"]);
    let r = send(&h, CHAT, "a/ma", &request_of(CHAT, "a/ma", false), false).await;
    assert_eq!(r.status, 429);
    assert_eq!(r.headers["retry-after"], "30");
    // The rate limit rests the key for that model only: the other model is still tried.
    let before = limited.count();
    let r = send(&h, CHAT, "a/mb", &request_of(CHAT, "a/mb", false), false).await;
    assert_eq!(r.status, 429);
    assert!(
        limited.count() > before,
        "model mb was not held back by ma's limit"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_messages_relay_that_refuses_cache_hints_gets_the_call_again_without_them() {
    let msg = Fake::start(|_, req| {
        if req.body.to_string().contains("cache_control") {
            Reply::Json(
                400,
                json!({"type": "error", "error": {"type": "invalid_request_error",
                    "message": "system.0.cache_control: Extra inputs are not permitted"}}),
            )
        } else {
            Reply::Json(200, canned_json(MSG, "up", &Canned::text(&["ok"])))
        }
    })
    .await;
    let h = Harness::start(|_| {});
    h.add_provider("m", msg.port, &[MSG], &["km"], &["claude"]);
    let mut chat = request_of(CHAT, "m/claude", false);
    chat["messages"] = json!([
        {"role": "system", "content": "be brief"},
        {"role": "user", "content": "hi"}
    ]);
    let r = send(&h, CHAT, "m/claude", &chat, false).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let seen = msg.requests();
    assert_eq!(
        seen.len(),
        2,
        "one refused attempt, one retry on the same target"
    );
    assert!(!seen[1].body.to_string().contains("cache_control"));
    assert_eq!(
        h.health("km", "claude")
            .map_or(0, |health| health.consecutive_failures),
        0,
        "a refused hint says nothing about the upstream's health"
    );
    // The provider is remembered: the next call goes without hints at once.
    assert_eq!(send(&h, CHAT, "m/claude", &chat, false).await.status, 200);
    assert_eq!(msg.count(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_without_count_tokens_gets_the_estimate_and_keeps_its_health() {
    let msg = Fake::start(|_, req| {
        if req.path.ends_with("/messages/count_tokens") {
            Reply::Json(404, json!({"error": {"message": "model claude not found"}}))
        } else {
            Reply::Json(200, canned_json(MSG, "up", &Canned::text(&["ok"])))
        }
    })
    .await;
    let h = Harness::start(|_| {});
    h.add_provider("m", msg.port, &[MSG], &["km"], &["claude"]);
    let bearer = format!("Bearer {TOKEN}");
    let count =
        json!({"model": "m/claude", "messages": [{"role": "user", "content": "hello there"}]});
    let r = h
        .send(
            "POST",
            "/v1/messages/count_tokens",
            &[("authorization", &bearer)],
            Some(&count),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.headers["x-pingdeck-token-estimate"], "1");
    assert!(r.json()["input_tokens"].as_u64().is_some());
    assert_eq!(msg.count(), 1, "the upstream was asked first");
    assert!(h.health("km", "claude").is_none(), "no health record");
    assert_eq!(h.llm.keys.cooling_until("km", "claude"), None);
    // Chat on the same key and model is not held back.
    let r = send(
        &h,
        MSG,
        "m/claude",
        &request_of(MSG, "m/claude", false),
        false,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
}
