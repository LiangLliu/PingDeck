//! The log row, the capture, the history entry, the console line, cost, assertions and
//! `pm.response.ai` of a run.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;

use super::support::{Fx, WS, body_for, pong, upstream};
use crate::app::history_get_impl;
use crate::llm::ai_run::{AiRunStatus, CostNoteCode};
use crate::llm::gateway::tests::{Fake, Reply};
use crate::llm::usage::{PriceOverride, PriceSource};
use crate::llm::wire::Protocol;
use crate::workspace::ai_request::RequestMode;
use crate::workspace::models::HistoryOriginRef;
use crate::workspace::{Assertion, ExecutionHistoryResultKind};

async fn streamed_fixture() -> (Fx, Fake, String) {
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
    (fx, fake, provider)
}

fn price(provider: &str) -> PriceOverride {
    PriceOverride {
        provider_id: provider.to_owned(),
        model_id: "m".to_owned(),
        input_per_mtok: 1.0,
        output_per_mtok: 2.0,
        cache_read_per_mtok: None,
        cache_write_per_mtok: None,
        reasoning_per_mtok: None,
        currency: "USD".to_owned(),
        updated_at_ms: 0,
    }
}

#[tokio::test]
async fn one_api_row_per_run_with_the_run_id_and_a_capture_under_every_policy() {
    let (fx, _fake, provider) = streamed_fixture().await;
    fx.capture_mode("off");
    let run = fx.run("r1").await;
    let dto = run.ok();
    let log = fx.log(dto);
    assert_eq!(log.entry.source, "api");
    assert_eq!(log.entry.source_ref.as_deref(), Some(dto.run_id.as_str()));
    assert_eq!(log.entry.provider_id.as_deref(), Some(provider.as_str()));
    assert_eq!(log.entry.protocol_in, "openai_chat");
    assert_eq!(log.entry.protocol_out.as_deref(), Some("openai_chat"));
    assert_eq!(log.entry.status, 200);
    assert_eq!(log.entry.outcome, "success");
    assert!(log.entry.stream);
    assert_eq!(log.entry.input_tokens, Some(9));
    assert_eq!(log.entry.output_tokens, Some(2));
    assert_eq!(log.entry.ttft_ms, dto.ttft_ms);
    assert!(log.entry.has_capture && dto.kept_in_log);
    assert!(!log.entry.translated);
    let timing = log.entry.timing.as_ref().unwrap();
    assert_eq!(timing, &dto.timing);
    let capture = log.capture.unwrap();
    let frames = capture.upstream_frames.unwrap();
    assert_eq!(frames.total_frames, dto.event_count);
    assert_eq!(frames.frames.len(), dto.frames.frames.len());
    assert_eq!(capture.upstream_status, Some(200));
    assert!(
        capture
            .client_req_body
            .unwrap()
            .text
            .unwrap()
            .contains("\"stream\":true")
    );
}

#[tokio::test]
async fn the_stored_capture_redacts_the_credential_while_the_live_view_keeps_the_reply() {
    let (fx, _fake, _) = streamed_fixture().await;
    let run = fx.run("r1").await;
    let capture = fx.log(run.ok()).capture.unwrap();
    let auth = capture
        .upstream_req_headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("authorization"))
        .unwrap();
    assert!(auth.redacted);
    assert!(!auth.value.contains(super::support::KEY));
    // The live response tabs and the capture show the same frames.
    let live: Vec<&str> = run
        .ok()
        .frames
        .frames
        .iter()
        .map(|f| f.data.as_str())
        .collect();
    let stored: Vec<&str> = capture
        .upstream_frames
        .as_ref()
        .unwrap()
        .frames
        .iter()
        .map(|f| f.data.as_str())
        .collect();
    assert_eq!(live, stored);
}

#[tokio::test]
async fn a_failed_run_still_gets_its_row_with_the_shaped_error() {
    let fake = Fake::always(Reply::Json(500, json!({"error": {"message": "boom"}}))).await;
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
    let log = fx.log(run.ok());
    assert_eq!(log.entry.status, 500);
    assert_eq!(log.entry.outcome, "failure");
    assert_eq!(log.entry.error.as_deref(), Some("Rejected 500: boom"));
    assert!(
        log.entry.has_capture,
        "the default keeps the exchange of a failure too"
    );
}

#[tokio::test]
async fn the_cost_uses_the_effective_price_and_says_when_there_is_none() {
    let (fx, _fake, provider) = streamed_fixture().await;
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert!(dto.cost.is_none());
    assert_eq!(dto.cost_note.as_deref(), Some("No price for this model"));
    assert_eq!(dto.cost_note_code, Some(CostNoteCode::NoPrice));
    assert!(dto.price.is_none());

    fx.save_price(price(&provider));
    let run = fx.run("r1").await;
    let dto = run.ok();
    let cost = dto.cost.as_ref().expect("a priced model has a cost");
    assert_eq!(cost.micro, 13, "9 in at 1 + 2 out at 2 per million");
    assert_eq!(cost.currency, "USD");
    assert_eq!(cost.source, PriceSource::Override);
    assert!(!cost.estimated);
    assert_eq!(dto.price.as_ref().unwrap().output_per_mtok, 2.0);
    assert!(dto.cost_note.is_none() && dto.cost_note_code.is_none());
    let log = fx.log(dto);
    assert_eq!(log.entry.cost_micro_usd, Some(13));
    assert_eq!(log.entry.price_source.as_deref(), Some("override"));
}

#[tokio::test]
async fn one_history_entry_per_run_with_the_ai_shape() {
    let (fx, _fake, _) = streamed_fixture().await;
    let run = fx.run("r1").await;
    let dto = run.ok();
    let page = crate::app::misc_logic_service::history_page_impl(
        &fx.state,
        WS.to_owned(),
        crate::workspace::HistoryFilter::default(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(page.entries.len(), 1);
    let entry = history_get_impl(
        &fx.state,
        WS.to_owned(),
        page.entries[0].id.as_str().to_owned(),
    )
    .unwrap();
    assert_eq!(entry.protocol, "openai_chat");
    assert_eq!(entry.status_code, 200);
    assert_eq!(
        entry.response_body_preview, "pong",
        "the merged reply, not the event stream"
    );
    assert_eq!(entry.result_kind, ExecutionHistoryResultKind::Success);
    let snapshot = entry.request_snapshot.as_ref().unwrap();
    assert_eq!(snapshot.mode, RequestMode::AiApi);
    assert!(snapshot.ai.is_some());
    let timing = entry.timing.unwrap();
    assert_eq!(timing.total_ms, u128::from(dto.total_ms));
    assert!(timing.request_ms <= timing.total_ms);
    assert_eq!(
        timing.first_token_ms, dto.ttft_ms,
        "the row carries the TTFT of a streamed call"
    );
    assert!(timing.first_token_ms.is_some(), "this fixture streams");
    assert!(entry.run_id.is_none(), "a single run is not a run of many");
    let call = fx
        .log(dto)
        .entry
        .request_id
        .expect("the log row has a request id");
    assert_eq!(dto.log_request_id.as_deref(), Some(call.as_str()));
    assert_eq!(
        entry.origin_ref,
        Some(HistoryOriginRef::Call { id: call }),
        "the row points at its call in Usage › Calls"
    );
    let sent = entry.sent_request.unwrap();
    assert!(
        !serde_json::to_string(&sent)
            .unwrap()
            .contains(super::support::KEY)
    );
    assert!(!entry.response_headers.is_empty());
    assert!(!entry.request_url.contains("127.0.0.1"));
}

#[tokio::test]
async fn a_failed_call_without_an_answer_is_a_failure_entry() {
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
    assert_eq!(run.ok().status, AiRunStatus::Unreachable);
    let page = crate::app::misc_logic_service::history_page_impl(
        &fx.state,
        WS.to_owned(),
        crate::workspace::HistoryFilter::default(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        page.entries[0].result_kind,
        ExecutionHistoryResultKind::Failure
    );
    assert_eq!(page.entries[0].error_kind.as_deref(), Some("Unreachable"));
}

#[tokio::test]
async fn the_console_gets_one_line_per_run_next_to_the_scripts_output() {
    let (fx, _fake, _) = streamed_fixture().await;
    let mut request = fx
        .state
        .store()
        .get_request(
            &crate::workspace::WorkspaceId::new(WS).unwrap(),
            &crate::workspace::RequestId::new("r1").unwrap(),
        )
        .unwrap()
        .unwrap();
    request.test_script = "console.log('after the call');".to_owned();
    fx.save(&request);
    let run = fx.run("r1").await;
    let dto = run.ok();
    let line = &dto.console[0];
    assert_eq!(
        line.message,
        format!("AI m · {} ms · 9 in / 2 out", dto.total_ms)
    );
    assert!(
        dto.console
            .iter()
            .any(|entry| entry.message.contains("after the call"))
    );
}

#[tokio::test]
async fn the_six_ai_assertions_run_on_the_merged_reply_and_the_numbers() {
    let (fx, _fake, _) = streamed_fixture().await;
    let mut request = fx
        .state
        .store()
        .get_request(
            &crate::workspace::WorkspaceId::new(WS).unwrap(),
            &crate::workspace::RequestId::new("r1").unwrap(),
        )
        .unwrap()
        .unwrap();
    request.assertions = vec![
        Assertion::AiReplyContains {
            value: "pong".into(),
        },
        Assertion::AiReplyContains {
            value: "PONG".into(),
        },
        Assertion::AiReplyMatches {
            pattern: "^po.g$".into(),
        },
        Assertion::AiJsonSchemaValid {
            schema: json!({"type": "string"}),
        },
        Assertion::AiTtftLessThanMs { value: 60_000 },
        Assertion::AiTokensLessThan { value: 50 },
        Assertion::AiTokensLessThan { value: 5 },
        Assertion::AiNoError {},
        Assertion::StatusEquals { value: 200 },
    ];
    fx.save(&request);
    let run = fx.run("r1").await;
    let passed: Vec<bool> = run
        .ok()
        .assertion_results
        .iter()
        .map(|r| r.passed)
        .collect();
    assert_eq!(
        passed,
        [true, false, true, false, true, true, false, true, true],
        "{:?}",
        run.ok().assertion_results
    );
    // The reply `pong` is not JSON, so the schema check says why.
    assert!(
        run.ok().assertion_results[3]
            .message
            .to_lowercase()
            .contains("json")
    );
}

#[tokio::test]
async fn pm_response_ai_is_filled_for_the_test_script() {
    let (fx, _fake, _) = streamed_fixture().await;
    let mut request = fx
        .state
        .store()
        .get_request(
            &crate::workspace::WorkspaceId::new(WS).unwrap(),
            &crate::workspace::RequestId::new("r1").unwrap(),
        )
        .unwrap()
        .unwrap();
    request.test_script = r#"
        pm.test("reply", () => { pm.expect(pm.response.ai.reply).to.equal("pong"); });
        pm.test("usage", () => {
            pm.expect(pm.response.ai.usage.inputTokens).to.equal(9);
            pm.expect(pm.response.ai.usage.outputTokens).to.equal(2);
        });
        pm.test("rate is null below 300 tokens", () => { pm.expect(pm.response.ai.tokensPerSecond).to.equal(null); });
        pm.test("events", () => { pm.expect(pm.response.ai.eventCount).to.be.above(0); });
        pm.test("the raw body is the event stream", () => {
            pm.expect(pm.response.text()).to.include("data:");
            let threw = false;
            try { pm.response.json(); } catch (e) { threw = true; }
            pm.expect(threw).to.equal(true);
        });
        const exposed = Object.keys(pm).filter((key) => /provider|apikey|baseurl/i.test(key));
        pm.test("no provider property", () => { pm.expect(exposed.length).to.equal(0); });
    "#
    .to_owned();
    fx.save(&request);
    let run = fx.run("r1").await;
    let dto = run.ok();
    assert!(dto.script_error.is_none(), "{:?}", dto.script_error);
    assert_eq!(dto.script_tests.len(), 6);
    assert!(
        dto.script_tests.iter().all(|test| test.passed),
        "{:?}",
        dto.script_tests
    );
}

#[tokio::test]
async fn a_failing_pre_request_script_sends_nothing_and_leaves_a_failure_entry() {
    let fake = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", fake.port);
    let mut request = fx.provider_request(
        "r1",
        &provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
    request.pre_request_script = "throw new Error('boom');".to_owned();
    fx.save(&request);
    let run = fx.run("r1").await;
    let error = run.result.unwrap_err();
    assert!(
        error.message().contains("pre-request script failed"),
        "{error}"
    );
    assert_eq!(fake.count(), 0);
    let page = crate::app::misc_logic_service::history_page_impl(
        &fx.state,
        WS.to_owned(),
        crate::workspace::HistoryFilter::default(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(page.entries[0].error_kind.as_deref(), Some("ScriptError"));
}

#[tokio::test]
async fn a_draft_or_a_saved_request_but_never_both_or_neither() {
    let fx = Fx::new();
    let sink = crate::events::NoEvents;
    let run = |request_id: Option<&str>| {
        crate::app::ai_request_run_impl(
            &fx.state,
            crate::app::AiRequestRunInput {
                workspace_id: WS.to_owned(),
                request_id: request_id.map(str::to_owned),
                request: None,
                environment_id: None,
                confirmed: None,
                keep_exchange: true,
            },
            None,
            &sink,
        )
    };
    assert!(matches!(
        run(None).await,
        Err(crate::error::AppError::Validation { .. })
    ));
    assert!(matches!(
        run(Some("nope")).await,
        Err(crate::error::AppError::NotFound { .. })
    ));
    let mut http =
        crate::workspace::Request::new("h1", WS, "Plain", "http://127.0.0.1:1/").unwrap();
    http.mode = RequestMode::Http;
    fx.save(&http);
    let error = run(Some("h1")).await.unwrap_err();
    assert!(error.message().contains("not in AI API mode"), "{error}");
}
