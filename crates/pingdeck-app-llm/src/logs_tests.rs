//! Tests of the request-log use cases: paging, detail with merged replies and action
//! availability, search, clear, capture deletion, curl and "Open in API client".

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;

use super::uc_support::Fx;
use super::{
    llm_gateway_overview_impl, llm_log_capture_stats_impl, llm_log_clear_impl, llm_log_count_impl,
    llm_log_curl_impl, llm_log_delete_capture_impl, llm_log_delete_captures_impl, llm_log_get_impl,
    llm_log_id_for_request_id_impl, llm_log_list_impl, llm_log_open_in_api_client_impl,
    llm_log_page_impl, llm_log_search_bodies_impl,
};

use crate::error::AppError;
use crate::llm::capture::CurlKind;
use crate::llm::capture_record::{CapturedBody, CapturedExchange, RedactedHeader, TimingJson};
use crate::llm::store::{self, LogFilter, LogWrite, RequestLogEntry, StatusClass};
use crate::llm::usage::{RollupDelta, local_day};
use crate::llm::wire::Protocol;

const NOW: i64 = 1_800_000_000_000;

fn entry(source: &str, status: u16, ts_ms: i64) -> RequestLogEntry {
    RequestLogEntry {
        ts_ms,
        protocol_in: "openai_chat".to_owned(),
        requested_model: "smart".to_owned(),
        alias: Some("smart".to_owned()),
        provider_id: Some("relay".to_owned()),
        provider_name: Some("Relay".to_owned()),
        upstream_model: Some("m".to_owned()),
        protocol_out: Some("openai_chat".to_owned()),
        status,
        outcome: if status < 400 { "success" } else { "failure" }.to_owned(),
        source: source.to_owned(),
        request_id: Some(format!("rq_{:08x}", ts_ms.unsigned_abs() % 0xffff_ffff)),
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

fn capture(request: &str, response: Option<&str>) -> CapturedExchange {
    CapturedExchange {
        request_log_id: 0,
        ts_ms: NOW,
        client_protocol: Protocol::OpenaiChat,
        client_req_headers: vec![RedactedHeader {
            name: "authorization".to_owned(),
            value: "Bearer pd_cc_••••9d2f".to_owned(),
            redacted: true,
        }],
        client_req_body: Some(body(request)),
        hub_request: None,
        upstream_url: "https://api.relay.test/v1/chat/completions".to_owned(),
        upstream_req_headers: vec![RedactedHeader {
            name: "authorization".to_owned(),
            value: "Bearer sk-…f00d".to_owned(),
            redacted: true,
        }],
        upstream_req_body: None,
        upstream_status: Some(200),
        upstream_resp_headers: Vec::new(),
        upstream_resp_body: None,
        client_resp_headers: Vec::new(),
        client_resp_body: response.map(body),
        upstream_frames: None,
        client_frames: None,
        translation_notes: Vec::new(),
        timing: TimingJson::default(),
        bytes_total: 1_000,
        redacted_count: 0,
    }
}

fn chat_response(text: &str) -> String {
    json!({"id": "c", "object": "chat.completion", "model": "m", "choices": [{"index": 0,
        "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}})
    .to_string()
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

fn request_body() -> String {
    json!({"model": "smart", "messages": [{"role": "user", "content": "hi"}]}).to_string()
}

#[test]
fn paging_and_count_respect_cursors_limits_and_filters() {
    let fx = Fx::new();
    let a = put(&fx, &entry("gateway", 200, NOW), None);
    let b = put(&fx, &entry("gateway", 429, NOW + 10), None);
    let c = put(&fx, &entry("probe", 503, NOW + 20), None);
    let page = llm_log_list_impl(&fx.state, None, Some(2), None).unwrap();
    assert_eq!(page.iter().map(|r| r.id).collect::<Vec<_>>(), [c, b]);
    let next = llm_log_list_impl(&fx.state, None, Some(2), Some(b)).unwrap();
    assert_eq!(next.iter().map(|r| r.id).collect::<Vec<_>>(), [a]);
    assert_eq!(
        llm_log_count_impl(
            &fx.state,
            Some(LogFilter {
                status_class: Some(StatusClass::ClientError),
                ..LogFilter::default()
            })
        )
        .unwrap(),
        1
    );
    assert!(matches!(
        llm_log_get_impl(&fx.state, 99_999),
        Err(AppError::NotFound { .. })
    ));
}

#[test]
fn detail_merges_reply_and_provides_redacted_curl_and_api_client_draft() {
    let fx = Fx::new();
    let id = put(
        &fx,
        &entry("gateway", 200, NOW),
        Some(&capture(&request_body(), Some(&chat_response("pong")))),
    );
    let detail = llm_log_get_impl(&fx.state, id).unwrap();
    assert_eq!(detail.merged_reply.unwrap().text, "pong");
    assert!(detail.curl_gateway.available && detail.curl_upstream.available);

    let gateway = llm_log_curl_impl(&fx.state, id, CurlKind::Gateway).unwrap();
    assert!(gateway.text.contains("$PINGDECK_TOKEN") && !gateway.text.contains("pd_cc_"));
    let upstream = llm_log_curl_impl(&fx.state, id, CurlKind::Upstream).unwrap();
    assert!(upstream.text.contains("$PROVIDER_API_KEY") && !upstream.text.contains("sk-"));

    let draft = llm_log_open_in_api_client_impl(&fx.state, id).unwrap();
    assert_eq!(draft.protocol, Protocol::OpenaiChat);
    assert!(draft.title.contains("m"));
}

#[tokio::test(flavor = "multi_thread")]
async fn search_clear_and_capture_deletion_preserve_rollups_and_escape_wildcards() {
    let fx = Fx::new();
    fx.state.llm().set_tz_offset_minutes(0);
    let day = local_day(crate::app::current_time_ms(), 0);
    store::upsert_rollup(
        fx.state.store().connection(),
        &RollupDelta {
            day,
            source: "gateway".to_owned(),
            provider_id: "relay".to_owned(),
            key_id: "k".to_owned(),
            model: "m".to_owned(),
            client_id: "c".to_owned(),
            alias: "smart".to_owned(),
            currency: "USD".to_owned(),
            requests: 7,
            ok_requests: 7,
            input_tokens: 1,
            output_tokens: 1,
            cached_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cost_micro: 0,
            failure_kind: None,
        },
    )
    .unwrap();

    let a = put(
        &fx,
        &entry("gateway", 200, NOW),
        Some(&capture(r#"{"q":"Hello 100%"}"#, None)),
    );
    let _b = put(
        &fx,
        &entry("gateway", 200, NOW + 1),
        Some(&capture(r#"{"q":"other"}"#, None)),
    );
    assert_eq!(
        llm_log_search_bodies_impl(&fx.state, "%").await.unwrap(),
        [a]
    );
    assert_eq!(
        llm_log_capture_stats_impl(&fx.state).await.unwrap(),
        store::CaptureStats {
            count: 2,
            bytes: 2_000
        }
    );
    assert_eq!(llm_log_delete_capture_impl(&fx.state, a).unwrap(), 1_000);
    assert_eq!(
        llm_log_delete_captures_impl(&fx.state).await.unwrap(),
        1_000
    );

    let cleared = llm_log_clear_impl(&fx.state, None).await.unwrap();
    assert_eq!(cleared.rows, 2);
    assert_eq!(
        llm_gateway_overview_impl(&fx.state)
            .unwrap()
            .today
            .unwrap()
            .usage
            .requests,
        7
    );
}

#[test]
fn filtered_log_page_and_request_id_lookup() {
    let fx = Fx::new();
    let mut e1 = entry("gateway", 200, NOW);
    e1.request_id = Some("rq_aaaa0001".to_owned());
    let mut e2 = entry("gateway", 500, NOW + 10);
    e2.request_id = Some("rq_aaaa0002".to_owned());
    let mut e3 = entry("agent", 502, NOW + 20);
    e3.request_id = Some("rq_aaaa0003".to_owned());

    let id1 = put(&fx, &e1, None);
    let id2 = put(&fx, &e2, None);
    let id3 = put(&fx, &e3, None);

    assert_eq!(
        llm_log_id_for_request_id_impl(&fx.state, "rq_aaaa0002").unwrap(),
        Some(id2)
    );
    assert_eq!(
        llm_log_id_for_request_id_impl(&fx.state, "rq_missing").unwrap(),
        None
    );

    let failed_p0 = llm_log_page_impl(
        &fx.state,
        Some(LogFilter {
            failed_only: true,
            ..LogFilter::default()
        }),
        0,
        Some(1),
    )
    .unwrap();
    assert_eq!(failed_p0.total, 2);
    assert_eq!(failed_p0.entries.len(), 1);
    assert_eq!(failed_p0.entries[0].id, id3);

    let failed_p1 = llm_log_page_impl(
        &fx.state,
        Some(LogFilter {
            failed_only: true,
            ..LogFilter::default()
        }),
        1,
        Some(1),
    )
    .unwrap();
    assert_eq!(failed_p1.total, 2);
    assert_eq!(failed_p1.entries[0].id, id2);
    assert_ne!(id1, id2);
}
