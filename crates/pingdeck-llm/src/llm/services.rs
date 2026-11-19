//! In-memory state of the LLM use cases that must outlive a single call.
//!
//! Single responsibility: hold what the sniffer, the balance refresh and the test bench keep
//! between commands — the last sniff reports, the auth styles that worked, the balance throttle
//! and the one probe run that may be active — so the use-case functions stay stateless and the
//! state is created and dropped with the [`LlmRuntime`](super::runtime::LlmRuntime).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::llm::{
    balance::BalanceThrottle,
    sniff::{AuthMemory, SniffReportCache},
    status::ProviderStatusDto,
};
use pingdeck_base::cancel::CancelToken;

/// The probe run in progress (at most one: a later start waits in `probe_queue`).
#[derive(Debug, Clone)]
pub struct ActiveProbeRun {
    /// Id of the run.
    pub run_id: String,
    /// Cancels it.
    pub cancel: CancelToken,
}

/// Shared state of the LLM use cases.
#[derive(Debug, Default)]
pub struct LlmServices {
    /// The last sniff reports (8, 10 minutes), keyed by report id.
    pub sniff_cache: SniffReportCache,
    /// Auth styles that worked per origin and protocol this session.
    pub auth_memory: AuthMemory,
    /// Per-key balance request throttle.
    pub balance_throttle: Mutex<BalanceThrottle>,
    /// The probe run in progress.
    pub active_probe: Mutex<Option<ActiveProbeRun>>,
    /// Probe runs wait here in arrival order, one at a time.
    pub probe_queue: tokio::sync::Mutex<()>,
    /// Per key: `None` while its own model list is being read, the failure's error code (empty
    /// when uncoded) after a failed read; absent once read or never asked.
    pub key_listing: Mutex<HashMap<String, Option<String>>>,
    /// Stops the background tasks of the LLM use cases (the automatic balance refresh, the
    /// catalog sync and the refreshes queued after a key or provider is added: each runs on a
    /// child of this token); `Some` while the balance refresh runs.
    pub balance_scheduler: Mutex<Option<CancelToken>>,
    /// Names of the background jobs running now (`catalog`): a job that finds its name taken
    /// does not start a second copy.
    pub background_flights: Mutex<HashSet<String>>,
    /// Wakes the automatic catalog sync to re-read when it is due (its interval changed, a
    /// manual sync finished).
    pub catalog_wake: tokio::sync::Notify,
    /// The last status computed per provider, to tell a change from a recompute.
    pub provider_status: Mutex<HashMap<String, ProviderStatusDto>>,
    /// The first-request watcher and the Verify secrets of the client use cases.
    pub client_watch: crate::llm::client_watch::ClientWatch,
    /// Providers that refused prompt-cache markers this session: later in-app calls to
    /// them go without. Not stored; the next launch tries once more.
    pub cache_refused: Mutex<std::collections::HashSet<String>>,
}
