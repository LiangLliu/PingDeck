//! Tests of the in-process chat client against fake upstreams.
//!
//! Uses the fake servers and canned answers of `gateway::tests`; no gateway listener is needed,
//! the client talks to the upstreams directly through a runtime over a real SQLite file.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::llm::capabilities::{Capability, CapabilitySource, ModelCapabilities};
use crate::llm::client::{ChatError, ChatErrorKind, ChatTarget, LlmClient};
use crate::llm::failure::FailureKind;
use crate::llm::gateway::tests::{
    CHAT_PROTOCOLS, Canned, Fake, Harness, Matrix, Reply, Step, canned_json, canned_sse,
    decode_request, partial_chat_stream, tag,
};
use crate::llm::health::{CircuitState, Permit};
use crate::llm::hub::{
    HubEvent, HubFinish, HubImage, HubMessage, HubPart, HubRequest, HubRole, HubTool,
    collect_events,
};
use crate::llm::wire::Protocol;
use spanbay_base::cancel::CancelToken;
use spanbay_base::error::AppError;

fn hello() -> HubRequest {
    let mut request = HubRequest::user_prompt("ignored-model", "hi");
    request.system = Some("be brief".into());
    request
}

fn named(name: &str) -> ChatTarget {
    let fallbacks: &[&str] = match name {
        "fo" => &["b/mb"],
        "dup" => &["a/m2"],
        "many" => &["p1/m", "p2/m", "p3/m", "p4/m"],
        _ => &[],
    };
    ChatTarget::Named {
        name: name.into(),
        fallbacks: fallbacks.iter().map(|f| (*f).to_owned()).collect(),
    }
}

fn pinned(provider: &str, key: Option<&str>, model: &str) -> ChatTarget {
    ChatTarget::Pinned {
        provider_id: provider.into(),
        key_id: key.map(str::to_owned),
        model: model.into(),
    }
}

fn busy() -> Reply {
    Reply::Json(503, json!({"error": {"message": "busy"}}))
}

fn rejection(status: u16) -> Reply {
    Reply::Json(status, json!({"error": {"message": "bad key"}}))
}

fn slow_down() -> Reply {
    Reply::Json(429, json!({"error": {"message": "slow down"}}))
}

fn with_header(name: &'static str, value: &str, reply: Reply) -> Reply {
    Reply::WithHeaders(vec![(name, value.to_owned())], Box::new(reply))
}

fn chat_reply(text: &'static str, stream: bool) -> Reply {
    let canned = Canned::text(&[text]);
    if stream {
        Reply::Sse(canned_sse(Protocol::OpenaiChat, "up", &canned))
    } else {
        Reply::Json(200, canned_json(Protocol::OpenaiChat, "up", &canned))
    }
}

async fn good_chat(text: &'static str) -> Fake {
    Fake::start(move |_, req| chat_reply(text, req.body["stream"] == true)).await
}

fn two_providers(
    a: &Fake,
    b: &Fake,
    tweak: impl FnOnce(&mut spanbay_model::settings::AiGatewaySettings),
) -> (Harness, LlmClient) {
    let h = Harness::build(tweak);
    h.add_provider("a", a.port, &[Protocol::OpenaiChat], &["ka"], &["ma"]);
    h.add_provider("b", b.port, &[Protocol::OpenaiChat], &["kb"], &["mb"]);
    h.add_alias("fo", &[("a", None, "ma"), ("b", None, "mb")]);
    let client = LlmClient::new(Arc::clone(&h.llm));
    (h, client)
}

async fn events_of(
    client: &LlmClient,
    target: ChatTarget,
    request: HubRequest,
) -> Vec<Result<HubEvent, AppError>> {
    client.stream(target, request, None).collect().await
}

async fn collect_text(client: &LlmClient, name: &str, stream: bool) -> Result<String, AppError> {
    if stream {
        let events: Vec<HubEvent> = events_of(client, named(name), hello())
            .await
            .into_iter()
            .collect::<Result<_, _>>()?;
        Ok(collect_events(&events).unwrap().text())
    } else {
        Ok(client.complete(&named(name), hello(), None).await?.text())
    }
}

fn reasoning_of(content: &[HubPart]) -> String {
    content
        .iter()
        .filter_map(|p| match p {
            HubPart::Reasoning { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn targets_for(protocol: Protocol) -> Vec<ChatTarget> {
    let t = tag(protocol);
    let model = format!("m-{t}");
    vec![
        named(&format!("al-{t}")),
        named(&format!("{t}/{model}")),
        pinned(t, Some(&format!("k-{t}")), &model),
        pinned(t, None, &model),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn complete_and_stream_work_across_protocols_and_targets() {
    let served = HubEvent::Served {
        provider_id: "p".into(),
        model: "m".into(),
        attempts: Vec::new(),
    };
    for protocol in CHAT_PROTOCOLS {
        assert!(
            crate::llm::adapters::codec(protocol)
                .unwrap()
                .stream_encoder()
                .push(&served)
                .is_empty()
        );
    }

    let m = Matrix::over(Harness::build(|_| {})).await;
    *m.canned.lock().unwrap() = Canned::reasoning();
    let client = LlmClient::new(Arc::clone(&m.h.llm));
    for protocol in CHAT_PROTOCOLS {
        let model = format!("m-{}", tag(protocol));
        for (round, target) in targets_for(protocol).into_iter().enumerate() {
            let resp = client.complete(&target, hello(), None).await.unwrap();
            assert_eq!(resp.text(), "Hello", "{protocol:?} {target:?}");
            assert_eq!(reasoning_of(&resp.content), "let me think");
            assert_eq!(
                (
                    resp.usage.input_tokens,
                    resp.usage.output_tokens,
                    resp.finish
                ),
                (Some(11), Some(7), HubFinish::Stop)
            );
            let seen = m.fake(protocol).requests();
            let req = decode_request(protocol, &seen[round * 2], &model);
            assert!(!req.stream && req.system.as_deref() == Some("be brief"));
            assert_eq!(req.messages.last().unwrap().text(), "hi");
            if protocol == Protocol::Gemini {
                assert!(
                    seen[round * 2]
                        .path
                        .contains(&format!("/models/{model}:generateContent"))
                );
            } else {
                assert_eq!(req.model, model);
            }

            let events: Vec<HubEvent> = events_of(&client, target, hello())
                .await
                .into_iter()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(
                events.first(),
                Some(&HubEvent::Served {
                    provider_id: tag(protocol).to_owned(),
                    model: model.clone(),
                    attempts: Vec::new(),
                })
            );
            assert!(matches!(events.get(1), Some(HubEvent::Start { model: s, .. }) if *s == model));
            let s_resp = collect_events(&events).unwrap();
            assert_eq!(s_resp.text(), "Hello");
            assert_eq!(reasoning_of(&s_resp.content), "let me think");
            assert_eq!(
                (s_resp.usage.input_tokens, s_resp.usage.output_tokens),
                (Some(11), Some(7))
            );
            let seen = m.fake(protocol).requests();
            assert!(decode_request(protocol, &seen[round * 2 + 1], &model).stream);
        }
    }
    assert_eq!(m.h.health("k-chat", "m-chat").unwrap().total, 8);
}

#[tokio::test(flavor = "multi_thread")]
async fn failover_final_errors_and_unresolvable_targets() {
    let a = Fake::always(busy()).await;
    let b = good_chat("from b").await;
    let (h, client) = two_providers(&a, &b, |_| {});
    assert_eq!(collect_text(&client, "fo", false).await.unwrap(), "from b");
    assert_eq!(collect_text(&client, "fo", true).await.unwrap(), "from b");
    assert_eq!((a.count(), b.count()), (2, 2));
    assert_eq!(h.health("ka", "ma").unwrap().consecutive_failures, 2);
    assert_eq!(h.health("kb", "mb").unwrap().consecutive_failures, 0);

    h.edit_provider("b", |p| p.enabled = false);
    assert!(matches!(
        client.complete(&named("b/mb"), hello(), None).await,
        Err(AppError::NotFound { .. })
    ));
    let pinned_err = client
        .complete(&pinned("b", None, "mb"), hello(), None)
        .await
        .unwrap_err();
    assert!(pinned_err.message().contains("no enabled provider key"));
    drop(client);
    drop(h);
    // Stream whose first answer is non-SSE JSON fails over too.
    let json_only = Fake::always(chat_reply("x", false)).await;
    let (_h2, client2) = two_providers(&json_only, &b, |_| {});
    assert_eq!(collect_text(&client2, "fo", true).await.unwrap(), "from b");
    drop(client2);
    drop(_h2);

    // When every attempt fails, complete errors and stream yields one error then ends.
    let slow = Fake::always(slow_down()).await;
    let (_h3, client3) = two_providers(&a, &slow, |_| {});
    let err = client3
        .complete(&named("fo"), hello(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Network { .. }) && err.message().contains("slow down"));
    let items = events_of(&client3, named("fo"), hello()).await;
    assert_eq!(items.len(), 1);
    let s_err = items.into_iter().next().unwrap().unwrap_err();
    assert!(matches!(s_err, AppError::Network { .. }) && s_err.message().contains("429"));

    // Unresolvable targets fail without calling upstream.
    assert!(matches!(
        client3.complete(&named("nope"), hello(), None).await,
        Err(AppError::NotFound { .. })
    ));
    assert!(matches!(
        client3
            .complete(&pinned("ghost", None, "m"), hello(), None)
            .await,
        Err(AppError::Validation { .. })
    ));
    let bad_key = events_of(&client3, pinned("a", Some("no-such-key"), "ma"), hello()).await;
    assert_eq!(bad_key.len(), 1);
    assert!(bad_key[0].is_err());
    drop(client3);
    drop(_h3);

    // Client errors (400, 404) and zero retry budget do not fail over.
    let ok = good_chat("never").await;
    for status in [400_u16, 404] {
        let bad = Fake::always(Reply::Json(
            status,
            json!({"error": {"message": "nope for sure"}}),
        ))
        .await;
        let (_h, c) = two_providers(&bad, &ok, |_| {});
        for stream in [false, true] {
            let err = collect_text(&c, "fo", stream).await.unwrap_err();
            assert!(err.message().contains("nope for sure"));
        }
        assert_eq!((bad.count(), ok.count()), (2, 0));
    }
    let (_h, zero_c) = two_providers(&a, &ok, |g| g.max_retries = 0);
    assert!(zero_c.complete(&named("fo"), hello(), None).await.is_err());
    assert_eq!(ok.count(), 0);
}

async fn cancel_when_seen(upstream: &Fake, count: usize, token: &CancelToken) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while upstream.count() < count {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    token.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_drop_and_cancellation() {
    let a = Fake::start(|n, _| {
        if n == 0 {
            busy()
        } else {
            Reply::Sse(partial_chat_stream())
        }
    })
    .await;
    let h = Harness::build(|g| {
        g.circuit_failure_threshold = 1;
        g.circuit_open_s = 1;
    });
    h.add_provider("a", a.port, &[Protocol::OpenaiChat], &["ka"], &["ma"]);
    let client = LlmClient::new(Arc::clone(&h.llm));
    assert!(
        client
            .complete(&named("a/ma"), hello(), None)
            .await
            .is_err()
    );
    assert_eq!(h.health("ka", "ma").unwrap().state, CircuitState::Open);
    h.llm.health.advance(Duration::from_secs(1));

    let mut stream = client.stream(named("a/ma"), hello(), None);
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        HubEvent::Served { .. }
    ));
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        HubEvent::Start { .. }
    ));
    assert!(h.llm.health.is_blocked("ka", "ma"));
    drop(stream);
    assert!(!h.llm.health.is_blocked("ka", "ma"));
    assert_eq!(h.llm.health.acquire("ka", "ma"), Permit::Allowed);
    assert_eq!(h.health("ka", "ma").unwrap().total, 1);

    // Cancel token while upstream hangs.
    let silent = Fake::always(Reply::Hang).await;
    h.add_provider("s", silent.port, &[Protocol::OpenaiChat], &["ks"], &["ms"]);
    let token = CancelToken::new();
    let target = named("s/ms");
    let (res, ()) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(
            client.complete(&target, hello(), Some(&token)),
            cancel_when_seen(&silent, 1, &token)
        )
    })
    .await
    .unwrap();
    assert!(res.unwrap_err().is_cancelled());

    let token = CancelToken::new();
    let mut s_stream = client.stream(named("s/ms"), hello(), Some(token.clone()));
    let (first, ()) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(s_stream.next(), cancel_when_seen(&silent, 2, &token))
    })
    .await
    .unwrap();
    assert!(first.unwrap().unwrap_err().is_cancelled());
    assert!(s_stream.next().await.is_none());
    // Cancelled attempts count for nothing and, as the first
    // attempts of the pair, leave no health row behind.
    assert!(h.health("ks", "ms").is_none());

    // Cancel mid-stream and pre-fired token.
    let partial = Fake::always(Reply::Sse(partial_chat_stream())).await;
    h.add_provider("p", partial.port, &[Protocol::OpenaiChat], &["kp"], &["mp"]);
    let token = CancelToken::new();
    let mut p_stream = client.stream(named("p/mp"), hello(), Some(token.clone()));
    for _ in 0..4 {
        assert!(p_stream.next().await.unwrap().is_ok());
    }
    token.cancel();
    assert!(p_stream.next().await.unwrap().unwrap_err().is_cancelled());
    assert!(p_stream.next().await.is_none());
    assert!(!h.llm.health.is_blocked("kp", "mp"));

    let fired = CancelToken::new();
    fired.cancel();
    assert!(
        client
            .complete(&named("p/mp"), hello(), Some(&fired))
            .await
            .unwrap_err()
            .is_cancelled()
    );
    assert_eq!(partial.count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_round_trip_echoes_reasoning_only_for_providers_that_need_it() {
    let tool = HubTool {
        name: "get_weather".into(),
        description: Some("weather".into()),
        parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        strict: None,
    };
    let fake = Fake::start(|number, req| {
        let canned = if number % 2 == 0 {
            Canned {
                reasoning: Some("pondering"),
                ..Canned::tool()
            }
        } else {
            Canned::text(&["It is sunny."])
        };
        if req.body["stream"] == true {
            Reply::Sse(canned_sse(Protocol::OpenaiChat, "up", &canned))
        } else {
            Reply::Json(200, canned_json(Protocol::OpenaiChat, "up", &canned))
        }
    })
    .await;
    let h = Harness::build(|_| {});
    h.add_provider("a", fake.port, &[Protocol::OpenaiChat], &["ka"], &["ma"]);
    let client = LlmClient::new(Arc::clone(&h.llm));

    for (round, (echo, streamed)) in [(true, false), (true, true), (false, false)]
        .into_iter()
        .enumerate()
    {
        h.edit_provider("a", |p| p.quirks.echo_reasoning_in_tool_turns = Some(echo));
        let mut first = HubRequest::user_prompt("x", "weather in Paris?");
        first.tools = vec![tool.clone()];
        let answer = if streamed {
            let events: Vec<HubEvent> = events_of(&client, named("a/ma"), first.clone())
                .await
                .into_iter()
                .collect::<Result<_, _>>()
                .unwrap();
            collect_events(&events).unwrap()
        } else {
            client
                .complete(&named("a/ma"), first.clone(), None)
                .await
                .unwrap()
        };
        assert_eq!(answer.finish, HubFinish::ToolCalls);
        assert_eq!(reasoning_of(&answer.content), "pondering");
        let calls = answer.tool_calls();
        assert_eq!((calls[0].0, calls[0].1), ("call_9", "get_weather"));
        assert_eq!(
            serde_json::from_str::<Value>(calls[0].2).unwrap(),
            json!({"city": "Paris"})
        );

        let mut second = first;
        second.messages.push(answer.into_message());
        second.messages.push(HubMessage {
            role: HubRole::Tool,
            content: vec![HubPart::ToolResult {
                call_id: "call_9".into(),
                name: Some("get_weather".into()),
                content: vec![HubPart::text("sunny")],
                is_error: false,
            }],
        });
        assert_eq!(
            client
                .complete(&named("a/ma"), second, None)
                .await
                .unwrap()
                .text(),
            "It is sunny."
        );
        let sent = &fake.requests()[round * 2 + 1].body;
        let assistant = &sent["messages"][1];
        assert_eq!(assistant["tool_calls"][0]["id"], "call_9");
        assert_eq!(
            sent["messages"][2],
            json!({"role": "tool", "tool_call_id": "call_9", "content": "sunny"})
        );
        assert_eq!(
            assistant.get("reasoning_content").and_then(Value::as_str),
            echo.then_some("pondering")
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn key_rejection_failover_rotation_and_circuit_opening() {
    for status in [401_u16, 402, 403] {
        let a = Fake::always(rejection(status)).await;
        let b = good_chat("from b").await;
        let (h, client) = two_providers(&a, &b, |_| {});
        h.add_alias("solo", &[("a", None, "ma")]);
        h.add_alias("dup", &[("a", Some("ka"), "m1"), ("a", Some("ka"), "m2")]);

        // The rejected key rests instead of charging the circuit; while it rests, the
        // target with a ready key goes first.
        for stream in [false, true] {
            assert_eq!(collect_text(&client, "fo", stream).await.unwrap(), "from b");
        }
        assert_eq!((a.count(), b.count()), (1, 2));
        assert_eq!(h.health("ka", "ma").unwrap().consecutive_failures, 0);
        assert_eq!(h.health("kb", "mb").unwrap().consecutive_failures, 0);

        // Without another key the resting key is still tried and the last rejection is final; a
        // key rejected for one model is not tried again for the next in the same request.
        for alias in ["solo", "dup"] {
            let err = collect_text(&client, alias, false).await.unwrap_err();
            assert!(
                err.message().contains(&status.to_string()) && err.message().contains("bad key")
            );
        }
        assert_eq!(a.count(), 1 + 1 + 1);
        assert_eq!(h.health("ka", "m1").unwrap().consecutive_failures, 0);
        assert!(h.health("ka", "m2").is_none());
    }

    // Two keys on one provider: second serves after first is rejected; pinned first key is final.
    let multi = Fake::start(|_, req| {
        if req.header("authorization") == Some("Bearer secret-k1") {
            rejection(401)
        } else {
            chat_reply("via k2", req.body["stream"] == true)
        }
    })
    .await;
    let h = Harness::build(|_| {});
    h.add_provider(
        "a",
        multi.port,
        &[Protocol::OpenaiChat],
        &["k1", "k2"],
        &["ma"],
    );
    let client = LlmClient::new(Arc::clone(&h.llm));
    for stream in [false, true] {
        assert_eq!(
            collect_text(&client, "a/ma", stream).await.unwrap(),
            "via k2"
        );
    }
    let auths: Vec<_> = multi
        .requests()
        .iter()
        .take(2)
        .map(|r| r.header("authorization").unwrap().to_owned())
        .collect();
    assert_eq!(auths, ["Bearer secret-k1", "Bearer secret-k2"]);
    assert!(
        client
            .complete(&pinned("a", Some("k1"), "ma"), hello(), None)
            .await
            .unwrap_err()
            .message()
            .contains("bad key")
    );

    // A rejected key rests: later calls go to the ready target first, the circuit stays closed.
    let bad = Fake::always(rejection(401)).await;
    let good = good_chat("from b").await;
    let (h, client) = two_providers(&bad, &good, |g| {
        g.circuit_failure_threshold = 2;
        g.circuit_open_s = 30;
    });
    for _ in 0..2 {
        assert_eq!(collect_text(&client, "fo", false).await.unwrap(), "from b");
    }
    assert_eq!(h.health("ka", "ma").unwrap().state, CircuitState::Closed);
    assert_eq!(collect_text(&client, "fo", true).await.unwrap(), "from b");
    assert_eq!(bad.count(), 1);
}

async fn redirector(location: String) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let location = location.clone();
            tokio::spawn(async move {
                let mut buf = [0_u8; 4096];
                let _ = socket.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nlocation: {location}\r\n\
                     content-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (port, task)
}

#[tokio::test(flavor = "multi_thread")]
async fn redirect_and_transport_error_hygiene() {
    let stolen = Fake::always(Reply::Json(200, json!({}))).await;
    let (port, task) = redirector(format!("http://127.0.0.1:{}/stolen", stolen.port)).await;
    let h = Harness::build(|_| {});
    h.add_provider("a", port, &[Protocol::OpenaiChat], &["ka"], &["ma"]);
    h.edit_provider("a", |p| {
        p.quirks.extra_headers.insert("x-beta".into(), "1".into());
    });
    let client = LlmClient::new(Arc::clone(&h.llm));
    for stream in [false, true] {
        let err = collect_text(&client, "a/ma", stream).await.unwrap_err();
        assert!(err.message().contains("redirect"));
    }
    assert_eq!(stolen.count(), 0);
    task.abort();

    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    h.add_provider("d", dead, &[Protocol::OpenaiChat], &["kd"], &["md"]);
    h.edit_provider("d", |p| {
        p.base_url = format!("http://127.0.0.1:{dead}/uniq-seg-5be1")
    });
    let err = client
        .complete(&named("d/md"), hello(), None)
        .await
        .unwrap_err();
    assert!(err.message().contains("network error") && !err.message().contains("uniq-seg-5be1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn failover_retry_after_model_errors_and_attempt_budget() {
    // Case A: rate-limited target with a healthy fallback does not wait out Retry-After.
    let a = Fake::always(with_header("retry-after", "30", slow_down())).await;
    let b = good_chat("from b").await;
    let (h, client) = two_providers(&a, &b, |_| {});
    for stream in [false, true] {
        let text =
            tokio::time::timeout(Duration::from_secs(10), collect_text(&client, "fo", stream))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(text, "from b");
    }
    assert_eq!(h.health("ka", "ma").unwrap().state, CircuitState::Closed);
    let rows = h.logs(2).await;
    assert_eq!(rows[0].attempts[0]["error"], "429 retry-after 30");
    assert_eq!(rows[0].attempts[1]["outcome"], "success");

    // Case B & C: sole target waits out a short retry-after once (unless max_retries=0), never a long one.
    let short = Fake::start(|n, req| {
        if n == 0 {
            with_header("retry-after-ms", "400", slow_down())
        } else {
            chat_reply("late", req.body["stream"] == true)
        }
    })
    .await;
    h.add_provider(
        "short",
        short.port,
        &[Protocol::OpenaiChat],
        &["ks"],
        &["ms"],
    );
    let started = std::time::Instant::now();
    assert_eq!(
        collect_text(&client, "short/ms", false).await.unwrap(),
        "late"
    );
    assert!(started.elapsed() >= Duration::from_millis(380));
    assert_eq!(short.count(), 2);

    h.add_provider("long", a.port, &[Protocol::OpenaiChat], &["kl"], &["ml"]);
    let err = tokio::time::timeout(
        Duration::from_secs(10),
        collect_text(&client, "long/ml", false),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(err.message().contains("429"));

    drop(client);
    drop(h);
    let h_zero = Harness::build(|g| g.max_retries = 0);
    let short_zero = Fake::always(with_header("retry-after-ms", "100", slow_down())).await;
    h_zero.add_provider(
        "z",
        short_zero.port,
        &[Protocol::OpenaiChat],
        &["kz"],
        &["mz"],
    );
    let c_zero = LlmClient::new(Arc::clone(&h_zero.llm));
    assert!(collect_text(&c_zero, "z/mz", false).await.is_err());
    assert_eq!(short_zero.count(), 1);
    drop(c_zero);
    drop(h_zero);

    // Case D & E: model_not_found, context_length_exceeded, request_too_large fail over to next model.
    for (status, body) in [
        (
            404,
            json!({"error": {"message": "The model `ma` does not exist", "code": "model_not_found"}}),
        ),
        (
            400,
            json!({"error": {"message": "maximum context length is 8 tokens", "code": "context_length_exceeded"}}),
        ),
        (
            413,
            json!({"type": "error", "error": {"type": "request_too_large", "message": "too big"}}),
        ),
    ] {
        let err_up = Fake::always(Reply::Json(status, body)).await;
        let (_h, c) = two_providers(&err_up, &b, |_| {});
        for stream in [false, true] {
            assert_eq!(collect_text(&c, "fo", stream).await.unwrap(), "from b");
        }
    }

    // Case G vs 429: 503 skips other keys of the same provider; 429 rotates keys first.
    let srv_err = Fake::always(busy()).await;
    let h_g = Harness::build(|_| {});
    h_g.add_provider(
        "a",
        srv_err.port,
        &[Protocol::OpenaiChat],
        &["k1", "k2"],
        &["ma"],
    );
    h_g.add_provider("b", b.port, &[Protocol::OpenaiChat], &["kb"], &["mb"]);
    h_g.add_alias("fo", &[("a", None, "ma"), ("b", None, "mb")]);
    let c_g = LlmClient::new(Arc::clone(&h_g.llm));
    assert_eq!(collect_text(&c_g, "fo", false).await.unwrap(), "from b");
    assert_eq!(srv_err.count(), 1, "k2 must not be tried on 503");

    let rl_rot = Fake::start(|_, req| {
        if req.header("authorization") == Some("Bearer secret-kr1") {
            slow_down()
        } else {
            chat_reply("via k2", false)
        }
    })
    .await;
    h_g.add_provider(
        "r",
        rl_rot.port,
        &[Protocol::OpenaiChat],
        &["kr1", "kr2"],
        &["mr"],
    );
    assert_eq!(collect_text(&c_g, "r/mr", false).await.unwrap(), "via k2");
    assert_eq!(rl_rot.count(), 2);
    drop(c_g);
    drop(h_g);

    // Attempt budget = 1 + max_retries, and open-circuit sentence.
    let dead: Vec<Fake> =
        futures_util::future::join_all((0..5).map(|_| Fake::always(busy()))).await;
    let h_bud = Harness::build(|g| {
        g.max_retries = 2;
        g.circuit_failure_threshold = 1;
        g.circuit_open_s = 30;
    });
    for (i, f) in dead.iter().enumerate() {
        h_bud.add_provider(
            &format!("p{i}"),
            f.port,
            &[Protocol::OpenaiChat],
            &[&format!("k{i}")],
            &["m"],
        );
    }
    h_bud.add_alias(
        "many",
        &[
            ("p0", None, "m"),
            ("p1", None, "m"),
            ("p2", None, "m"),
            ("p3", None, "m"),
            ("p4", None, "m"),
        ],
    );
    h_bud.add_alias("claude-code", &[("p4", None, "m")]);
    let c_bud = LlmClient::new(Arc::clone(&h_bud.llm));
    let err = collect_text(&c_bud, "many", false).await.unwrap_err();
    assert!(
        err.message()
            .starts_with("every upstream attempt failed; last: ")
    );
    assert_eq!(
        dead.iter().map(Fake::count).collect::<Vec<_>>(),
        [1, 1, 1, 0, 0]
    );
    assert!(collect_text(&c_bud, "claude-code", false).await.is_err());
    let open_err = collect_text(&c_bud, "claude-code", false)
        .await
        .unwrap_err();
    assert_eq!(
        open_err.message(),
        "No target available for 'claude-code'. The earliest retry is in 30 s."
    );
}

async fn typed(client: &LlmClient, target: ChatTarget) -> (Vec<HubEvent>, ChatError) {
    let mut events = Vec::new();
    let mut stream = client.stream_typed(target, hello(), None);
    while let Some(item) = stream.next().await {
        match item {
            Ok(ev) => events.push(ev),
            Err(err) => {
                assert!(stream.next().await.is_none());
                return (events, err);
            }
        }
    }
    panic!("stream ended without error: {events:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn capability_gap_typed_streams_and_agent_logs() {
    let a = good_chat("from a").await;
    let b = good_chat("from b").await;
    let (h, client) = two_providers(&a, &b, |g| {
        g.first_byte_timeout_s = 1;
        g.idle_timeout_s = 1;
    });

    // Capability gap skips target and names capability when none left.
    let set_no_vision = |prov: &str, m: &str| {
        let mut mat = ModelCapabilities::default();
        mat.apply(Capability::Vision, false, CapabilitySource::UserOverride);
        crate::llm::store::set_model_capabilities_override(&h.db, prov, m, &mat).unwrap();
        h.llm.touch();
    };
    set_no_vision("a", "ma");
    let mut img = hello();
    img.messages[0].content.push(HubPart::Image {
        image: HubImage::Url {
            url: "https://example.test/x.png".into(),
        },
    });
    assert_eq!(
        client
            .complete(&named("fo"), img.clone(), None)
            .await
            .unwrap()
            .text(),
        "from b"
    );
    set_no_vision("b", "mb");
    assert!(
        client
            .complete(&named("fo"), img, None)
            .await
            .unwrap_err()
            .message()
            .contains("vision")
    );

    // Typed stream errors: MidStream, Refusal, KeyRejected, Failed, IdleStall, Cancelled.
    let mut cut_steps =
        canned_sse(Protocol::OpenaiChat, "up", &Canned::text(&["Hel", "lo"]))[..2].to_vec();
    cut_steps.push(Step::Die);
    let cut = Fake::always(Reply::Sse(cut_steps)).await;
    h.add_provider("cut", cut.port, &[Protocol::OpenaiChat], &["kc"], &["mc"]);
    h.add_alias("cut-fo", &[("cut", None, "mc"), ("b", None, "mb")]);
    let (ev, err) = typed(
        &client,
        ChatTarget::Named {
            name: "cut-fo".into(),
            fallbacks: vec!["b/mb".into()],
        },
    )
    .await;
    assert_eq!(err.kind, ChatErrorKind::MidStream);
    assert!(ev.iter().any(|e| matches!(e, HubEvent::TextDelta { .. })));
    assert_eq!(cut.count(), 1, "no failover after content");

    let empty = Fake::always(Reply::Sse(Vec::new())).await;
    h.add_provider("emp", empty.port, &[Protocol::OpenaiChat], &["ke"], &["me"]);
    let (_, err) = typed(&client, named("emp/me")).await;
    assert_eq!(
        (err.kind, err.failure),
        (ChatErrorKind::Refusal, Some(FailureKind::EmptyOutput))
    );

    let rej = Fake::always(rejection(401)).await;
    h.add_provider("rej", rej.port, &[Protocol::OpenaiChat], &["kr"], &["mr"]);
    let (_, err) = typed(&client, named("rej/mr")).await;
    assert_eq!(
        (err.kind, err.failure),
        (ChatErrorKind::KeyRejected, Some(FailureKind::Auth))
    );

    let bad = Fake::always(Reply::Json(400, json!({"error": {"message": "bad"}}))).await;
    h.add_provider("bad", bad.port, &[Protocol::OpenaiChat], &["kb2"], &["mb2"]);
    assert_eq!(
        typed(&client, named("bad/mb2")).await.1.kind,
        ChatErrorKind::Failed
    );

    let stall = Fake::always(Reply::Sse(partial_chat_stream())).await;
    h.add_provider(
        "stl",
        stall.port,
        &[Protocol::OpenaiChat],
        &["kst"],
        &["mst"],
    );
    let (_, err) = tokio::time::timeout(Duration::from_secs(30), typed(&client, named("stl/mst")))
        .await
        .unwrap();
    assert_eq!(
        (err.kind, err.failure),
        (ChatErrorKind::IdleStall, Some(FailureKind::Timeout))
    );

    let tok = CancelToken::new();
    tok.cancel();
    let c_err = client
        .stream_typed(named("stl/mst"), hello(), Some(tok))
        .next()
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(c_err.kind, ChatErrorKind::Cancelled);
    assert!(AppError::from(c_err).is_cancelled());

    drop(client);
    drop(h);
    // Agent log rows & extra provider headers.
    let h2 = Harness::build(|_| {});
    let f_busy = Fake::always(busy()).await;
    let f_ok = good_chat("from b").await;
    h2.add_provider("a", f_busy.port, &[Protocol::OpenaiChat], &["ka"], &["ma"]);
    h2.add_provider("b", f_ok.port, &[Protocol::OpenaiChat], &["kb"], &["mb"]);
    h2.edit_provider("b", |p| {
        p.quirks
            .extra_headers
            .insert("x-org".into(), "org-7".into());
    });
    h2.add_alias("fo", &[("a", None, "ma"), ("b", None, "mb")]);
    let sess = LlmClient::new(Arc::clone(&h2.llm)).for_session("sess-1");
    assert_eq!(
        sess.complete(&named("fo"), hello(), None)
            .await
            .unwrap()
            .text(),
        "from b"
    );
    assert_eq!(f_ok.requests()[0].header("x-org"), Some("org-7"));
    let _ = events_of(&sess, named("fo"), hello()).await;
    let rows = h2.logs(2).await;
    for (row, is_stream) in rows.iter().zip([false, true]) {
        assert_eq!(
            (row.source.as_str(), row.source_ref.as_deref()),
            ("agent", Some("sess-1"))
        );
        assert_eq!(
            (row.outcome.as_str(), row.status, row.stream),
            ("success", 200, is_stream)
        );
        assert_eq!(row.attempts.as_array().unwrap().len(), 2);
        assert!(row.timing.as_ref().unwrap().check_total(row.latency_ms));
        assert_eq!(row.ttft_ms.is_some(), is_stream);
    }
    h2.add_provider("p", stall.port, &[Protocol::OpenaiChat], &["kp"], &["mp"]);
    let bare = LlmClient::new(Arc::clone(&h2.llm));
    let mut drop_s = bare.stream(named("p/mp"), hello(), None);
    for _ in 0..3 {
        drop_s.next().await.unwrap().unwrap();
    }
    drop(drop_s);
    assert!(bare.complete(&named("a/ma"), hello(), None).await.is_err());
    let rows = h2.logs(4).await;
    assert_eq!(
        (rows[2].outcome.as_str(), rows[2].source_ref.as_deref()),
        ("canceled", None)
    );
    assert_eq!((rows[3].outcome.as_str(), rows[3].status), ("failure", 503));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_body_that_echoes_the_key_is_masked_everywhere_it_lands() {
    // The key is `secret-ka`; the body echoes it whole and as a fragment.
    let echo = Fake::always(Reply::Json(
        401,
        json!({"error": {"message": "Incorrect API key provided: secret-k***. Key secret-ka is revoked."}}),
    ))
    .await;
    let h = Harness::build(|_| {});
    h.add_provider("a", echo.port, &[Protocol::OpenaiChat], &["ka"], &["ma"]);
    let client = LlmClient::new(Arc::clone(&h.llm));
    let error = client
        .complete(&named("a/ma"), hello(), None)
        .await
        .unwrap_err();
    let row = serde_json::to_string(&h.logs(1).await).unwrap();
    for text in [error.message(), row.as_str()] {
        assert!(!text.contains("secret-k"), "{text}");
    }
    assert!(error.message().contains("Incorrect API key provided"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_old_thinking_block_is_dropped_and_retried_once() {
    // The upstream refuses every replayed thinking block, as Anthropic does once an old
    // block's signature no longer verifies (here after the system prompt changed).
    const BLOCK: &str = r#""type":"thinking""#;
    let fake = Fake::start(|_, req| {
        if req.body.to_string().contains(BLOCK) {
            return Reply::Json(
                400,
                json!({"type": "error", "error": {"type": "invalid_request_error",
                    "message": "messages.1.content.0: Invalid `signature` in `thinking` block"}}),
            );
        }
        let canned = Canned::text(&["resumed"]);
        if req.body["stream"] == true {
            Reply::Sse(canned_sse(Protocol::AnthropicMessages, "up", &canned))
        } else {
            Reply::Json(200, canned_json(Protocol::AnthropicMessages, "up", &canned))
        }
    })
    .await;
    let h = Harness::build(|_| {});
    h.add_provider(
        "a",
        fake.port,
        &[Protocol::AnthropicMessages],
        &["ka"],
        &["ma"],
    );
    let client = LlmClient::new(Arc::clone(&h.llm));
    let thought = |text: &str| HubPart::Reasoning {
        text: text.into(),
        signature: Some("sig".into()),
        encrypted: None,
        id: None,
        origin: Some(Protocol::AnthropicMessages),
    };

    let mut resumed = HubRequest::user_prompt("x", "first question");
    resumed.system = Some("a changed system prompt".into());
    resumed.messages.push(HubMessage {
        role: HubRole::Assistant,
        content: vec![thought("yesterday"), HubPart::text("first answer")],
    });
    resumed.messages.push(HubMessage::user("and now?"));
    let answer = client.complete(&named("a/ma"), resumed.clone(), None).await;
    assert_eq!(answer.unwrap().text(), "resumed");
    let events: Vec<HubEvent> = events_of(&client, named("a/ma"), resumed)
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(collect_events(&events).unwrap().text(), "resumed");
    let sent = fake.requests();
    assert_eq!(sent.len(), 4);
    for retry in [&sent[1], &sent[3]] {
        assert!(!retry.body.to_string().contains(BLOCK));
        assert_eq!(
            retry.body["messages"][1]["content"][0]["text"],
            "first answer"
        );
    }

    // Mid-turn thinking-signature refusals (e.g. failover across two Anthropic providers/keys
    // during a tool loop) also recover by stripping the refused thinking blocks and disabling
    // thinking on the one recovery retry.
    let mut tool_turn = HubRequest::user_prompt("x", "weather?");
    tool_turn.messages.push(HubMessage {
        role: HubRole::Assistant,
        content: vec![
            thought("look it up"),
            HubPart::ToolCall {
                id: "t1".into(),
                name: "get_weather".into(),
                arguments: "{}".into(),
                signature: None,
                origin: None,
            },
        ],
    });
    tool_turn.messages.push(HubMessage {
        role: HubRole::Tool,
        content: vec![HubPart::ToolResult {
            call_id: "t1".into(),
            name: Some("get_weather".into()),
            content: vec![HubPart::text("sunny")],
            is_error: false,
        }],
    });
    let recovered = client
        .complete(&named("a/ma"), tool_turn, None)
        .await
        .unwrap();
    assert_eq!(recovered.text(), "resumed");
    let sent = fake.requests();
    assert_eq!(sent.len(), 6);
    assert!(!sent[5].body.to_string().contains(BLOCK));
    assert!(sent[5].body.get("thinking").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_keyless_provider_is_called_without_credentials() {
    // An `auth: none` provider (Ollama, LM Studio) with no key row at all.
    let fake = good_chat("local").await;
    let h = Harness::build(|_| {});
    h.add_provider("ol", fake.port, &[Protocol::OpenaiChat], &[], &["llama3"]);
    h.edit_provider("ol", |p| p.auth = crate::llm::wire::AuthStyle::None);
    let client = LlmClient::new(Arc::clone(&h.llm));
    assert_eq!(
        collect_text(&client, "ol/llama3", false).await.unwrap(),
        "local"
    );
    assert_eq!(
        collect_text(&client, "ol/llama3", true).await.unwrap(),
        "local"
    );
    for request in fake.requests() {
        assert!(!request.headers.contains_key("authorization"));
        assert!(!request.headers.contains_key("x-api-key"));
    }
    assert_eq!(fake.count(), 2);
}

// With a reshaper, a fallback known not to read images is still tried, after every
// target that reads them, and gets the request without the images; each attempt is reshaped for
// its own target.
#[tokio::test(flavor = "multi_thread")]
async fn a_reshaped_stream_drops_images_for_a_fallback_that_cannot_read_them() {
    let a = Fake::always(busy()).await;
    let b = good_chat("from b").await;
    let (h, client) = two_providers(&a, &b, |_| {});
    let no_vision = |provider: &str, model: &str| {
        let mut capabilities = ModelCapabilities::default();
        capabilities.apply(Capability::Vision, false, CapabilitySource::UserOverride);
        crate::llm::store::set_model_capabilities_override(&h.db, provider, model, &capabilities)
            .unwrap();
        h.llm.touch();
    };
    let mut request = hello();
    request.messages[0].content.push(HubPart::Image {
        image: HubImage::Url {
            url: "https://example.test/x.png".into(),
        },
    });
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let reshape: crate::llm::client::Reshape = {
        let seen = Arc::clone(&seen);
        Arc::new(move |facts, _| seen.lock().unwrap().push(facts.model.clone()))
    };
    let text_of_stream = |request: HubRequest| {
        let stream = client.stream_reshaped(named("fo"), request, Arc::clone(&reshape), None);
        async move {
            let events: Vec<HubEvent> = stream.map(Result::unwrap).collect().await;
            collect_events(&events).unwrap().text()
        }
    };

    // The primary is busy; the fallback cannot read images and answers without them.
    no_vision("b", "mb");
    assert_eq!(text_of_stream(request.clone()).await, "from b");
    let mut models = seen.lock().unwrap().clone();
    models.dedup();
    assert_eq!(models, ["ma", "mb"], "an in-place retry is reshaped too");
    let to_a = a.requests()[0].body.to_string();
    let to_b = b.requests()[0].body.to_string();
    assert!(to_a.contains("x.png"), "{to_a}");
    assert!(
        !to_b.contains("x.png") && to_b.contains("[Image omitted: mb"),
        "{to_b}"
    );

    // A target that cannot read images goes after one that can, whatever the chain's order.
    crate::llm::store::set_model_capabilities_override(
        &h.db,
        "b",
        "mb",
        &ModelCapabilities::default(),
    )
    .unwrap();
    no_vision("a", "ma");
    seen.lock().unwrap().clear();
    let calls_to_a = a.count();
    assert_eq!(text_of_stream(request).await, "from b");
    assert_eq!(*seen.lock().unwrap(), ["mb"]);
    assert_eq!(
        a.count(),
        calls_to_a,
        "the target without vision waits behind b"
    );
    assert!(b.requests()[1].body.to_string().contains("x.png"));
}

// A stream the caller stops after it served is billed for what it received; the
// output nobody reported is estimated from the text and the cost is marked as an estimate.
#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_stream_is_billed_with_estimated_output() {
    let h = Harness::build(|_| {});
    let stall = Fake::always(Reply::Sse(partial_chat_stream())).await;
    h.add_provider("p", stall.port, &[Protocol::OpenaiChat], &["kp"], &["mp"]);
    crate::llm::store::save_price_override(
        &h.db,
        crate::llm::usage::PriceOverride {
            provider_id: "p".into(),
            model_id: "mp".into(),
            input_per_mtok: 1.0,
            output_per_mtok: 1_000.0,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            reasoning_per_mtok: None,
            currency: "USD".into(),
            updated_at_ms: 0,
        },
        1,
    )
    .unwrap();
    h.llm.touch();
    let client = LlmClient::new(Arc::clone(&h.llm));
    let mut stream = client.stream(named("p/mp"), hello(), None);
    let mut text = String::new();
    while text.len() < 3 {
        if let HubEvent::TextDelta { text: delta } = stream.next().await.unwrap().unwrap() {
            text.push_str(&delta);
        }
    }
    drop(stream);
    let row = h.logs(1).await.remove(0);
    assert_eq!(row.outcome, "canceled");
    assert_eq!(row.output_tokens, Some(1), "`Hel` is one estimated token");
    assert!(row.tokens_estimated);
    assert_eq!(row.price_source.as_deref(), Some("override"));
    assert!(
        row.cost_micro_usd.unwrap_or(0) > 0,
        "the stopped call has a cost"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_that_refuses_cache_markers_gets_one_retry_and_is_remembered() {
    // A relay rejects `cache_control`; the call retries once without it and later calls
    // to the provider never send the hint again this session.
    let fake = Fake::start(|_, req| {
        if req.body.to_string().contains("cache_control") {
            return Reply::Json(
                400,
                json!({"type": "error", "error": {"type": "invalid_request_error",
                    "message": "system.0.cache_control: Extra inputs are not permitted"}}),
            );
        }
        let canned = Canned::text(&["ok"]);
        if req.body["stream"] == true {
            Reply::Sse(canned_sse(Protocol::AnthropicMessages, "up", &canned))
        } else {
            Reply::Json(200, canned_json(Protocol::AnthropicMessages, "up", &canned))
        }
    })
    .await;
    let h = Harness::build(|_| {});
    h.add_provider(
        "a",
        fake.port,
        &[Protocol::AnthropicMessages],
        &["ka"],
        &["ma"],
    );
    let client = LlmClient::new(Arc::clone(&h.llm));
    let mut cached = HubRequest::user_prompt("x", "hi");
    cached.system = Some("sys".into());
    cached.cache = Some(crate::llm::hub::CacheHint {
        breakpoints: vec![crate::llm::hub::CacheBreakpoint::SystemAndTools],
        key: None,
    });
    let answer = client.complete(&named("a/ma"), cached.clone(), None).await;
    assert_eq!(answer.unwrap().text(), "ok");
    let events: Vec<HubEvent> = events_of(&client, named("a/ma"), cached)
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(collect_events(&events).unwrap().text(), "ok");
    let sent = fake.requests();
    // Refused, retried bare; then the stream goes bare at once.
    assert_eq!(sent.len(), 3);
    assert!(sent[0].body.to_string().contains("cache_control"));
    assert!(!sent[1].body.to_string().contains("cache_control"));
    assert!(!sent[2].body.to_string().contains("cache_control"));
}

// Every agent call's row carries the run key, how the answer ended and an `rq_` id; a
// call that never reached a target still writes one.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_call_row_carries_its_run_key_even_when_planning_fails() {
    use crate::llm::client::{CallMeta, CallPurpose};
    let h = Harness::build(|_| {});
    let upstream = good_chat("hello").await;
    h.add_provider(
        "p",
        upstream.port,
        &[Protocol::OpenaiChat],
        &["kp"],
        &["mp"],
    );
    let base = LlmClient::new(Arc::clone(&h.llm)).for_session("as_1");
    let meta = |message_id: Option<&str>, purpose| CallMeta {
        run_id: "run_1".into(),
        message_id: message_id.map(str::to_owned),
        purpose,
    };
    let step = base.for_call(&meta(Some("am_1"), CallPurpose::Step));
    let events: Vec<_> = events_of(&step, named("p/mp"), hello()).await;
    assert!(events.iter().all(Result::is_ok));
    let summary = base.for_call(&meta(None, CallPurpose::Compaction));
    summary
        .complete(&named("p/mp"), hello(), None)
        .await
        .unwrap();
    let failed: Vec<_> = events_of(&step, named("nowhere/x"), hello()).await;
    assert!(failed.iter().any(Result::is_err));

    let rows = h.logs(3).await;
    assert!(rows.iter().all(|row| {
        row.run_id.as_deref() == Some("run_1")
            && row.session_id.as_deref() == Some("as_1")
            && row
                .request_id
                .as_deref()
                .is_some_and(|id| id.starts_with("rq_"))
    }));
    let of = |purpose: &str, outcome: &str| {
        rows.iter()
            .find(|row| row.purpose.as_deref() == Some(purpose) && row.outcome == outcome)
            .unwrap_or_else(|| panic!("no {purpose} {outcome} row"))
    };
    let streamed = of("step", "success");
    assert_eq!(streamed.message_id.as_deref(), Some("am_1"));
    assert_eq!(streamed.finish_reason.as_deref(), Some("stop"));
    let compaction = of("compaction", "success");
    assert_eq!(compaction.message_id, None);
    assert_eq!(compaction.finish_reason.as_deref(), Some("stop"));
    let unplanned = of("step", "failure");
    assert_eq!(unplanned.requested_model, "nowhere/x");
    assert_eq!(unplanned.provider_id, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_canceled_and_circuit_blocked_calls_keep_their_provider_and_key() {
    let down = Fake::always(Reply::Json(500, json!({"error": {"message": "boom"}}))).await;
    let hang = Fake::always(Reply::Hang).await;
    let h = Harness::build(|g| {
        g.max_retries = 0;
        g.circuit_failure_threshold = 1;
        g.circuit_open_s = 3600;
    });
    h.add_provider("p", down.port, &[Protocol::OpenaiChat], &["kp"], &["mp"]);
    h.add_provider("q", down.port, &[Protocol::OpenaiChat], &["kq"], &["mq"]);
    h.add_provider("h", hang.port, &[Protocol::OpenaiChat], &["kh"], &["mh"]);
    let client = LlmClient::new(Arc::clone(&h.llm));

    // 1. Upstream 500 before serving (non-streaming) -> attributed to p/kp, and trips p's circuit.
    assert!(
        client
            .complete(&named("p/mp"), hello(), None)
            .await
            .is_err()
    );

    // 2. Single-provider circuit open during planning -> still attributed to p/kp.
    assert!(
        client
            .complete(&named("p/mp"), hello(), None)
            .await
            .is_err()
    );

    // 3. Upstream 500 before serving (streaming) -> attributed to q/kq, and trips q's circuit.
    let streamed = events_of(&client, named("q/mq"), hello()).await;
    assert!(streamed.iter().any(Result::is_err));

    // 4. Multi-provider fallback where every provider's circuit is open -> ambiguous, stays unrouted.
    let multi = ChatTarget::Named {
        name: "p/mp".into(),
        fallbacks: vec!["q/mq".into()],
    };
    assert!(client.complete(&multi, hello(), None).await.is_err());

    // 5. Cancelled via token before upstream sends headers -> attributed to h/kh.
    let token = CancelToken::new();
    let trigger = token.clone();
    let cancel_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        trigger.cancel();
    });
    assert!(
        client
            .complete(&named("h/mh"), hello(), Some(&token))
            .await
            .is_err()
    );
    cancel_task.await.unwrap();

    // 6. Dropped stream future before upstream sends headers -> attributed to h/kh.
    let mut stream = client.stream(named("h/mh"), hello(), None);
    let _ = tokio::time::timeout(Duration::from_millis(40), stream.next()).await;
    drop(stream);

    let rows = h.logs(6).await;
    // Row 0: p/mp 500 failure.
    assert_eq!(
        (
            rows[0].outcome.as_str(),
            rows[0].provider_id.as_deref(),
            rows[0].key_id.as_deref(),
            rows[0].upstream_model.as_deref(),
            rows[0].protocol_out.as_deref(),
        ),
        (
            "failure",
            Some("p"),
            Some("kp"),
            Some("mp"),
            Some("openai_chat"),
        )
    );
    // Row 1: p/mp blocked by open circuit (single provider).
    assert_eq!(
        (
            rows[1].outcome.as_str(),
            rows[1].provider_id.as_deref(),
            rows[1].key_id.as_deref(),
            rows[1].upstream_model.as_deref(),
            rows[1].attempts[0]["error"].as_str(),
        ),
        (
            "failure",
            Some("p"),
            Some("kp"),
            Some("mp"),
            Some("circuit open"),
        )
    );
    // Row 2: q/mq streaming 500 before serving.
    assert_eq!(
        (
            rows[2].outcome.as_str(),
            rows[2].provider_id.as_deref(),
            rows[2].key_id.as_deref(),
            rows[2].upstream_model.as_deref(),
        ),
        ("failure", Some("q"), Some("kq"), Some("mq"))
    );
    // Row 3: p/mp -> q/mq both blocked -> multi-provider skipped remains unrouted.
    assert_eq!(
        (
            rows[3].outcome.as_str(),
            rows[3].provider_id.as_deref(),
            rows[3].key_id.as_deref(),
        ),
        ("failure", None, None)
    );
    // Row 4: h/mh canceled via token before headers.
    assert_eq!(
        (
            rows[4].outcome.as_str(),
            rows[4].status,
            rows[4].provider_id.as_deref(),
            rows[4].key_id.as_deref(),
            rows[4].upstream_model.as_deref(),
            rows[4].attempts[0]["outcome"].as_str(),
        ),
        (
            "canceled",
            499,
            Some("h"),
            Some("kh"),
            Some("mh"),
            Some("canceled"),
        )
    );
    // Row 5: h/mh dropped stream before headers.
    assert_eq!(
        (
            rows[5].outcome.as_str(),
            rows[5].status,
            rows[5].provider_id.as_deref(),
            rows[5].key_id.as_deref(),
            rows[5].upstream_model.as_deref(),
            rows[5].attempts[0]["outcome"].as_str(),
        ),
        (
            "canceled",
            499,
            Some("h"),
            Some("kh"),
            Some("mh"),
            Some("canceled"),
        )
    );
}
