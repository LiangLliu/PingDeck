//! Fixtures shared by the llm store tests (log, captures, usage, ledger, clients, pricing).

use rusqlite::Connection;
use serde_json::json;

use crate::llm::capture_record::{
    AttemptTiming, CapturedBody, CapturedExchange, CapturedFrame, CapturedFrames, RedactedHeader,
    TimingJson, TranslationKind, TranslationNote,
};
use crate::llm::store::{LogWrite, RequestLogEntry};
use crate::llm::usage::RollupDelta;
use crate::llm::wire::Protocol;
use spanbay_model::phase::PhaseValue;

pub(in crate::llm::store) fn connection() -> Connection {
    crate::storage::migrated_memory_connection()
}

/// The write of `entry` with its optional capture and rollup.
pub(in crate::llm::store) fn write<'a>(
    entry: &'a RequestLogEntry,
    capture: Option<&'a CapturedExchange>,
    rollup: Option<&'a RollupDelta>,
) -> LogWrite<'a> {
    LogWrite {
        entry,
        capture,
        rollup,
    }
}

/// How many rows `llm_request_log` holds.
pub(in crate::llm::store) fn rows(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT COUNT(*) FROM llm_request_log", [], |row| row.get(0))
        .unwrap()
}

pub(in crate::llm::store) fn entry(model: &str) -> RequestLogEntry {
    RequestLogEntry {
        ts_ms: 1_000,
        protocol_in: "openai_chat".into(),
        requested_model: model.into(),
        status: 200,
        outcome: "success".into(),
        ..RequestLogEntry::default()
    }
}

pub(in crate::llm::store) fn delta(
    day: &str,
    provider: &str,
    failure: Option<&str>,
) -> RollupDelta {
    RollupDelta {
        day: day.into(),
        source: "gateway".into(),
        provider_id: provider.into(),
        key_id: "k1".into(),
        model: "m1".into(),
        client_id: String::new(),
        alias: String::new(),
        currency: "USD".into(),
        requests: 1,
        ok_requests: u64::from(failure.is_none()),
        input_tokens: 10,
        output_tokens: 5,
        cached_tokens: 2,
        cache_write_tokens: 1,
        reasoning_tokens: 3,
        cost_micro: 100,
        failure_kind: failure.map(str::to_owned),
    }
}

fn header(name: &str, value: &str, redacted: bool) -> RedactedHeader {
    RedactedHeader {
        name: name.into(),
        value: value.into(),
        redacted,
    }
}

fn body(text: &str, truncated: bool, elided: bool, original: u64) -> CapturedBody {
    CapturedBody {
        text: Some(text.into()),
        truncated,
        elided,
        original_bytes: original,
    }
}

/// A capture with every field set; `client_text` is the client request body.
pub(in crate::llm::store) fn capture(client_text: &str) -> CapturedExchange {
    CapturedExchange {
        request_log_id: 0,
        ts_ms: 1_234,
        client_protocol: Protocol::AnthropicMessages,
        client_req_headers: vec![
            header("Authorization", "pd_…cdef", true),
            header("content-type", "application/json", false),
        ],
        client_req_body: Some(body(client_text, true, true, 9_999)),
        hub_request: Some(json!({"model": "m", "messages": [{"role": "user"}]})),
        upstream_url: "https://api.example.com/v1/chat/completions?key=[redacted]".into(),
        upstream_req_headers: vec![header("x-api-key", "sk-…3f9a", true)],
        upstream_req_body: Some(body("{\"up\":1}", false, false, 8)),
        upstream_status: Some(429),
        upstream_resp_headers: vec![header("retry-after", "2", false)],
        upstream_resp_body: Some(body("{\"error\":\"slow down\"}", false, true, 500)),
        client_resp_headers: vec![header("content-type", "text/event-stream", false)],
        client_resp_body: Some(body("{\"out\":1}", true, false, 70_000)),
        upstream_frames: Some(CapturedFrames {
            frames: vec![CapturedFrame {
                index: 1,
                t_ms: 0,
                event: "message".into(),
                data: "{\"delta\":\"hel\\\"lo\"}\nsecond line".into(),
            }],
            total_frames: 6_413,
            truncated: true,
        }),
        client_frames: Some(CapturedFrames {
            frames: vec![CapturedFrame {
                index: 1,
                t_ms: 12,
                event: "content_block_delta".into(),
                data: "{\"text\":\"Héllo\"}".into(),
            }],
            total_frames: 1,
            truncated: false,
        }),
        translation_notes: vec![TranslationNote {
            field: "top_k".into(),
            kind: TranslationKind::Dropped,
            reason: "no equivalent in Chat Completions".into(),
        }],
        timing: TimingJson {
            queue_ms: 4,
            attempts: vec![AttemptTiming {
                dns: PhaseValue::NotNeeded,
                connect: PhaseValue::Ms(49),
                tls: PhaseValue::NotMeasured,
                connect_includes_tls: true,
                headers_ms: PhaseValue::Ms(233),
                first_token_ms: PhaseValue::Reused,
                end_ms: PhaseValue::Ms(900),
                reused: false,
            }],
        },
        bytes_total: 1_500,
        redacted_count: 3,
    }
}

/// Inserts `rows` log rows (ids 1..=rows, ts = id) and, when `capture_bytes` is set, a capture
/// of that many `bytes_total` per row whose client body is `needle`, using SQL only.
pub(in crate::llm::store) fn seed(
    connection: &Connection,
    rows: u32,
    capture_bytes: Option<u32>,
    needle: &str,
) {
    connection
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
             INSERT INTO llm_request_log (ts_ms, protocol_in, requested_model, status, outcome, \
                 has_capture) SELECT i, 'openai_chat', 'm', 200, 'success', ?2 FROM n",
            rusqlite::params![rows, i64::from(capture_bytes.is_some())],
        )
        .unwrap();
    if let Some(bytes) = capture_bytes {
        connection
            .execute(
                "INSERT INTO llm_debug_captures (request_log_id, ts_ms, client_protocol, \
                     client_headers_json, client_body_text, upstream_url, \
                     upstream_headers_json, bytes_total) \
                 SELECT id, ts_ms, 'openai_chat', '[]', ?1, '', '[]', ?2 FROM llm_request_log",
                rusqlite::params![needle, bytes],
            )
            .unwrap();
    }
}
