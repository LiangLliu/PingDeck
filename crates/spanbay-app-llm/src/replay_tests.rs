//! Tests of the replay use cases: eligibility reasons, a replay with the
//! gateway stopped, the two modes, and Stop.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::uc_support::Fx;
use super::{llm_log_replay_cancel_impl, llm_log_replay_impl};
use crate::error::AppError;
use crate::events::EventSink;
use crate::llm::capture_record::{CapturedBody, CapturedExchange, RedactedHeader, TimingJson};
use crate::llm::gateway::replay::{ReplayEvent, ReplayMode};
use crate::llm::gateway::tests::{Canned, Fake, Reply, Step, canned_json, canned_sse};
use crate::llm::store::{self, LogFilter, LogWrite, RequestLogEntry};
use crate::llm::wire::Protocol;

const NOW: i64 = 1_800_000_000_000;

fn entry(stream: bool) -> RequestLogEntry {
    RequestLogEntry {
        ts_ms: NOW,
        protocol_in: "openai_chat".to_owned(),
        requested_model: "smart".to_owned(),
        alias: Some("smart".to_owned()),
        status: 200,
        outcome: "success".to_owned(),
        stream,
        source: "gateway".to_owned(),
        request_id: Some("rq_00000001".to_owned()),
        ..RequestLogEntry::default()
    }
}

fn body(text: &str) -> CapturedBody {
    CapturedBody {
        text: Some(text.to_owned()),
        truncated: false,
        elided: false,
        original_bytes: text.len() as u64,
    }
}

fn capture(request: CapturedBody) -> CapturedExchange {
    CapturedExchange {
        request_log_id: 0,
        ts_ms: NOW,
        client_protocol: Protocol::OpenaiChat,
        client_req_headers: vec![RedactedHeader {
            name: "authorization".to_owned(),
            value: "Bearer pd_cc_••••9d2f".to_owned(),
            redacted: true,
        }],
        client_req_body: Some(request),
        hub_request: None,
        upstream_url: String::new(),
        upstream_req_headers: Vec::new(),
        upstream_req_body: None,
        upstream_status: None,
        upstream_resp_headers: Vec::new(),
        upstream_resp_body: None,
        client_resp_headers: Vec::new(),
        client_resp_body: None,
        upstream_frames: None,
        client_frames: None,
        translation_notes: Vec::new(),
        timing: TimingJson::default(),
        bytes_total: 100,
        redacted_count: 0,
    }
}

fn put(fx: &Fx, entry: &RequestLogEntry, capture: Option<&CapturedExchange>) -> i64 {
    let store = fx.state.store();
    store::insert_log(
        store.connection(),
        &LogWrite {
            entry,
            capture,
            rollup: None,
        },
    )
    .unwrap()
}

fn request_body(stream: bool) -> String {
    json!({"model": "smart", "stream": stream, "messages": [{"role": "user", "content": "hi"}]})
        .to_string()
}

fn channel() -> (
    std::sync::Arc<dyn EventSink<ReplayEvent>>,
    mpsc::Receiver<ReplayEvent>,
) {
    let (tx, rx) = mpsc::channel();
    let sink: std::sync::Arc<dyn EventSink<ReplayEvent>> =
        std::sync::Arc::new(move |event| tx.send(event).is_ok());
    (sink, rx)
}

fn until_done(rx: &mpsc::Receiver<ReplayEvent>) -> (String, Option<i64>) {
    let mut text = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left).expect("the replay never ended") {
            ReplayEvent::Frames { text: more } => text.push_str(&more),
            ReplayEvent::Done { new_log_id } => return (text, new_log_id),
        }
    }
}

fn refused(result: Result<super::ReplayStarted, AppError>) -> String {
    match result {
        Err(AppError::Validation { message, .. }) => message,
        other => panic!("expected a validation refusal, got {other:?}"),
    }
}

async fn chat_upstream() -> Fake {
    Fake::start(|_, request| {
        let canned = Canned::text(&["pong"]);
        if request.body["stream"] == true {
            Reply::Sse(canned_sse(Protocol::OpenaiChat, "up", &canned))
        } else {
            Reply::Json(200, canned_json(Protocol::OpenaiChat, "up", &canned))
        }
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pruned_row_is_not_found() {
    let fx = Fx::new();
    let (sink, _rx) = channel();
    let result = llm_log_replay_impl(&fx.state, 999, ReplayMode::Replay, false, sink);
    assert!(
        matches!(result, Err(AppError::NotFound { .. })),
        "{result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn redacted_values_need_a_confirm() {
    let fx = Fx::new();
    let marker = crate::secrets::REDACTED;
    let text = json!({"model": "smart", "messages": [{"role": "user", "content": "key"}], "api_key": marker})
        .to_string();
    let id = put(&fx, &entry(false), Some(&capture(body(&text))));
    let (sink, _rx) = channel();
    let reason = refused(llm_log_replay_impl(
        &fx.state,
        id,
        ReplayMode::Replay,
        false,
        sink,
    ));
    assert!(reason.contains("1 redacted value"), "{reason}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_that_is_not_json_is_refused_before_anything_starts() {
    let fx = Fx::new();
    let id = put(&fx, &entry(false), Some(&capture(body("{not json"))));
    let (sink, _rx) = channel();
    let reason = refused(llm_log_replay_impl(
        &fx.state,
        id,
        ReplayMode::Replay,
        false,
        sink,
    ));
    assert!(reason.contains("not valid JSON"), "{reason}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replay_on_the_served_target_needs_a_served_target() {
    let fx = Fx::new();
    let id = put(
        &fx,
        &entry(false),
        Some(&capture(body(&request_body(false)))),
    );
    let (sink, _rx) = channel();
    let reason = refused(llm_log_replay_impl(
        &fx.state,
        id,
        ReplayMode::ReplayOnServedTarget,
        false,
        sink,
    ));
    assert_eq!(reason, "This request was not served by any target");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replay_runs_with_the_gateway_stopped_and_links_its_row() {
    let upstream = chat_upstream().await;
    let fx = Fx::new();
    fx.wire("smart", &upstream, &["m"]);
    let original = put(
        &fx,
        &entry(false),
        Some(&capture(body(&request_body(false)))),
    );
    let (sink, rx) = channel();
    let started =
        llm_log_replay_impl(&fx.state, original, ReplayMode::Replay, false, sink).unwrap();
    assert!(started.replay_id.starts_with("rp_"));
    assert_eq!(started.new_log_id, None);
    let (text, new_id) = until_done(&rx);
    assert!(text.contains("pong"), "{text}");
    let new_id = new_id.expect("the row is stored when the replay ends");
    let rows = {
        let store = fx.state.store();
        store::list_log(
            store.connection(),
            &store::LogPage {
                limit: 10,
                ..store::LogPage::default()
            },
        )
        .unwrap()
    };
    let row = rows.iter().find(|row| row.id == new_id).unwrap();
    assert_eq!(row.replay_of, Some(original));
    assert_eq!(row.client_name.as_deref(), Some("SpanBay replay"));
    assert_eq!(row.source, "gateway");
    assert!(
        !llm_log_replay_cancel_impl(&fx.state, &started.replay_id),
        "it already ended"
    );
    let _ = LogFilter::default();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replay_on_the_served_target_calls_the_provider_that_served_the_original() {
    let upstream = chat_upstream().await;
    let fx = Fx::new();
    let wired = fx.wire("smart", &upstream, &["m"]);
    let mut served = entry(false);
    served.provider_id = Some(wired.provider_id.clone());
    served.upstream_model = Some("m".to_owned());
    // the alias no longer exists by this name: the pinned replay does not need it
    let mut text = request_body(false);
    text = text.replace("smart", "renamed-alias");
    let original = put(&fx, &served, Some(&capture(body(&text))));
    let (sink, rx) = channel();
    llm_log_replay_impl(
        &fx.state,
        original,
        ReplayMode::ReplayOnServedTarget,
        false,
        sink,
    )
    .unwrap();
    let (text, new_id) = until_done(&rx);
    assert!(text.contains("pong"), "{text}");
    assert!(new_id.is_some());
    assert_eq!(upstream.count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_cancels_a_running_replay_within_a_second() {
    let mut steps =
        canned_sse(Protocol::OpenaiChat, "up", &Canned::text(&["Hel", "lo"]))[..2].to_vec();
    steps.push(Step::Hang);
    let upstream = Fake::always(Reply::Sse(steps)).await;
    let fx = Fx::new();
    fx.wire("smart", &upstream, &["m"]);
    let original = put(&fx, &entry(true), Some(&capture(body(&request_body(true)))));
    let (sink, rx) = channel();
    let started =
        llm_log_replay_impl(&fx.state, original, ReplayMode::Replay, false, sink).unwrap();
    // wait for the first frame, then press Stop
    let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(first, ReplayEvent::Frames { .. }), "{first:?}");
    let pressed = Instant::now();
    assert!(llm_log_replay_cancel_impl(&fx.state, &started.replay_id));
    let (_, new_id) = until_done(&rx);
    assert!(
        pressed.elapsed() < Duration::from_secs(1),
        "{:?}",
        pressed.elapsed()
    );
    let new_id = new_id.unwrap();
    let store = fx.state.store();
    let detail = store::get_log(store.connection(), new_id).unwrap().unwrap();
    assert_eq!(detail.entry.outcome, "canceled");
    assert_eq!(detail.entry.replay_of, Some(original));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_an_unknown_replay_is_false() {
    let fx = Fx::new();
    assert!(!llm_log_replay_cancel_impl(&fx.state, "rp_nope"));
}
