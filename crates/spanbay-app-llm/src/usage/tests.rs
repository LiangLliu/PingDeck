//! Tests of the usage and price use-cases: the two-currency series, local days at UTC+8 and
//! UTC-8, "Today" against the log, free and unpriced models, and price overrides.

use super::super::test_support::{add_key, add_provider, events_of, open, provider_template};
use super::*;
use crate::app::current_time_ms;
use crate::llm::failure::FailureKind;
use crate::llm::store::{
    LogFilter, LogWrite, RequestLogEntry, count_log, insert_log, upsert_rollup,
};
use crate::llm::usage::{
    PriceSource, RequestOutcome, RollupDelta, RollupInput, UsageGroup, UsageSource, delta_for,
    local_midnight_ms,
};
use crate::{llm_catalog_search_impl, llm_provider_save_impl};

const DAY: i64 = 86_400_000;

/// Where the price in force for the model comes from (the resolver every cost caller uses).
fn effective_source(state: &AppState, provider_id: &str, model_id: &str) -> Option<PriceSource> {
    store::effective_price(state.store().connection(), provider_id, model_id)
        .unwrap()
        .map(|price| price.source)
}

fn range(first_day: &str, days: i64, tz: i16) -> UsageQuery {
    let from_ms = local_midnight_ms(first_day, tz).expect("day");
    UsageQuery {
        provider_id: None,
        key_id: None,
        client_id: None,
        from_ms,
        to_ms: from_ms + days * DAY,
        group_by: Vec::new(),
        sources: Vec::new(),
        tz_offset_minutes: tz,
        bucket_minutes: None,
    }
}

fn delta(day: &str, source: &str, provider: &str, currency: &str, micro: i64) -> RollupDelta {
    RollupDelta {
        day: day.to_owned(),
        source: source.to_owned(),
        provider_id: provider.to_owned(),
        key_id: "k".to_owned(),
        model: "m1".to_owned(),
        client_id: String::new(),
        alias: String::new(),
        currency: currency.to_owned(),
        requests: 1,
        ok_requests: 1,
        input_tokens: 100,
        output_tokens: 50,
        cached_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        cost_micro: micro,
        failure_kind: None,
    }
}

fn log_gateway(state: &AppState, ts_ms: i64, tz: i16, source: UsageSource, failed: bool) {
    let status = if failed { 502 } else { 200 };
    let entry = RequestLogEntry {
        ts_ms,
        protocol_in: "openai_chat".to_owned(),
        requested_model: "m".to_owned(),
        provider_id: Some("p".to_owned()),
        key_id: Some("k".to_owned()),
        status,
        outcome: if failed { "failure" } else { "success" }.to_owned(),
        source: source.as_str().to_owned(),
        ..RequestLogEntry::default()
    };
    let delta = delta_for(&RollupInput {
        ts_ms,
        tz_offset_minutes: tz,
        source,
        provider_id: "p",
        key_id: "k",
        model: "m",
        client_id: "",
        alias: "",
        final_status: Some(status),
        outcome: if failed {
            RequestOutcome::Failure
        } else {
            RequestOutcome::Success
        },
        failure_kind: failed.then_some(FailureKind::ProviderError),
        usage: None,
        cost: None,
        price_currency: Some("USD"),
    });
    insert_log(
        state.store().connection(),
        &LogWrite {
            entry: &entry,
            capture: None,
            rollup: Some(&delta),
        },
    )
    .expect("log");
}

#[tokio::test]
async fn summary_latency_and_failures_aggregate_daily_and_minute_buckets() {
    let state = open();
    let remote = add_provider(&state, "Remote", "https://remote.example.net", None);
    let mut local = provider_template("Ollama", "http://127.0.0.1:11434", None);
    local.auth = crate::llm::wire::AuthStyle::None;
    let local = llm_provider_save_impl(&state, local).expect("local");
    upsert_rollup(
        state.store().connection(),
        &delta("2026-11-03", "gateway", &remote.id, "", 0),
    )
    .unwrap();
    upsert_rollup(
        state.store().connection(),
        &delta("2026-11-03", "gateway", &local.id, "", 0),
    )
    .unwrap();

    let summary = llm_usage_summary_impl(&state, range("2026-11-03", 1, 0))
        .await
        .expect("summary");
    assert_eq!(summary.models_without_price, ["m1"]);
    assert_eq!(summary.totals.unpriced_requests, 1);

    let tz = 8 * 60;
    let base = local_midnight_ms("2026-11-09", tz).expect("day") + 14 * 3_600_000;
    log_gateway(&state, base + 30_000, tz, UsageSource::Agent, false);
    log_gateway(&state, base + 6 * 60_000, tz, UsageSource::Agent, true);
    let query = UsageQuery {
        from_ms: base,
        to_ms: base + 10 * 60_000,
        group_by: vec![UsageGroup::Day],
        bucket_minutes: Some(5),
        ..range("2026-11-09", 1, tz)
    };
    let bucketed = llm_usage_summary_impl(&state, query.clone())
        .await
        .expect("bucketed");
    assert_eq!((bucketed.totals.requests, bucketed.totals.failed), (2, 1));
    let latency = llm_usage_latency_impl(&state, query.clone())
        .await
        .expect("latency");
    assert_eq!(latency[0].timed_requests, 1);
    let failures = llm_usage_failures_impl(&state, query)
        .await
        .expect("failures");
    assert_eq!(failures[0].kind, "provider_error");
}

#[tokio::test]
async fn today_summary_and_tz_offset_match_gateway_logs() {
    let state = open();
    let tz = 8 * 60;
    llm_usage_set_tz_offset_impl(&state, tz).expect("offset");
    assert!(llm_usage_set_tz_offset_impl(&state, 15 * 60).is_err());

    let now = current_time_ms();
    let midnight = local_midnight_ms(&crate::llm::usage::local_day(now, tz), tz).expect("midnight");
    log_gateway(&state, midnight, tz, UsageSource::Gateway, false);
    log_gateway(&state, now, tz, UsageSource::Gateway, true);
    log_gateway(&state, now, tz, UsageSource::Agent, false);

    let today = llm_today_summary_impl(&state).await.expect("today");
    let in_log = count_log(
        state.store().connection(),
        &LogFilter {
            source: Some("gateway".to_owned()),
            since_ms: Some(midnight),
            ..LogFilter::default()
        },
    )
    .expect("count");
    assert_eq!(today.usage.requests, in_log);
    assert_eq!((today.usage.requests, today.usage.failed), (2, 1));
}

#[test]
fn price_overrides_validate_replace_catalog_and_emit_events() {
    let state = open();
    let provider = add_provider(&state, "Remote", "https://remote.example.net", None);
    add_key(
        &state,
        &provider.id,
        "k",
        "sk-k-0123456789abcdef0123",
        0,
        true,
    );
    let model = llm_catalog_search_impl(&state, "gpt", 200)
        .expect("search")
        .into_iter()
        .find(|e| e.input_cost_per_mtok.is_some() && e.output_cost_per_mtok.is_some())
        .unwrap()
        .model_id;
    assert_eq!(
        effective_source(&state, &provider.id, &model),
        Some(PriceSource::Catalog)
    );

    let seen = events_of(&state);
    let override_row = PriceOverride {
        provider_id: provider.id.clone(),
        model_id: model.clone(),
        input_per_mtok: 1.5,
        output_per_mtok: 6.0,
        cache_read_per_mtok: Some(0.15),
        cache_write_per_mtok: None,
        reasoning_per_mtok: None,
        currency: " cny ".to_owned(),
        updated_at_ms: 0,
    };
    let saved = llm_price_override_save_impl(&state, override_row.clone()).expect("save");
    assert_eq!(saved.currency, "CNY");
    assert_eq!(
        effective_source(&state, &provider.id, &model),
        Some(PriceSource::Override)
    );
    assert_eq!(llm_price_override_list_impl(&state).unwrap().len(), 1);
    assert!(llm_price_override_delete_impl(&state, &provider.id, &model).expect("delete"));
    assert!(!seen.lock().unwrap().is_empty());

    let mut invalid = override_row;
    invalid.input_per_mtok = -1.0;
    assert!(llm_price_override_save_impl(&state, invalid).is_err());
}
