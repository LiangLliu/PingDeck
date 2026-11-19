//! Rollup deltas, row arithmetic and the success rule.

use super::{ms, price};
use crate::llm::failure::FailureKind;
use crate::llm::usage::{
    Cost, CostUsage, RequestOutcome, RollupDelta, RollupInput, RollupRow, UNPRICED_CURRENCY,
    UsageSource, cost, delta_for,
};

fn input<'a>() -> RollupInput<'a> {
    RollupInput {
        ts_ms: ms("2026-11-07", 480) + 3_600_000,
        tz_offset_minutes: 480,
        source: UsageSource::Gateway,
        provider_id: "deepseek",
        key_id: "key-1",
        model: "deepseek-chat",
        client_id: "cc",
        alias: "claude-code",
        final_status: Some(200),
        outcome: RequestOutcome::Success,
        failure_kind: None,
        usage: None,
        cost: None,
        price_currency: None,
    }
}

fn ok_of(edit: impl FnOnce(&mut RollupInput<'static>)) -> bool {
    let mut req = input();
    edit(&mut req);
    let delta = delta_for(&req);
    assert_eq!(delta.ok_requests == 1, req.succeeded());
    assert_eq!(delta.failure_kind.is_none(), req.succeeded());
    req.succeeded()
}

fn counted_usage() -> CostUsage {
    CostUsage {
        input_tokens: 100,
        output_tokens: 20,
        cached_tokens: 30,
        cache_write_tokens: 5,
        reasoning_tokens: 8,
        ..CostUsage::default()
    }
}

/// The cost of [`counted_usage`] at a CNY price.
fn cny_cost() -> Cost {
    let mut p = price(1.0, 2.0);
    p.currency = "CNY".to_owned();
    cost(&counted_usage(), &p)
}

/// The delta of a priced, counted success.
fn priced_delta() -> RollupDelta {
    let (u, c) = (counted_usage(), cny_cost());
    delta_for(&RollupInput {
        usage: Some(&u),
        cost: Some(&c),
        ..input()
    })
}

#[test]
fn a_delta_carries_the_row_key_and_the_counts() {
    let c = cny_cost();
    let d = priced_delta();
    assert_eq!(
        (
            d.day.as_str(),
            d.source.as_str(),
            d.provider_id.as_str(),
            d.key_id.as_str(),
            d.model.as_str(),
            d.client_id.as_str(),
            d.alias.as_str(),
            d.currency.as_str()
        ),
        (
            "2026-11-07",
            "gateway",
            "deepseek",
            "key-1",
            "deepseek-chat",
            "cc",
            "claude-code",
            "CNY"
        )
    );
    assert_eq!(
        (d.requests, d.ok_requests, d.cost_micro, &d.failure_kind),
        (1, 1, c.micro, &None)
    );
    assert_eq!(
        (
            d.input_tokens,
            d.output_tokens,
            d.cached_tokens,
            d.cache_write_tokens,
            d.reasoning_tokens
        ),
        (100, 20, 30, 5, 8)
    );
}

#[test]
fn the_delta_day_follows_the_offset() {
    assert_eq!(
        delta_for(&RollupInput {
            tz_offset_minutes: 0,
            ..input()
        })
        .day,
        "2026-11-06"
    );
}

#[test]
fn the_delta_currency_is_the_cost_s_then_the_price_s_else_unpriced() {
    let c = cny_cost();
    assert_eq!(delta_for(&input()).currency, UNPRICED_CURRENCY);
    assert_eq!(
        delta_for(&RollupInput {
            price_currency: Some("CNY"),
            ..input()
        })
        .currency,
        "CNY"
    );
    assert_eq!(
        delta_for(&RollupInput {
            cost: Some(&c),
            price_currency: Some("EUR"),
            ..input()
        })
        .currency,
        "CNY"
    );
}

#[test]
fn rows_add_deltas_and_count_failures() {
    let d = priced_delta();
    let rl = delta_for(&RollupInput {
        final_status: Some(429),
        outcome: RequestOutcome::Failure,
        failure_kind: Some(FailureKind::RateLimit),
        ..input()
    });
    assert_eq!(
        (rl.ok_requests, rl.failure_kind.as_deref()),
        (0, Some("rate_limit"))
    );
    let to = delta_for(&RollupInput {
        final_status: Some(504),
        outcome: RequestOutcome::Failure,
        failure_kind: Some(FailureKind::Timeout),
        ..input()
    });
    let mut row = RollupRow::from(&d);
    row.apply(&rl);
    row.apply(&to);
    row.apply(&d);
    assert_eq!((row.requests, row.ok_requests, row.failed()), (4, 2, 2));
}

#[test]
fn row_counters_saturate() {
    let mut sat = RollupRow {
        requests: u64::MAX,
        cost_micro: i64::MAX,
        ..RollupRow::default()
    };
    sat.apply(&delta_for(&input()));
    assert_eq!((sat.requests, sat.cost_micro), (u64::MAX, i64::MAX));
}

#[test]
fn success_needs_a_success_outcome_a_good_status_and_a_whole_answer() {
    assert!(ok_of(|_| {}));
    assert!(ok_of(|r| r.final_status = Some(399)));
    assert!(ok_of(|r| r.final_status = None));
    // A stream that broke after its answer started (status 200, outcome failure) and a
    // truncated answer are failures, whatever attempts came before.
    assert!(!ok_of(|r| r.outcome = RequestOutcome::Failure));
    assert!(!ok_of(|r| r.failure_kind = Some(FailureKind::Truncated)));
    for st in [400, 429, 502] {
        assert!(!ok_of(|r| r.final_status = Some(st)));
    }
    assert!(!ok_of(|r| r.outcome = RequestOutcome::Canceled));
}

#[test]
fn a_failed_delta_names_its_kind() {
    let canceled = delta_for(&RollupInput {
        outcome: RequestOutcome::Canceled,
        final_status: None,
        failure_kind: Some(FailureKind::Network),
        ..input()
    });
    assert_eq!(canceled.failure_kind.as_deref(), Some("cancelled"));
    assert_eq!(
        delta_for(&RollupInput {
            final_status: Some(418),
            ..input()
        })
        .failure_kind
        .as_deref(),
        Some("unknown")
    );
}

#[test]
fn log_outcomes_read_unknown_text_as_a_failure() {
    assert_eq!(RequestOutcome::from_log("success"), RequestOutcome::Success);
    assert_eq!(
        RequestOutcome::from_log("canceled"),
        RequestOutcome::Canceled
    );
    assert_eq!(RequestOutcome::from_log("failure"), RequestOutcome::Failure);
    assert_eq!(RequestOutcome::from_log("weird"), RequestOutcome::Failure);
}

#[test]
fn an_outcome_s_text_is_its_serde_name_and_reads_back() {
    for outcome in [
        RequestOutcome::Success,
        RequestOutcome::Failure,
        RequestOutcome::Canceled,
    ] {
        assert_eq!(
            serde_json::to_value(outcome).unwrap(),
            serde_json::json!(outcome.as_str())
        );
        assert_eq!(RequestOutcome::from_log(outcome.as_str()), outcome);
    }
}
