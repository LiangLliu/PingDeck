//! The telemetry-free diagnostics bundle: snapshots the caller collected, turned into one JSON
//! text a user can hand to someone else. Pure: no network, no clipboard, no database.
//!
//! Free-text fields (`last_start_error`, row errors) must already be redacted by the caller; this
//! module is the last gate and refuses to produce text in which redaction would still change
//! anything (rules R1-R10, fail closed).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::CaptureMode;
use super::model::{CapturedBody, CapturedExchange, CapturedFrames};
use super::stored::bare_url;
use crate::llm::health::HealthSnapshot;
use pingdeck_base::secrets::redact_json;

/// The bundle never exceeds this many bytes.
pub const DIAGNOSTICS_MAX_BYTES: usize = 256 * 1024;
/// The most log rows a bundle lists (the newest ones).
pub const DIAGNOSTICS_MAX_ROWS: usize = 50;
/// Said next to included bodies.
const BODIES_NOTICE: &str = "This includes prompts and replies.";

/// The `app` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDiag {
    /// App version.
    pub version: String,
    /// Operating system.
    pub os: String,
    /// CPU architecture.
    pub arch: String,
    /// UI language.
    pub language: String,
}

/// The `gateway` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayDiag {
    /// `running`, `stopped`, `starting`, `error`.
    pub state: String,
    /// `loopback` or `lan`.
    pub bind_scope: String,
    /// Listening port.
    pub port: u16,
    /// Whether clients must present a token.
    pub token_required: bool,
    /// Whether LAN access is enabled.
    pub lan: bool,
    /// How many extra Host names are allowed (a count, never the names).
    pub allowed_hosts: u32,
    /// The capture policy.
    pub capture_policy: CaptureMode,
    /// Seconds since the gateway started.
    pub uptime_s: Option<u64>,
    /// Requests in flight.
    pub in_flight: u32,
    /// Log rows dropped under load.
    pub rows_dropped: u64,
    /// Captures dropped under load (their rows were kept).
    pub captures_dropped: u64,
    /// Failed authentications since start.
    pub auth_failures: u64,
    /// The last start error text (already redacted).
    pub last_start_error: Option<String>,
}

/// One entry of the `providers` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDiag {
    /// Provider id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Protocols the provider speaks.
    pub protocols: Vec<String>,
    /// Enabled keys.
    pub enabled_keys: u32,
    /// Enabled models.
    pub enabled_models: u32,
    /// Latest probe status word.
    pub probe_status: Option<String>,
    /// Base URL as stored; shown (without user info and query) only when the caller asks for
    /// provider addresses, never otherwise.
    pub base_url: Option<String>,
}

/// One entry of the `fallbacks` section: a gateway rule (a requested model with its fallback
/// chain; only gateway requests use it, the in-app agent brings its own chains).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackDiag {
    /// The primary model (`provider_id/model`).
    pub model: String,
    /// Its fallbacks in order (`provider_id/model`).
    pub fallbacks: Vec<String>,
}

/// One entry of the `clients` section: no token prefix, no hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientDiag {
    /// Client name.
    pub name: String,
    /// What the client may call (`all models`, `2 models`, …).
    pub allow_summary: String,
    /// Creation time.
    pub created_at_ms: i64,
    /// Last use.
    pub last_used_at_ms: Option<i64>,
    /// Whether the token is revoked.
    pub revoked: bool,
}

/// The `log_summary` section: the last 24 hours.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LogSummaryDiag {
    /// Rows by status class (`2xx`, `4xx`, `5xx`, `canceled`).
    pub by_status_class: BTreeMap<String, u64>,
    /// Rows by provider name.
    pub by_provider: BTreeMap<String, u64>,
    /// Rows by source.
    pub by_source: BTreeMap<String, u64>,
    /// Median time to first token.
    pub ttft_p50_ms: Option<u64>,
    /// 95th percentile time to first token.
    pub ttft_p95_ms: Option<u64>,
    /// Median latency.
    pub latency_p50_ms: Option<u64>,
    /// 95th percentile latency.
    pub latency_p95_ms: Option<u64>,
}

/// One entry of `recent_rows`: the export columns, no bodies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowDiag {
    /// `rq_<16 hex>`.
    pub request_id: String,
    /// Arrival time.
    pub ts_ms: i64,
    /// `gateway`, `agent`, `probe`, `api`.
    pub source: String,
    /// Caller name.
    pub client_name: Option<String>,
    /// Matched alias.
    pub alias: Option<String>,
    /// Requested model.
    pub requested_model: String,
    /// Served model.
    pub upstream_model: Option<String>,
    /// Serving provider name.
    pub provider_name: Option<String>,
    /// Inbound protocol.
    pub protocol_in: String,
    /// Upstream protocol.
    pub protocol_out: Option<String>,
    /// HTTP status.
    pub status: u16,
    /// `success`, `failure`, `canceled`.
    pub outcome: String,
    /// Streamed.
    pub stream: bool,
    /// Protocols differ.
    pub translated: bool,
    /// Total latency.
    pub latency_ms: u64,
    /// Time to first token.
    pub ttft_ms: Option<u64>,
    /// Prompt tokens.
    pub input_tokens: Option<u64>,
    /// Output tokens.
    pub output_tokens: Option<u64>,
    /// Cost in millionths.
    pub cost_micro: Option<i64>,
    /// Cost currency.
    pub cost_currency: Option<String>,
    /// Failure kind word.
    pub failure_kind: Option<String>,
    /// Error text (already redacted).
    pub error: Option<String>,
}

/// Everything a bundle is built from. The caller collects the snapshots; nothing here reads state.
/// `Debug` shows how many needles there are and whether a capture is selected, never their
/// content.
#[derive(Clone, PartialEq)]
pub struct DiagnosticsInput {
    /// App facts.
    pub app: AppDiag,
    /// Gateway state.
    pub gateway: GatewayDiag,
    /// Providers.
    pub providers: Vec<ProviderDiag>,
    /// Gateway rules: the fallback chain of each requested model.
    pub fallbacks: Vec<FallbackDiag>,
    /// Gateway clients.
    pub clients: Vec<ClientDiag>,
    /// Circuit snapshot, as the health book reports it.
    pub health: Vec<HealthSnapshot>,
    /// Last-24-hours summary.
    pub log_summary: LogSummaryDiag,
    /// Log rows, newest first (the drawer passes the selected row plus the 20 before it).
    pub recent_rows: Vec<RowDiag>,
    /// The "Include provider addresses" switch (default off).
    pub include_provider_addresses: bool,
    /// The drawer's "Include this request's bodies" switch: the selected row's stored capture.
    pub selected_capture: Option<CapturedExchange>,
    /// Exact secret values the check must not find.
    pub needles: Vec<String>,
}

impl fmt::Debug for DiagnosticsInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiagnosticsInput")
            .field("app", &self.app)
            .field("gateway", &self.gateway)
            .field("providers", &self.providers)
            .field("fallbacks", &self.fallbacks)
            .field("clients", &self.clients)
            .field("health", &self.health)
            .field("log_summary", &self.log_summary)
            .field("recent_rows", &self.recent_rows)
            .field(
                "include_provider_addresses",
                &self.include_provider_addresses,
            )
            .field("selected_capture", &self.selected_capture.is_some())
            .field("needles", &self.needles.len())
            .finish()
    }
}

/// Why no bundle was produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticsRefusal {
    /// Redaction rules still matched the final text; the caller logs the event.
    Unsafe,
    /// The bundle exceeds 256 KB even without log rows.
    TooLarge,
}

impl DiagnosticsRefusal {
    /// The message shown to the user.
    #[must_use]
    pub fn message(&self) -> &'static str {
        match self {
            Self::Unsafe => "Diagnostics could not be made safe. Nothing was copied.",
            Self::TooLarge => {
                "Diagnostics are larger than 256 KB even without log rows. Nothing was copied."
            }
        }
    }
}

impl fmt::Display for DiagnosticsRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for DiagnosticsRefusal {}

fn provider_value(provider: &ProviderDiag, include_address: bool) -> Value {
    let mut value = json!({
        "id": provider.id,
        "name": provider.name,
        "protocols": provider.protocols,
        "enabled_keys": provider.enabled_keys,
        "enabled_models": provider.enabled_models,
        "probe_status": provider.probe_status,
    });
    if let (true, Some(url), Some(object)) = (
        include_address,
        provider.base_url.as_deref(),
        value.as_object_mut(),
    ) {
        object.insert("base_url".to_owned(), Value::String(bare_url(url)));
    }
    value
}

fn body_value(body: Option<&CapturedBody>) -> Value {
    body.map_or(
        Value::Null,
        |body| json!({"text": body.text, "truncated": body.truncated, "elided": body.elided}),
    )
}

fn frames_value(frames: Option<&CapturedFrames>) -> Value {
    frames.map_or(Value::Null, |frames| json!(frames))
}

/// The selected row's stored bodies and frames (headers stay out: they are tagged, not text).
fn capture_value(capture: &CapturedExchange) -> Value {
    json!({
        "notice": BODIES_NOTICE,
        "request_log_id": capture.request_log_id,
        "client_request": body_value(capture.client_req_body.as_ref()),
        "upstream_request": body_value(capture.upstream_req_body.as_ref()),
        "upstream_response": body_value(capture.upstream_resp_body.as_ref()),
        "client_response": body_value(capture.client_resp_body.as_ref()),
        "upstream_frames": frames_value(capture.upstream_frames.as_ref()),
        "client_frames": frames_value(capture.client_frames.as_ref()),
    })
}

/// Every section except `recent_rows`.
fn sections(input: &DiagnosticsInput) -> Map<String, Value> {
    let mut root = Map::new();
    root.insert("app".to_owned(), json!(input.app));
    root.insert("gateway".to_owned(), json!(input.gateway));
    root.insert(
        "providers".to_owned(),
        Value::Array(
            input
                .providers
                .iter()
                .map(|p| provider_value(p, input.include_provider_addresses))
                .collect(),
        ),
    );
    root.insert("fallbacks".to_owned(), json!(input.fallbacks));
    root.insert("clients".to_owned(), json!(input.clients));
    root.insert("health".to_owned(), json!(input.health));
    root.insert("log_summary".to_owned(), json!(input.log_summary));
    if let Some(capture) = &input.selected_capture {
        root.insert("capture".to_owned(), capture_value(capture));
    }
    root
}

/// Slack per row for the indentation of pretty-printing when estimating the size.
const ROW_INDENT_SLACK: usize = 24;

/// Sorts object keys recursively so the diagnostics bundle stays deterministic regardless of
/// `serde_json/preserve_order` feature unification across the workspace.
fn sort_json_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .into_iter()
                .map(|(k, v)| (k, sort_json_keys(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_json_keys).collect()),
        other => other,
    }
}

/// The pretty-printed bundle with `rows` as `recent_rows`.
fn render(root: &mut Map<String, Value>, rows: &[Value]) -> String {
    root.insert("recent_rows".to_owned(), Value::Array(rows.to_vec()));
    let sorted = sort_json_keys(Value::Object(root.clone()));
    // Serialising a JSON value to a string cannot fail.
    serde_json::to_string_pretty(&sorted).unwrap_or_default()
}

/// Builds the bundle text.
///
/// Sections: `app`, `gateway`, `providers`, `fallbacks`, `clients`, `health`, `log_summary`,
/// `recent_rows` (the newest [`DIAGNOSTICS_MAX_ROWS`] at most, the oldest dropped first until the
/// text fits `DIAGNOSTICS_MAX_BYTES`) and, only when `selected_capture` is set, `capture`.
/// Provider `base_url` appears only with `include_provider_addresses`.
///
/// # Errors
/// [`DiagnosticsRefusal::Unsafe`] when rules R1-R10 or an exact needle still match the final text;
/// [`DiagnosticsRefusal::TooLarge`] when it does not fit even without rows.
pub fn build_diagnostics(input: &DiagnosticsInput) -> Result<String, DiagnosticsRefusal> {
    let rows: Vec<Value> = input
        .recent_rows
        .iter()
        .take(DIAGNOSTICS_MAX_ROWS)
        .map(|row| json!(row))
        .collect();
    let mut root = sections(input);
    let base = render(&mut root, &[]).len();
    if base > DIAGNOSTICS_MAX_BYTES {
        return Err(DiagnosticsRefusal::TooLarge);
    }
    // Estimate how many rows fit, then take the most rows up to the estimate that really fit. The
    // rendered length grows with every row and zero rows fit (checked above), so a binary search
    // finds the same count as dropping one row at a time.
    let mut estimate = 0;
    let mut used = base;
    for row in &rows {
        used += row.to_string().len() + ROW_INDENT_SLACK;
        if used > DIAGNOSTICS_MAX_BYTES {
            break;
        }
        estimate += 1;
    }
    let mut text = render(&mut root, &rows[..estimate]);
    if text.len() > DIAGNOSTICS_MAX_BYTES {
        // Invariant: `low` rows fit, `high` rows do not.
        let (mut low, mut high) = (0, estimate);
        while high - low > 1 {
            let mid = low + (high - low) / 2;
            if render(&mut root, &rows[..mid]).len() <= DIAGNOSTICS_MAX_BYTES {
                low = mid;
            } else {
                high = mid;
            }
        }
        text = render(&mut root, &rows[..low]);
    }
    // Check value by value: a header-shaped string ends at its own quote here, while over the
    // pretty-printed text the rule would run on to the end of the line and mangle the JSON.
    let shown = Value::Object(root);
    if redact_json(&shown, &input.needles) != shown {
        return Err(DiagnosticsRefusal::Unsafe);
    }
    Ok(text)
}
