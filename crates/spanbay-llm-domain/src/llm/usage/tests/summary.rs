//! Usage queries and the summary fold.

use super::ms;
use crate::llm::usage::{
    FailureCount, RollupRow, SummaryContext, UNPRICED_CURRENCY, UsageGroup, UsageQuery, UsageRow,
    UsageSource, add_days, summarize,
};

fn query(from: &str, to: &str) -> UsageQuery {
    UsageQuery {
        provider_id: None,
        key_id: None,
        client_id: None,
        from_ms: ms(from, 0),
        to_ms: ms(to, 0),
        group_by: Vec::new(),
        sources: Vec::new(),
        tz_offset_minutes: 0,
        bucket_minutes: None,
    }
}

/// The week the summary tests read: 2026-11-01 to 2026-11-07.
fn week() -> UsageQuery {
    query("2026-11-01", "2026-11-08")
}

fn row(day: &str, source: &str, provider: &str, model: &str, requests: u64, ok: u64) -> RollupRow {
    RollupRow {
        day: day.to_owned(),
        source: source.to_owned(),
        provider_id: provider.to_owned(),
        key_id: "k".to_owned(),
        model: model.to_owned(),
        currency: "USD".to_owned(),
        requests,
        ok_requests: ok,
        input_tokens: requests * 100,
        output_tokens: requests * 10,
        cost_micro: i64::try_from(requests).unwrap() * 1_000,
        ..RollupRow::default()
    }
}

fn ctx(now: &str) -> SummaryContext {
    SummaryContext {
        now_ms: ms(now, 0) + 3_600_000,
        ..SummaryContext::default()
    }
}

fn fail(day: &str, src: &str, prov: &str, m: &str, kind: &str, count: u64) -> FailureCount {
    FailureCount {
        day: day.to_owned(),
        source: src.to_owned(),
        provider_id: prov.to_owned(),
        model: m.to_owned(),
        kind: kind.to_owned(),
        count,
    }
}

#[test]
fn usage_sources_parse_their_own_text() {
    for src in [
        UsageSource::Gateway,
        UsageSource::Agent,
        UsageSource::Probe,
        UsageSource::Api,
    ] {
        assert_eq!(UsageSource::parse(src.as_str()), Some(src));
    }
    assert_eq!(UsageSource::parse("chat"), None);
}

#[test]
fn a_query_counts_every_source_but_probes_by_default() {
    let w = week();
    assert_eq!(w.effective_sources().len(), 3);
    assert!(!w.effective_sources().contains(&UsageSource::Probe));
    let deser: UsageQuery = serde_json::from_str(r#"{"from_ms":0,"to_ms":1}"#).unwrap();
    assert!(deser.group_by.is_empty() && deser.sources.is_empty());
}

#[test]
fn the_day_window_is_the_first_and_last_day_of_the_range() {
    assert_eq!(
        week().day_window(),
        Some(("2026-11-01".to_owned(), "2026-11-07".to_owned()))
    );
    assert_eq!(query("2026-11-08", "2026-11-01").day_window(), None);
}

#[test]
fn the_day_window_keeps_the_newest_days_when_capped() {
    let long = query("2000-01-01", "2030-01-01");
    let (first, last) = long.day_window().unwrap();
    assert_eq!(last, "2029-12-31");
    assert_eq!(add_days(&first, 3_651).as_deref(), Some("2029-12-31"));
    assert_eq!(long.slots().first(), Some(&first));
}

#[test]
fn summary_ranks_models_by_requests_then_tokens() {
    let rows = [
        row("2026-11-02", "gateway", "p", "small", 10, 9),
        row("2026-11-02", "gateway", "p", "big", 30, 30),
        row("2026-11-03", "gateway", "q", "big", 20, 20),
        row("2026-11-03", "gateway", "p", "mid", 20, 20),
    ];
    let sum = summarize(&rows, &[], &week(), &ctx("2026-11-07"));
    assert_eq!(
        (
            sum.series.len(),
            sum.series[1].usage.requests,
            sum.series[1].usage.failed
        ),
        (7, 40, 1)
    );
    assert_eq!(
        (sum.totals.requests, sum.totals.cost_micro["USD"]),
        (80, 80_000)
    );
    let order: Vec<_> = sum
        .by_model
        .iter()
        .map(|m| (m.provider_id.as_str(), m.model.as_str()))
        .collect();
    assert_eq!(
        order,
        [("p", "big"), ("p", "mid"), ("q", "big"), ("p", "small")]
    );
    assert!((sum.by_model[0].share_requests - 0.375).abs() < 1e-12);
    assert!(sum.groups.is_empty());
}

#[test]
fn costs_stay_per_currency_and_free_rows_are_never_unpriced() {
    let mut cny = row("2026-11-02", "gateway", "deepseek", "chat", 4, 4);
    cny.currency = "CNY".to_owned();
    cny.cost_micro = 5_000_000;
    let mut unpriced = row("2026-11-02", "gateway", "p", "mystery", 3, 3);
    unpriced.currency = UNPRICED_CURRENCY.to_owned();
    unpriced.cost_micro = 0;
    let mut free_row = row("2026-11-02", "gateway", "ollama", "llama", 6, 6);
    free_row.currency = UNPRICED_CURRENCY.to_owned();
    free_row.cost_micro = 0;
    let mut free_ctx = ctx("2026-11-07");
    free_ctx.free_providers.insert("ollama".to_owned());
    let mixed = summarize(
        &[
            cny,
            row("2026-11-02", "gateway", "openai", "gpt", 2, 2),
            unpriced,
            free_row,
        ],
        &[],
        &week(),
        &free_ctx,
    );
    assert_eq!(
        (
            mixed.totals.cost_micro["CNY"],
            mixed.totals.cost_micro["USD"]
        ),
        (5_000_000, 2_000)
    );
    assert_eq!(
        (mixed.models_without_price, mixed.totals.unpriced_requests),
        (vec!["mystery".to_owned()], 3)
    );
    let m_free = mixed.by_model.iter().find(|m| m.model == "llama").unwrap();
    assert!(m_free.free && !m_free.price_missing);
}

#[test]
fn unpriced_models_are_listed_once_and_sorted() {
    let unpriced = |provider: &str, model: &str| RollupRow {
        currency: UNPRICED_CURRENCY.to_owned(),
        cost_micro: 0,
        ..row("2026-11-02", "gateway", provider, model, 1, 1)
    };
    let sum = summarize(
        &[
            unpriced("p", "zeta"),
            unpriced("q", "alpha"),
            unpriced("p", "alpha"),
            row("2026-11-02", "gateway", "p", "priced", 1, 1),
        ],
        &[],
        &week(),
        &ctx("2026-11-07"),
    );
    assert_eq!(sum.models_without_price, ["alpha", "zeta"]);
    assert_eq!(sum.totals.unpriced_requests, 3);
}

#[test]
fn sources_scope_and_range_filter_the_rows() {
    let w = week();
    let mut a = row("2026-11-02", "gateway", "p", "m", 4, 4);
    a.client_id = "cc".to_owned();
    a.alias = "claude-code".to_owned();
    let mut b = row("2026-11-02", "probe", "p", "m", 3, 3);
    b.key_id = "k2".to_owned();
    let c = row("2026-10-31", "gateway", "p", "m", 100, 100);
    assert_eq!(
        summarize(&[a.clone(), b.clone(), c], &[], &w, &ctx("2026-11-07"))
            .totals
            .requests,
        4
    );
    assert_eq!(
        summarize(
            &[a, b],
            &[],
            &UsageQuery {
                sources: vec![UsageSource::Gateway, UsageSource::Probe],
                key_id: Some("k2".into()),
                ..w
            },
            &ctx("2026-11-07")
        )
        .totals
        .requests,
        3
    );
    assert!(
        summarize(
            &[row("2026-11-02", "gateway", "p", "m", 5, 5)],
            &[],
            &query("2026-11-08", "2026-11-01"),
            &ctx("2026-11-07")
        )
        .series
        .is_empty()
    );
}

#[test]
fn group_keys_follow_the_order_of_group_by() {
    let mut a = row("2026-11-02", "gateway", "p", "m", 4, 4);
    a.client_id = "cc".to_owned();
    a.alias = "claude-code".to_owned();
    let grouped = summarize(
        &[a],
        &[],
        &UsageQuery {
            group_by: vec![UsageGroup::Day, UsageGroup::Client, UsageGroup::Alias],
            ..week()
        },
        &ctx("2026-11-07"),
    );
    assert_eq!(grouped.groups[0].key, ["2026-11-02", "cc", "claude-code"]);
}

#[test]
fn groups_are_ordered_by_key_whatever_the_row_order() {
    let rows = [
        row("2026-11-02", "gateway", "q", "a", 1, 1),
        row("2026-11-02", "gateway", "p", "b", 1, 1),
        row("2026-11-03", "gateway", "q", "a", 1, 1),
        row("2026-11-03", "gateway", "p", "a", 1, 1),
        row("2026-11-04", "gateway", "p", "b", 1, 1),
    ];
    let sum = summarize(
        &rows,
        &[],
        &UsageQuery {
            group_by: vec![UsageGroup::Provider, UsageGroup::Model],
            ..week()
        },
        &ctx("2026-11-07"),
    );
    let keys: Vec<(&[String], u64)> = sum
        .groups
        .iter()
        .map(|bucket| (bucket.key.as_slice(), bucket.usage.requests))
        .collect();
    assert_eq!(
        keys,
        [
            (&["p".to_owned(), "a".to_owned()][..], 1),
            (&["p".to_owned(), "b".to_owned()][..], 2),
            (&["q".to_owned(), "a".to_owned()][..], 2),
        ]
    );
}

#[test]
fn top_failures_come_from_the_counters_of_the_range_and_of_today() {
    let r_today = row("2026-11-07", "gateway", "deepseek", "pro", 1_284, 1_247);
    let failures = [
        fail("2026-11-07", "gateway", "deepseek", "pro", "rate_limit", 37),
        fail("2026-11-07", "agent", "deepseek", "pro", "auth", 90),
        fail("2026-11-06", "gateway", "kimi", "k2", "timeout", 99),
    ];
    let s_today = summarize(
        &[
            r_today,
            row("2026-11-07", "agent", "deepseek", "pro", 50, 50),
        ],
        &failures,
        &week(),
        &ctx("2026-11-07"),
    );
    assert_eq!(
        (
            s_today.today.day.as_str(),
            s_today.today.usage.requests,
            s_today.today.usage.failed
        ),
        ("2026-11-07", 1_284, 37)
    );
    assert_eq!(
        s_today.today.top_failure.as_ref().unwrap().kind,
        "rate_limit"
    );
    assert_eq!(s_today.top_failure.as_ref().unwrap().kind, "timeout");
}

#[test]
fn today_follows_the_offset() {
    let w = week();
    let mut tz_ctx = ctx("2026-11-07");
    tz_ctx.now_ms = ms("2026-11-07", 0) + 30 * 60_000;
    let tz_rows = [
        row("2026-11-06", "gateway", "p", "m", 3, 3),
        row("2026-11-07", "gateway", "p", "m", 8, 8),
    ];
    assert_eq!(
        summarize(
            &tz_rows,
            &[],
            &UsageQuery {
                tz_offset_minutes: 480,
                ..w.clone()
            },
            &tz_ctx
        )
        .today
        .usage
        .requests,
        8
    );
    assert_eq!(
        summarize(
            &tz_rows,
            &[],
            &UsageQuery {
                tz_offset_minutes: -300,
                ..w
            },
            &tz_ctx
        )
        .today
        .usage
        .requests,
        3
    );
}

#[test]
fn cancellations_are_neither_failures_nor_on_the_wire_when_absent() {
    // 10 requests, 6 ok, 4 not ok of which 3 cancelled -> failed 1, canceled 3.
    let mut cancelled = row("2026-11-02", "gateway", "p", "m", 10, 6);
    cancelled.canceled = 3;
    let sum = summarize(
        &[cancelled, row("2026-11-03", "gateway", "p", "m", 5, 5)],
        &[],
        &week(),
        &ctx("2026-11-07"),
    );
    assert_eq!(
        (sum.totals.requests, sum.totals.failed, sum.totals.canceled),
        (15, 1, 3)
    );
    let wire = |row: &UsageRow| serde_json::to_value(row).unwrap();
    assert_eq!(wire(&sum.totals)["canceled"], 3);
    assert!(wire(&sum.series[2].usage).get("canceled").is_none());
}

#[test]
fn unrouted_rows_and_failures_are_excluded_from_summary_and_today() {
    let mut unrouted = row("2026-11-07", "gateway", "", "", 5, 0);
    unrouted.currency = UNPRICED_CURRENCY.to_owned();
    unrouted.cost_micro = 0;
    let routed = row("2026-11-07", "gateway", "p", "m", 2, 2);
    let failures = [
        fail("2026-11-07", "gateway", "", "", "auth", 5),
        fail("2026-11-07", "gateway", "p", "m", "timeout", 1),
    ];
    let sum = summarize(
        &[unrouted, routed],
        &failures,
        &UsageQuery {
            group_by: vec![UsageGroup::Provider],
            ..week()
        },
        &ctx("2026-11-07"),
    );
    assert_eq!((sum.totals.requests, sum.totals.failed), (2, 0));
    assert_eq!(sum.today.usage.requests, 2);
    assert!(sum.models_without_price.is_empty());
    assert_eq!(sum.groups.len(), 1);
    assert_eq!(sum.groups[0].key, ["p"]);
    assert_eq!(sum.top_failure.as_ref().unwrap().kind, "timeout");
    assert_eq!(sum.today.top_failure.as_ref().unwrap().kind, "timeout");
}
