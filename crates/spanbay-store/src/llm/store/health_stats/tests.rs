use rusqlite::{Connection, params};

use super::*;
use crate::llm::store::test_support::connection;

const NOW: i64 = 1_800_000_000_000;
const MINUTE: i64 = 60_000;

/// One `llm_request_log` row; the defaults are a successful gateway request of `p1` a
/// minute ago.
#[derive(Clone, Copy)]
struct LogRow<'a> {
    provider: &'a str,
    ts_ms: i64,
    status: i64,
    outcome: &'a str,
    source: &'a str,
    stream: bool,
    ttft_ms: Option<i64>,
    failure_kind: Option<&'a str>,
    timing_json: Option<&'a str>,
}

impl Default for LogRow<'_> {
    fn default() -> Self {
        Self {
            provider: "p1",
            ts_ms: NOW - MINUTE,
            status: 200,
            outcome: "success",
            source: "gateway",
            stream: false,
            ttft_ms: None,
            failure_kind: None,
            timing_json: None,
        }
    }
}

fn insert(c: &Connection, row: LogRow<'_>) {
    c.execute(
        "INSERT INTO llm_request_log (ts_ms, protocol_in, requested_model, provider_id, \
             status, outcome, stream, ttft_ms, source, failure_kind, timing_json) \
         VALUES (?1, 'openai_chat', 'm1', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            row.ts_ms,
            row.provider,
            row.status,
            row.outcome,
            i64::from(row.stream),
            row.ttft_ms,
            row.source,
            row.failure_kind,
            row.timing_json,
        ],
    )
    .unwrap();
}

#[test]
fn counts_ttfts_and_429_buckets_cover_the_window_of_forwarded_requests() {
    let c = connection();
    let rate_limited = LogRow {
        status: 429,
        outcome: "failure",
        ..LogRow::default()
    };
    // The first instant of the window counts as a request, but its 429 is 24 hours ago,
    // past the last bucket, and is dropped from the timeline.
    insert(
        &c,
        LogRow {
            ts_ms: NOW - HEALTH_WINDOW_MS,
            ..rate_limited
        },
    );
    insert(
        &c,
        LogRow {
            ts_ms: NOW - HEALTH_WINDOW_MS - 1,
            ..LogRow::default()
        },
    );
    insert(
        &c,
        LogRow {
            stream: true,
            ttft_ms: Some(120),
            ..LogRow::default()
        },
    );
    // Successes that do not count as succeeded: a 4xx status, a truncated answer.
    insert(
        &c,
        LogRow {
            status: 400,
            ..LogRow::default()
        },
    );
    insert(
        &c,
        LogRow {
            failure_kind: Some("truncated"),
            ..LogRow::default()
        },
    );
    insert(
        &c,
        LogRow {
            ts_ms: NOW - 30 * MINUTE,
            ..rate_limited
        },
    );
    insert(
        &c,
        LogRow {
            ts_ms: NOW - 90 * MINUTE,
            ..rate_limited
        },
    );
    // Neither a cancel, a probe nor another provider is a forwarded request of `p1`.
    insert(
        &c,
        LogRow {
            outcome: "canceled",
            ..LogRow::default()
        },
    );
    insert(
        &c,
        LogRow {
            source: "probe",
            ts_ms: NOW - 30 * MINUTE,
            ..rate_limited
        },
    );
    insert(
        &c,
        LogRow {
            source: "probe",
            stream: true,
            ttft_ms: Some(999),
            ..LogRow::default()
        },
    );
    insert(
        &c,
        LogRow {
            provider: "p2",
            ..rate_limited
        },
    );
    let stats = provider_log_stats(&c, "p1", NOW).unwrap();
    assert_eq!((stats.requests, stats.succeeded), (6, 1));
    assert_eq!(stats.ttft_ms, [120]);
    let mut buckets = [0_u32; HEALTH_HOURS];
    buckets[0] = 1;
    buckets[1] = 1;
    assert_eq!(stats.rate_limited, buckets);
    assert!(stats.timings.is_empty());
}

#[test]
fn timings_are_the_newest_successes_cut_before_unparsable_ones_are_dropped() {
    let c = connection();
    // Successes 1..=21 minutes ago, `queue_ms` = minutes ago; the newest is not JSON.
    for minutes_ago in 1..=21_u64 {
        let text = if minutes_ago == 1 {
            "not json".to_owned()
        } else {
            serde_json::to_string(&TimingJson {
                queue_ms: minutes_ago,
                attempts: Vec::new(),
            })
            .unwrap()
        };
        insert(
            &c,
            LogRow {
                ts_ms: NOW - i64::try_from(minutes_ago).unwrap() * MINUTE,
                timing_json: Some(&text),
                ..LogRow::default()
            },
        );
    }
    // A failure's timing is never a sample.
    insert(
        &c,
        LogRow {
            ts_ms: NOW - 1_000,
            outcome: "failure",
            status: 500,
            timing_json: Some(r#"{"queue_ms":999,"attempts":[]}"#),
            ..LogRow::default()
        },
    );
    let stats = provider_log_stats(&c, "p1", NOW).unwrap();
    // LIMIT 20 keeps minutes 1..=20, then the unparsable one is dropped: 19, newest first.
    let queued: Vec<u64> = stats.timings.iter().map(|t| t.queue_ms).collect();
    assert_eq!(queued, (2..=20).collect::<Vec<u64>>());
}

#[test]
fn every_read_uses_the_provider_index() {
    let c = connection();
    for (sql, with_now) in [
        (COUNTS_SQL, false),
        (TTFT_SQL, false),
        (RATE_LIMITED_SQL, true),
        (TIMING_SQL, true),
    ] {
        let mut statement = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let detail = |row: &rusqlite::Row<'_>| row.get::<_, String>(3);
        let plan: Vec<String> = if with_now {
            statement.query_map(params!["p1", 0_i64, 0_i64], detail)
        } else {
            statement.query_map(params!["p1", 0_i64], detail)
        }
        .unwrap()
        .map(Result::unwrap)
        .collect();
        let text = plan.join(" | ");
        assert!(
            text.contains("idx_llm_request_log_provider") && !text.contains("SCAN llm_request_log"),
            "{sql}: {text}"
        );
    }
}
