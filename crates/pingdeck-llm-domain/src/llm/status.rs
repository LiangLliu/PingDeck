//! Provider status: the one word on every provider card and the reasons behind it.
//!
//! Single responsibility: a pure function from the stored evidence of one provider
//! ([`StatusInputs`]) to a [`ProviderStatusDto`]. The use-case layer gathers the inputs with a
//! few indexed reads; the webview never re-derives the status. Rule order: Disabled,
//! Unverified (no key), Unreachable, Needs attention, Healthy, Unverified ("no model has answered
//! yet" or "Last verified N days ago"); only the first matching rule decides. The key checks of
//! a sniff count as measured Connectivity and Auth results, so a provider added from a passing
//! sniff reads "Unverified: no model has answered yet" and one added with a rejected key reads
//! "Needs attention: Key rejected".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::llm::probe::ProbeStatus;

/// Positive evidence younger than this makes a provider Healthy.
pub const FRESH_MS: i64 = DAY_MS;
/// A failure older than this no longer decides the status: a failed connectivity check or a
/// model's failed test is old news after a week (the age a bench run stops being applicable).
pub const FAILURE_EVIDENCE_MS: i64 = 7 * DAY_MS;
/// An exhausted balance reading older than this no longer makes a provider need attention.
pub const BALANCE_EVIDENCE_MS: i64 = DAY_MS;
/// The window of the "last 3 requests failed" rule.
pub const OUTAGE_WINDOW_MS: i64 = 15 * 60 * 1000;
/// Consecutive failed gateway requests that make a provider Unreachable.
const OUTAGE_REQUESTS: usize = 3;
/// A pair needs at least this many requests before its failure rate counts.
const MIN_PAIR_REQUESTS: u32 = 10;
/// Failure rate above which a pair needs attention.
const MAX_FAILURE_RATE: f64 = 0.5;
/// Gateway requests whose TTFT forms the latency.
const LATENCY_REQUESTS: usize = 20;
/// Fewest gateway requests for the latency to come from traffic.
const MIN_LATENCY_REQUESTS: usize = 3;

const MINUTE_MS: i64 = 60 * 1000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

/// The closed set of provider states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    /// Enabled, nothing is wrong and fresh evidence says it works.
    Healthy,
    /// Enabled and reachable, but something needs a look.
    NeedsAttention,
    /// Enabled and the host is not answering.
    Unreachable,
    /// Enabled but there is no (fresh) evidence yet, or no key.
    Unverified,
    /// Switched off in Settings.
    Disabled,
}

/// Which rule produced a reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    /// Unverified: no enabled key.
    NoKey,
    /// Unverified: the evidence is older than a day.
    LastVerified,
    /// Unverified: the key checks passed, but no model has answered a request yet.
    NoModelAnswered,
    /// Unreachable: the newest Connectivity result is a transport failure.
    ConnectivityFailed,
    /// Unreachable: the last three gateway requests failed.
    RecentFailures,
    /// Needs attention: every enabled key was rejected.
    AllKeysRejected,
    /// Needs attention: enabled models failed their newest Chat or Embeddings test.
    ModelsFailed,
    /// Needs attention: a circuit is open or half-open.
    CircuitOpen,
    /// Needs attention: the account balance is exhausted.
    BalanceExhausted,
    /// Needs attention: every enabled key's own quota is exhausted.
    AllKeysExhausted,
    /// Needs attention: the balance refresh was rejected.
    BalanceAuthFailed,
    /// Needs attention: a (key, model) pair fails more than half of its requests.
    HighFailureRate,
}

/// A reason shown on the card's second line and in the Overview header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReason {
    /// The rule behind it.
    pub code: ReasonCode,
    /// The sentence to show ("2 of 38 models failed the last test"), English only.
    pub text: String,
    /// The numbers inside `text`, for a translated sentence: `models_failed` has `failed` and
    /// `total`, `last_verified` has `days`, `all_keys_rejected` has `keys`, `circuit_open` has
    /// `count`, `recent_failures` has `count`; the other codes have none.
    #[serde(default)]
    pub params: BTreeMap<String, u64>,
}

impl StatusReason {
    /// Adds the numbers of the sentence.
    fn with_params(mut self, params: &[(&str, u64)]) -> Self {
        self.params = params
            .iter()
            .map(|(name, value)| ((*name).to_owned(), *value))
            .collect();
        self
    }
}

fn reason(code: ReasonCode, text: impl Into<String>) -> StatusReason {
    StatusReason {
        code,
        text: text.into(),
        params: BTreeMap::new(),
    }
}

/// Where the latency came from (the tooltip of the value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencySource {
    /// "TTFT p50, last 20 requests".
    Traffic,
    /// "TTFT median, last test run".
    TestRun,
}

/// The wire form of a provider's status (`llm_provider_status_list`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderStatusDto {
    /// Provider id.
    pub provider_id: String,
    /// The state.
    pub status: ProviderStatus,
    /// Why, most important first; empty for Healthy and Disabled.
    pub reasons: Vec<StatusReason>,
    /// TTFT in ms ([`latency`]); `None` is shown as "—".
    pub latency_ms: Option<u64>,
    /// Where `latency_ms` came from.
    pub latency_source: Option<LatencySource>,
    /// When the newest evidence was recorded, Unix ms.
    pub evidence_at_ms: Option<i64>,
}

/// How a forwarded gateway request ended, for the outage rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayOutcome {
    /// 2xx.
    Success,
    /// A transport failure, a timeout or a 5xx.
    TransportFailure,
    /// Any other failure (a 4xx): not an outage signal.
    OtherFailure,
}

/// One forwarded gateway request to the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayAttempt {
    /// When it ended, Unix ms.
    pub at_ms: i64,
    /// How it ended.
    pub outcome: GatewayOutcome,
}

/// An enabled model and its newest Chat (Embeddings for an embedding model) result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelEvidence {
    /// The newest result's status; `None` when the model was never tested.
    pub newest_test: Option<ProbeStatus>,
    /// When the newest result was stored, Unix ms; `None` is treated as current.
    pub tested_at_ms: Option<i64>,
    /// The newest result is a refused request (HTTP 400 or 422 on Chat or Stream,
    /// [`ProbeStatus::InvalidResponse`] with `probe::status::is_request_refused`):
    /// neither a pass nor a failure.
    pub request_refused: bool,
}

/// The state of the balance, as plain values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BalanceSignal {
    /// The account balance is exhausted (scope Account).
    pub account_exhausted: bool,
    /// Enabled keys whose own (Key-scope) quota is exhausted. The provider needs attention only
    /// when every enabled key is: the account balance is not known from a key.
    pub exhausted_keys: u32,
    /// The last refresh failed with an authentication error.
    pub auth_failed: bool,
    /// When the newest exhausted reading behind `account_exhausted` or `exhausted_keys` was
    /// taken. A successful forwarded request after it means the balance was topped up since:
    /// the provider then no longer needs attention for it (the balance card still shows the
    /// reading as it was).
    pub exhausted_at_ms: Option<i64>,
}

/// The request count and failure rate of one enabled (key, model) pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PairHealth {
    /// Requests the failure rate is based on.
    pub requests: u32,
    /// Failure-rate EWMA, 0 to 1.
    pub failure_rate: f64,
}

/// Everything the status of one provider is computed from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusInputs {
    /// Provider id.
    pub provider_id: String,
    /// `provider.enabled`.
    pub enabled: bool,
    /// The provider's auth style is `none` (a local runtime): no key is not a problem.
    pub auth_none: bool,
    /// The newest Auth result of each enabled key (`None`: never tested). Empty: no enabled key.
    pub key_auth: Vec<Option<ProbeStatus>>,
    /// The newest Connectivity result across the provider's keys.
    pub newest_connectivity: Option<ProbeStatus>,
    /// When that result was stored, Unix ms; `None` is treated as current.
    pub newest_connectivity_ms: Option<i64>,
    /// When the newest passing Connectivity, Auth, Chat, Stream or Embeddings result was stored.
    pub newest_passing_test_ms: Option<i64>,
    /// When the newest Ready Chat, Stream or Embeddings result was stored.
    pub newest_ready_model_test_ms: Option<i64>,
    /// When the newest successful forwarded request (sources gateway, agent and api, as the
    /// health card counts them) ended.
    pub newest_gateway_success_ms: Option<i64>,
    /// The newest forwarded requests (same sources), newest first (at least the last three).
    pub recent_gateway: Vec<GatewayAttempt>,
    /// Every enabled model.
    pub models: Vec<ModelEvidence>,
    /// (key, model) circuits that are open or half-open.
    pub open_circuits: u32,
    /// Balance signals.
    pub balance: BalanceSignal,
    /// Health of every enabled pair.
    pub pairs: Vec<PairHealth>,
    /// TTFT of the last 20 successful streamed gateway requests in the last 24 hours, newest
    /// first ([`latency`] takes the first 20 as the newest).
    pub gateway_ttft_ms: Vec<u64>,
    /// Stream-test TTFT of the newest run over the enabled models.
    pub test_ttft_ms: Vec<u64>,
}

/// A count as a reason parameter.
fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn median(values: &[u64]) -> Option<u64> {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    match sorted.len() {
        0 => None,
        len if len % 2 == 1 => Some(sorted[middle]),
        _ => {
            // The midpoint rounded up, `(lo + hi).div_ceil(2)`, written so it cannot overflow.
            let (lo, hi) = (sorted[middle - 1], sorted[middle]);
            Some(hi - (hi - lo) / 2)
        }
    }
}

/// The latency of a provider: the median TTFT of the last 20 successful streamed
/// gateway requests; with fewer than 3 of them, the median Stream-test TTFT of the newest run;
/// with neither, `None`. Non-streamed traffic never contributes (the caller passes streamed
/// requests only).
#[must_use]
pub fn latency(gateway_ttft_ms: &[u64], test_ttft_ms: &[u64]) -> Option<(u64, LatencySource)> {
    let traffic = &gateway_ttft_ms[..gateway_ttft_ms.len().min(LATENCY_REQUESTS)];
    if traffic.len() >= MIN_LATENCY_REQUESTS {
        return median(traffic).map(|value| (value, LatencySource::Traffic));
    }
    median(test_ttft_ms).map(|value| (value, LatencySource::TestRun))
}

/// "3 days ago", "5 h ago", "12 min ago", "just now".
fn age_text(age_ms: i64) -> String {
    let age_ms = age_ms.max(0);
    if age_ms >= DAY_MS {
        let days = age_ms / DAY_MS;
        format!("{days} {} ago", if days == 1 { "day" } else { "days" })
    } else if age_ms >= HOUR_MS {
        format!("{} h ago", age_ms / HOUR_MS)
    } else if age_ms >= MINUTE_MS {
        format!("{} min ago", age_ms / MINUTE_MS)
    } else {
        "just now".to_owned()
    }
}

/// Whether the last three forwarded requests inside the outage window all failed in transport.
/// A success among them, or fewer than three requests, is no outage.
fn recent_outage(inputs: &StatusInputs, now_ms: i64) -> bool {
    let (seen, all_failed) = inputs
        .recent_gateway
        .iter()
        .filter(|attempt| now_ms.saturating_sub(attempt.at_ms) <= OUTAGE_WINDOW_MS)
        .take(OUTAGE_REQUESTS)
        .fold((0, true), |(seen, all_failed), attempt| {
            (
                seen + 1,
                all_failed && attempt.outcome == GatewayOutcome::TransportFailure,
            )
        });
    seen == OUTAGE_REQUESTS && all_failed
}

/// Whether evidence stored at `at_ms` (`None`: current) is still inside `window_ms` at `now_ms`.
fn within(at_ms: Option<i64>, now_ms: i64, window_ms: i64) -> bool {
    at_ms.is_none_or(|at| now_ms.saturating_sub(at) <= window_ms)
}

/// Whether the newest Connectivity result is a transport failure that still decides: younger
/// than a week, and no later passing check or forwarded success proved the host answers.
fn connectivity_failed(inputs: &StatusInputs, now_ms: i64) -> bool {
    if !matches!(
        inputs.newest_connectivity,
        Some(ProbeStatus::Unreachable | ProbeStatus::TimedOut)
    ) {
        return false;
    }
    let at = inputs.newest_connectivity_ms;
    let answered_since = at.is_some_and(|at| {
        [
            inputs.newest_passing_test_ms,
            inputs.newest_gateway_success_ms,
        ]
        .into_iter()
        .flatten()
        .any(|success| success > at)
    });
    !answered_since && within(at, now_ms, FAILURE_EVIDENCE_MS)
}

/// The reasons the host is not answering; empty when it answers.
fn unreachable_reasons(inputs: &StatusInputs, now_ms: i64) -> Vec<StatusReason> {
    let mut reasons = Vec::new();
    if connectivity_failed(inputs, now_ms) {
        reasons.push(reason(
            ReasonCode::ConnectivityFailed,
            "The last connectivity check failed",
        ));
    }
    if recent_outage(inputs, now_ms) {
        reasons.push(
            reason(
                ReasonCode::RecentFailures,
                format!("The last {OUTAGE_REQUESTS} requests failed"),
            )
            .with_params(&[("count", count(OUTAGE_REQUESTS))]),
        );
    }
    reasons
}

/// Every enabled key was rejected.
fn all_keys_rejected(inputs: &StatusInputs) -> Option<StatusReason> {
    let keys = inputs.key_auth.len();
    let rejected = keys > 0
        && inputs
            .key_auth
            .iter()
            .all(|auth| *auth == Some(ProbeStatus::Rejected));
    rejected.then(|| {
        let text = if keys == 1 {
            "Key rejected".to_owned()
        } else {
            format!("All {keys} keys rejected")
        };
        reason(ReasonCode::AllKeysRejected, text).with_params(&[("keys", count(keys))])
    })
}

/// Enabled models failed their newest test within the last week (rate limits, skips, "not
/// observed" and refused requests are no failures; an older failure is old news).
fn models_failed(inputs: &StatusInputs, now_ms: i64) -> Option<StatusReason> {
    let failed = inputs
        .models
        .iter()
        .filter(|model| {
            !model.request_refused
                && within(model.tested_at_ms, now_ms, FAILURE_EVIDENCE_MS)
                && model.newest_test.is_some_and(|status| {
                    !matches!(
                        status,
                        ProbeStatus::Ready
                            | ProbeStatus::Skipped
                            | ProbeStatus::RateLimited
                            | ProbeStatus::NotObserved
                    )
                })
        })
        .count();
    let total = inputs.models.len();
    (failed > 0).then(|| {
        reason(
            ReasonCode::ModelsFailed,
            format!("{failed} of {total} models failed the last test"),
        )
        .with_params(&[("failed", count(failed)), ("total", count(total))])
    })
}

/// Circuits are open or half-open.
fn circuits_open(inputs: &StatusInputs) -> Option<StatusReason> {
    let open = inputs.open_circuits;
    (open > 0).then(|| {
        let text = if open == 1 {
            "1 circuit is open".to_owned()
        } else {
            format!("{open} circuits are open")
        };
        reason(ReasonCode::CircuitOpen, text).with_params(&[("count", u64::from(open))])
    })
}

/// Whether a forwarded request succeeded after the newest exhausted reading: the balance was
/// topped up since, so the reading no longer decides the status.
fn topped_up_since(inputs: &StatusInputs) -> bool {
    match (
        inputs.balance.exhausted_at_ms,
        inputs.newest_gateway_success_ms,
    ) {
        (Some(read), Some(success)) => success > read,
        _ => false,
    }
}

/// The account balance is exhausted (a fresh reading no later success contradicts).
fn balance_exhausted(inputs: &StatusInputs) -> Option<StatusReason> {
    (inputs.balance.account_exhausted && !topped_up_since(inputs))
        .then(|| reason(ReasonCode::BalanceExhausted, "Balance exhausted"))
}

/// Every enabled key's own quota is exhausted (fresh readings no later success contradicts).
fn all_keys_exhausted(inputs: &StatusInputs) -> Option<StatusReason> {
    let exhausted = !inputs.key_auth.is_empty()
        && !topped_up_since(inputs)
        && usize::try_from(inputs.balance.exhausted_keys)
            .is_ok_and(|exhausted| exhausted >= inputs.key_auth.len());
    exhausted.then(|| reason(ReasonCode::AllKeysExhausted, "All keys exhausted"))
}

/// The balance refresh was rejected.
fn balance_auth_failed(inputs: &StatusInputs) -> Option<StatusReason> {
    inputs.balance.auth_failed.then(|| {
        reason(
            ReasonCode::BalanceAuthFailed,
            "The balance could not be read: the key was rejected",
        )
    })
}

/// A pair with enough requests fails more than half of them.
fn high_failure_rate(inputs: &StatusInputs) -> Option<StatusReason> {
    inputs
        .pairs
        .iter()
        .any(|pair| pair.requests >= MIN_PAIR_REQUESTS && pair.failure_rate > MAX_FAILURE_RATE)
        .then(|| {
            reason(
                ReasonCode::HighFailureRate,
                "Over half of the recent requests of a model failed",
            )
        })
}

/// The needs-attention reasons, in rule order (the order the UI lists them).
fn attention_reasons(inputs: &StatusInputs, now_ms: i64) -> Vec<StatusReason> {
    [
        all_keys_rejected(inputs),
        models_failed(inputs, now_ms),
        circuits_open(inputs),
        balance_exhausted(inputs),
        all_keys_exhausted(inputs),
        balance_auth_failed(inputs),
        high_failure_rate(inputs),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// When the newest model-level evidence (a Ready Chat, Stream or Embeddings result, or a
/// forwarded success) and the newest evidence of any kind were recorded.
fn evidence_times(inputs: &StatusInputs) -> (Option<i64>, Option<i64>) {
    let model_evidence_at = [
        inputs.newest_ready_model_test_ms,
        inputs.newest_gateway_success_ms,
    ]
    .into_iter()
    .flatten()
    .max();
    let evidence_at_ms = [inputs.newest_passing_test_ms, model_evidence_at]
        .into_iter()
        .flatten()
        .max();
    (model_evidence_at, evidence_at_ms)
}

/// Healthy needs model-level evidence younger than a day; otherwise the provider is Unverified,
/// either because no model ever answered or because the evidence is old.
fn freshness(model_evidence_at: Option<i64>, now_ms: i64) -> (ProviderStatus, Vec<StatusReason>) {
    let Some(at) = model_evidence_at else {
        return (
            ProviderStatus::Unverified,
            vec![reason(
                ReasonCode::NoModelAnswered,
                "No model has answered yet",
            )],
        );
    };
    let age = now_ms.saturating_sub(at);
    if age < FRESH_MS {
        return (ProviderStatus::Healthy, Vec::new());
    }
    let days = u64::try_from(age / DAY_MS).unwrap_or(0);
    (
        ProviderStatus::Unverified,
        vec![
            reason(
                ReasonCode::LastVerified,
                format!("Last verified {}", age_text(age)),
            )
            .with_params(&[("days", days)]),
        ],
    )
}

/// The state and its reasons; the first matching rule decides.
fn decide(
    inputs: &StatusInputs,
    now_ms: i64,
    model_evidence_at: Option<i64>,
) -> (ProviderStatus, Vec<StatusReason>) {
    // 1. Disabled wins over everything.
    if !inputs.enabled {
        return (ProviderStatus::Disabled, Vec::new());
    }
    // 2. Unverified because there is no key to use.
    if inputs.key_auth.is_empty() && !inputs.auth_none {
        return (
            ProviderStatus::Unverified,
            vec![reason(ReasonCode::NoKey, "No key")],
        );
    }
    // 3. Unreachable: the host does not answer.
    let unreachable = unreachable_reasons(inputs, now_ms);
    if !unreachable.is_empty() {
        return (ProviderStatus::Unreachable, unreachable);
    }
    // 4. Needs attention.
    let attention = attention_reasons(inputs, now_ms);
    if !attention.is_empty() {
        return (ProviderStatus::NeedsAttention, attention);
    }
    // 5. Healthy, or Unverified for missing or old evidence.
    freshness(model_evidence_at, now_ms)
}

/// Computes the status of one provider at `now_ms`. The latency is computed for every state,
/// Disabled included.
#[must_use]
pub fn provider_status(inputs: &StatusInputs, now_ms: i64) -> ProviderStatusDto {
    let (model_evidence_at, evidence_at_ms) = evidence_times(inputs);
    let (latency_ms, latency_source) = latency(&inputs.gateway_ttft_ms, &inputs.test_ttft_ms)
        .map_or((None, None), |(value, source)| (Some(value), Some(source)));
    let (status, reasons) = decide(inputs, now_ms, model_evidence_at);
    ProviderStatusDto {
        provider_id: inputs.provider_id.clone(),
        status,
        reasons,
        latency_ms,
        latency_source,
        evidence_at_ms,
    }
}

#[cfg(test)]
mod tests;
