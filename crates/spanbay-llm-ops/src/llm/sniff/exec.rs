//! The request executor of a scan: budget, stop rules, events and redacted evidence.
//!
//! Every sniffer request goes through [`Exec::call`]. It enforces the request budget, stops the
//! scan on an HTML or challenge page, keeps at most three requests in flight per host, retries a
//! rate-limited answer once after a back-off shared by the host, records a 3xx as evidence and a
//! warning without following it, applies the response caps and timeouts, and redacts every
//! evidence row. Requests go to the typed origin only: callers pass paths, never hosts.
//!
//! `budget.rs` keeps the ledger of sent and reserved requests; `evidence.rs` writes the evidence
//! rows and applies the response rules (truncation, stop, redirect).

mod budget;
mod evidence;
#[cfg(test)]
mod tests;

use std::{
    borrow::Cow,
    collections::BTreeSet,
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::Semaphore;

pub(super) use self::budget::{
    Spend, TIER_BALANCE, TIER_BALANCE_EXTRA, TIER_IDENTIFY, TIER_KEY_CHECK, TIER_MODELS,
};
pub(super) use self::evidence::{no_description, note_text};
use self::{
    budget::Ledger,
    evidence::{is_rate_limit_body, retry_after},
};
use super::{
    route_verdict::route_conclusion,
    types::{EvidenceItem, PlannedRow, SkippedItem, SniffEvent, SniffPhase, SniffWarning},
};
use crate::llm::{
    safe_http::{SafeHttp, SafeMethod, SafeRequest, SafeResponse, SafeTransportError},
    wire::Protocol,
};
use spanbay_base::{
    cancel::{CancelToken, until_cancelled},
    secrets::redact_url_path,
};
use spanbay_model::events::EventSink;

/// Response cap of every request except the model list.
pub(super) const BODY_CAP: usize = 256 * 1024;
/// Response cap of the model list.
pub(super) const MODELS_BODY_CAP: usize = 2 * 1024 * 1024;
/// In-flight requests per host.
const PER_HOST: usize = 3;

/// The time limits of a scan; tests run the same engine with scaled-down values.
#[derive(Debug, Clone, Copy)]
pub(super) struct Limits {
    /// Connection setup of every request.
    pub connect: Duration,
    /// Identify and route probes.
    pub probe: Duration,
    /// Model list and balance requests.
    pub models: Duration,
    /// Local fingerprints.
    pub local: Duration,
    /// The whole scan.
    pub total: Duration,
    /// Longest wait before the one retry of a 429.
    pub retry_cap: Duration,
    /// Wait when a 429 names no `Retry-After`.
    pub retry_default: Duration,
    /// Replaces the plan's request budget (tests that force a short budget).
    pub budget_override: Option<u32>,
}

impl Limits {
    /// The production limits.
    pub(super) const PRODUCTION: Self = Self {
        connect: Duration::from_secs(3),
        probe: Duration::from_secs(5),
        models: Duration::from_secs(10),
        local: Duration::from_millis(1500),
        total: Duration::from_secs(30),
        retry_cap: Duration::from_secs(5),
        retry_default: Duration::from_secs(1),
        budget_override: None,
    };

    /// Every limit except the whole-scan cap divided by `by` (tests).
    #[cfg(test)]
    pub(super) fn scaled(by: u32) -> Self {
        let p = Self::PRODUCTION;
        Self {
            connect: p.connect / by,
            probe: p.probe / by,
            models: p.models / by,
            local: p.local / by,
            total: p.total,
            retry_cap: p.retry_cap / by,
            retry_default: p.retry_default / by,
            budget_override: None,
        }
    }
}

/// Why a request was not sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SkipReason {
    /// The request budget is used up.
    Budget,
    /// An HTML or challenge page ended the scan, or the guard refused the host.
    Stopped,
    /// The cancel token fired.
    Cancelled,
}

impl SkipReason {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Budget => "budget",
            Self::Stopped => "stopped",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Why the scan stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StopReason {
    /// A 2xx HTML page.
    Html,
    /// A bot-challenge page.
    Challenge,
}

/// What a request is for. It names the request in the `skipped` list and gives its evidence
/// row the phase and the report field it supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Purpose {
    /// A local-runtime fingerprint.
    Local,
    /// An identify request (`/api/status`, Sub2API, VoAPI, LiteLLM).
    Identify,
    /// A model-list page.
    Models,
    /// The empty probe of a protocol's route.
    Route(Protocol),
    /// The opt-in 1-token request of a protocol's route.
    Verify(Protocol),
    /// A balance request.
    Balance,
}

impl Purpose {
    /// The name in the `skipped` list.
    pub(super) fn what(self) -> Cow<'static, str> {
        match self {
            Self::Local => Cow::Borrowed("local"),
            Self::Identify => Cow::Borrowed("identify"),
            Self::Models => Cow::Borrowed("models"),
            Self::Route(protocol) => Cow::Owned(route_conclusion(protocol)),
            Self::Verify(_) => Cow::Borrowed("verify"),
            Self::Balance => Cow::Borrowed("balance"),
        }
    }

    /// The evidence phase.
    pub(super) const fn phase(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Identify => "identify",
            Self::Models => "models",
            Self::Route(_) => "routes",
            Self::Verify(_) => "verify",
            Self::Balance => "balance",
        }
    }

    /// The report field the evidence row supports.
    pub(super) fn conclusion(self) -> String {
        match self {
            Self::Local | Self::Identify => "identity".to_owned(),
            Self::Models => "models".to_owned(),
            Self::Route(protocol) | Self::Verify(protocol) => route_conclusion(protocol),
            Self::Balance => "balance".to_owned(),
        }
    }
}

/// One request to send.
#[derive(Clone)]
pub(super) struct Call {
    /// What the request is for.
    pub purpose: Purpose,
    /// Row label for the progress view.
    pub label: String,
    /// `GET` or `POST`.
    pub method: SafeMethod,
    /// Path and query relative to the typed origin.
    pub path: String,
    /// Headers on top of the scan's base headers (credentials, content type).
    pub headers: Vec<(String, String)>,
    /// `POST` body.
    pub body: Option<Vec<u8>>,
    /// Total time limit.
    pub timeout: Duration,
    /// Response cap.
    pub max_body: usize,
    /// How the request counts against the budget (local fingerprints do not).
    pub spend: Spend,
    /// The planned row to report on; `None` plans a new one.
    pub row: Option<u32>,
    /// An HTML answer is expected on this request and must not stop the scan.
    pub html_ok: bool,
    /// A short sentence about the answer (never a body excerpt); `None` keeps the default.
    pub describe: fn(&SafeResponse) -> Option<String>,
}

impl Call {
    /// A request without extra headers or body, capped at [`BODY_CAP`], that plans its own row,
    /// stops the scan on an HTML answer and describes nothing beyond the status line.
    pub(super) fn new(
        purpose: Purpose,
        label: impl Into<String>,
        method: SafeMethod,
        path: impl Into<String>,
        timeout: Duration,
        spend: Spend,
    ) -> Self {
        Self {
            purpose,
            label: label.into(),
            method,
            path: path.into(),
            headers: Vec::new(),
            body: None,
            timeout,
            max_body: BODY_CAP,
            spend,
            row: None,
            html_ok: false,
            describe: no_description,
        }
    }
}

/// What became of a [`Call`].
#[derive(Debug)]
pub(super) enum Sent {
    /// The request left; `result` is the answer or the transport failure.
    Done {
        /// The answer.
        result: Result<SafeResponse, SafeTransportError>,
    },
    /// The request was not sent.
    Skipped(SkipReason),
}

impl Sent {
    /// The answer, when the request left and got one.
    pub(super) const fn response(&self) -> Option<&SafeResponse> {
        match self {
            Self::Done {
                result: Ok(response),
            } => Some(response),
            _ => None,
        }
    }

    /// The answer by value, when the request left and got one.
    pub(super) fn into_response(self) -> Option<SafeResponse> {
        match self {
            Self::Done {
                result: Ok(response),
            } => Some(response),
            _ => None,
        }
    }
}

/// One row to announce in a `Planned` event.
pub(super) struct RowPlan<'a> {
    /// `GET` or `POST`.
    pub method: &'static str,
    /// The request path (or path template); its query is never shown.
    pub path: Cow<'a, str>,
    /// Row label.
    pub label: &'a str,
    /// Whether the row counts toward the budget shown to the user.
    pub counted: bool,
}

#[derive(Default)]
struct Shared {
    evidence: Vec<EvidenceItem>,
    warnings: Vec<SniffWarning>,
    suggested_url: Option<String>,
    skipped: Vec<SkippedItem>,
    first_error: Option<SafeTransportError>,
    any_response: bool,
    stop: Option<StopReason>,
    auth_budget_gap: bool,
    /// The guard refused the host: every further request would be refused too.
    blocked: bool,
    /// Planned rows without their `RequestFinished` yet.
    open_rows: BTreeSet<u32>,
}

/// Everything the executor collected.
pub(super) struct Collected {
    pub evidence: Vec<EvidenceItem>,
    pub warnings: Vec<SniffWarning>,
    pub suggested_url: Option<String>,
    pub skipped: Vec<SkippedItem>,
    pub first_error: Option<SafeTransportError>,
    pub any_response: bool,
    pub stop: Option<StopReason>,
    pub auth_budget_gap: bool,
}

/// The executor of one scan.
pub(super) struct Exec<'a> {
    http: &'a dyn SafeHttp,
    cancel: &'a CancelToken,
    sink: &'a dyn EventSink<SniffEvent>,
    pub(super) limits: Limits,
    budget: u32,
    ledger: Mutex<Ledger>,
    next_row: AtomicU32,
    pub(super) origin: String,
    typed_mount: String,
    allow_private: bool,
    base_headers: Vec<(String, String)>,
    gate: Semaphore,
    secrets: Vec<String>,
    shared: Mutex<Shared>,
    /// The shared back-off of the host after a 429.
    pause_until: Mutex<Option<tokio::time::Instant>>,
    /// The destination is a name resolved by a proxy.
    proxy_dns: bool,
    /// `proxy_dns` was written to the first evidence row already.
    proxy_noted: AtomicBool,
}

/// Settings of an [`Exec`].
pub(super) struct ExecConfig<'a> {
    pub origin: String,
    pub typed_mount: String,
    pub allow_private: bool,
    pub budget: u32,
    pub limits: Limits,
    pub base_headers: Vec<(String, String)>,
    pub secrets: Vec<String>,
    pub proxy_dns: bool,
    pub http: &'a dyn SafeHttp,
    pub cancel: &'a CancelToken,
    pub sink: &'a dyn EventSink<SniffEvent>,
}

/// Locks `mutex`, recovering the data of a poisoned lock: every guarded value stays consistent
/// between statements, so a panic elsewhere never leaves it half-written.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `path` without its query and fragment.
fn path_without_query(path: &str) -> &str {
    path.split(['?', '#']).next().unwrap_or_default()
}

/// Whole seconds of `wait`, rounded up and saturating at `u32::MAX`.
fn ceil_seconds(wait: Duration) -> u32 {
    let seconds = wait
        .as_secs()
        .saturating_add(u64::from(wait.subsec_nanos() > 0));
    u32::try_from(seconds).unwrap_or(u32::MAX)
}

impl<'a> Exec<'a> {
    pub(super) fn new(config: ExecConfig<'a>) -> Self {
        Self {
            http: config.http,
            cancel: config.cancel,
            sink: config.sink,
            limits: config.limits,
            budget: config.limits.budget_override.unwrap_or(config.budget),
            ledger: Mutex::default(),
            next_row: AtomicU32::new(0),
            origin: config.origin,
            typed_mount: config.typed_mount,
            allow_private: config.allow_private,
            base_headers: config.base_headers,
            gate: Semaphore::new(PER_HOST),
            secrets: config.secrets,
            shared: Mutex::default(),
            pause_until: Mutex::new(None),
            proxy_dns: config.proxy_dns,
            proxy_noted: AtomicBool::new(false),
        }
    }

    fn shared(&self) -> MutexGuard<'_, Shared> {
        lock(&self.shared)
    }

    fn emit(&self, event: SniffEvent) {
        if self.sink.is_active() {
            self.sink.emit(event);
        }
    }

    /// Whether requests may reach private addresses (a local host, or a confirmed private
    /// resolution).
    pub(super) const fn allow_private(&self) -> bool {
        self.allow_private
    }

    /// Whether an HTML or challenge page ended the scan.
    pub(super) fn stop_reason(&self) -> Option<StopReason> {
        self.shared().stop
    }

    /// The scan's cancel token.
    pub(super) const fn cancel(&self) -> &CancelToken {
        self.cancel
    }

    /// Adds a warning once.
    pub(super) fn warn(&self, warning: SniffWarning) {
        let mut shared = self.shared();
        if !shared.warnings.contains(&warning) {
            shared.warnings.push(warning);
        }
    }

    /// Records a request that was left out, once per `what` and `reason`.
    pub(super) fn skip(&self, what: &str, reason: &str) {
        let mut shared = self.shared();
        let item = SkippedItem {
            what: what.to_owned(),
            reason: reason.to_owned(),
        };
        if !shared.skipped.contains(&item) {
            shared.skipped.push(item);
        }
    }

    /// Notes that a `401` got no second credential variant because the budget was gone.
    pub(super) fn note_auth_budget_gap(&self) {
        self.shared().auth_budget_gap = true;
    }

    /// Announces the start of a phase.
    pub(super) fn announce(&self, phase: SniffPhase) {
        self.emit(SniffEvent::Phase { phase });
    }

    /// Announces the finished report.
    pub(super) fn announce_done(&self, report_id: &str) {
        self.emit(SniffEvent::Done {
            report_id: report_id.to_owned(),
        });
    }

    /// `future` unless the cancel token fires first.
    pub(super) async fn until_cancelled<F: Future>(&self, future: F) -> Option<F::Output> {
        until_cancelled(Some(self.cancel), future).await
    }

    /// The progress row of `row` with a fresh id; the path is shown without its query, with
    /// opaque segments redacted.
    fn planned_row(&self, row: &RowPlan<'_>) -> PlannedRow {
        PlannedRow {
            id: self.next_row.fetch_add(1, Ordering::SeqCst),
            method: row.method.to_owned(),
            path: self.sanitize(&redact_url_path(path_without_query(&row.path))),
            label: row.label.to_owned(),
            counted: row.counted,
        }
    }

    /// Announces `rows` in one `Planned` event and returns their ids. An empty slice still
    /// sends the (empty) event.
    pub(super) fn plan(&self, rows: &[RowPlan<'_>]) -> Vec<u32> {
        let planned: Vec<PlannedRow> = rows.iter().map(|row| self.planned_row(row)).collect();
        let ids: Vec<u32> = planned.iter().map(|row| row.id).collect();
        self.shared().open_rows.extend(ids.iter().copied());
        self.emit(SniffEvent::Planned { rows: planned });
        ids
    }

    /// Announces one row in its own `Planned` event and returns its id.
    pub(super) fn plan_one(&self, row: &RowPlan<'_>) -> u32 {
        let planned = self.planned_row(row);
        let id = planned.id;
        self.shared().open_rows.insert(id);
        self.emit(SniffEvent::Planned {
            rows: vec![planned],
        });
        id
    }

    /// Ends every planned row that has not finished, as skipped for `reason`, in row order (the
    /// whole-scan cap dropped the requests still queued or in flight).
    pub(super) fn finish_open_rows(&self, reason: &str) {
        let open = std::mem::take(&mut self.shared().open_rows);
        for row in open {
            self.emit_skipped(row, reason);
        }
    }

    /// Ends a planned row that was never sent.
    fn finish_skipped(&self, row: u32, reason: &str) {
        self.shared().open_rows.remove(&row);
        self.emit_skipped(row, reason);
    }

    fn emit_skipped(&self, row: u32, reason: &str) {
        self.emit(SniffEvent::RequestFinished {
            id: row,
            status: None,
            latency_ms: 0,
            note: format!("skipped ({reason})"),
        });
    }

    /// Waits out the host's shared back-off, if one is running.
    async fn wait_pause(&self) {
        loop {
            let until = *lock(&self.pause_until);
            let Some(until) = until else { return };
            let now = tokio::time::Instant::now();
            if until <= now {
                return;
            }
            if until_cancelled(Some(self.cancel), tokio::time::sleep(until - now))
                .await
                .is_none()
            {
                return;
            }
        }
    }

    /// Starts the host's back-off unless one is already running (three simultaneous 429s cause
    /// one pause) and tells the progress view.
    fn start_pause(&self, wait: Duration) {
        let mut until = lock(&self.pause_until);
        let now = tokio::time::Instant::now();
        if until.is_some_and(|at| at > now) {
            return;
        }
        *until = Some(now + wait);
        drop(until);
        self.emit(SniffEvent::Waiting {
            seconds: ceil_seconds(wait),
        });
    }

    /// Sends `spec`, retrying a rate-limited answer once.
    pub(super) async fn call(&self, spec: Call) -> Sent {
        let first = self.call_once(&spec).await;
        let Some(response) = first.response() else {
            return first;
        };
        if response.status != 429 && !is_rate_limit_body(response) {
            return first;
        }
        let wait = retry_after(response)
            .unwrap_or(self.limits.retry_default)
            .min(self.limits.retry_cap);
        self.start_pause(wait);
        self.wait_pause().await;
        if self.cancel.is_cancelled() {
            return first;
        }
        let retry = Call {
            row: None,
            label: format!("{} (retry)", spec.label),
            spend: spec.spend.retry(),
            ..spec
        };
        match self.call_once(&retry).await {
            Sent::Skipped(_) => first,
            done => {
                if done.response().is_some_and(|r| r.status == 429) {
                    self.warn(SniffWarning::RateLimited);
                }
                done
            }
        }
    }

    async fn call_once(&self, spec: &Call) -> Sent {
        if self.cancel.is_cancelled() {
            return Sent::Skipped(SkipReason::Cancelled);
        }
        let row = match spec.row {
            Some(row) => row,
            None => self.plan_one(&RowPlan {
                method: spec.method.as_str(),
                path: Cow::Borrowed(&spec.path),
                label: &spec.label,
                counted: matches!(spec.spend, Spend::Budgeted { .. }),
            }),
        };
        self.wait_pause().await;
        let skip = if (self.stop_reason().is_some() && !spec.html_ok) || self.shared().blocked {
            Some(SkipReason::Stopped)
        } else if !self.reserve(spec.spend) {
            Some(SkipReason::Budget)
        } else {
            None
        };
        if let Some(reason) = skip {
            self.skip(&spec.purpose.what(), reason.as_str());
            self.finish_skipped(row, reason.as_str());
            return Sent::Skipped(reason);
        }
        let Some(Ok(permit)) = until_cancelled(Some(self.cancel), self.gate.acquire()).await else {
            return Sent::Skipped(SkipReason::Cancelled);
        };
        self.emit(SniffEvent::RequestStarted { id: row });
        let request = SafeRequest {
            method: spec.method,
            url: format!("{}{}", self.origin, spec.path),
            headers: self.merged_headers(&spec.headers),
            body: spec.body.clone(),
            timeout: spec.timeout,
            connect_timeout: self.limits.connect.min(spec.timeout),
            max_body: spec.max_body,
            allow_private: self.allow_private,
        };
        let started = Instant::now();
        let result = self.http.send(request, self.cancel).await;
        drop(permit);
        if matches!(result, Err(SafeTransportError::Cancelled)) {
            return Sent::Skipped(SkipReason::Cancelled);
        }
        self.record(row, spec, &result, started.elapsed());
        Sent::Done { result }
    }

    /// `own` followed by every base header whose name is not used yet (case-insensitive).
    fn merged_headers(&self, own: &[(String, String)]) -> Vec<(String, String)> {
        let mut headers = own.to_vec();
        for (name, value) in &self.base_headers {
            if !headers
                .iter()
                .any(|(present, _)| present.eq_ignore_ascii_case(name))
            {
                headers.push((name.clone(), value.clone()));
            }
        }
        headers
    }

    /// Everything collected; the executor is done.
    pub(super) fn collect(&self) -> Collected {
        let mut shared = self.shared();
        let mut evidence = std::mem::take(&mut shared.evidence);
        evidence.sort_by_key(|item| item.id);
        Collected {
            evidence,
            warnings: std::mem::take(&mut shared.warnings),
            suggested_url: shared.suggested_url.take(),
            skipped: std::mem::take(&mut shared.skipped),
            first_error: shared.first_error.take(),
            any_response: shared.any_response,
            stop: shared.stop,
            auth_budget_gap: shared.auth_budget_gap,
        }
    }
}
