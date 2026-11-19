//! Tests of the usage rollups: upserts, reads, failure counts and retention.

use super::*;
use crate::llm::store::test_support::{connection, delta, entry};
use crate::llm::store::{LogWrite, insert_log};
use crate::llm::usage::local_midnight_ms;

/// Midnight UTC of `day`.
fn midnight(day: &str) -> i64 {
    local_midnight_ms(day, 0).unwrap()
}

/// Logs a request of `m1` at `ts_ms` through `insert_log`, with its rollup on the UTC day of
/// `ts_ms`: a success when `failure` is `None`.
fn log_request(c: &Connection, ts_ms: i64, provider: &str, failure: Option<&str>, latency_ms: u64) {
    let mut row = entry("m1");
    row.ts_ms = ts_ms;
    row.latency_ms = latency_ms;
    insert_log(
        c,
        &LogWrite {
            entry: &row,
            capture: None,
            rollup: Some(&delta(&local_day(ts_ms, 0), provider, failure)),
        },
    )
    .unwrap();
}

fn row_count(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT COUNT(*) FROM llm_usage_daily", [], |r| r.get(0))
        .unwrap()
}

fn query(from_day: &str, to_day: &str) -> UsageQuery {
    // Midnight UTC of `from_day` to the end of `to_day`.
    let start = midnight(from_day);
    let end = midnight(to_day) + 86_400_000;
    UsageQuery {
        provider_id: None,
        key_id: None,
        client_id: None,
        from_ms: start,
        to_ms: end,
        group_by: Vec::new(),
        sources: Vec::new(),
        tz_offset_minutes: 0,
        bucket_minutes: None,
    }
}

#[test]
fn counters_add_up_in_one_row() {
    let c = connection();
    upsert_rollup(&c, &delta("2026-11-07", "p1", None)).unwrap();
    upsert_rollup(&c, &delta("2026-11-07", "p1", Some("timeout"))).unwrap();
    assert_eq!(row_count(&c), 1);
    let rows = usage_rows(&c, &query("2026-11-07", "2026-11-07")).unwrap();
    let row = &rows[0];
    assert_eq!((row.requests, row.ok_requests, row.failed()), (2, 1, 1));
    assert_eq!((row.input_tokens, row.output_tokens), (20, 10));
    assert_eq!((row.cached_tokens, row.cache_write_tokens), (4, 2));
    assert_eq!((row.reasoning_tokens, row.cost_micro), (6, 200));
}

#[test]
fn alias_currency_and_day_split_rows() {
    let c = connection();
    let base = delta("2026-11-07", "p1", None);
    upsert_rollup(&c, &base).unwrap();
    upsert_rollup(
        &c,
        &RollupDelta {
            alias: "fast".into(),
            ..base.clone()
        },
    )
    .unwrap();
    upsert_rollup(
        &c,
        &RollupDelta {
            currency: "CNY".into(),
            ..base.clone()
        },
    )
    .unwrap();
    upsert_rollup(
        &c,
        &RollupDelta {
            currency: String::new(),
            ..base.clone()
        },
    )
    .unwrap();
    upsert_rollup(
        &c,
        &RollupDelta {
            day: "2026-11-08".into(),
            ..base
        },
    )
    .unwrap();
    assert_eq!(row_count(&c), 5);
}

#[test]
fn failure_counters_are_exact_per_kind() {
    let c = connection();
    for kind in ["timeout", "timeout", "auth"] {
        upsert_rollup(&c, &delta("2026-11-07", "p1", Some(kind))).unwrap();
    }
    upsert_rollup(&c, &delta("2026-11-07", "p1", None)).unwrap();
    let counts = failure_counts(&c, &query("2026-11-07", "2026-11-07")).unwrap();
    let by_kind: Vec<(&str, u64)> = counts.iter().map(|f| (f.kind.as_str(), f.count)).collect();
    assert_eq!(by_kind, [("auth", 1), ("timeout", 2)]);
    let other = failure_counts(&c, &query("2026-11-01", "2026-11-06")).unwrap();
    assert!(other.is_empty());
}

#[test]
fn rows_are_selected_by_window_source_provider_key_and_client() {
    let c = connection();
    let base = delta("2026-11-07", "p1", None);
    upsert_rollup(&c, &base).unwrap();
    upsert_rollup(
        &c,
        &RollupDelta {
            day: "2026-11-09".into(),
            ..base.clone()
        },
    )
    .unwrap();
    upsert_rollup(
        &c,
        &RollupDelta {
            provider_id: "p2".into(),
            key_id: "k2".into(),
            client_id: "cl_1".into(),
            ..base.clone()
        },
    )
    .unwrap();
    upsert_rollup(
        &c,
        &RollupDelta {
            source: "probe".into(),
            ..base.clone()
        },
    )
    .unwrap();
    upsert_rollup(
        &c,
        &RollupDelta {
            source: "agent".into(),
            ..base
        },
    )
    .unwrap();
    let count = |q: &UsageQuery| usage_rows(&c, q).unwrap().len();
    let window = query("2026-11-07", "2026-11-08");
    assert_eq!(
        count(&window),
        3,
        "probe rows are not listed by default, day 09 is outside"
    );
    assert_eq!(count(&query("2026-11-07", "2026-11-09")), 4);
    let mut only = window.clone();
    only.provider_id = Some("p2".into());
    assert_eq!(count(&only), 1);
    let mut only = window.clone();
    only.key_id = Some("k1".into());
    assert_eq!(count(&only), 2);
    let mut only = window.clone();
    only.client_id = Some("cl_1".into());
    assert_eq!(count(&only), 1);
    let mut probes = window.clone();
    probes.sources = vec![UsageSource::Probe];
    assert_eq!(count(&probes), 1);
    let mut empty = window;
    empty.to_ms = empty.from_ms;
    assert_eq!(count(&empty), 0);
}

#[test]
fn today_reads_the_gateway_rows_of_the_local_day() {
    let c = connection();
    let base = delta("2026-11-07", "p1", None);
    upsert_rollup(&c, &base).unwrap();
    upsert_rollup(
        &c,
        &RollupDelta {
            source: "agent".into(),
            ..base.clone()
        },
    )
    .unwrap();
    upsert_rollup(
        &c,
        &RollupDelta {
            day: "2026-11-06".into(),
            ..base
        },
    )
    .unwrap();
    let noon = midnight("2026-11-07") + 43_200_000;
    let rows = today_gateway_rows(&c, noon, 0).unwrap();
    assert_eq!((rows.len(), rows[0].requests), (1, 1));
    // The same instant is already 2026-11-08 in UTC+13.
    assert!(
        today_gateway_rows(&c, noon + 3_600_000 * 12, 13 * 60)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn usage_pruning_keeps_the_retention_window_with_a_day_of_slack() {
    let c = connection();
    for day in ["2026-01-01", "2026-10-07", "2026-10-08", "2026-11-07"] {
        upsert_rollup(&c, &delta(day, "p1", Some("auth"))).unwrap();
    }
    let now = midnight("2026-11-07");
    // 30 days back from 2026-11-07 is 2026-10-08; one slack day keeps 2026-10-07.
    assert_eq!(prune_usage(&c, 30, now).unwrap(), 1);
    let days: Vec<String> = c
        .prepare("SELECT day FROM llm_usage_daily ORDER BY day")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(days, ["2026-10-07", "2026-10-08", "2026-11-07"]);
    let failure_days: i64 = c
        .query_row("SELECT COUNT(*) FROM llm_usage_failures", [], |r| r.get(0))
        .unwrap();
    assert_eq!(failure_days, 3);
}

#[test]
fn successful_requests_add_their_latency_and_pruning_drops_it_with_the_row() {
    let c = connection();
    let log = |day: &str, provider: &str, failure: Option<&str>, latency_ms: u64| {
        log_request(&c, midnight(day), provider, failure, latency_ms);
    };
    log("2026-10-01", "p1", None, 900);
    log("2026-11-06", "p1", None, 300);
    log("2026-11-06", "p1", None, 500);
    // A failure is counted as a request but never timed.
    log("2026-11-06", "p1", Some("timeout"), 60_000);
    log("2026-11-07", "p2", None, 1_000);
    // A failure-only pair has nothing timed and is left out.
    log("2026-11-07", "p3", Some("auth"), 10);
    let latency = |from: &str, to: &str| usage_latency(&c, &query(from, to)).unwrap();
    let pair = |provider: &str, timed: u64, ms: u64| UsageLatency {
        provider_id: provider.into(),
        model: "m1".into(),
        timed_requests: timed,
        latency_ms: ms,
        key: Vec::new(),
    };
    assert_eq!(
        latency("2026-11-01", "2026-11-07"),
        vec![pair("p1", 2, 800), pair("p2", 1, 1_000)]
    );
    assert_eq!(
        latency("2026-10-01", "2026-11-07"),
        vec![pair("p1", 3, 1_700), pair("p2", 1, 1_000)]
    );
    // `group_by` splits each pair further, with the grouped values in `key`.
    let mut by_day = query("2026-10-01", "2026-11-07");
    by_day.group_by = vec![UsageGroup::Day];
    let keyed = |provider: &str, timed: u64, ms: u64, day: &str| UsageLatency {
        key: vec![day.to_owned()],
        ..pair(provider, timed, ms)
    };
    assert_eq!(
        usage_latency(&c, &by_day).unwrap(),
        vec![
            keyed("p1", 1, 900, "2026-10-01"),
            keyed("p1", 2, 800, "2026-11-06"),
            keyed("p2", 1, 1_000, "2026-11-07"),
        ]
    );
    let requests: i64 = c
        .query_row(
            "SELECT requests FROM llm_usage_daily WHERE day = '2026-11-06'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(requests, 3);
    // Retention keeps the window's rows (and their latency) and drops older ones whole.
    let now = midnight("2026-11-07");
    assert_eq!(prune_usage(&c, 30, now).unwrap(), 1);
    assert_eq!(
        latency("2026-10-01", "2026-11-07"),
        vec![pair("p1", 2, 800), pair("p2", 1, 1_000)]
    );
}

#[test]
fn cancellations_are_counted_apart_in_both_rollups() {
    let c = connection();
    let noon = midnight("2026-11-07") + 43_200_000;
    for failure in [None, Some("cancelled"), Some("cancelled"), Some("timeout")] {
        log_request(&c, noon, "p1", failure, 700);
    }
    let daily = query("2026-11-07", "2026-11-07");
    let bucketed = UsageQuery {
        bucket_minutes: Some(1_440),
        ..daily.clone()
    };
    for q in [daily, bucketed] {
        let rows = usage_rows(&c, &q).unwrap();
        let sum = |f: fn(&RollupRow) -> u64| rows.iter().map(f).sum::<u64>();
        assert_eq!(
            (
                sum(|r| r.requests),
                sum(RollupRow::failed),
                sum(|r| r.canceled)
            ),
            (4, 3, 2)
        );
    }
}

#[test]
fn the_minute_rollup_follows_the_daily_retention_up_to_92_days() {
    let c = connection();
    let now = midnight("2026-11-07");
    for (back_days, failure) in [
        (93, None),
        (91, Some("auth")),
        (20, None),
        (1, None),
        (1, None),
    ] {
        log_request(&c, now - back_days * MS_PER_DAY + 1_234, "p1", failure, 700);
    }
    let sums = |c: &Connection| -> (i64, i64, i64) {
        c.query_row(
            "SELECT COUNT(*), SUM(requests), SUM(timed_requests) FROM llm_usage_minute",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    // The two calls of the same minute share a row; the failure has its own and no latency.
    assert_eq!(sums(&c), (4, 5, 4));
    prune_usage(&c, 3_650, now).unwrap();
    assert_eq!(sums(&c), (3, 4, 3), "92 days at most");
    prune_usage(&c, 10, now).unwrap();
    assert_eq!(
        sums(&c),
        (1, 2, 2),
        "a 10-day retention keeps 12 days of minutes"
    );
}

#[test]
fn failures_come_from_bucketed_minutes_only() {
    let c = connection();
    let noon = midnight("2026-11-07") + 43_200_000;
    for failure in [None, Some("timeout"), Some("timeout"), Some("auth")] {
        log_request(&c, noon, "p1", failure, 700);
    }
    let daily = query("2026-11-07", "2026-11-07");
    assert!(usage_failures(&c, &daily).unwrap().is_empty());
    let bucketed = UsageQuery {
        bucket_minutes: Some(1_440),
        group_by: vec![UsageGroup::Model],
        ..daily
    };
    let failures = usage_failures(&c, &bucketed).unwrap();
    let got: Vec<(&str, u64, &[String])> = failures
        .iter()
        .map(|f| (f.kind.as_str(), f.count, f.key.as_slice()))
        .collect();
    let m1 = ["m1".to_owned()];
    assert_eq!(got, [("auth", 1, &m1[..]), ("timeout", 2, &m1[..])]);
}

#[test]
fn the_retention_preview_counts_what_pruning_deletes() {
    let now = midnight("2026-11-07");
    for (retention_days, deleted) in [(1, 4), (30, 2), (3_650, 1)] {
        let c = connection();
        for day in [
            "2016-01-01",
            "2026-01-01",
            "2026-10-07",
            "2026-10-08",
            "2026-11-05",
            "2026-11-06",
            "2026-11-07",
        ] {
            upsert_rollup(&c, &delta(day, "p1", None)).unwrap();
        }
        let (rows, bytes) = usage_retention_preview(&c, retention_days, now).unwrap();
        assert!(bytes > 0);
        assert_eq!(rows, deleted, "{retention_days} days");
        assert_eq!(prune_usage(&c, retention_days, now).unwrap(), rows);
    }
}

#[test]
fn latency_modes_split_streamed_calls_and_sum_first_token_times_of_successes_only() {
    let c = connection();
    let day = midnight("2026-11-06");
    let log = |stream: bool,
               ttft: Option<u64>,
               latency_ms: u64,
               status: u16,
               failure: Option<FailureKind>| {
        let mut row = entry("m1");
        row.ts_ms = day + 1_000;
        row.provider_id = Some("p1".into());
        row.key_id = Some("k1".into());
        row.upstream_model = Some("m1-upstream".into());
        row.stream = stream;
        row.ttft_ms = ttft;
        row.latency_ms = latency_ms;
        row.status = status;
        row.failure_kind = failure;
        insert_log(
            &c,
            &LogWrite {
                entry: &row,
                capture: None,
                rollup: None,
            },
        )
        .unwrap();
    };
    log(true, Some(200), 4_000, 200, None);
    log(true, Some(400), 6_000, 200, None);
    // A streamed success without a first-token time is timed end to end only.
    log(true, None, 5_000, 200, None);
    log(false, None, 1_500, 200, None);
    // Not successes: an error status and a stream cut short.
    log(false, None, 9_000, 502, None);
    log(true, Some(100), 9_000, 200, Some(FailureKind::Truncated));
    let mode =
        |stream: bool, timed: u64, ms: u64, ttft_requests: u64, ttft_ms: u64| UsageLatencyMode {
            source: "gateway".into(),
            provider_id: "p1".into(),
            key_id: "k1".into(),
            model: "m1-upstream".into(),
            client_id: String::new(),
            stream,
            timed_requests: timed,
            latency_ms: ms,
            ttft_requests,
            ttft_ms,
        };
    assert_eq!(
        usage_latency_modes(&c, &query("2026-11-06", "2026-11-06")).unwrap(),
        vec![mode(false, 1, 1_500, 0, 0), mode(true, 3, 15_000, 2, 600)]
    );
    // Outside the window, or another provider: nothing.
    assert!(
        usage_latency_modes(&c, &query("2026-11-07", "2026-11-07"))
            .unwrap()
            .is_empty()
    );
    let mut other = query("2026-11-06", "2026-11-06");
    other.provider_id = Some("p2".into());
    assert!(usage_latency_modes(&c, &other).unwrap().is_empty());
}

#[test]
fn unrouted_rows_are_excluded_from_all_usage_reads() {
    let c = connection();
    let noon = midnight("2026-11-07") + 43_200_000;
    // Simulate legacy unrouted rollup rows alongside a real routed provider row.
    log_request(&c, noon, "", Some("auth"), 100);
    log_request(&c, noon, "", None, 200);
    log_request(&c, noon, "p1", None, 500);
    log_request(&c, noon, "p1", Some("timeout"), 1_000);
    let mut unrouted_log = entry("m1");
    unrouted_log.ts_ms = noon;
    unrouted_log.provider_id = None;
    unrouted_log.latency_ms = 300;
    insert_log(
        &c,
        &LogWrite {
            entry: &unrouted_log,
            capture: None,
            rollup: None,
        },
    )
    .unwrap();

    let daily = query("2026-11-07", "2026-11-07");
    let bucketed = UsageQuery {
        bucket_minutes: Some(1_440),
        ..daily.clone()
    };
    for q in [&daily, &bucketed] {
        let rows = usage_rows(&c, q).unwrap();
        assert!(rows.iter().all(|r| r.provider_id == "p1"));
        assert_eq!(rows.iter().map(|r| r.requests).sum::<u64>(), 2);
        let lat = usage_latency(&c, q).unwrap();
        assert_eq!(lat.len(), 1);
        assert_eq!(lat[0].provider_id, "p1");
        let fc = failure_counts(&c, q).unwrap();
        assert_eq!(fc.len(), 1);
        assert_eq!(
            (fc[0].provider_id.as_str(), fc[0].kind.as_str()),
            ("p1", "timeout")
        );
    }
    let uf = usage_failures(&c, &bucketed).unwrap();
    assert_eq!(uf.len(), 1);
    assert_eq!(uf[0].kind, "timeout");
    let today = today_gateway_rows(&c, noon, 0).unwrap();
    assert!(today.iter().all(|r| r.provider_id == "p1"));
    assert_eq!(today.iter().map(|r| r.requests).sum::<u64>(), 2);
    assert!(
        usage_latency_modes(&c, &daily)
            .unwrap()
            .iter()
            .all(|m| !m.provider_id.is_empty())
    );
}
