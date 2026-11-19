use serde_json::json;

use super::*;
use crate::llm::store::StatusClass;
use crate::llm::store::test_support::{capture, connection, delta, entry, rows, seed, write};

fn no_prune_limits() -> PruneLimits {
    PruneLimits {
        max_rows: 100_000,
        per_source_rows: 0,
        max_days: 36_500,
        max_capture_days: 36_500,
        max_capture_bytes: u64::MAX / 2,
    }
}

fn full_entry() -> RequestLogEntry {
    RequestLogEntry {
        id: 0,
        ts_ms: 1_700_000_000_000,
        protocol_in: "anthropic_messages".into(),
        requested_model: "claude-code".into(),
        provider_id: Some("p1".into()),
        provider_name: Some("DeepSeek".into()),
        key_id: Some("k1".into()),
        upstream_model: Some("deepseek-v4-pro".into()),
        protocol_out: Some("openai_chat".into()),
        status: 429,
        outcome: "failure".into(),
        stream: true,
        latency_ms: 690,
        ttft_ms: Some(233),
        input_tokens: Some(18_204),
        output_tokens: Some(912),
        cached_tokens: Some(100),
        attempts: json!([{"provider": "p1", "status": 429}, {"provider": "p2", "status": 200}]),
        error: Some("rate limited".into()),
        source: "agent".into(),
        source_ref: Some("session-1".into()),
        request_id: Some("rq_3fa91c2e".into()),
        client_id: Some("cl_0123456789ab".into()),
        client_name: Some("Claude Code".into()),
        peer: Some("192.168.1.5:5000".into()),
        alias: Some("fast".into()),
        translated: true,
        reasoning_tokens: Some(40),
        cache_write_tokens: Some(7),
        tokens_estimated: true,
        cost_micro_usd: Some(23_100),
        cost_currency: "CNY".into(),
        price_source: Some("override".into()),
        failure_kind: Some(FailureKind::RateLimit),
        upstream_request_id: Some("req_abc".into()),
        req_bytes: Some(5_242_880),
        resp_bytes: Some(1_024),
        timing: Some(capture("x").timing),
        has_capture: false,
        replay_of: Some(3),
        session_id: Some("session-1".into()),
        run_id: Some("run-9".into()),
        message_id: Some("am-9".into()),
        purpose: Some("step".into()),
        finish_reason: Some("tool_calls".into()),
        price_basis: None,
        retry_after_s: None,
        reason: None,
    }
}

#[test]
fn every_field_round_trips_through_insert_and_get() {
    let c = connection();
    let original = full_entry();
    let id = insert_log(&c, &write(&original, None, None)).unwrap();
    let detail = get_log(&c, id).unwrap().unwrap();
    assert_eq!(detail.capture, None);
    // `retry_after_s` and `reason` are derived on read, not stored.
    let derived = (detail.entry.retry_after_s, detail.entry.reason.clone());
    assert_eq!(
        detail.entry,
        RequestLogEntry {
            id,
            retry_after_s: derived.0,
            reason: derived.1,
            ..original
        }
    );
}

#[test]
fn insert_log_stores_the_row_the_capture_and_the_rollup_together() {
    let c = connection();
    let entry = entry("m");
    let capture = capture("body");
    let rollup = delta("2026-11-07", "p1", Some("rate_limit"));
    let id = insert_log(&c, &write(&entry, Some(&capture), Some(&rollup))).unwrap();
    let detail = get_log(&c, id).unwrap().unwrap();
    assert!(detail.entry.has_capture);
    assert_eq!(detail.capture.unwrap().request_log_id, id);
    let (requests, failures): (i64, i64) = c
        .query_row(
            "SELECT (SELECT requests FROM llm_usage_daily), (SELECT count FROM llm_usage_failures)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((requests, failures), (1, 1));
}

#[test]
fn has_capture_follows_the_write_not_the_entry_flag() {
    let c = connection();
    let mut claims = entry("m");
    claims.has_capture = true;
    let id = insert_log(&c, &write(&claims, None, None)).unwrap();
    assert!(!get_log(&c, id).unwrap().unwrap().entry.has_capture);
    assert!(get_log(&c, id).unwrap().unwrap().capture.is_none());
}

#[test]
fn a_failing_rollup_rolls_back_the_row_and_the_capture() {
    let c = connection();
    c.execute_batch("DROP TABLE llm_usage_daily").unwrap();
    let capture = capture("body");
    let rollup = delta("2026-11-07", "p1", None);
    let result = insert_log(&c, &write(&entry("m"), Some(&capture), Some(&rollup)));
    assert!(matches!(result, Err(AppError::Database { .. })));
    assert_eq!(rows(&c), 0);
    let captures: i64 = c
        .query_row("SELECT COUNT(*) FROM llm_debug_captures", [], |r| r.get(0))
        .unwrap();
    assert_eq!(captures, 0);
}

#[test]
fn a_duplicate_request_id_is_refused_and_leaves_no_rollup() {
    let c = connection();
    let mut first = entry("m");
    first.request_id = Some("rq_00000001".into());
    let rollup = delta("2026-11-07", "p1", None);
    insert_log(&c, &write(&first, None, Some(&rollup))).unwrap();
    assert!(insert_log(&c, &write(&first, None, Some(&rollup))).is_err());
    assert_eq!(rows(&c), 1);
    let requests: i64 = c
        .query_row("SELECT requests FROM llm_usage_daily", [], |r| r.get(0))
        .unwrap();
    assert_eq!(requests, 1, "the refused insert did not touch the rollup");
    // Rows without a request id never collide.
    insert_log(&c, &write(&entry("a"), None, None)).unwrap();
    insert_log(&c, &write(&entry("b"), None, None)).unwrap();
}

#[test]
fn request_ids_resolve_to_row_ids() {
    let c = connection();
    let mut tagged = entry("m");
    tagged.request_id = Some("rq_3fa91c2e".into());
    let id = insert_log(&c, &write(&tagged, None, None)).unwrap();
    assert_eq!(log_id_for_request_id(&c, "rq_3fa91c2e").unwrap(), Some(id));
    assert_eq!(log_id_for_request_id(&c, "rq_00000000").unwrap(), None);
}

#[test]
fn keyset_paging_walks_every_row_once_newest_first() {
    let c = connection();
    for index in 0..25 {
        insert_log(&c, &write(&entry(&format!("m{index}")), None, None)).unwrap();
    }
    let mut seen: Vec<i64> = Vec::new();
    let mut cursor = None;
    loop {
        let page = list_log(
            &c,
            &LogPage {
                before_id: cursor,
                limit: 10,
                ..LogPage::default()
            },
        )
        .unwrap();
        let Some(last) = page.last() else { break };
        cursor = Some(last.id);
        seen.extend(page.iter().map(|row| row.id));
    }
    assert_eq!(seen.len(), 25);
    assert!(seen.windows(2).all(|pair| pair[0] > pair[1]));
}

#[test]
fn page_limits_are_clamped() {
    let c = connection();
    seed(&c, 600, None, "");
    let page = |limit| {
        list_log(
            &c,
            &LogPage {
                limit,
                ..LogPage::default()
            },
        )
        .unwrap()
        .len()
    };
    assert_eq!(page(0), 1);
    assert_eq!(page(10_000), MAX_LOG_PAGE);
}

#[test]
fn pages_filter_by_source_and_provider() {
    let c = connection();
    for (source, provider) in [("gateway", "p1"), ("agent", "p1"), ("agent", "p2")] {
        let mut row = entry("m");
        row.source = source.into();
        row.provider_id = Some(provider.into());
        insert_log(&c, &write(&row, None, None)).unwrap();
    }
    let count = |source: Option<&str>, provider: Option<&str>| {
        list_log(
            &c,
            &LogPage {
                source: source.map(str::to_owned),
                provider_id: provider.map(str::to_owned),
                ..LogPage::default()
            },
        )
        .unwrap()
        .len()
    };
    assert_eq!(count(None, None), 3);
    assert_eq!(count(Some("agent"), None), 2);
    assert_eq!(count(None, Some("p1")), 2);
    assert_eq!(count(Some("agent"), Some("p2")), 1);
}

#[test]
fn a_missing_row_is_none() {
    assert_eq!(get_log(&connection(), 42).unwrap(), None);
}

#[test]
fn count_filters_by_every_field() {
    let c = connection();
    let add = |source: &str, provider: &str, client: &str, status: u16, outcome: &str, ts: i64| {
        let mut row = entry("m");
        row.source = source.into();
        row.provider_id = Some(provider.into());
        row.client_id = Some(client.into());
        row.status = status;
        row.outcome = outcome.into();
        row.ts_ms = ts;
        insert_log(&c, &write(&row, None, None)).unwrap();
    };
    add("gateway", "p1", "c1", 200, "success", 100);
    add("gateway", "p1", "c2", 404, "failure", 200);
    add("agent", "p2", "c1", 503, "failure", 300);
    add("gateway", "p2", "c1", 200, "canceled", 400);
    let count = |filter: LogFilter| count_log(&c, &filter).unwrap();
    assert_eq!(count(LogFilter::default()), 4);
    let by = |f: fn(&mut LogFilter)| {
        let mut filter = LogFilter::default();
        f(&mut filter);
        count(filter)
    };
    assert_eq!(by(|f| f.source = Some("gateway".into())), 3);
    assert_eq!(by(|f| f.provider_id = Some("p2".into())), 2);
    assert_eq!(by(|f| f.client_id = Some("c1".into())), 3);
    assert_eq!(by(|f| f.since_ms = Some(300)), 2);
    assert_eq!(by(|f| f.status_class = Some(StatusClass::Success)), 1);
    assert_eq!(by(|f| f.status_class = Some(StatusClass::ClientError)), 1);
    assert_eq!(by(|f| f.status_class = Some(StatusClass::ServerError)), 1);
    assert_eq!(by(|f| f.status_class = Some(StatusClass::Canceled)), 1);
    let combined = LogFilter {
        source: Some("gateway".into()),
        client_id: Some("c1".into()),
        ..LogFilter::default()
    };
    assert_eq!(count(combined), 2);
}

#[test]
fn the_failed_over_class_counts_rows_with_several_attempts() {
    let c = connection();
    let mut one = entry("a");
    one.attempts = json!([{"n": 1}]);
    let mut two = entry("b");
    two.attempts = json!([{"n": 1}, {"n": 2}]);
    insert_log(&c, &write(&one, None, None)).unwrap();
    insert_log(&c, &write(&two, None, None)).unwrap();
    c.execute(
        "UPDATE llm_request_log SET attempts_json = 'not json' WHERE requested_model = 'a'",
        [],
    )
    .unwrap();
    let filter = LogFilter {
        status_class: Some(StatusClass::FailedOver),
        ..LogFilter::default()
    };
    assert_eq!(count_log(&c, &filter).unwrap(), 1);
}

#[test]
fn clearing_deletes_rows_and_captures_but_not_the_rollup() {
    let c = connection();
    let capture = capture("body");
    let rollup = delta("2026-11-07", "p1", None);
    for _ in 0..3 {
        insert_log(&c, &write(&entry("m"), Some(&capture), Some(&rollup))).unwrap();
    }
    assert_eq!(
        clear_log(&c, &LogFilter::default()).unwrap(),
        ClearReport {
            rows: 3,
            capture_bytes: 4_500
        }
    );
    assert_eq!(rows(&c), 0);
    let (captures, requests): (i64, i64) = c
        .query_row(
            "SELECT (SELECT COUNT(*) FROM llm_debug_captures), (SELECT requests FROM llm_usage_daily)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((captures, requests), (0, 3));
}

#[test]
fn a_filtered_clear_only_touches_matching_rows() {
    let c = connection();
    let capture = capture("body");
    let mut probe = entry("m");
    probe.source = "probe".into();
    insert_log(&c, &write(&probe, Some(&capture), None)).unwrap();
    insert_log(&c, &write(&entry("m"), Some(&capture), None)).unwrap();
    let filter = LogFilter {
        source: Some("probe".into()),
        ..LogFilter::default()
    };
    assert_eq!(clear_log(&c, &filter).unwrap().rows, 1);
    assert_eq!(rows(&c), 1);
    let captures: i64 = c
        .query_row("SELECT COUNT(*) FROM llm_debug_captures", [], |r| r.get(0))
        .unwrap();
    assert_eq!(captures, 1);
}

#[test]
fn five_thousand_and_one_inserts_are_pruned_to_five_thousand() {
    let c = connection();
    seed(&c, 5_001, None, "");
    assert_eq!(rows(&c), 5_001, "seed does not prune");
    let limits = PruneLimits {
        max_rows: 5_000,
        ..no_prune_limits()
    };
    let report = prune_log(&c, &limits, 1_000).unwrap();
    assert_eq!(report.rows_deleted, 1);
    assert_eq!(rows(&c), 5_000);
    let oldest: i64 = c
        .query_row(
            "SELECT id FROM llm_request_log ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(oldest, 2);
}

// An agent writing far past the cap leaves the gateway's newest rows (its floor) in
// place; the agent's oldest go first.
#[test]
fn each_source_keeps_its_floor_when_another_floods_the_log() {
    let c = connection();
    seed(&c, 30, None, "");
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 120) \
         INSERT INTO llm_request_log (ts_ms, protocol_in, requested_model, status, outcome, \
             source) SELECT 100 + i, 'openai_chat', 'm', 200, 'success', 'agent' FROM n",
        [],
    )
    .unwrap();
    let limits = PruneLimits {
        max_rows: 100,
        per_source_rows: 20,
        ..no_prune_limits()
    };
    prune_log(&c, &limits, 1_000).unwrap();
    let count = |source: &str| -> i64 {
        c.query_row(
            "SELECT COUNT(*) FROM llm_request_log WHERE source = ?1",
            [source],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!((count("gateway"), count("agent")), (20, 80));
    let newest_gateway: i64 = c
        .query_row(
            "SELECT MIN(id) FROM llm_request_log WHERE source = 'gateway'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(newest_gateway, 11, "the gateway keeps its 20 newest rows");
}

#[test]
fn pruning_with_fewer_rows_than_the_cap_deletes_nothing() {
    let c = connection();
    seed(&c, 10, None, "");
    let limits = PruneLimits {
        max_rows: 100,
        ..no_prune_limits()
    };
    assert_eq!(
        prune_log(&c, &limits, 1_000).unwrap(),
        PruneReport::default()
    );
    assert_eq!(rows(&c), 10);
}

#[test]
fn rows_older_than_the_age_limit_are_deleted() {
    let c = connection();
    let day = 86_400_000;
    for ts in [1_000, 2 * day, 9 * day] {
        let mut row = entry("m");
        row.ts_ms = ts;
        insert_log(&c, &write(&row, None, None)).unwrap();
    }
    let limits = PruneLimits {
        max_days: 7,
        ..no_prune_limits()
    };
    // now = day 10: the row from day 0 is 10 days old, the one from day 2 is 8, day 9 is 1.
    let report = prune_log(&c, &limits, 10 * day).unwrap();
    assert_eq!(report.rows_deleted, 2);
    assert_eq!(rows(&c), 1);
}

#[test]
fn the_capture_byte_cap_deletes_the_oldest_captures_and_keeps_every_row() {
    let c = connection();
    seed(&c, 10, Some(100), "needle");
    let limits = PruneLimits {
        max_capture_bytes: 350,
        ..no_prune_limits()
    };
    let report = prune_log(&c, &limits, 1_000).unwrap();
    assert_eq!((report.rows_deleted, report.captures_deleted), (0, 7));
    assert_eq!(rows(&c), 10);
    assert_eq!(crate::llm::store::capture_bytes_total(&c).unwrap(), 300);
    let kept: Vec<i64> = c
        .prepare("SELECT request_log_id FROM llm_debug_captures ORDER BY 1")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(kept, [8, 9, 10], "the newest captures stay");
    let flags: Vec<(i64, i64)> = c
        .prepare("SELECT id, has_capture FROM llm_request_log ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        flags.iter().all(|(id, flag)| *flag == i64::from(*id >= 8)),
        "{flags:?}"
    );
}

#[test]
fn a_single_capture_over_the_cap_is_deleted_too() {
    let c = connection();
    seed(&c, 1, Some(500), "x");
    let limits = PruneLimits {
        max_capture_bytes: 100,
        ..no_prune_limits()
    };
    assert_eq!(prune_log(&c, &limits, 1_000).unwrap().captures_deleted, 1);
    assert_eq!(crate::llm::store::capture_bytes_total(&c).unwrap(), 0);
}

#[test]
fn pruning_rows_deletes_their_captures_even_without_foreign_keys() {
    let c = connection();
    c.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    seed(&c, 5, Some(10), "x");
    let limits = PruneLimits {
        max_rows: 2,
        ..no_prune_limits()
    };
    let report = prune_log(&c, &limits, 1_000).unwrap();
    assert_eq!((report.rows_deleted, report.captures_deleted), (3, 3));
    assert_eq!(crate::llm::store::capture_bytes_total(&c).unwrap(), 20);
}

#[test]
fn pruning_never_touches_the_rollup() {
    let c = connection();
    let rollup = delta("2026-11-07", "p1", Some("timeout"));
    for _ in 0..4 {
        insert_log(&c, &write(&entry("m"), None, Some(&rollup))).unwrap();
    }
    let snapshot = |c: &Connection| -> (i64, i64) {
        c.query_row(
            "SELECT (SELECT requests FROM llm_usage_daily), (SELECT count FROM llm_usage_failures)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    let before = snapshot(&c);
    assert_eq!(before, (4, 4));
    let limits = PruneLimits {
        max_rows: 1,
        per_source_rows: 0,
        max_days: 1,
        max_capture_days: 1,
        max_capture_bytes: 0,
    };
    prune_log(&c, &limits, i64::MAX / 4).unwrap();
    assert_eq!(rows(&c), 0);
    assert_eq!(snapshot(&c), before);
}

#[test]
fn only_the_four_sources_are_accepted() {
    let c = connection();
    for source in LOG_SOURCES {
        let mut row = entry("m");
        row.source = source.into();
        insert_log(&c, &write(&row, None, None)).unwrap();
    }
    for bad in ["debug", "", "Gateway", "chat"] {
        let mut row = entry("m");
        row.source = bad.into();
        let result = insert_log(&c, &write(&row, None, None));
        assert!(matches!(result, Err(AppError::Validation { .. })), "{bad}");
    }
    assert_eq!(rows(&c), 4);
}

#[test]
fn captures_older_than_their_age_limit_go_but_their_rows_stay() {
    let c = connection();
    let day = 86_400_000;
    let mut old = capture("old");
    old.ts_ms = day;
    let mut fresh = capture("fresh");
    fresh.ts_ms = 9 * day;
    let old_id = insert_log(&c, &write(&entry("a"), Some(&old), None)).unwrap();
    let fresh_id = insert_log(&c, &write(&entry("b"), Some(&fresh), None)).unwrap();
    let limits = PruneLimits {
        max_capture_days: 7,
        ..no_prune_limits()
    };
    // now = day 10: the first capture is 9 days old, the second 1 day.
    let report = prune_log(&c, &limits, 10 * day).unwrap();
    assert_eq!((report.rows_deleted, report.captures_deleted), (0, 1));
    assert!(!get_log(&c, old_id).unwrap().unwrap().entry.has_capture);
    assert!(get_log(&c, old_id).unwrap().unwrap().capture.is_none());
    assert!(get_log(&c, fresh_id).unwrap().unwrap().entry.has_capture);
}

#[test]
fn a_row_keeps_its_capture_age_apart_from_the_row_age() {
    let c = connection();
    let day = 86_400_000;
    let mut row = entry("m");
    row.ts_ms = day;
    let mut kept = capture("x");
    kept.ts_ms = 9 * day;
    insert_log(&c, &write(&row, Some(&kept), None)).unwrap();
    let limits = PruneLimits {
        max_days: 30,
        max_capture_days: 7,
        ..no_prune_limits()
    };
    assert_eq!(
        prune_log(&c, &limits, 10 * day).unwrap(),
        PruneReport::default()
    );
}

#[test]
fn clear_reports_only_the_bytes_of_the_rows_it_deleted() {
    let c = connection();
    let capture = capture("body");
    let mut probe = entry("p");
    probe.source = "probe".into();
    insert_log(&c, &write(&probe, Some(&capture), None)).unwrap();
    insert_log(&c, &write(&entry("g"), Some(&capture), None)).unwrap();
    let filter = LogFilter {
        source: Some("probe".into()),
        ..LogFilter::default()
    };
    assert_eq!(
        clear_log(&c, &filter).unwrap(),
        ClearReport {
            rows: 1,
            capture_bytes: 1_500
        }
    );
    assert_eq!(crate::llm::store::capture_bytes_total(&c).unwrap(), 1_500);
}

#[test]
fn clearing_walks_every_batch() {
    let c = connection();
    seed(&c, 1_201, Some(10), "x");
    assert_eq!(
        clear_log(&c, &LogFilter::default()).unwrap(),
        ClearReport {
            rows: 1_201,
            capture_bytes: 12_010
        }
    );
    assert_eq!(rows(&c), 0);
}
