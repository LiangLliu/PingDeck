//! End-to-end tests of the gateway request pipeline through a real listener: byte-exact
//! passthrough, the model name of re-encoded answers, the failover and waiting
//! rules, the no-target answer, the capability pre-filter, refusals
//! and visible drops.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::tests::{
    Canned, Fake, Harness, Reply, Resp, TOKEN, canned_json, canned_sse, decode_stream, frame,
    sse_bytes,
};
use crate::llm::capabilities::{Capability, CapabilitySource, ModelCapabilities};
use crate::llm::capture::CaptureMode;
use crate::llm::health::CircuitState;
use crate::llm::hub::collect_events;
use crate::llm::store;
use crate::llm::wire::Protocol;

const CHAT: Protocol = Protocol::OpenaiChat;
const RESP: Protocol = Protocol::OpenaiResponses;
const MSG: Protocol = Protocol::AnthropicMessages;
const GEM: Protocol = Protocol::Gemini;

pub(super) fn path_of(protocol: Protocol, model: &str, stream: bool) -> String {
    match protocol {
        CHAT => "/v1/chat/completions".to_owned(),
        RESP => "/v1/responses".to_owned(),
        MSG => "/v1/messages".to_owned(),
        _ => format!(
            "/v1beta/models/{model}:{}",
            if stream {
                "streamGenerateContent"
            } else {
                "generateContent"
            }
        ),
    }
}

pub(super) fn request_of(protocol: Protocol, model: &str, stream: bool) -> Value {
    match protocol {
        CHAT => {
            json!({"model": model, "stream": stream, "messages": [{"role": "user", "content": "hi"}]})
        }
        RESP => json!({"model": model, "stream": stream, "input": "hi"}),
        MSG => {
            json!({"model": model, "stream": stream, "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]})
        }
        _ => json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]}),
    }
}

pub(super) async fn upstream(protocol: Protocol, text: &'static str) -> Fake {
    Fake::start(move |_, req| {
        let canned = Canned::text(&[text]);
        let stream = req.body["stream"] == true || req.path.contains("streamGenerateContent");
        if stream {
            Reply::Sse(canned_sse(protocol, "up", &canned))
        } else {
            Reply::Json(200, canned_json(protocol, "up", &canned))
        }
    })
    .await
}

pub(super) async fn send(
    h: &Harness,
    protocol: Protocol,
    model: &str,
    body: &Value,
    stream: bool,
) -> Resp {
    let bearer = format!("Bearer {TOKEN}");
    h.send(
        "POST",
        &path_of(protocol, model, stream),
        &[("authorization", &bearer)],
        Some(body),
    )
    .await
}

fn text_of(response: &Resp, protocol: Protocol) -> String {
    if let Ok(json) = serde_json::from_slice::<Value>(&response.body) {
        crate::llm::adapters::codec(protocol)
            .unwrap()
            .decode_response(&json)
            .unwrap()
            .text()
    } else {
        collect_events(&decode_stream(protocol, &response.body).unwrap())
            .unwrap()
            .text()
    }
}

fn raw_client_body(protocol: Protocol, alias: &str, stream: bool) -> String {
    let flag = if stream { "true" } else { "false" };
    match protocol {
        CHAT => format!(r#"{{"messages": [{{"role":"user","content":"hi"}}], "x_custom": 1, "model": "{alias}", "stream": {flag}}}"#),
        MSG => format!(r#"{{"max_tokens": 5, "x_custom": 1, "model":"{alias}", "messages":[{{"role":"user","content":[{{"type":"text","text":"hi","cache_control":{{"type":"ephemeral"}}}}]}}], "stream": {flag}}}"#),
        RESP => format!(r#"{{"input":"hi", "x_custom":1, "model":"{alias}", "stream":{flag}}}"#),
        _ => r#"{"contents":[{"parts":[{"text":"hi"}],"role":"user"}], "x_custom": 1, "cache_control": {"type":"ephemeral"}}"#.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn passthrough_is_byte_exact_and_re_encoded_answers_carry_requested_name() {
    let h = Harness::start(|_| {});
    let bearer = format!("Bearer {TOKEN}");
    let mut fakes = Vec::new();
    for (idx, protocol) in [CHAT, MSG, RESP, GEM].into_iter().enumerate() {
        let fake = upstream(protocol, "pong").await;
        let prov = format!("p{idx}");
        let key = format!("k{idx}");
        let alias = format!("the-alias-{idx}");
        h.add_provider(&prov, fake.port, &[protocol], &[&key], &["real-model"]);
        h.add_alias(&alias, &[(&prov, None, "real-model")]);
        fakes.push((protocol, alias, fake));
    }

    for (protocol, alias, fake) in &fakes {
        for stream in [false, true] {
            let sent = raw_client_body(*protocol, alias, stream);
            let r = h
                .send_raw(
                    "POST",
                    &path_of(*protocol, alias, stream),
                    &[("authorization", &bearer)],
                    Some(sent.clone().into_bytes()),
                )
                .await;
            assert_eq!(r.status, 200);
            let seen = fake.requests().pop().unwrap();
            assert_eq!(
                String::from_utf8_lossy(&seen.raw),
                sent.replace(&format!("\"{alias}\""), "\"real-model\"")
            );
            let canned = Canned::text(&["pong"]);
            if stream {
                assert_eq!(r.text(), sse_bytes(&canned_sse(*protocol, "up", &canned)));
            } else {
                assert_eq!(
                    r.body,
                    serde_json::to_vec(&canned_json(*protocol, "up", &canned)).unwrap()
                );
            }
        }
    }
    let rows = h.logs(8).await;
    assert!(
        rows.iter()
            .all(|row| !row.translated && row.alias.is_none())
    );

    for (_, alias, _) in &fakes {
        for client in [CHAT, RESP, MSG, GEM] {
            let r = send(&h, client, alias, &request_of(client, alias, false), false).await;
            assert_eq!(r.status, 200);
            if r.text().contains("\"up\"") {
                continue; // same-protocol passthrough keeps upstream response bytes
            }
            let json = r.json();
            let model = json
                .get("model")
                .or_else(|| json.get("modelVersion"))
                .and_then(Value::as_str);
            assert_eq!(model, Some(alias.as_str()));
        }
    }
}

fn retry_after_ms(ms: u64, status: u16) -> Reply {
    Reply::WithHeaders(
        vec![("retry-after-ms", ms.to_string())],
        Box::new(Reply::Json(
            status,
            json!({"error": {"message": "slow down"}}),
        )),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn failover_retry_after_and_open_circuit_503() {
    let a = Fake::always(retry_after_ms(20_000, 429)).await;
    let b = upstream(CHAT, "from b").await;
    let h = Harness::start(|_| {});
    h.add_provider("a", a.port, &[CHAT], &["ka"], &["ma"]);
    h.add_provider("b", b.port, &[CHAT], &["kb"], &["mb"]);
    h.add_alias("fo", &[("a", None, "ma"), ("b", None, "mb")]);

    let r = tokio::time::timeout(
        Duration::from_secs(10),
        send(&h, CHAT, "fo", &request_of(CHAT, "fo", false), false),
    )
    .await
    .unwrap();
    assert_eq!(text_of(&r, CHAT), "from b");
    // The answer names the target that served and counts the attempts.
    assert_eq!(
        (
            r.headers["x-pingdeck-target"].as_str(),
            r.headers["x-pingdeck-attempts"].as_str()
        ),
        ("b/mb", "2")
    );
    let row = &h.logs(1).await[0];
    assert_eq!(row.attempts[0]["error"], "429 retry-after 20");
    assert_eq!((row.retry_after_s, &row.reason), (Some(20), &None));
    assert_eq!(h.health("ka", "ma").unwrap().state, CircuitState::Closed);

    // Short retry-after on sole target is waited out once.
    let short = Fake::start(|n, _| {
        if n == 0 {
            retry_after_ms(400, 429)
        } else {
            Reply::Json(200, canned_json(CHAT, "up", &Canned::text(&["after wait"])))
        }
    })
    .await;
    h.add_provider("s", short.port, &[CHAT], &["ks"], &["ms"]);
    h.add_alias("solo", &[("s", None, "ms")]);
    let started = Instant::now();
    let r = send(&h, CHAT, "solo", &request_of(CHAT, "solo", false), false).await;
    assert!(started.elapsed() >= Duration::from_millis(350));
    assert_eq!(text_of(&r, CHAT), "after wait");
    assert!(h.logs(2).await[1].timing.as_ref().unwrap().queue_ms >= 350);
    drop(h);

    // Open circuit returns 503 with Retry-After and exact sentence.
    let down = Fake::always(Reply::Json(500, json!({"error": {"message": "down"}}))).await;
    let h2 = Harness::start(|g| {
        g.circuit_failure_threshold = 1;
        g.circuit_open_s = 3600;
    });
    h2.add_provider("a", down.port, &[CHAT], &["ka"], &["ma"]);
    h2.add_alias("claude-code", &[("a", None, "ma")]);
    assert_eq!(
        send(
            &h2,
            CHAT,
            "claude-code",
            &request_of(CHAT, "claude-code", false),
            false
        )
        .await
        .status,
        500
    );
    let r = send(
        &h2,
        CHAT,
        "claude-code",
        &request_of(CHAT, "claude-code", false),
        false,
    )
    .await;
    assert_eq!(r.status, 503);
    let secs: u64 = r.headers["retry-after"].parse().unwrap();
    assert_eq!(
        r.json()["error"]["message"],
        format!("No target available for 'claude-code'. The earliest retry is in {secs} s.")
    );
    assert_eq!(r.json()["error"]["type"], "overloaded_error");
    assert_eq!(down.count(), 1);
    let rows = h2.logs(2).await;
    assert_eq!(rows[1].reason.as_deref(), Some("circuit open"));
    assert_eq!(
        (rows[1].provider_id.as_deref(), rows[1].key_id.as_deref()),
        (Some("a"), Some("ka"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn capability_skip_refused_features_and_visible_drops() {
    let a = upstream(CHAT, "from a").await;
    let b = upstream(CHAT, "from b").await;
    let h = Harness::start(|g| g.capture_mode = CaptureMode::All);
    h.add_provider("a", a.port, &[CHAT], &["ka"], &["ma"]);
    h.add_provider("b", b.port, &[CHAT], &["kb"], &["mb"]);
    h.add_alias("fo", &[("a", None, "ma"), ("b", None, "mb")]);

    let mut capabilities = ModelCapabilities::default();
    capabilities.apply(Capability::Vision, false, CapabilitySource::UserOverride);
    store::set_model_capabilities_override(&h.db, "a", "ma", &capabilities).unwrap();
    capabilities.apply(Capability::Vision, true, CapabilitySource::UserOverride);
    store::set_model_capabilities_override(&h.db, "b", "mb", &capabilities).unwrap();
    h.llm.touch();

    let img = json!({"model": "fo", "messages": [{"role": "user", "content": [
        {"type": "text", "text": "what is this"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,QUJD"}}
    ]}]});
    let r = send(&h, CHAT, "fo", &img, false).await;
    assert_eq!(text_of(&r, CHAT), "from b");
    assert_eq!(a.count(), 0);
    assert_eq!(h.logs(1).await[0].attempts[0]["error"], "no vision");
    h.add_alias("al", &[("a", None, "ma")]);

    // Refused feature -> 400 in client's dialect.
    let mut refused = request_of(MSG, "al", false);
    refused["context_management"] = json!({"edits": []});
    let r = send(&h, MSG, "al", &refused, false).await;
    assert_eq!(r.status, 400);
    assert_eq!(
        r.json()["error"]["message"],
        "PingDeck cannot translate context_management from Messages to Chat Completions (Prov a/ma). \
         Use a target that speaks Messages, or remove context_management from the request."
    );
    assert_eq!(a.count(), 0);
    assert_eq!(
        h.logs(2).await[1].attempts[0]["error"],
        "cannot carry context_management"
    );

    // Visible drops header and capture translation notes.
    let drop_body = json!({
        "model": "al", "max_tokens": 5, "top_k": 4,
        "messages": [{"role": "user", "content": [{"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}]}]
    });
    let r = send(&h, MSG, "al", &drop_body, false).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.headers["x-pingdeck-dropped"], "top_k,cache_control");
    let seen = &a.requests()[0].body;
    assert!(seen.get("top_k").is_none());
    assert!(!seen.to_string().contains("cache_control"));
    let id = h.logs(3).await[2].id;
    let cap = store::get_log(&h.db, id).unwrap().unwrap().capture.unwrap();
    let cache = cap
        .translation_notes
        .iter()
        .find(|n| n.field == "cache_control")
        .unwrap();
    assert_eq!(
        cache.reason,
        "prompt cache not available on target, cost may differ"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn passthrough_forwards_the_clients_anthropic_version() {
    // The upstream only accepts temperature with the version the client pinned.
    let up = Fake::start(|_, req| {
        if req.header("anthropic-version") == Some("2025-01-01") {
            Reply::Json(200, canned_json(MSG, "up", &Canned::text(&["ok"])))
        } else {
            Reply::Json(
                400,
                json!({"error": {"message": "temperature: unsupported"}}),
            )
        }
    })
    .await;
    let h = Harness::start(|_| {});
    h.add_provider("a", up.port, &[MSG], &["ka"], &["claude"]);
    h.edit_provider("a", |p| {
        p.quirks.anthropic_version = Some("2023-06-01".into())
    });
    h.add_alias("cc", &[("a", None, "claude")]);
    let mut body = request_of(MSG, "cc", false);
    body["temperature"] = json!(1);
    let bearer = format!("Bearer {TOKEN}");
    let r = h
        .send(
            "POST",
            "/v1/messages",
            &[
                ("authorization", &bearer),
                ("anthropic-version", "2025-01-01"),
            ],
            Some(&body),
        )
        .await;
    assert_eq!(r.status, 200);
    // Without the client's header the provider's pinned value is sent.
    send(&h, MSG, "cc", &body, false).await;
    assert_eq!(
        up.requests()[1].header("anthropic-version"),
        Some("2023-06-01")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn text_mentioning_the_signature_tag_still_passes_byte_exact() {
    // Only a signature value starting with the tag marks a body as tagged.
    let up = upstream(MSG, "pong").await;
    let h = Harness::start(|_| {});
    h.add_provider("a", up.port, &[MSG], &["ka"], &["real"]);
    h.add_alias("al", &[("a", None, "real")]);
    let sent = r#"{"max_tokens":5,"x_custom":1,"model":"al","messages":[{"role":"user","content":"is pdk1.gemini.x a key?"}]}"#;
    let bearer = format!("Bearer {TOKEN}");
    let r = h
        .send_raw(
            "POST",
            "/v1/messages",
            &[("authorization", &bearer)],
            Some(sent.as_bytes().to_vec()),
        )
        .await;
    assert_eq!(r.status, 200);
    let seen = up.requests().pop().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&seen.raw),
        sent.replace("\"al\"", "\"real\"")
    );
    let tagged = super::handlers::carries_signature_tag;
    assert!(tagged(br#"{"signature":"pdk1.gemini.abc"}"#));
    assert!(!tagged(br#"{"text":"\"pdk1.gemini.abc"}"#));
    assert!(!tagged(br#"{"text":"pdk1.nope.abc"}"#));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upstream_client_error_reaches_a_cross_protocol_client_in_its_own_dialect() {
    // An Anthropic client of an OpenAI upstream gets an Anthropic error body.
    let up = Fake::start(|_, _| {
        Reply::Json(
            400,
            json!({"error": {"message": "temperature: unsupported", "type": "invalid_request_error", "param": "temperature", "code": null}}),
        )
    })
    .await;
    let h = Harness::start(|_| {});
    h.add_provider("a", up.port, &[CHAT], &["ka"], &["gpt"]);
    h.add_alias("cc", &[("a", None, "gpt")]);
    let r = send(&h, MSG, "cc", &request_of(MSG, "cc", false), false).await;
    assert_eq!(r.status, 400);
    let body = r.json();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["message"], "temperature: unsupported");
    assert!(body["error"].get("param").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bare_unknown_model_name_goes_to_the_clients_default_model_when_it_opts_in() {
    // Claude Code's haiku tier names a model the gateway does not know.
    let up = upstream(MSG, "ok").await;
    let h = Harness::start(|_| {});
    h.add_provider("a", up.port, &[MSG], &["ka"], &["claude"]);
    let haiku = request_of(MSG, "claude-3-5-haiku-latest", false);
    assert_ne!(send(&h, MSG, "x", &haiku, false).await.status, 200);
    let grant = store::ModelGrant {
        default_model: Some("a/claude".into()),
        extend: Vec::new(),
        unknown_model_to_default: Some(true),
    };
    store::grant_model(&h.db, "gc_base", &grant).unwrap();
    h.llm.touch();
    assert_eq!(send(&h, MSG, "x", &haiku, false).await.status, 200);
    assert_eq!(up.requests().last().unwrap().body["model"], "claude");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_same_protocol_stream_with_an_unknown_output_item_passes_whole() {
    // The Responses decoder refuses an `mcp_list_tools` item. The
    // pass-through still forwards every byte, logs a success and counts no upstream failure.
    let mut steps = canned_sse(RESP, "up", &Canned::text(&["pong"]));
    let item = json!({"type": "response.output_item.added", "output_index": 0,
        "item": {"type": "mcp_list_tools", "id": "mcp_1", "tools": []}});
    steps.insert(1, frame(Some("response.output_item.added"), &item));
    let expected = sse_bytes(&steps);
    let up = Fake::start(move |_, _| Reply::Sse(steps.clone())).await;
    let h = Harness::start(|_| {});
    h.add_provider("a", up.port, &[RESP], &["ka"], &["ma"]);
    h.add_alias("al", &[("a", None, "ma")]);
    let r = send(&h, RESP, "al", &request_of(RESP, "al", true), true).await;
    assert_eq!(r.status, 200);
    assert_eq!(String::from_utf8_lossy(&r.body), expected);
    assert_eq!(h.logs(1).await[0].outcome, "success");
    assert_ne!(
        h.health("ka", "ma").map(|health| health.state),
        Some(CircuitState::Open)
    );
}
