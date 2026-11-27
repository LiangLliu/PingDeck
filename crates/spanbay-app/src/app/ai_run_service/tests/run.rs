//! Runs of provider targets: every protocol, streaming and not, events, stop, limits, errors.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use serde_json::json;

use super::support::{Fx, body_for, pong, upstream};
use crate::llm::ai_run::{AiRunEvent, AiRunStatus};
use crate::llm::gateway::tests::{Canned, Fake, Reply, Step};
use crate::llm::wire::{AuthStyle, Protocol};

/// Runs `protocol` against a fake upstream that answers `pong`, and checks the answer.
async fn answers(protocol: Protocol, prefix: &str, auth: AuthStyle, stream: bool, path: &str) {
    let fake = upstream(protocol, pong()).await;
    let fx = Fx::new();
    let provider = fx.provider("Relay", fake.port, auth, &[(protocol, prefix, None)]);
    let body = body_for(protocol, "m", stream);
    let request = fx.provider_request("r1", &provider, protocol, "m", &body);
    // Gemini's streaming is the URL action, which the label carries.
    let request = if protocol == Protocol::Gemini && stream {
        let mut request = request;
        request.url = crate::workspace::ai_request::non_routable_label(
            &provider,
            "/models/m:streamGenerateContent",
        );
        fx.save(&request);
        request
    } else {
        request
    };
    let run = fx.run(request.id.as_str()).await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Done, "{:?}", dto.message);
    assert_eq!(dto.http_status, Some(200));
    assert_eq!(dto.reply, "pong");
    assert_eq!(dto.streamed, stream);
    assert_eq!(dto.usage.input_tokens, Some(9));
    assert_eq!(dto.usage.output_tokens, Some(2));
    assert!(!dto.usage.estimated);
    let seen = fake.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, path, "{protocol:?}");
    assert_eq!(dto.ttft_ms.is_some(), stream);
}

#[tokio::test]
async fn chat_completions_answer_without_streaming() {
    answers(
        Protocol::OpenaiChat,
        "/v1",
        AuthStyle::Bearer,
        false,
        "/v1/chat/completions",
    )
    .await;
}

#[tokio::test]
async fn chat_completions_answer_streaming() {
    answers(
        Protocol::OpenaiChat,
        "/v1",
        AuthStyle::Bearer,
        true,
        "/v1/chat/completions",
    )
    .await;
}

#[tokio::test]
async fn responses_answer_without_streaming() {
    answers(
        Protocol::OpenaiResponses,
        "/v1",
        AuthStyle::Bearer,
        false,
        "/v1/responses",
    )
    .await;
}

#[tokio::test]
async fn responses_answer_streaming() {
    answers(
        Protocol::OpenaiResponses,
        "/v1",
        AuthStyle::Bearer,
        true,
        "/v1/responses",
    )
    .await;
}

#[tokio::test]
async fn messages_answer_without_streaming() {
    answers(
        Protocol::AnthropicMessages,
        "/v1",
        AuthStyle::XApiKey,
        false,
        "/v1/messages",
    )
    .await;
}

#[tokio::test]
async fn messages_answer_streaming() {
    answers(
        Protocol::AnthropicMessages,
        "/v1",
        AuthStyle::XApiKey,
        true,
        "/v1/messages",
    )
    .await;
}

#[tokio::test]
async fn gemini_answers_without_streaming_on_the_generate_action() {
    answers(
        Protocol::Gemini,
        "/v1beta",
        AuthStyle::XGoogApiKey,
        false,
        "/v1beta/models/m:generateContent",
    )
    .await;
}

#[tokio::test]
async fn gemini_streams_on_the_stream_action_with_sse() {
    answers(
        Protocol::Gemini,
        "/v1beta",
        AuthStyle::XGoogApiKey,
        true,
        "/v1beta/models/m:streamGenerateContent?alt=sse",
    )
    .await;
}

#[tokio::test]
async fn embeddings_give_vectors_and_no_reply() {
    let fake = Fake::always(Reply::Json(
        200,
        json!({"data": [{"embedding": [0.5, 0.25, 0.125]}, {"embedding": [1.0, 2.0]}],
               "usage": {"prompt_tokens": 4, "total_tokens": 4}}),
    ))
    .await;
    let fx = Fx::new();
    let provider = fx.provider(
        "Relay",
        fake.port,
        AuthStyle::Bearer,
        &[(Protocol::Embeddings, "/v1", None)],
    );
    fx.provider_request(
        "r1",
        &provider,
        Protocol::Embeddings,
        "emb",
        &body_for(Protocol::Embeddings, "emb", false),
    );
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Done, "{:?}", dto.message);
    assert_eq!(dto.reply, "");
    assert_eq!(dto.vectors.len(), 2);
    assert_eq!(dto.vectors[0].dimensions, 3);
    assert_eq!(dto.vectors[0].first_values, [0.5, 0.25, 0.125]);
    assert_eq!(dto.usage.input_tokens, Some(4));
    assert!(dto.notice.is_none(), "vectors are an answer");
    assert_eq!(fake.requests()[0].path, "/v1/embeddings");
}

#[tokio::test]
async fn a_reasoning_answer_keeps_reasoning_and_a_tool_call_keeps_its_id() {
    let fake = upstream(Protocol::OpenaiChat, Canned::reasoning()).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", true),
    );
    let dto = fx.run("r1").await;
    let dto = dto.ok();
    assert_eq!(dto.reasoning, "let me think");
    assert_eq!(dto.reply, "Hello");

    let fake = upstream(Protocol::OpenaiChat, Canned::tool()).await;
    let provider = fx.chat_provider("Relay 2", fake.port);
    fx.provider_request(
        "r2",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", true),
    );
    let run = fx.run("r2").await;
    let dto = run.ok();
    assert_eq!(dto.tool_calls.len(), 1);
    assert_eq!(dto.tool_calls[0].id, "call_9");
    assert_eq!(dto.tool_calls[0].name, "get_weather");
    assert_eq!(dto.tool_calls[0].arguments, "{\"city\":\"Paris\"}");
    assert!(dto.ttft_ms.is_some(), "a tool call start is content");
}

#[tokio::test]
async fn events_run_started_then_frames_then_done_last() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", true),
    );
    let run = fx.run("r1").await;
    let events = run.events.all();
    assert!(
        matches!(&events[0], AiRunEvent::Started { model, .. } if model == "m"),
        "{events:?}"
    );
    let last = events.last().unwrap();
    assert!(matches!(last, AiRunEvent::Done { result } if result.reply == "pong"));
    let done = events
        .iter()
        .filter(|event| matches!(event, AiRunEvent::Done { .. }))
        .count();
    assert_eq!(done, 1);
    let (batches, frames) = run.events.frame_batches();
    assert!(batches >= 1);
    assert_eq!(frames as u64, run.ok().event_count);
    // Frames carry 1-based indexes and growing offsets.
    let items: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AiRunEvent::Frames { items } => Some(items.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(items[0].index, 1);
    // Offsets count from the send (not from the first frame: that was the earlier contract), so
    // the first one is the time to first byte; see `frame_offsets_count_from_the_send`.
    assert!(
        items
            .windows(2)
            .all(|pair| pair[1].index == pair[0].index + 1 && pair[1].t_ms >= pair[0].t_ms)
    );
}

#[tokio::test]
async fn a_fast_stream_is_coalesced_to_at_most_30_events_per_second() {
    let words: Vec<&'static str> = vec!["w"; 400];
    let canned = Canned {
        text: words,
        usage: (5, 400),
        ..Canned::default()
    };
    let fake = upstream(Protocol::OpenaiChat, canned).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", true),
    );
    let started = Instant::now();
    let run = fx.run("r1").await;
    let elapsed = started.elapsed();
    let dto = run.ok();
    let (batches, frames) = run.events.frame_batches();
    assert!(frames >= 400, "{frames} frames");
    assert_eq!(frames as u64, dto.event_count);
    let allowed = 2 + (elapsed.as_millis() / 33) as usize;
    assert!(
        batches <= allowed,
        "{batches} batches in {elapsed:?} (at most {allowed})"
    );
    assert!(
        batches < frames / 4,
        "frames were merged: {batches} batches for {frames} frames"
    );
}

#[tokio::test]
async fn stop_mid_stream_keeps_the_partial_reply_within_a_second() {
    let steps = vec![
        Step::Frame(format!(
            "data: {}\n\n",
            json!({"id": "c", "model": "m", "choices": [{"index": 0, "delta": {"content": "par"}}]})
        )),
        Step::Hang,
    ];
    let fake = Fake::always(Reply::Sse(steps)).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", true),
    );
    let events = super::support::Events::default();
    let sink = events.sink();
    let state = fx.state.clone();
    let stopper = {
        let events = events.clone();
        tokio::spawn(async move {
            // Wait for the first frames, then press Stop.
            loop {
                if events.frame_batches().1 > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let pressed = Instant::now();
            assert!(crate::app::ai_request_cancel_impl(
                &state,
                "stop-me".to_owned()
            ));
            pressed
        })
    };
    let result = crate::app::ai_request_run_impl(
        &fx.state,
        crate::app::AiRequestRunInput {
            workspace_id: super::support::WS.to_owned(),
            request_id: Some("r1".to_owned()),
            request: None,
            environment_id: None,
            confirmed: None,
            keep_exchange: true,
        },
        Some("stop-me".to_owned()),
        &sink,
    )
    .await;
    let pressed = stopper.await.unwrap();
    assert!(
        pressed.elapsed() < Duration::from_secs(1),
        "{:?}",
        pressed.elapsed()
    );
    let dto = result.unwrap();
    assert_eq!(dto.status, AiRunStatus::Stopped);
    assert_eq!(dto.reply, "par");
    assert!(dto.message.as_deref().unwrap().starts_with("Stopped at "));
    assert_eq!(dto.finish_reason, None, "a cut stream has no finish reason");
    assert!(matches!(events.all().last(), Some(AiRunEvent::Done { .. })));
    let log = fx.log(&dto);
    assert_eq!(log.entry.outcome, "canceled");
}

#[tokio::test]
async fn silence_before_the_first_byte_times_out() {
    let fake = Fake::always(Reply::Hang).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    let mut request = fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", true),
    );
    request.network_options.timeout_ms = 300;
    fx.save(&request);
    let started = Instant::now();
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::TimedOut);
    assert!(dto.message.as_deref().unwrap().starts_with("Timed out"));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        fx.log(dto).entry.failure_kind,
        Some(crate::llm::failure::FailureKind::Timeout)
    );
}

#[tokio::test]
async fn a_silent_gap_in_a_stream_times_out_and_keeps_the_partial_reply() {
    let steps = vec![
        Step::Frame(format!(
            "data: {}\n\n",
            json!({"id": "c", "model": "m", "choices": [{"index": 0, "delta": {"content": "pong so far"}}]})
        )),
        Step::Hang,
    ];
    let fake = Fake::always(Reply::Sse(steps)).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    let mut request = fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", true),
    );
    request.network_options.timeout_ms = 300;
    fx.save(&request);
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::TimedOut);
    assert_eq!(dto.reply, "pong so far");
    assert!(dto.message.as_deref().unwrap().contains("no data for"));
    assert!(dto.ttft_ms.is_some());
}

#[tokio::test]
async fn a_non_streaming_call_is_limited_as_a_whole() {
    let fake = Fake::always(Reply::After(
        Duration::from_secs(5),
        Box::new(Reply::Json(200, json!({}))),
    ))
    .await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    let mut request = fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    request.network_options.timeout_ms = 250;
    fx.save(&request);
    let started = Instant::now();
    let run = fx.run("r1").await;
    assert_eq!(run.ok().status, AiRunStatus::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn a_401_reads_rejected_with_the_provider_message() {
    let fake = Fake::always(Reply::Json(
        401,
        json!({"error": {"message": "Invalid API key", "type": "auth"}}),
    ))
    .await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Rejected);
    assert_eq!(dto.http_status, Some(401));
    assert_eq!(
        dto.message.as_deref(),
        Some("Rejected 401: Invalid API key")
    );
    assert!(
        dto.response_body
            .as_ref()
            .unwrap()
            .text
            .as_deref()
            .unwrap()
            .contains("Invalid API key")
    );
    let log = fx.log(dto);
    assert_eq!(log.entry.status, 401);
    assert_eq!(log.entry.outcome, "failure");
    assert_eq!(
        log.entry.failure_kind,
        Some(crate::llm::failure::FailureKind::Auth)
    );
}

#[tokio::test]
async fn a_429_lists_retry_after_and_rate_limit_headers_first() {
    let fake = Fake::always(Reply::WithHeaders(
        vec![
            ("x-extra", "1".to_owned()),
            ("retry-after", "2".to_owned()),
            ("x-ratelimit-remaining", "0".to_owned()),
            ("set-cookie", "sid=abc; Path=/".to_owned()),
            ("x-request-id", "req_77".to_owned()),
        ],
        Box::new(Reply::Json(429, json!({"error": {"message": "Slow down"}}))),
    ))
    .await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.message.as_deref(), Some("Rejected 429: Slow down"));
    let names: Vec<&str> = dto
        .response_headers
        .iter()
        .map(|h| h.name.as_str())
        .collect();
    assert_eq!(names[0], "retry-after");
    assert_eq!(dto.response_headers[0].value, "2");
    let rest = &names[..3];
    assert!(
        rest.contains(&"x-ratelimit-remaining") && rest.contains(&"x-request-id"),
        "{names:?}"
    );
    let cookie = dto
        .response_headers
        .iter()
        .find(|h| h.name == "set-cookie")
        .unwrap();
    assert_eq!(
        cookie.value, "sid=abc; Path=/",
        "cookies are shown as they came"
    );
    assert_eq!(
        fx.log(dto).entry.upstream_request_id.as_deref(),
        Some("req_77")
    );
}

#[tokio::test]
async fn an_unreachable_host_reads_cannot_reach_with_the_reason() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Unreachable);
    assert_eq!(dto.http_status, None);
    let message = dto.message.as_deref().unwrap();
    assert!(message.starts_with("Cannot reach 127.0.0.1: "), "{message}");
    assert!(
        message.contains("refused") && message.contains("(connect)"),
        "{message}"
    );
    let log = fx.log(dto);
    assert_eq!(log.entry.outcome, "failure");
    assert_eq!(log.entry.status, 502);
}

#[tokio::test]
async fn an_error_event_in_the_stream_is_rejected_and_keeps_the_text_before_it() {
    let steps = vec![
        Step::Frame(format!(
            "data: {}\n\n",
            json!({"id": "c", "model": "m", "choices": [{"index": 0, "delta": {"content": "half"}}]})
        )),
        Step::Frame(format!(
            "data: {}\n\n",
            json!({"error": {"message": "context deadline exceeded", "type": "server_error"}})
        )),
    ];
    let fake = Fake::always(Reply::Sse(steps)).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", true),
    );
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::Rejected);
    assert_eq!(dto.http_status, Some(200));
    assert_eq!(
        dto.stream_error.as_deref(),
        Some("context deadline exceeded")
    );
    assert_eq!(dto.reply, "half");
    assert!(
        dto.message
            .as_deref()
            .unwrap()
            .starts_with("Stream error: ")
    );
}

#[tokio::test]
async fn a_body_that_is_not_a_valid_answer_is_an_invalid_response() {
    let fake = Fake::always(Reply::Text(
        200,
        "application/json",
        "{\"nope\": true}".to_owned(),
    ))
    .await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert_eq!(dto.status, AiRunStatus::InvalidResponse);
    assert!(dto.message.as_deref().unwrap().contains("Chat Completions"));
    let html = Fake::always(Reply::Text(
        200,
        "text/html",
        "<html>login</html>".to_owned(),
    ))
    .await;
    let provider = fx.chat_provider("Relay 2", html.port);
    fx.provider_request(
        "r2",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    assert_eq!(fx.run("r2").await.ok().status, AiRunStatus::InvalidResponse);
}

#[tokio::test]
async fn cancel_reports_whether_a_run_was_registered() {
    let fx = Fx::new();
    assert!(!crate::app::ai_request_cancel_impl(
        &fx.state,
        "nobody".to_owned()
    ));
}
