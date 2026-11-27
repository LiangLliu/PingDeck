use std::time::Duration;

use serde_json::json;
use spanbay_base::cancel::CancelToken;

use super::*;
use crate::llm::store::test_support::{capture, connection, delta, entry, rows, seed, write};
use crate::llm::store::{LogWrite, get_log, insert_log};

/// An unbounded search (the product's limits, never cancelled).
fn search_log_bodies(connection: &Connection, text: &str) -> Result<Vec<i64>, AppError> {
    super::search_log_bodies(connection, text, &SearchBudget::new(CancelToken::new()))
}

fn store(connection: &Connection, capture: &CapturedExchange) -> i64 {
    insert_log(
        connection,
        &LogWrite {
            entry: &entry("m"),
            capture: Some(capture),
            rollup: None,
        },
    )
    .unwrap()
}

#[test]
fn every_field_round_trips() {
    let c = connection();
    let original = capture("client body");
    let id = store(&c, &original);
    let loaded = get_capture(&c, id).unwrap().unwrap();
    assert_eq!(
        loaded,
        CapturedExchange {
            request_log_id: id,
            ..original
        }
    );
}

#[test]
fn an_empty_capture_round_trips_with_absent_slots_absent() {
    let c = connection();
    let original = CapturedExchange {
        request_log_id: 0,
        ts_ms: 5,
        client_protocol: Protocol::Embeddings,
        client_req_headers: Vec::new(),
        client_req_body: None,
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
        bytes_total: 0,
        redacted_count: 0,
    };
    let id = store(&c, &original);
    let loaded = get_capture(&c, id).unwrap().unwrap();
    assert_eq!(
        loaded,
        CapturedExchange {
            request_log_id: id,
            ..original
        }
    );
}

#[test]
fn a_stored_body_without_text_differs_from_no_body() {
    let c = connection();
    let mut original = capture("x");
    original.client_req_body = Some(CapturedBody {
        text: None,
        truncated: false,
        elided: false,
        original_bytes: 12,
    });
    original.upstream_req_body = None;
    let id = store(&c, &original);
    let loaded = get_capture(&c, id).unwrap().unwrap();
    assert_eq!(loaded.client_req_body, original.client_req_body);
    assert_eq!(loaded.upstream_req_body, None);
}

#[test]
fn header_redaction_flags_survive() {
    let c = connection();
    let id = store(&c, &capture("x"));
    let loaded = get_capture(&c, id).unwrap().unwrap();
    let flags: Vec<bool> = loaded
        .client_req_headers
        .iter()
        .map(|h| h.redacted)
        .collect();
    assert_eq!(flags, [true, false]);
    assert_eq!(loaded.upstream_req_headers[0].value, "sk-…3f9a");
}

#[test]
fn re_inserting_a_capture_replaces_it() {
    let c = connection();
    let id = store(&c, &capture("first"));
    insert_capture(&c, id, &capture("second")).unwrap();
    let loaded = get_capture(&c, id).unwrap().unwrap();
    assert_eq!(
        loaded.client_req_body.unwrap().text.as_deref(),
        Some("second")
    );
    let count: i64 = c
        .query_row("SELECT COUNT(*) FROM llm_debug_captures", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn a_missing_capture_is_none() {
    assert_eq!(get_capture(&connection(), 9).unwrap(), None);
}

#[test]
fn an_unknown_stored_protocol_is_an_internal_error() {
    let c = connection();
    let id = store(&c, &capture("x"));
    c.execute(
        "UPDATE llm_debug_captures SET client_protocol = 'smoke_signals'",
        [],
    )
    .unwrap();
    assert!(matches!(
        get_capture(&c, id),
        Err(AppError::Internal { .. })
    ));
}

#[test]
fn bytes_total_sums_every_capture() {
    let c = connection();
    assert_eq!(capture_bytes_total(&c).unwrap(), 0);
    assert_eq!(
        capture_stats(&c).unwrap(),
        CaptureStats { count: 0, bytes: 0 }
    );
    store(&c, &capture("a"));
    store(&c, &capture("b"));
    assert_eq!(capture_bytes_total(&c).unwrap(), 3_000);
    assert_eq!(
        capture_stats(&c).unwrap(),
        CaptureStats {
            count: 2,
            bytes: 3_000
        }
    );
}

#[test]
fn body_search_finds_all_four_bodies_case_insensitively() {
    let c = connection();
    let mut hit = capture("nothing here");
    hit.client_req_body = Some(CapturedBody::default());
    hit.upstream_req_body = None;
    hit.upstream_resp_body = None;
    hit.client_resp_body = None;
    hit.upstream_frames = None;
    hit.client_frames = None;
    let mut ids = Vec::new();
    for slot in 0..4 {
        let mut one = hit.clone();
        let body = Some(CapturedBody {
            text: Some("The Needle Is Here".into()),
            ..CapturedBody::default()
        });
        match slot {
            0 => one.client_req_body = body,
            1 => one.upstream_req_body = body,
            2 => one.upstream_resp_body = body,
            _ => one.client_resp_body = body,
        }
        ids.push(store(&c, &one));
    }
    store(&c, &hit);
    let mut found = search_log_bodies(&c, "NEEDLE").unwrap();
    found.sort_unstable();
    assert_eq!(found, ids);
    assert!(search_log_bodies(&c, "haystack").unwrap().is_empty());
}

#[test]
fn body_search_ignores_headers_and_stream_frames() {
    let c = connection();
    let mut original = capture("plain");
    original.client_req_headers[1].value = "needle-header".into();
    original.upstream_frames.as_mut().unwrap().frames[0].data = "needle-frame".into();
    original.client_frames.as_mut().unwrap().frames[0].data = "needle-frame".into();
    store(&c, &original);
    assert!(search_log_bodies(&c, "needle").unwrap().is_empty());
}

#[test]
fn body_search_is_injection_safe_and_ignores_empty_text() {
    let c = connection();
    store(&c, &capture("max_tokens"));
    assert!(
        search_log_bodies(&c, "'; DROP TABLE llm_request_log; --")
            .unwrap()
            .is_empty()
    );
    assert!(search_log_bodies(&c, "%").unwrap().is_empty());
    assert!(search_log_bodies(&c, "").unwrap().is_empty());
    let count: i64 = c
        .query_row("SELECT COUNT(*) FROM llm_request_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(search_log_bodies(&c, "max_tokens").unwrap().len(), 1);
}

#[test]
fn a_body_search_that_stored_the_injection_text_finds_it_literally() {
    let c = connection();
    let id = store(&c, &capture("x'; DROP TABLE llm_request_log; --"));
    assert_eq!(search_log_bodies(&c, "'; DROP TABLE").unwrap(), [id]);
}

#[test]
fn search_results_are_capped_and_newest_first() {
    let c = connection();
    seed(&c, 5_010, Some(1), "needle");
    let ids = search_log_bodies(&c, "needle").unwrap();
    assert_eq!(ids.len(), MAX_SEARCH_RESULTS);
    assert_eq!(ids[0], 5_010);
    assert!(ids.windows(2).all(|pair| pair[0] > pair[1]));
}

#[test]
fn hub_requests_keep_their_json_shape() {
    let c = connection();
    let mut original = capture("x");
    original.hub_request = Some(json!({"nested": {"a": [1, 2, {"b": null}]}}));
    let id = store(&c, &original);
    assert_eq!(
        get_capture(&c, id).unwrap().unwrap().hub_request,
        original.hub_request
    );
}

#[test]
fn delete_captures_frees_the_bytes_and_keeps_rows_and_rollup() {
    let c = connection();
    let capture = capture("body");
    let rollup = delta("2026-11-07", "p1", None);
    let first = insert_log(&c, &write(&entry("a"), Some(&capture), Some(&rollup))).unwrap();
    insert_log(&c, &write(&entry("b"), Some(&capture), Some(&rollup))).unwrap();
    insert_log(&c, &write(&entry("c"), None, None)).unwrap();
    assert_eq!(delete_captures(&c).unwrap(), 3_000);
    assert_eq!(delete_captures(&c).unwrap(), 0);
    assert_eq!(rows(&c), 3);
    assert_eq!(capture_bytes_total(&c).unwrap(), 0);
    assert!(!get_log(&c, first).unwrap().unwrap().entry.has_capture);
    let requests: i64 = c
        .query_row("SELECT requests FROM llm_usage_daily", [], |r| r.get(0))
        .unwrap();
    assert_eq!(requests, 2);
}

#[test]
fn delete_capture_removes_one_capture() {
    let c = connection();
    let capture = capture("body");
    let a = insert_log(&c, &write(&entry("a"), Some(&capture), None)).unwrap();
    let b = insert_log(&c, &write(&entry("b"), Some(&capture), None)).unwrap();
    assert_eq!(delete_capture(&c, a).unwrap(), 1_500);
    assert_eq!(delete_capture(&c, a).unwrap(), 0);
    assert_eq!(delete_capture(&c, 999).unwrap(), 0);
    assert!(!get_log(&c, a).unwrap().unwrap().entry.has_capture);
    assert!(get_log(&c, b).unwrap().unwrap().capture.is_some());
    assert_eq!(rows(&c), 2);
}

#[test]
fn delete_captures_walks_every_batch() {
    let c = connection();
    seed(&c, 1_201, Some(10), "x");
    assert_eq!(delete_captures(&c).unwrap(), 12_010);
    assert_eq!(capture_bytes_total(&c).unwrap(), 0);
    let flagged: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM llm_request_log WHERE has_capture = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(flagged, 0);
    assert_eq!(rows(&c), 1_201);
}

#[test]
fn a_row_budget_stops_the_search_at_the_newest_rows() {
    let c = connection();
    seed(&c, 300, Some(1), "needle");
    // 150 rows: one full step of 100, then a step cut to the 50 rows left.
    let budget = SearchBudget::with_limits(150, Duration::from_secs(10), CancelToken::new());
    assert_eq!(
        super::search_log_bodies(&c, "needle", &budget).unwrap(),
        (151..=300).rev().collect::<Vec<i64>>()
    );
}

#[test]
fn a_cancelled_search_returns_nothing() {
    let c = connection();
    seed(&c, 10, Some(1), "needle");
    let token = CancelToken::new();
    token.cancel();
    let budget = SearchBudget::new(token);
    assert!(
        super::search_log_bodies(&c, "needle", &budget)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_spent_time_budget_returns_nothing() {
    let c = connection();
    seed(&c, 10, Some(1), "needle");
    let budget = SearchBudget::with_limits(MAX_SEARCH_ROWS, Duration::ZERO, CancelToken::new());
    assert!(
        super::search_log_bodies(&c, "needle", &budget)
            .unwrap()
            .is_empty()
    );
}

/// The byte reads and the over-cap delete read `llm_debug_captures` through the covering index
/// only, never its payload pages.
#[test]
fn the_byte_queries_use_the_covering_index() {
    let c = connection();
    for sql in [BYTES_TOTAL_SQL, STATS_SQL, OVER_CAP_SQL] {
        let mut statement = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let zeros = vec![0_i64; statement.parameter_count()];
        let plan: Vec<String> = statement
            .query_map(rusqlite::params_from_iter(zeros), |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let scans: Vec<&String> = plan
            .iter()
            .filter(|step| step.contains("llm_debug_captures"))
            .collect();
        assert!(!scans.is_empty(), "{sql}: {plan:?}");
        for step in scans {
            assert!(
                step.contains("USING COVERING INDEX idx_llm_debug_captures_bytes")
                    // The over-cap DELETE finds the rows it removes by rowid.
                    || step.starts_with("SEARCH llm_debug_captures USING INTEGER PRIMARY KEY"),
                "{sql}: {plan:?}"
            );
        }
    }
}
