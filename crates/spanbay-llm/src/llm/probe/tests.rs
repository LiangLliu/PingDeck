//! Tests of the single-check probes (`run_test`) against a fake upstream.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::net::TcpListener;

use super::checks::cache_system_prompt;
use super::context::filler;
use super::http::{MAX_LIST_PAGES, model_entries, parse_model_list};
use super::test_support::*;
use super::*;
use crate::llm::failure::FailureKind;
use crate::llm::wire::Protocol;

const ANTHROPIC: &[Protocol] = &[Protocol::AnthropicMessages];
const SLOW_DOWN: &str = r#"{"error":{"message":"slow down"}}"#;
const KEEPALIVE: &str =
    r#"{"id":"c1","model":"m","choices":[{"index":0,"delta":{"role":"assistant"}}]}"#;
const TEXT: &str = r#"{"choices":[{"index":0,"delta":{"content":"x"}}]}"#;

fn anthropic_text(text: &str) -> Reply {
    anthropic_message(&json!([{"type": "text", "text": text}]), "end_turn")
}

async fn run_logged(fake: &Fake, test: ProbeTest, timeouts: &CheckTimeouts) -> CheckOutput {
    run_timed(
        &runtime(),
        &target(fake.addr, OPENAI),
        test,
        &opts(),
        timeouts,
        None,
    )
    .await
}

fn fast(chat: u64) -> CheckTimeouts {
    CheckTimeouts {
        chat: Duration::from_millis(chat),
        stream_first: Duration::from_millis(chat),
        stream_idle: Duration::from_millis(chat),
        stream_ceiling: Duration::from_millis(chat * 2),
        ..CheckTimeouts::default()
    }
}

#[test]
fn model_list_shapes_prompts_and_bundled_pdf() {
    let ids = |value: Value| parse_model_list(&value);
    assert_eq!(
        ids(json!({"object": "list", "data": [{"id": "a"}, {"id": "b"}]})),
        ["a", "b"]
    );
    assert_eq!(
        ids(json!({"models": [{"name": "models/gemini-2"}, {"model": "qwen"}]})),
        ["gemini-2", "qwen"]
    );
    assert_eq!(ids(json!(["x", "y"])), ["x", "y"]);
    assert_eq!(model_entries(&json!({"error": "no"})), None);

    let words = cache_system_prompt().split_whitespace().count();
    assert!((1300..1600).contains(&words));
    let text = filler(4000);
    assert_eq!(text, filler(4000));
    assert!(text.len() >= 4000 && text.len() < 4020);

    let unknown = ContextProbeInfo::for_window(None);
    assert_eq!(
        (unknown.start_tokens, unknown.total_tokens),
        (8_000, 168_000)
    );
    assert_eq!(unknown.warning, CONTEXT_PROBE_WARNING);
    assert_eq!(ContextProbeInfo::for_window(Some(4_000)).total_tokens, 0);

    let pdf = native::pdf_bytes();
    assert!(pdf.starts_with(b"%PDF-1.4\n") && pdf.ends_with(b"%%EOF\n"));
    let pdf_str = String::from_utf8(pdf).unwrap();
    assert!(pdf_str.contains(&format!("({}) Tj", native::PDF_WORD)));
}

#[tokio::test]
async fn connectivity_auth_failures_redirects_and_url_redaction() {
    let fake = spawn_fake(openai_fake).await;
    let conn = run(&fake, OPENAI, ProbeTest::Connectivity).await;
    assert!(conn.ok && conn.status == Some(404) && conn.endpoint == "base");
    assert!(!fake.requests()[0].headers.contains("authorization"));

    let auth = run(&fake, OPENAI, ProbeTest::Auth).await;
    assert!(auth.ok && auth.status == Some(200) && auth.endpoint == "models");

    // Auth without a list endpoint falls back to a 1-token chat; 400 still proves key acceptance.
    let no_list = ProbeOptions {
        models_endpoint: None,
        ..opts()
    };
    assert!(run_with(&fake, OPENAI, ProbeTest::Auth, &no_list).await.ok);
    let bad_query = spawn_fake(|_| Reply::json(400, &json!({"error": {"message": "bad"}}))).await;
    assert!(run(&bad_query, OPENAI, ProbeTest::Auth).await.ok);

    // Error status classification.
    for (status, body, kind) in [
        (
            401,
            r#"{"error":{"message":"Incorrect API key"}}"#,
            FailureKind::Auth,
        ),
        (
            402,
            r#"{"error":{"message":"payment required"}}"#,
            FailureKind::Quota,
        ),
        (
            403,
            r#"{"error":{"message":"not available in your region"}}"#,
            FailureKind::Region,
        ),
        (429, SLOW_DOWN, FailureKind::RateLimit),
        (500, "boom", FailureKind::ProviderError),
    ] {
        let srv = spawn_fake(move |_| Reply::Json(status, body.to_owned())).await;
        let res = run(&srv, OPENAI, ProbeTest::Auth).await;
        assert!(!res.ok && res.failure_kind == Some(kind));
    }

    // Redirect is not followed and never replays credentials.
    let stolen = spawn_fake(|_| Reply::json(200, &json!({"data": []}))).await;
    let loc = format!("http://{}/stolen", stolen.addr);
    let redirector = spawn_fake(move |_| Reply::Redirect(loc.clone())).await;
    let redir = run(&redirector, OPENAI, ProbeTest::Auth).await;
    assert!(!redir.ok && redir.message.contains("redirect"));
    assert!(stolen.requests().is_empty());

    // Refused socket & URL redaction on transport error.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut closed = target(SocketAddr::from(([127, 0, 0, 1], addr.port())), OPENAI);
    closed.base_url = format!("http://127.0.0.1:{}/uniq-seg-91d3", addr.port());
    let down = run_test(&runtime(), &closed, ProbeTest::Auth, &opts(), None).await;
    assert_eq!(down.failure_kind, Some(FailureKind::Network));
    assert!(!down.message.contains("uniq-seg-91d3"));
}

#[tokio::test]
async fn models_check_and_paginated_fetching() {
    let fake = spawn_fake(openai_fake).await;
    let res = run(&fake, OPENAI, ProbeTest::Models).await;
    assert!(res.ok && res.detail["listed"] == true && res.detail["count"] == 2);

    let no_list = ProbeOptions {
        models_endpoint: None,
        ..opts()
    };
    assert_eq!(
        run_with(&fake, OPENAI, ProbeTest::Models, &no_list)
            .await
            .failure_kind,
        Some(FailureKind::Unknown)
    );
    let garbage = spawn_fake(|_| Reply::json(200, &json!({"hello": "world"}))).await;
    assert_eq!(
        run(&garbage, OPENAI, ProbeTest::Models).await.failure_kind,
        Some(FailureKind::ProtocolMismatch)
    );

    // Anthropic after_id pagination & bounded loop cap.
    let paged = spawn_fake(|req| {
        if req.path.contains("after_id=a") {
            Reply::json(200, &json!({"data": [{"id": "b"}], "has_more": false}))
        } else {
            Reply::json(
                200,
                &json!({"data": [{"id": "a"}], "has_more": true, "last_id": "a"}),
            )
        }
    })
    .await;
    let ids = fetch_model_ids(
        &runtime(),
        &target(paged.addr, ANTHROPIC),
        "/v1/models",
        None,
    )
    .await
    .unwrap();
    assert_eq!(ids, ["a", "b"]);

    let loop_fake = spawn_fake(|_| {
        Reply::json(
            200,
            &json!({"models": [{"name": "models/x"}], "nextPageToken": "again"}),
        )
    })
    .await;
    let capped = fetch_model_ids(
        &runtime(),
        &target(loop_fake.addr, &[Protocol::Gemini]),
        "/models",
        None,
    )
    .await
    .unwrap();
    assert_eq!(capped.len(), MAX_LIST_PAGES);
}

#[tokio::test]
async fn chat_protocol_selection_and_stream_metrics() {
    let fake = spawn_fake(openai_fake).await;
    let chat = run(&fake, OPENAI, ProbeTest::Chat).await;
    assert!(chat.ok && chat.detail["preview"] == "pong");
    assert_eq!((chat.input_tokens, chat.output_tokens), (Some(10), Some(4)));

    let empty = spawn_fake(|_| oa_text("")).await;
    assert_eq!(
        run(&empty, OPENAI, ProbeTest::Chat).await.failure_kind,
        Some(FailureKind::EmptyOutput)
    );
    assert_eq!(
        run(&fake, &[Protocol::Embeddings], ProbeTest::Chat)
            .await
            .failure_kind,
        Some(FailureKind::ProtocolMismatch)
    );

    // Forced protocol selection.
    let dual = spawn_fake(|req| {
        if req.path == "/v1/messages" {
            anthropic_text("from anthropic")
        } else {
            oa_text("from openai")
        }
    })
    .await;
    let both = [Protocol::OpenaiChat, Protocol::AnthropicMessages];
    let forced = ProbeOptions {
        protocol: Some(Protocol::AnthropicMessages),
        ..opts()
    };
    assert_eq!(
        run_with(&dual, &both, ProbeTest::Chat, &forced)
            .await
            .endpoint,
        "anthropic_messages"
    );

    // Stream under 300 tokens has no tps; >= 300 tokens computes whole tps; non-SSE fails.
    let stream = run(&fake, OPENAI, ProbeTest::Stream).await;
    assert!(stream.ok && stream.ttft_ms.is_some() && stream.tps.is_none());

    let fast_stream = spawn_fake(|_| {
        Reply::Sse(
            vec![
                r#"{"id":"c1","model":"m","choices":[{"index":0,"delta":{"content":"one"}}]}"#.to_owned(),
                r#"{"choices":[{"index":0,"delta":{"content":"two"},"finish_reason":"stop"}]}"#.to_owned(),
                r#"{"choices":[],"usage":{"prompt_tokens":40,"completion_tokens":300,"total_tokens":340}}"#.to_owned(),
                "[DONE]".to_owned(),
            ],
            40,
        )
    })
    .await;
    let with_tps = run(&fast_stream, OPENAI, ProbeTest::Stream).await;
    assert!(with_tps.tps.unwrap() > 0.0);

    let not_sse = spawn_fake(|_| oa_text("not a stream")).await;
    assert_eq!(
        run(&not_sse, OPENAI, ProbeTest::Stream).await.failure_kind,
        Some(FailureKind::ProtocolMismatch)
    );
}

#[tokio::test]
async fn tools_json_schema_vision_reasoning_and_cache_checks() {
    let fake = spawn_fake(openai_fake).await;

    let tools = run(&fake, OPENAI, ProbeTest::Tools).await;
    assert!(tools.ok && tools.detail["called"] == json!({"city": "Paris"}));
    assert_eq!(
        (tools.input_tokens, tools.output_tokens),
        (Some(30), Some(12))
    );

    // The tool call is the evidence: a failed answer after the tool result keeps it a pass.
    let broken_follow_up = spawn_fake(|request| {
        if request.body.contains("\"role\":\"tool\"") {
            Reply::json(500, &json!({"error": "upstream"}))
        } else {
            openai_fake(request)
        }
    })
    .await;
    let called = run(&broken_follow_up, OPENAI, ProbeTest::Tools).await;
    assert!(called.ok && called.detail["round_trip"] == false);

    let schema = run(&fake, OPENAI, ProbeTest::JsonSchema).await;
    assert!(schema.ok && schema.detail["parsed"]["capital"] == "Paris");

    let vision = run(&fake, OPENAI, ProbeTest::Vision).await;
    assert!(vision.ok);

    let reasoning = run(&fake, OPENAI, ProbeTest::Reasoning).await;
    assert!(reasoning.ok && reasoning.detail["reasoning_tokens"] == 30);

    // Failures when model ignores tool / schema / image / reasoning.
    let plain = spawn_fake(|_| oa_text("plain text")).await;
    assert!(!run(&plain, OPENAI, ProbeTest::Tools).await.ok);
    assert_eq!(
        run(&plain, OPENAI, ProbeTest::JsonSchema)
            .await
            .failure_kind,
        Some(FailureKind::ProtocolMismatch)
    );
    assert!(!run(&plain, OPENAI, ProbeTest::Vision).await.ok);
    assert!(!run(&plain, OPENAI, ProbeTest::Reasoning).await.ok);

    // Cache hit across two identical requests.
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let hit = spawn_fake(move |_| {
        let cached = if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            0
        } else {
            1280
        };
        oa_message(
            &json!({"role": "assistant", "content": "OK"}),
            "stop",
            &json!({"prompt_tokens": 1500, "completion_tokens": 2,
                    "prompt_tokens_details": {"cached_tokens": cached}}),
        )
    })
    .await;
    let cache = run(&hit, OPENAI, ProbeTest::Cache).await;
    assert!(cache.ok && cache.detail["second_cached"] == 1280);
}

#[tokio::test]
async fn embeddings_decision_search_and_pdf_checks() {
    let emb = spawn_fake(|req| match req.path.as_str() {
        "/v1/embeddings" => Reply::json(
            200,
            &json!({"data": [{"embedding": [0.1, 0.2, 0.3], "index": 0}], "usage": {"prompt_tokens": 5}}),
        ),
        "/v1beta/models/m:embedContent" => {
            Reply::json(200, &json!({"embedding": {"values": [0.1, 0.2, 0.3, 0.4]}}))
        }
        _ => Reply::json(404, &json!({})),
    })
    .await;
    assert_eq!(
        run(&emb, &[Protocol::Embeddings], ProbeTest::Embeddings)
            .await
            .detail["dimension"],
        3
    );
    assert_eq!(
        run(&emb, &[Protocol::Gemini], ProbeTest::Embeddings)
            .await
            .detail["dimension"],
        4
    );

    let dec = spawn_fake(|req| match req.path.as_str() {
        "/v1/systemone" => Reply::json(
            200,
            &json!({"model": "jev-1", "answers": {"paid": {"type": "noul", "noul": 0.9}}}),
        ),
        "/v1/decisions" => Reply::json(
            200,
            &json!({"answers": [{"type": "predicate", "name": "paid", "probability": 0.9}]}),
        ),
        _ => Reply::json(404, &json!({})),
    })
    .await;
    assert!(
        run(&dec, &[Protocol::DecisionSystemone], ProbeTest::Decision)
            .await
            .ok
    );
    assert!(
        run(&dec, &[Protocol::DecisionOpenai], ProbeTest::Decision)
            .await
            .ok
    );

    // Search & PDF on Anthropic.
    let search_ok = spawn_fake(|_| {
        anthropic_message(
            &json!([{"type": "text", "text": "Rust 1.99.", "citations": [
                {"type": "web_search_result_location", "url": "https://www.rust-lang.org",
                 "title": "Rust", "cited_text": "1.99", "encrypted_index": "e"}]}]),
            "end_turn",
        )
    })
    .await;
    assert!(run(&search_ok, ANTHROPIC, ProbeTest::Search).await.ok);

    let search_refused = spawn_fake(|_| {
        Reply::Json(
            400,
            r#"{"error":{"message":"unknown tool type web_search_20250305"}}"#.to_owned(),
        )
    })
    .await;
    // A 400 refused the request; it is no verdict on the feature (only 415, 501 or a protocol
    // mismatch say "unsupported").
    let res = run(&search_refused, ANTHROPIC, ProbeTest::Search).await;
    assert_eq!(
        ProbeStatus::from_result(&res),
        Some(ProbeStatus::InvalidResponse)
    );

    let pdf_ok = spawn_fake(|_| anthropic_text("Pineapple")).await;
    assert!(run(&pdf_ok, ANTHROPIC, ProbeTest::Pdf).await.ok);
    let pdf_415 = spawn_fake(|_| Reply::Json(415, "{}".to_owned())).await;
    let res = run(&pdf_415, ANTHROPIC, ProbeTest::Pdf).await;
    assert_eq!(
        ProbeStatus::from_result(&res),
        Some(ProbeStatus::Unsupported)
    );
}

#[tokio::test]
async fn context_probe_opt_in_and_ladder_stopping() {
    let fake = spawn_fake(|req| {
        if req.body.len() > 100_000 {
            Reply::json(
                400,
                &json!({"error": {"message": "context length exceeded"}}),
            )
        } else {
            oa_message(
                &json!({"role": "assistant", "content": "OK"}),
                "stop",
                &json!({"prompt_tokens": req.body.len() / 4, "completion_tokens": 1}),
            )
        }
    })
    .await;
    assert_eq!(
        run(&fake, OPENAI, ProbeTest::Context).await.failure_kind,
        Some(FailureKind::PolicyBlocked)
    );

    let opts_200k = ProbeOptions {
        allow_context_probe: true,
        declared_context: Some(200_000),
        ..opts()
    };
    let result = run_with(&fake, OPENAI, ProbeTest::Context, &opts_200k).await;
    assert!(result.ok && result.detail["max_ok_tokens"] == 8000);
    assert_eq!(result.detail["tried"].as_array().unwrap().len(), 2);

    let tiny = ProbeOptions {
        allow_context_probe: true,
        declared_context: Some(4096),
        ..opts()
    };
    assert!(!run_with(&fake, OPENAI, ProbeTest::Context, &tiny).await.ok);
}

#[tokio::test]
async fn cancellation_timeouts_request_logging_and_429_retries() {
    // Pre-cancelled token sends nothing and has no status word.
    let hang = spawn_fake(|_| Reply::Hang).await;
    let cancel = CancelToken::new();
    cancel.cancel();
    let cancelled = run_timed(
        &runtime(),
        &target(hang.addr, OPENAI),
        ProbeTest::Chat,
        &opts(),
        &CheckTimeouts::default(),
        Some(&cancel),
    )
    .await;
    assert_eq!(cancelled.result.failure_kind, Some(FailureKind::Cancelled));
    assert_eq!(ProbeStatus::from_result(&cancelled.result), None);

    // Chat and stream timeouts.
    let timed_out = run_logged(&hang, ProbeTest::Chat, &fast(200)).await;
    assert_eq!(
        ProbeStatus::from_result(&timed_out.result),
        Some(ProbeStatus::TimedOut)
    );
    let stalled_stream = spawn_fake(|_| {
        Reply::Sse(
            vec![KEEPALIVE.to_owned(), TEXT.to_owned(), "[DONE]".to_owned()],
            600,
        )
    })
    .await;
    assert_eq!(
        run_logged(&stalled_stream, ProbeTest::Stream, &fast(200))
            .await
            .result
            .failure_kind,
        Some(FailureKind::Timeout)
    );

    // Secret redaction in logged error and health book isolation.
    let leak = spawn_fake(|_| {
        Reply::Json(
            401,
            r#"{"error":{"message":"bad key sk-secret-0123456789abcdef"}}"#.to_owned(),
        )
    })
    .await;
    let mut t = target(leak.addr, OPENAI);
    t.secret = "sk-secret-0123456789abcdef".to_owned();
    let rt = runtime();
    let out = run_timed(
        &rt,
        &t,
        ProbeTest::Chat,
        &opts(),
        &CheckTimeouts::default(),
        None,
    )
    .await;
    assert!(!out.result.message.contains("sk-secret"));
    assert!(
        !out.requests[0]
            .error
            .as_deref()
            .unwrap()
            .contains("sk-secret")
    );
    assert_eq!(rt.health.failure_rate("k1", "m"), 0.0);

    // Short 429 Retry-After is waited out once; long Retry-After returns immediately.
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let short_429 = spawn_fake(move |_| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            Reply::RetryAfter(429, 1, SLOW_DOWN.to_owned())
        } else {
            oa_text("pong")
        }
    })
    .await;
    let retried = run_logged(&short_429, ProbeTest::Chat, &CheckTimeouts::default()).await;
    assert!(retried.result.ok && retried.requests.len() == 2);

    let long_429 = spawn_fake(|_| Reply::RetryAfter(429, 30, SLOW_DOWN.to_owned())).await;
    let not_waited = run_logged(&long_429, ProbeTest::Chat, &CheckTimeouts::default()).await;
    assert_eq!(not_waited.requests.len(), 1);
    assert_eq!(not_waited.result.detail["retry_after_s"], 30);
}

/// A reasoning model that spends the cap on thinking is Ready and marked truncated; the
/// vision verdict reads words, not substrings.
#[tokio::test]
async fn a_capped_answer_is_ready_and_truncated_and_vision_reads_words() {
    let capped = spawn_fake(|_| {
        oa_message(
            &json!({"role": "assistant", "content": ""}),
            "length",
            &json!({"prompt_tokens": 5, "completion_tokens": 256}),
        )
    })
    .await;
    let chat = run(&capped, OPENAI, ProbeTest::Chat).await;
    assert!(chat.ok);
    assert_eq!(chat.failure_kind, Some(FailureKind::Truncated));
    assert_eq!(chat.detail["truncated"], true);

    for (answer, red) in [
        ("Red.", true),
        ("red", true),
        ("The colour is RED", true),
        ("红色", true),
        ("bored", false),
        ("It is covered in blue", false),
        ("not red, blue", false),
        ("不是红色", false),
    ] {
        assert_eq!(super::checks::names_red(answer), red, "{answer}");
    }
    let reasoning = ProbeOptions {
        reasoning_model: true,
        ..opts()
    };
    assert!(
        super::checks::chat_request("m", &reasoning).max_tokens
            > super::checks::chat_request("m", &opts()).max_tokens
    );
}
