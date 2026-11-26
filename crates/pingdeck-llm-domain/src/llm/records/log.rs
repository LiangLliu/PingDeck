//! Request log, log filter/paging/clear report and capture stats DTOs, and the booking rule of
//! a log row.

use serde::{Deserialize, Serialize};

use crate::llm::capture_record::{CapturedExchange, TimingJson};
use crate::llm::failure::FailureKind;
use crate::llm::usage::{
    BookedCost, CostUsage, PriceResolution, RequestOutcome, RollupDelta, RollupInput, UsageSource,
    book_cost, default_currency, delta_for, request_succeeded,
};

/// Newest rows of each source `prune_log` keeps. A constant of the product, no setting.
pub const LOG_ROWS_PER_SOURCE: u32 = 1_000;

/// Most rows one `list_log` page returns.
pub const MAX_LOG_PAGE: usize = 500;
/// Rows of a `list_log` page that names no limit.
pub const DEFAULT_LOG_PAGE: usize = 200;

/// The values `source` may take; `insert_log` rejects any other.
pub const LOG_SOURCES: [&str; 4] = [
    UsageSource::ALL[0].as_str(),
    UsageSource::ALL[1].as_str(),
    UsageSource::ALL[2].as_str(),
    UsageSource::ALL[3].as_str(),
];

fn gateway_source() -> String {
    UsageSource::Gateway.as_str().to_owned()
}

/// One model call as logged. Every field beyond the required core has a serde default, so
/// minimal payloads parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestLogEntry {
    /// Row id (0 before insert).
    #[serde(default)]
    pub id: i64,
    pub ts_ms: i64,
    pub protocol_in: String,
    pub requested_model: String,
    /// Provider that finally answered (or was last tried).
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub key_id: Option<String>,
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default)]
    pub protocol_out: Option<String>,
    /// HTTP status returned to the client.
    pub status: u16,
    /// `success`, `failure` or `canceled`.
    pub outcome: String,
    pub stream: bool,
    pub latency_ms: u64,
    /// Time to first token.
    #[serde(default)]
    pub ttft_ms: Option<u64>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    /// Cached prompt tokens.
    #[serde(default)]
    pub cached_tokens: Option<u64>,
    /// Every upstream attempt, as JSON (no secrets).
    #[serde(default)]
    pub attempts: serde_json::Value,
    #[serde(default)]
    pub error: Option<String>,
    /// `gateway`, `agent`, `probe` or `api`.
    #[serde(default = "gateway_source")]
    pub source: String,
    /// Session id, probe run id or API run id.
    #[serde(default)]
    pub source_ref: Option<String>,
    /// `rq_<16 hex>`, unique among rows that have one.
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    /// That client's name, kept when the client is deleted (`Anonymous` without tokens).
    #[serde(default)]
    pub client_name: Option<String>,
    /// Remote `ip:port` of a non-loopback peer.
    #[serde(default)]
    pub peer: Option<String>,
    /// The alias that matched `requested_model`.
    #[serde(default)]
    pub alias: Option<String>,
    /// `protocol_in` differs from `protocol_out`.
    #[serde(default)]
    pub translated: bool,
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    /// Cache-write tokens.
    #[serde(default)]
    pub cache_write_tokens: Option<u64>,
    /// The token counts are the characters/4 estimate.
    #[serde(default)]
    pub tokens_estimated: bool,
    /// Cost in millionths of `cost_currency`; `None` when no price was known.
    #[serde(default)]
    pub cost_micro_usd: Option<i64>,
    /// ISO code of the price used (`USD` when there is no cost).
    #[serde(default = "default_currency")]
    pub cost_currency: String,
    /// Where the price came from (`override`, `catalog`, `fixed`) or why there is none
    /// (`local_free`, `not_in_catalog`, `catalog_without_price`, `no_usage`); set by `book_entry`.
    #[serde(default)]
    pub price_source: Option<String>,
    #[serde(default)]
    pub failure_kind: Option<FailureKind>,
    /// Request id header the serving upstream sent.
    #[serde(default)]
    pub upstream_request_id: Option<String>,
    /// Client request body size.
    #[serde(default)]
    pub req_bytes: Option<u64>,
    /// Client response body size.
    #[serde(default)]
    pub resp_bytes: Option<u64>,
    #[serde(default)]
    pub timing: Option<TimingJson>,
    /// A capture row exists. Read-only: `insert_log` sets it from the write.
    #[serde(default)]
    pub has_capture: bool,
    /// The row a replay re-sent.
    #[serde(default)]
    pub replay_of: Option<i64>,
    /// Chat session of an `agent` row.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Agent run of an `agent` row.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Assistant message (the step) of an `agent` row.
    #[serde(default)]
    pub message_id: Option<String>,
    /// What an `agent` call was for: `step`, `compaction`, `title` or `subagent`.
    #[serde(default)]
    pub purpose: Option<String>,
    /// How the serving answer ended (`stop`, `length`, `tool_calls`, `content_filter`,
    /// `needs_continuation` or the provider's word), when it ended.
    #[serde(default)]
    pub finish_reason: Option<String>,
    /// The rule that gave the price (`PriceBasisKind`), when the pricing step knows it.
    #[serde(default)]
    pub price_basis: Option<String>,
    /// The wait in seconds the serving or last failed attempt's `Retry-After` named. Derived on
    /// read from `attempts` (no column): ignored by `insert_log`.
    #[serde(default)]
    pub retry_after_s: Option<u64>,
    /// The short word after the status ("retry-after 2", "host not allowed", "upstream error"),
    /// `None` for a row that succeeded. Derived on read by [`super::derive_log_reason`]: ignored
    /// by `insert_log`.
    #[serde(default)]
    pub reason: Option<String>,
}

impl Default for RequestLogEntry {
    /// An empty gateway row at time 0 (callers fill what they know).
    fn default() -> Self {
        Self {
            id: 0,
            ts_ms: 0,
            protocol_in: String::new(),
            requested_model: String::new(),
            provider_id: None,
            provider_name: None,
            key_id: None,
            upstream_model: None,
            protocol_out: None,
            status: 0,
            outcome: String::new(),
            stream: false,
            latency_ms: 0,
            ttft_ms: None,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            attempts: serde_json::json!([]),
            error: None,
            source: gateway_source(),
            source_ref: None,
            request_id: None,
            client_id: None,
            client_name: None,
            peer: None,
            alias: None,
            translated: false,
            reasoning_tokens: None,
            cache_write_tokens: None,
            tokens_estimated: false,
            cost_micro_usd: None,
            cost_currency: default_currency(),
            price_source: None,
            failure_kind: None,
            upstream_request_id: None,
            req_bytes: None,
            resp_bytes: None,
            timing: None,
            has_capture: false,
            replay_of: None,
            session_id: None,
            run_id: None,
            message_id: None,
            purpose: None,
            finish_reason: None,
            price_basis: None,
            retry_after_s: None,
            reason: None,
        }
    }
}

/// A row and its capture, as the debug drawer reads them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogDetail {
    pub entry: RequestLogEntry,
    pub capture: Option<CapturedExchange>,
}

/// One page request for `list_log`: rows older than `before_id`, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogPage {
    /// Keyset cursor: only rows with `id < before_id`; `None` starts at the newest.
    pub before_id: Option<i64>,
    /// Rows wanted, clamped to `1..=`[`MAX_LOG_PAGE`].
    pub limit: usize,
    pub source: Option<String>,
    pub provider_id: Option<String>,
}

impl Default for LogPage {
    fn default() -> Self {
        Self {
            before_id: None,
            limit: DEFAULT_LOG_PAGE,
            source: None,
            provider_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusClass {
    /// 2xx and not canceled.
    Success,
    /// 4xx.
    ClientError,
    /// 5xx.
    ServerError,
    /// More than one upstream attempt.
    FailedOver,
    /// `outcome = canceled`.
    Canceled,
}

impl StatusClass {
    /// Text token used by filtered SQLite queries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::ClientError => "client_error",
            Self::ServerError => "server_error",
            Self::FailedOver => "failed_over",
            Self::Canceled => "canceled",
        }
    }
}

/// One paged slice of `llm_request_log` together with the total matching row count.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogPageResult {
    /// Rows on the requested page, newest first (`ORDER BY id DESC`).
    pub entries: Vec<RequestLogEntry>,
    /// Total rows matching the active [`LogFilter`] across the database.
    pub total: u64,
    /// Zero-based row offset used for this page.
    pub offset: u64,
    /// Effective page size used for this page.
    pub limit: usize,
}

/// Which rows `query_log_page`, `count_log` and `clear_log` act on: all the set fields must
/// match. The default (every field unset) selects every row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogFilter {
    pub source: Option<String>,
    pub provider_id: Option<String>,
    pub client_id: Option<String>,
    pub status_class: Option<StatusClass>,
    /// Only rows at or after this time (Unix ms).
    pub since_ms: Option<i64>,
    /// Only rows at or before this time (Unix ms).
    pub until_ms: Option<i64>,
    /// Only rows served by this provider key.
    pub key_id: Option<String>,
    /// Exact match against `requested_model` or `upstream_model`.
    pub model: Option<String>,
    /// Case-insensitive substring match against `requested_model` or `upstream_model`.
    pub model_contains: Option<String>,
    /// Case-insensitive substring match against `run_id`.
    pub run_id_contains: Option<String>,
    /// Only rows where `outcome = 'failure'` or `status >= 400`.
    pub failed_only: bool,
    /// Only rows with tool calls (`finish_reason = 'tool_calls'` or `purpose = 'tool'`).
    pub with_tools: bool,
    /// Free-text search tokens; every token must match at least one searchable column.
    pub search_terms: Vec<String>,
    /// Restrict results to a set of row ids (for example from `body:` capture search).
    /// `Some(empty)` matches zero rows.
    pub ids: Option<Vec<i64>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearReport {
    pub rows: u64,
    /// Bytes (`bytes_total`) of the captures deleted with them.
    pub capture_bytes: u64,
}

/// How many captures are stored and the bytes they occupy: what "Delete captures only" frees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureStats {
    pub count: u64,
    /// The sum of their `bytes_total`.
    pub bytes: u64,
}

/// Prices `entry` at `price` (sets its `cost_micro_usd`, `cost_currency` and `price_source`) and
/// returns the rollup delta of its local day under `tz_offset_minutes`. The one booking rule of
/// every log row (gateway, Chat, probes). A fixed price is known without token counts and only
/// paid by a success; a per-token price bills whatever usage arrived, so a cancelled or broken
/// stream pays for what it received.
#[must_use]
pub fn book_entry(
    entry: &mut RequestLogEntry,
    price: &PriceResolution,
    usage: Option<&CostUsage>,
    tz_offset_minutes: i16,
) -> RollupDelta {
    let outcome = RequestOutcome::from_log(&entry.outcome);
    let failure_kind = entry
        .failure_kind
        .or_else(|| (outcome == RequestOutcome::Canceled).then_some(FailureKind::Cancelled));
    let succeeded = request_succeeded(outcome, Some(entry.status), failure_kind);
    let booked = book_cost(price, usage, succeeded);
    entry.price_source = Some(booked.price_source().to_owned());
    let cost = match booked {
        BookedCost::Known(cost) => {
            entry.cost_micro_usd = Some(cost.micro);
            entry.cost_currency.clone_from(&cost.currency);
            Some(cost)
        }
        // A local runtime costs nothing; unknown stays NULL, never 0.
        BookedCost::Free => {
            entry.cost_micro_usd = Some(0);
            None
        }
        BookedCost::Unknown(_) | BookedCost::NoUsage => None,
    };
    delta_for(&RollupInput {
        ts_ms: entry.ts_ms,
        tz_offset_minutes,
        source: UsageSource::parse(&entry.source).unwrap_or(UsageSource::Gateway),
        provider_id: entry.provider_id.as_deref().unwrap_or(""),
        key_id: entry.key_id.as_deref().unwrap_or(""),
        model: entry
            .upstream_model
            .as_deref()
            .unwrap_or(&entry.requested_model),
        client_id: entry.client_id.as_deref().unwrap_or(""),
        alias: entry.alias.as_deref().unwrap_or(""),
        final_status: Some(entry.status),
        outcome,
        failure_kind,
        usage,
        cost: cost.as_ref(),
        price_currency: price.known().map(|price| price.currency.as_str()),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{RequestLogEntry, book_entry};
    use crate::llm::usage::{CostUsage, Price, PriceResolution, PriceSource, UnknownReason};

    #[test]
    fn a_minimal_payload_parses_with_gateway_and_usd_defaults() {
        let minimal = json!({
            "ts_ms": 5, "protocol_in": "openai_chat", "requested_model": "m", "status": 200,
            "outcome": "success", "stream": false, "latency_ms": 3
        });
        let parsed: RequestLogEntry = serde_json::from_value(minimal).unwrap();
        assert_eq!(parsed.source, "gateway");
        assert_eq!(parsed.cost_currency, "USD");
        assert!(!parsed.has_capture && !parsed.translated && parsed.request_id.is_none());
    }

    fn entry(outcome: &str, status: u16) -> RequestLogEntry {
        RequestLogEntry {
            ts_ms: 0,
            outcome: outcome.to_owned(),
            status,
            upstream_model: Some("m".to_owned()),
            ..RequestLogEntry::default()
        }
    }

    fn cny_price() -> Price {
        Price {
            input_per_mtok: 1.0,
            output_per_mtok: 2.0,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            reasoning_per_mtok: None,
            currency: "CNY".to_owned(),
            source: PriceSource::Override,
            per_call: None,
        }
    }

    #[test]
    fn booking_sets_cost_currency_and_price_source() {
        let usage = CostUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..CostUsage::default()
        };
        let known = PriceResolution::Known(cny_price());
        let mut row = entry("success", 200);
        let delta = book_entry(&mut row, &known, Some(&usage), 0);
        assert_eq!(
            (
                row.cost_micro_usd,
                row.cost_currency.as_str(),
                row.price_source.as_deref()
            ),
            (Some(3_000_000), "CNY", Some("override"))
        );
        assert_eq!(
            (delta.currency.as_str(), delta.cost_micro),
            ("CNY", 3_000_000)
        );

        let mut row = entry("success", 200);
        let delta = book_entry(&mut row, &known, None, 0);
        assert_eq!(
            (
                row.cost_micro_usd,
                row.cost_currency.as_str(),
                row.price_source.as_deref()
            ),
            (None, "USD", Some("no_usage"))
        );
        assert_eq!(delta.currency, "CNY");

        let mut row = entry("success", 200);
        let delta = book_entry(&mut row, &PriceResolution::Free, Some(&usage), 0);
        assert_eq!(
            (
                row.cost_micro_usd,
                row.cost_currency.as_str(),
                row.price_source.as_deref()
            ),
            (Some(0), "USD", Some("local_free"))
        );
        assert_eq!((delta.currency.as_str(), delta.cost_micro), ("", 0));

        for (reason, word) in [
            (UnknownReason::NotInCatalog, "not_in_catalog"),
            (UnknownReason::CatalogWithoutPrice, "catalog_without_price"),
        ] {
            let mut row = entry("success", 200);
            let delta = book_entry(&mut row, &PriceResolution::Unknown(reason), Some(&usage), 0);
            assert_eq!(
                (
                    row.cost_micro_usd,
                    row.cost_currency.as_str(),
                    row.price_source.as_deref()
                ),
                (None, "USD", Some(word))
            );
            assert_eq!(delta.currency, "");
        }
    }

    #[test]
    fn a_cancelled_row_without_a_kind_books_as_cancelled() {
        let mut row = entry("canceled", 200);
        let delta = book_entry(&mut row, &PriceResolution::Free, None, 0);
        assert_eq!(
            (delta.ok_requests, delta.failure_kind.as_deref()),
            (0, Some("cancelled"))
        );
        assert_eq!(row.failure_kind, None);
    }
}
