//! The progress of a scan: planned rows, events and per-phase outcomes.

use serde::{Deserialize, Serialize};

/// State of one phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseState {
    /// The phase produced its result.
    Ok,
    /// The phase ran and failed.
    Failed,
    /// The phase did not run.
    Skipped,
}

/// A phase of the scan; the event stream announces them in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SniffPhase {
    /// Local-runtime fingerprints (loopback and private hosts only).
    Local,
    /// Unauthenticated fingerprint requests.
    Identify,
    /// The model list.
    Models,
    /// Route probes.
    Routes,
    /// Key verdict (no request).
    Key,
    /// Balance adapter.
    Balance,
    /// Opt-in verification with real requests.
    Verify,
}

/// Outcome of one phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseSummary {
    /// The phase.
    pub phase: SniffPhase,
    /// Its state.
    pub state: PhaseState,
    /// Short reason for `Failed` and `Skipped`.
    pub reason: Option<String>,
}

/// One planned request row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedRow {
    /// Row id, also the evidence id of the request.
    pub id: u32,
    /// `GET` or `POST`.
    pub method: String,
    /// Path (no query, no host).
    pub path: String,
    /// What the request is for ("Identify", "Models", "Chat route", ...).
    pub label: String,
    /// Whether the request counts against the budget ("Request 5 of 8").
    pub counted: bool,
}

/// Progress of a scan. Rows may be appended by further `Planned` events when a
/// fall-through candidate or an auth retry is needed. A row that is never sent ends with a
/// `RequestFinished` whose `status` is `None` and whose note starts with `skipped`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SniffEvent {
    /// Rows the scan intends to send, in plan order.
    Planned {
        /// The rows.
        rows: Vec<PlannedRow>,
    },
    /// A row's request left.
    RequestStarted {
        /// Row id.
        id: u32,
    },
    /// A row's request ended (or was skipped).
    RequestFinished {
        /// Row id.
        id: u32,
        /// HTTP status.
        status: Option<u16>,
        /// Latency.
        latency_ms: u64,
        /// Short meaning.
        note: String,
    },
    /// The host's queue pauses after a 429 ("Waiting 3 s (rate limited)").
    Waiting {
        /// Whole seconds of the pause.
        seconds: u32,
    },
    /// A phase started.
    Phase {
        /// The phase.
        phase: SniffPhase,
    },
    /// The report is ready.
    Done {
        /// The report's cache key.
        report_id: String,
    },
}
