//! The gateway's status as the UI reads it.
//!
//! Single responsibility: [`GatewayStatus`], its [`GatewayState`] vocabulary and the
//! [`GatewayError`] a failed start leaves behind. Assembling a status from the
//! manager's internals lives in `GatewayManager::status`.

use serde::{Deserialize, Serialize};

use crate::llm::capture::CaptureMode;

/// Where the gateway is in its life cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayState {
    /// Listening.
    Running,
    /// Not listening and nothing went wrong.
    Stopped,
    /// Binding the port.
    Starting,
    /// No longer accepting; in-flight requests are finishing.
    Stopping,
    /// The last start failed; see `last_error`.
    Error,
}

/// What a [`crate::llm::events::LlmEvent::GatewayNotice`] is about: the UI words it by
/// this code and keeps the banner until the state it names recovers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayNoticeCode {
    /// Local network access was switched off: no active client is left.
    LanOff,
    /// The machine's local network address changed while serving the LAN.
    LanAddressChanged,
    /// A start failed; the text is the reason (the status keeps `last_error`).
    StartFailed,
    /// Stored settings turn tokens off next to extra hosts: tokens are required anyway.
    TokensForced,
    /// Restricted clients skip gateway fallbacks outside their allow-list.
    FallbacksNarrowed,
    /// A client reached (or came close to) one of its limits; the text names it.
    ClientLimit,
}

/// One name that reaches the gateway on the local network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanAddress {
    /// An IPv4 address or `<hostname>.local`.
    pub host: String,
    /// The interface that carries the address (`en0`); `None` for the `.local` name.
    pub interface: Option<String>,
    /// A private-range address (or the `.local` name): listed first; the rest are shown under
    /// "Show other addresses".
    pub private: bool,
}

/// Why the last start failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayError {
    /// `port_in_use`, `cannot_bind`, `database_unavailable`, `invalid_settings`, `no_entropy`
    /// (no OS randomness for tokens) or `start_failed`.
    pub code: String,
    /// The reason in one line.
    pub message: String,
    /// The port that was being bound.
    pub port: u16,
}

/// Whether the gateway runs, where, and how it is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayStatus {
    /// Life-cycle state.
    pub state: GatewayState,
    /// A short human line for [`GatewayState::Error`] (`port 4141 is in use`); `None` otherwise.
    pub message: Option<String>,
    /// `state` is [`GatewayState::Running`] (kept for callers that only need the flag).
    pub running: bool,
    /// Listening on the local network (`0.0.0.0`) rather than `127.0.0.1` only; while not
    /// running, the configured scope.
    pub lan: bool,
    /// Bound port (0 when not running).
    pub port: u16,
    /// Base URL clients use, e.g. `http://127.0.0.1:4141`; on the LAN the first private address
    /// (empty when not running).
    pub base_url: String,
    /// Names that reach the gateway when listening on the LAN, private-range first (empty
    /// otherwise).
    pub addresses: Vec<LanAddress>,
    /// Start time in Unix milliseconds (0 when not running).
    pub started_at_ms: i64,
    /// Seconds since the start.
    pub uptime_s: u64,
    /// Requests handled since the start (refusals are counted in `auth_failures` and the log).
    pub requests: u64,
    /// Requests being answered now, streams included.
    pub in_flight: u64,
    /// Open client connections (a restart drops them).
    pub open_connections: u64,
    /// Connections refused over the 256 cap since the start.
    pub rejected_connections: u64,
    /// Log rows lost since the start because the queue was full or a write failed.
    pub log_dropped: u64,
    /// Captures lost since the start because the capture queue was full (their rows are kept).
    pub captures_dropped: u64,
    /// Refused token attempts since the start.
    pub auth_failures: u64,
    /// Active clients (not revoked).
    pub clients: u32,
    /// Whether clients must present a token.
    pub token_required: bool,
    /// What the log captures (`ai.gateway.capture_mode`).
    pub capture_mode: CaptureMode,
    /// `ai.gateway.enabled_on_start`.
    pub enabled_on_start: bool,
    /// Why the last start failed; cleared by the next successful start.
    pub last_error: Option<GatewayError>,
    /// A message that explains a change: set on the event that announces it (LAN switched off),
    /// and on every status of a run whose start found an upgrade note.
    pub notice: Option<String>,
}

/// What a restart did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartReport {
    /// Open client connections the restart dropped (counted when it began).
    pub dropped_connections: u64,
    /// The status after the restart.
    pub status: GatewayStatus,
}
