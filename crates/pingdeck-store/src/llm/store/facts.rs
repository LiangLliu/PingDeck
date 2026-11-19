//! The stored evidence behind every provider status.
//!
//! Single responsibility: gather, with a few indexed reads, the SQLite-side inputs of
//! [`crate::llm::status::provider_status`] for every provider. Circuit and health data live in
//! memory and are merged by the use-case layer (`open_circuits`, `pairs`).

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    Provider, ProviderKey, ProviderModel, db, list_keys, list_models, list_providers, to_u64,
};
use crate::llm::failure::FailureKind;
use crate::llm::probe::{ProbeStatus, ProbeTest};
use crate::llm::status::{
    BalanceSignal, FRESH_MS, GatewayAttempt, GatewayOutcome, ModelEvidence, OUTAGE_WINDOW_MS,
    StatusInputs,
};
use crate::llm::wire::AuthStyle;
use pingdeck_base::error::AppError;
use pingdeck_llm_domain::llm::records::{balance_signal, decisive_test};

/// TTFT of the last streamed, successful forwarded requests of a provider in the latency window
/// (`?1` provider, `?2` earliest start). Forwarded means the sources the health card counts
/// (gateway, agent and api), so the card and the status word read the same traffic. Served by
/// `idx_llm_request_log_provider`; `source` is only a filter.
pub(super) const GATEWAY_TTFT_SQL: &str = "SELECT ttft_ms FROM llm_request_log \
     WHERE provider_id = ?1 AND ts_ms >= ?2 AND +source IN ('gateway', 'agent', 'api') \
       AND stream = 1 \
       AND outcome = 'success' AND ttft_ms IS NOT NULL \
     ORDER BY ts_ms DESC LIMIT 20";

/// The newest forwarded requests of a provider inside the outage window (`?1` provider, `?2`
/// earliest start): end time, status, outcome, failure kind. Served by
/// `idx_llm_request_log_provider`.
pub(super) const GATEWAY_RECENT_SQL: &str = "SELECT ts_ms + latency_ms, status, outcome, failure_kind \
     FROM llm_request_log \
     WHERE provider_id = ?1 AND ts_ms >= ?2 AND +source IN ('gateway', 'agent', 'api') \
       AND outcome <> 'canceled' \
     ORDER BY ts_ms DESC LIMIT 3";

/// When the newest successful forwarded request of a provider ended (`?1` provider, `?2` earliest
/// start). Served by `idx_llm_request_log_provider`.
pub(super) const GATEWAY_SUCCESS_SQL: &str = "SELECT ts_ms + latency_ms FROM llm_request_log \
     WHERE provider_id = ?1 AND ts_ms >= ?2 AND +source IN ('gateway', 'agent', 'api') \
       AND outcome = 'success' \
     ORDER BY ts_ms DESC LIMIT 1";

struct ResultFact {
    provider_id: String,
    key_id: String,
    model_id: String,
    test: String,
    ok: bool,
    status: ProbeStatus,
    ttft_ms: Option<u64>,
    checked_at_ms: i64,
    run_id: Option<String>,
    http_status: Option<u16>,
}

/// The status of a stored result. `ok` is authoritative: an ok row is Ready; a failed row takes
/// its stored word, except `ready` (the column default, left by an insert that sets no word) or
/// an unknown word, which read as InvalidResponse. A failed row is therefore never Ready.
fn status_of(ok: bool, word: &str) -> ProbeStatus {
    if ok {
        return ProbeStatus::Ready;
    }
    ProbeStatus::parse(word)
        .filter(|status| *status != ProbeStatus::Ready)
        .unwrap_or(ProbeStatus::InvalidResponse)
}

/// Every stored latest result with its provider, newest first.
fn result_facts(connection: &Connection) -> Result<Vec<ResultFact>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT k.provider_id, r.key_id, r.model_id, r.test, r.ok, r.status_word, r.ttft_ms, \
                 r.checked_at_ms, r.run_id, r.status \
             FROM llm_probe_results r JOIN llm_keys k ON k.id = r.key_id \
             ORDER BY r.checked_at_ms DESC",
        )
        .map_err(db)?;
    let rows = statement
        .query_map([], |row| {
            let ok = row.get::<_, i64>(4)? != 0;
            Ok(ResultFact {
                provider_id: row.get(0)?,
                key_id: row.get(1)?,
                model_id: row.get(2)?,
                test: row.get(3)?,
                ok,
                status: status_of(ok, &row.get::<_, String>(5)?),
                ttft_ms: row.get::<_, Option<i64>>(6)?.map(to_u64),
                checked_at_ms: row.get(7)?,
                run_id: row.get(8)?,
                http_status: row
                    .get::<_, Option<i64>>(9)?
                    .and_then(|code| u16::try_from(code).ok()),
            })
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

#[derive(Default)]
struct ProbeEvidence {
    /// Newest Auth status per key.
    key_auth: BTreeMap<String, ProbeStatus>,
    /// Newest Connectivity status and when it was stored.
    newest_connectivity: Option<(ProbeStatus, i64)>,
    newest_passing_test_ms: Option<i64>,
    newest_ready_model_test_ms: Option<i64>,
    /// Newest status per model and model-scoped check: status, refused request, stored at.
    model_test: BTreeMap<(String, ProbeTest), (ProbeStatus, bool, i64)>,
    /// Passing Stream results with a TTFT, newest first: `(model, run, ttft)`.
    streams: Vec<(String, Option<String>, u64)>,
}

/// The model-scoped checks that say whether a model answers: Chat, Embeddings or Decision.
const fn is_model_test(test: ProbeTest) -> bool {
    matches!(
        test,
        ProbeTest::Chat | ProbeTest::Embeddings | ProbeTest::Decision
    )
}

/// The checks whose Ready result is evidence that the provider works.
const fn is_passing_evidence(test: ProbeTest) -> bool {
    matches!(
        test,
        ProbeTest::Connectivity
            | ProbeTest::Auth
            | ProbeTest::Chat
            | ProbeTest::Stream
            | ProbeTest::Embeddings
            | ProbeTest::Decision
    )
}

/// The checks whose Ready result is evidence that a model answers (Stream included).
const fn is_answer_test(test: ProbeTest) -> bool {
    matches!(
        test,
        ProbeTest::Chat | ProbeTest::Stream | ProbeTest::Embeddings | ProbeTest::Decision
    )
}

/// Folds the results (newest first) into the probe evidence of each provider.
fn fold_results(facts: Vec<ResultFact>) -> BTreeMap<String, ProbeEvidence> {
    let mut by_provider: BTreeMap<String, ProbeEvidence> = BTreeMap::new();
    for fact in facts {
        let evidence = by_provider.entry(fact.provider_id).or_default();
        let test = ProbeTest::parse(&fact.test);
        // The facts are newest first, so the first value seen of a kind is the newest.
        match test {
            Some(ProbeTest::Auth) => {
                evidence.key_auth.entry(fact.key_id).or_insert(fact.status);
            }
            Some(ProbeTest::Connectivity) => {
                evidence
                    .newest_connectivity
                    .get_or_insert((fact.status, fact.checked_at_ms));
            }
            _ => {}
        }
        // Only a Ready status is evidence.
        if fact.status == ProbeStatus::Ready && test.is_some_and(is_passing_evidence) {
            evidence
                .newest_passing_test_ms
                .get_or_insert(fact.checked_at_ms);
        }
        if fact.status == ProbeStatus::Ready && test.is_some_and(is_answer_test) {
            evidence
                .newest_ready_model_test_ms
                .get_or_insert(fact.checked_at_ms);
        }
        if let Some(test) = test.filter(|test| is_model_test(*test)) {
            let refused = matches!(fact.http_status, Some(400 | 422))
                && fact.status == ProbeStatus::InvalidResponse;
            evidence
                .model_test
                .entry((fact.model_id.clone(), test))
                .or_insert((fact.status, refused, fact.checked_at_ms));
        }
        if test == Some(ProbeTest::Stream)
            && fact.ok
            && let Some(ttft) = fact.ttft_ms
        {
            evidence.streams.push((fact.model_id, fact.run_id, ttft));
        }
    }
    by_provider
}

/// How a gateway request ended, for the outage rule: a transport failure, a timeout or a 5xx
/// counts; a client error or a cancel does not.
fn gateway_outcome(status: i64, outcome: &str, failure_kind: Option<&str>) -> GatewayOutcome {
    // Not `RequestOutcome::from_log`: that maps unknown outcomes to Failure, while only a literal
    // `failure` may count as a transport failure here.
    match outcome {
        "success" => GatewayOutcome::Success,
        "failure"
            if status == 0
                || status >= 500
                || failure_kind == Some(FailureKind::Network.as_str())
                || failure_kind == Some(FailureKind::Timeout.as_str()) =>
        {
            GatewayOutcome::TransportFailure
        }
        _ => GatewayOutcome::OtherFailure,
    }
}

/// The traffic evidence of one provider (forwarded requests of the gateway, the agent and the
/// API workbench, as the health card counts them): newest success, the newest requests and the
/// streamed TTFTs, each one indexed read.
fn gateway_evidence(
    connection: &Connection,
    provider_id: &str,
    now_ms: i64,
    inputs: &mut StatusInputs,
) -> Result<(), AppError> {
    inputs.newest_gateway_success_ms = connection
        .prepare_cached(GATEWAY_SUCCESS_SQL)
        .map_err(db)?
        .query_row(params![provider_id, now_ms - FRESH_MS], |row| {
            row.get::<_, i64>(0)
        })
        .optional()
        .map_err(db)?;
    let mut recent = connection.prepare_cached(GATEWAY_RECENT_SQL).map_err(db)?;
    inputs.recent_gateway = recent
        .query_map(params![provider_id, now_ms - OUTAGE_WINDOW_MS], |row| {
            Ok(GatewayAttempt {
                at_ms: row.get(0)?,
                outcome: gateway_outcome(
                    row.get(1)?,
                    &row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?.as_deref(),
                ),
            })
        })
        .map_err(db)?
        .collect::<Result<_, _>>()
        .map_err(db)?;
    let mut ttft = connection.prepare_cached(GATEWAY_TTFT_SQL).map_err(db)?;
    inputs.gateway_ttft_ms = ttft
        .query_map(params![provider_id, now_ms - FRESH_MS], |row| {
            row.get::<_, i64>(0).map(to_u64)
        })
        .map_err(db)?
        .collect::<Result<_, _>>()
        .map_err(db)?;
    Ok(())
}

/// The TTFTs of the newest run that stream-tested an enabled model, over every enabled model it
/// tested; empty without such a run.
fn newest_run_ttfts(
    streams: &[(String, Option<String>, u64)],
    enabled_models: &[&ProviderModel],
) -> Vec<u64> {
    let enabled = |model: &String| enabled_models.iter().any(|m| &m.model_id == model);
    let Some((_, run, _)) = streams.iter().find(|(model, _, _)| enabled(model)) else {
        return Vec::new();
    };
    streams
        .iter()
        .filter(|(model, stream_run, _)| stream_run == run && enabled(model))
        .map(|(_, _, ttft)| *ttft)
        .collect()
}

/// The status inputs of `provider` from its rows and probe evidence; the gateway traffic is
/// added by [`gateway_evidence`].
fn provider_inputs(
    provider: &Provider,
    keys: &[&ProviderKey],
    enabled_models: &[&ProviderModel],
    probes: &ProbeEvidence,
    now_ms: i64,
) -> StatusInputs {
    StatusInputs {
        provider_id: provider.id.clone(),
        enabled: provider.routable(),
        auth_none: provider.auth == AuthStyle::None,
        key_auth: keys
            .iter()
            .filter(|key| key.enabled)
            .map(|key| probes.key_auth.get(&key.id).copied())
            .collect(),
        newest_connectivity: probes.newest_connectivity.map(|(status, _)| status),
        newest_connectivity_ms: probes.newest_connectivity.map(|(_, at)| at),
        newest_passing_test_ms: probes.newest_passing_test_ms,
        newest_ready_model_test_ms: probes.newest_ready_model_test_ms,
        models: enabled_models
            .iter()
            .map(|model| {
                let newest = probes
                    .model_test
                    .get(&(model.model_id.clone(), decisive_test(provider, model)))
                    .copied();
                ModelEvidence {
                    newest_test: newest.map(|(status, _, _)| status),
                    tested_at_ms: newest.map(|(_, _, at)| at),
                    request_refused: newest.is_some_and(|(_, refused, _)| refused),
                }
            })
            .collect(),
        balance: if provider.balance_off {
            BalanceSignal::default()
        } else {
            balance_signal(keys, now_ms)
        },
        test_ttft_ms: newest_run_ttfts(&probes.streams, enabled_models),
        ..StatusInputs::default()
    }
}

/// The stored inputs of the status of every provider, in display order: newest connectivity and
/// auth results, newest decisive (Chat, Embeddings or Decision) result of every enabled model,
/// the evidence timestamps, the gateway traffic inside the freshness and outage windows, the
/// stream-test TTFTs of the newest run and the balance flags.
///
/// `open_circuits` and `pairs` stay empty: the health book is in memory and the use-case layer
/// merges it.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn provider_status_facts(
    connection: &Connection,
    now_ms: i64,
) -> Result<Vec<StatusInputs>, AppError> {
    let providers = list_providers(connection)?;
    let keys = list_keys(connection, None)?;
    let models = list_models(connection, None)?;
    let mut evidence = fold_results(result_facts(connection)?);
    let mut facts = Vec::with_capacity(providers.len());
    for provider in providers {
        let provider_keys: Vec<&ProviderKey> = keys
            .iter()
            .filter(|key| key.provider_id == provider.id)
            .collect();
        let enabled_models: Vec<&ProviderModel> = models
            .iter()
            .filter(|model| model.provider_id == provider.id && model.enabled)
            .collect();
        let probes = evidence.remove(&provider.id).unwrap_or_default();
        let mut inputs =
            provider_inputs(&provider, &provider_keys, &enabled_models, &probes, now_ms);
        if provider.routable() {
            gateway_evidence(connection, &provider.id, now_ms, &mut inputs)?;
        }
        facts.push(inputs);
    }
    Ok(facts)
}
