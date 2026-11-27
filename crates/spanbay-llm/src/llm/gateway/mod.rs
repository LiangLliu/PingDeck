//! The local gateway: an HTTP server that exposes configured providers and models through the
//! OpenAI, Anthropic and Gemini API shapes.
//!
//! Single responsibility: own the server lifecycle and the status the UI shows.
//! [`GatewayManager`] binds `127.0.0.1:<port>` (`0.0.0.0` with `ai.gateway.serve_lan`), runs the
//! accept loop on its own small tokio runtime (so UI work can never starve it) and stops it:
//! gracefully ([`GatewayManager::stop`]: the listener closes at once, in-flight requests finish
//! for at most 10 s, the rest are aborted and logged `canceled`), or at once
//! ([`GatewayManager::stop_now`]); on app quit the drain is 3 s ([`GatewayManager::shutdown`]).
//! Request handling lives in `handlers` and `chat`, streaming in `pump`.
//!
//! Settings are read per request (`LlmRuntime::settings`). A port or `serve_lan` change needs a
//! new listener: [`GatewayManager::apply_settings`] restarts a running gateway when the
//! configured port or interface scope differs from the bound one; a failed rebind leaves the
//! gateway stopped with `last_error` set. LAN serving needs `require_token` and one active
//! client; when the last active client goes, [`GatewayManager::on_clients_changed`] turns LAN
//! off and restarts on loopback. Extra allowed hosts need tokens too; the status of a
//! start says so when the stored settings still turn them off (`startup`).
//!
//! Access control, in order, for every request (`handlers::handle`): `guard` (Host, then Origin),
//! the route (404), `auth` (token, 401, rate limited per peer), the allow-list (403), body
//! limits. `GET /health` is the one endpoint that needs no token: it only says that SpanBay is
//! listening, but it passes Host/Origin like everything else. Every response carries
//! `x-spanbay-request-id`; refusals and finished requests become log rows through `logwriter`.
//!
//! Every state change is announced as an [`LlmEvent::GatewayStatus`]; stored log rows as batched
//! [`LlmEvent::GatewayLogBatch`] events, both on the runtime's event hub.

mod auth;
mod calls;
mod chat;
mod ending;
mod errors;
mod exchange;
mod fields;
mod guard;
mod handlers;
mod journal;
mod lan;
mod limiter;
pub(crate) mod logwriter;
mod passthrough;
mod pump;
mod quota;
pub mod replay;
mod reply;
mod server;
mod socket;
mod startup;
mod status;
mod tz;
mod walk;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;
use tokio::sync::watch;

use crate::llm::events::LlmEvent;
use crate::llm::runtime::{LlmRuntime, now_ms};
use spanbay_base::error::AppError;
use spanbay_model::settings::AiGatewaySettings;

use lan::LanNames;
use server::{Control, Shared};

pub(crate) use auth::SeenTokens;
pub use status::{
    GatewayError, GatewayNoticeCode, GatewayState, GatewayStatus, LanAddress, RestartReport,
};
pub use tz::TzOffset;

/// How long a Stop lets in-flight requests finish.
const DRAIN: Duration = Duration::from_secs(10);
/// The same when the app quits.
const QUIT_DRAIN: Duration = Duration::from_secs(3);
/// Grace after the drain limit for the aborted tasks to unwind before Stop gives up waiting.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// How often a draining Stop looks at the in-flight count to announce it.
const DRAIN_POLL: Duration = Duration::from_millis(200);

/// How often a LAN gateway looks for a change of the machine's addresses.
const ADDRESS_CHECK: Duration = Duration::from_secs(5);

/// The `notice` of the status event that announces LAN being switched off.
const LAN_OFF_NOTICE: &str = "Local network turned off: no active clients";

/// Limits of the request path and the lifecycle; the tests lower them instead of sending 32 MiB,
/// opening 256 connections or waiting 10 s.
#[derive(Debug, Clone, Copy)]
struct Limits {
    /// Largest request body.
    max_body: usize,
    /// How long reading one request body may take.
    body_timeout: Duration,
    /// How long Stop lets in-flight requests finish.
    drain: Duration,
    /// The same when the app quits.
    quit_drain: Duration,
    /// Connections served at once.
    max_connections: usize,
    /// Failed token attempts per peer and minute before 429.
    auth_failures_per_minute: usize,
    /// Failed-authentication log rows are written at most this often per peer.
    auth_row_interval: Duration,
    /// Log jobs that may wait for the writer.
    log_queue: usize,
    /// How often a LAN gateway looks for a change of the machine's addresses (within 10 s).
    address_check: Duration,
    /// Smallest first-byte wait of a request that enables reasoning.
    reasoning_first_byte: Duration,
    /// How long a client may leave response bytes unaccepted before its connection is dropped.
    write_stall: Duration,
    /// Request-body bytes held at once by the requests being read and served (declared
    /// `Content-Length`, else `max_body`); over it a request is refused with 503.
    body_budget: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body: server::GATEWAY_MAX_BODY_BYTES,
            body_timeout: handlers::BODY_TIMEOUT,
            drain: DRAIN,
            quit_drain: QUIT_DRAIN,
            max_connections: server::MAX_CONNECTIONS,
            auth_failures_per_minute: limiter::MAX_FAILURES,
            auth_row_interval: limiter::ROW_INTERVAL,
            log_queue: logwriter::LOG_QUEUE,
            address_check: ADDRESS_CHECK,
            reasoning_first_byte: crate::llm::failover::REASONING_FIRST_BYTE,
            write_stall: socket::WRITE_STALL,
            body_budget: server::BODY_BUDGET_BYTES,
        }
    }
}

/// A started gateway.
struct Live {
    runtime: Runtime,
    control: Arc<watch::Sender<Control>>,
    closed: std::sync::mpsc::Receiver<()>,
    shared: Arc<Shared>,
    started_at_ms: i64,
    /// What an upgrade made stricter for this configuration ([`startup::notices`]): every status
    /// of this run carries it, so a view opened later still says why.
    startup_notice: Option<String>,
}

/// What a stopping gateway keeps visible while its [`Live`] is owned by the thread stopping it.
struct Draining {
    control: Arc<watch::Sender<Control>>,
    shared: Arc<Shared>,
    started_at_ms: i64,
}

enum Phase {
    Stopped,
    Starting,
    Running(Live),
    Stopping(Draining),
}

struct Inner {
    phase: Phase,
    /// Why the last start failed; cleared by the next successful start.
    last_error: Option<GatewayError>,
}

/// What a caller of a stop found.
enum Begin {
    /// This caller stops the gateway.
    Mine(Live),
    /// Another caller is stopping it.
    Joined,
    /// Nothing to stop.
    Idle,
}

/// Owns the (at most one) running gateway.
pub struct GatewayManager {
    inner: Mutex<Inner>,
    /// Signalled when a stop finished.
    stopped: Condvar,
    limits: Mutex<Limits>,
    lan_names: Arc<LanNames>,
    /// Held by [`GatewayManager::apply_settings`]: two settings writes in a row (each spawns
    /// one) check and rebind one after the other, so a port change restarts once.
    applying: Mutex<()>,
}

impl std::fmt::Debug for GatewayManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GatewayManager")
            .finish_non_exhaustive()
    }
}

/// A [`GatewayError`] for the failure `error` while binding `port`, with the `AppError` the
/// caller of `start` gets.
fn failure(code: &str, port: u16, error: AppError) -> (AppError, GatewayError) {
    let detail = GatewayError {
        code: code.to_owned(),
        message: error.message().to_owned(),
        port,
    };
    (error, detail)
}

/// Fails the start with `no_entropy` when `fill` cannot draw random bytes: no client token could
/// be created on such a system, and the Gateway page says so.
#[allow(clippy::result_large_err)] // a cold failure path: boxing would allocate and change the private signature
fn check_entropy(
    port: u16,
    fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
) -> Result<(), (AppError, GatewayError)> {
    fill(&mut [0_u8; 1]).map_err(|_| {
        failure(
            "no_entropy",
            port,
            AppError::internal("SpanBay cannot create a secure token on this system."),
        )
    })
}

impl GatewayManager {
    /// A manager with nothing running.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                phase: Phase::Stopped,
                last_error: None,
            }),
            stopped: Condvar::new(),
            limits: Mutex::new(Limits::default()),
            lan_names: Arc::new(LanNames::system()),
            applying: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn limits(&self) -> Limits {
        *self.limits.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces every limit of gateways started afterwards.
    #[cfg(test)]
    fn set_all_limits(&self, edit: impl FnOnce(&mut Limits)) {
        edit(&mut self.limits.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// Makes the Host guard and the status see this machine as `snapshot` (tests have no LAN).
    #[cfg(test)]
    fn pin_lan(&self, snapshot: lan::LanSnapshot) {
        self.lan_names.pin(snapshot);
    }

    /// The current status.
    #[must_use]
    pub fn status(&self, llm: &LlmRuntime) -> GatewayStatus {
        self.build_status(llm, None)
    }

    /// The names that reach this machine on the local network, private-range addresses first.
    /// Looked up (at most every 10 s) whether or not the gateway runs, so the
    /// "Accepted Host names" block can list them for a configured LAN scope. Blocks while
    /// looking.
    #[must_use]
    pub fn lan_addresses(&self) -> Vec<LanAddress> {
        self.lan_names.snapshot_blocking().rows()
    }

    fn build_status(&self, llm: &LlmRuntime, notice: Option<String>) -> GatewayStatus {
        let settings = llm.settings();
        let gateway = &settings.ai.gateway;
        let (state, last_error, running, notice) = {
            let inner = self.lock();
            let mut notice = notice;
            let (state, running) = match &inner.phase {
                Phase::Starting => (GatewayState::Starting, None),
                Phase::Running(live) => {
                    notice = notice.or_else(|| live.startup_notice.clone());
                    (
                        GatewayState::Running,
                        Some((Arc::clone(&live.shared), live.started_at_ms)),
                    )
                }
                Phase::Stopping(draining) => (
                    GatewayState::Stopping,
                    Some((Arc::clone(&draining.shared), draining.started_at_ms)),
                ),
                Phase::Stopped if inner.last_error.is_some() => (GatewayState::Error, None),
                Phase::Stopped => (GatewayState::Stopped, None),
            };
            (state, inner.last_error.clone(), running, notice)
        };
        let clients = llm.client_hashes_blocking().map_or(0, |hashes| {
            u32::try_from(
                hashes
                    .iter()
                    .filter(|entry| entry.client.is_active())
                    .count(),
            )
            .unwrap_or(u32::MAX)
        });
        let mut status = GatewayStatus {
            state,
            running: state == GatewayState::Running,
            message: None,
            lan: gateway.serve_lan,
            port: 0,
            base_url: String::new(),
            addresses: Vec::new(),
            started_at_ms: 0,
            uptime_s: 0,
            requests: 0,
            in_flight: 0,
            open_connections: 0,
            rejected_connections: 0,
            log_dropped: 0,
            captures_dropped: 0,
            auth_failures: 0,
            clients,
            token_required: gateway.token_required(),
            capture_mode: gateway.capture_mode,
            enabled_on_start: gateway.enabled_on_start,
            last_error,
            notice,
        };
        if state == GatewayState::Error {
            status.message = status.last_error.as_ref().map(|error| {
                if error.code == "port_in_use" {
                    format!("port {} is in use", error.port)
                } else {
                    error.message.clone()
                }
            });
        }
        if let Some((shared, started_at_ms)) = running {
            status.lan = shared.lan;
            status.port = shared.port;
            status.started_at_ms = started_at_ms;
            status.uptime_s = u64::try_from((now_ms() - started_at_ms) / 1000).unwrap_or(0);
            if state == GatewayState::Running {
                let mut host = "127.0.0.1".to_owned();
                if shared.lan {
                    status.addresses = shared.lan_names.snapshot_blocking().rows();
                    if let Some(first) = status.addresses.iter().find(|row| row.private) {
                        host.clone_from(&first.host);
                    }
                }
                status.base_url = format!("http://{host}:{}", shared.port);
            }
            status.requests = shared.requests.load(Ordering::Relaxed);
            status.in_flight = shared.in_flight.load(Ordering::Relaxed);
            status.open_connections = shared.open_connections.load(Ordering::Relaxed);
            status.rejected_connections = shared.rejected_connections.load(Ordering::Relaxed);
            status.log_dropped = shared.log_dropped();
            status.captures_dropped = shared.captures_dropped();
            status.auth_failures = shared.auth_failures.load(Ordering::Relaxed);
        }
        status
    }

    /// Builds the status and pushes it to the event hub (no lock is held).
    fn announce(&self, llm: &LlmRuntime, notice: Option<String>) -> GatewayStatus {
        let status = self.build_status(llm, notice);
        llm.emit(LlmEvent::GatewayStatus {
            status: status.clone(),
        });
        status
    }

    /// Checks the LAN rule for `gateway`: serving the LAN needs `require_token` and at
    /// least one active client. Settings validation cannot see clients, so the use-case that
    /// changes `serve_lan` calls this first.
    ///
    /// # Errors
    /// [`AppError::Validation`] when the rule is broken; [`AppError::Database`] when the
    /// clients cannot be read.
    pub fn validate_lan(llm: &LlmRuntime, gateway: &AiGatewaySettings) -> Result<(), AppError> {
        if !gateway.serve_lan {
            return Ok(());
        }
        if !gateway.require_token {
            return Err(AppError::validation(
                "Local network access needs tokens: turn on Require a token first",
            ));
        }
        let active = llm
            .client_hashes_blocking()?
            .iter()
            .any(|entry| entry.client.is_active());
        if active {
            Ok(())
        } else {
            Err(AppError::validation(
                "Local network access needs at least one active client: connect a client first",
            ))
        }
    }

    /// Binds the configured port and starts serving. Starting a running gateway is a no-op that
    /// returns its status. A failure is also kept in the status (`last_error`, state `error`)
    /// until the next successful start.
    ///
    /// # Errors
    /// [`AppError::Conflict`] when the port is taken (or the gateway is stopping),
    /// [`AppError::Validation`] when LAN serving is configured without a token or a client,
    /// [`AppError::Unsupported`] for an in-memory store, [`AppError::Io`] for other bind or
    /// runtime failures.
    pub fn start(&self, llm: &Arc<LlmRuntime>) -> Result<GatewayStatus, AppError> {
        self.start_with(llm, None)
    }

    fn start_with(
        &self,
        llm: &Arc<LlmRuntime>,
        notice: Option<String>,
    ) -> Result<GatewayStatus, AppError> {
        {
            let mut inner = self.lock();
            match &inner.phase {
                Phase::Running(_) => {
                    drop(inner);
                    return Ok(self.status(llm));
                }
                Phase::Starting | Phase::Stopping(_) => {
                    return Err(AppError::conflict(
                        "the gateway is changing state; try again in a moment",
                    ));
                }
                Phase::Stopped => inner.phase = Phase::Starting,
            }
        }
        self.announce(llm, None);
        match self.bring_up(llm) {
            Ok(mut live) => {
                let parts = startup::notices(llm);
                live.startup_notice = startup::joined(&parts);
                {
                    let mut inner = self.lock();
                    inner.phase = Phase::Running(live);
                    inner.last_error = None;
                }
                let status = self.announce(llm, notice);
                for (code, text) in parts {
                    notify(llm, code, text);
                }
                Ok(status)
            }
            Err((error, detail)) => {
                let reason = detail.message.clone();
                {
                    let mut inner = self.lock();
                    inner.phase = Phase::Stopped;
                    inner.last_error = Some(detail);
                }
                self.announce(llm, None);
                notify(llm, GatewayNoticeCode::StartFailed, reason);
                Err(error)
            }
        }
    }

    #[allow(clippy::result_large_err)] // a cold failure path: boxing would allocate and change the private signature
    fn bring_up(&self, llm: &Arc<LlmRuntime>) -> Result<Live, (AppError, GatewayError)> {
        let settings = llm.settings();
        let configured = settings.ai.gateway.port;
        let lan = settings.ai.gateway.serve_lan;
        Self::validate_lan(llm, &settings.ai.gateway)
            .map_err(|error| failure("invalid_settings", configured, error))?;
        check_entropy(configured, getrandom::fill)?;
        // Fail early and visibly: an unreachable database would otherwise fail every request.
        llm.db()
            .open()
            .map_err(|error| failure("database_unavailable", configured, error))?;
        let address = if lan {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        let socket = socket::bind(SocketAddr::from((address, configured))).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AddrInUse {
                failure(
                    "port_in_use",
                    configured,
                    AppError::conflict(format!("port {configured} is already in use")),
                )
            } else {
                failure(
                    "cannot_bind",
                    configured,
                    AppError::io(format!("cannot listen on {address}:{configured}: {error}")),
                )
            }
        })?;
        // Port 0 (tests) lets the OS pick; the status then reports the port actually bound.
        let port = socket
            .local_addr()
            .map_or(configured, |address| address.port());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("spanbay-gateway")
            .enable_all()
            .build()
            .map_err(|error| {
                failure(
                    "start_failed",
                    port,
                    AppError::io(format!("cannot start the gateway runtime: {error}")),
                )
            })?;
        // Listening registers the socket with a reactor, so it happens inside the gateway runtime.
        let listener = {
            let _entered = runtime.enter();
            socket
                .listen(socket::LISTEN_BACKLOG)
                .map_err(|error| failure("cannot_bind", port, AppError::io(error.to_string())))?
        };
        let shared = Arc::new(
            Shared::new(
                Arc::clone(llm),
                port,
                lan,
                self.limits(),
                Arc::clone(&self.lan_names),
            )
            .map_err(|error| failure("start_failed", port, error))?,
        );
        let (control, control_signal) = watch::channel(Control::Run);
        let (closed_tx, closed) = std::sync::mpsc::channel();
        if lan {
            // Read before the watcher runs, so "what changed since the start" does not depend on
            // when its task is first scheduled.
            let known = self.lan_names.snapshot_blocking();
            runtime.spawn(watch_addresses(
                Arc::clone(llm),
                Arc::clone(&shared),
                known,
                control_signal.clone(),
            ));
        }
        runtime.spawn(server::accept_loop(
            listener,
            Arc::clone(&shared),
            control_signal,
            closed_tx,
        ));
        Ok(Live {
            runtime,
            control: Arc::new(control),
            closed,
            shared,
            started_at_ms: now_ms(),
            startup_notice: None,
        })
    }

    /// Stops the gateway gracefully: the listener closes at once, in-flight requests
    /// finish for at most 10 s, the rest are aborted and logged `canceled` ("gateway stopped").
    /// The status is `stopping` meanwhile. Stopping a stopped gateway only announces its status
    /// again. Blocks.
    pub fn stop(&self, llm: &LlmRuntime) -> GatewayStatus {
        self.stop_with(llm, Some(self.limits().drain), None)
    }

    /// Stops the gateway without waiting for in-flight requests ("Stop now"). Called while
    /// another thread is draining, it makes that drain end at once. Blocks briefly.
    pub fn stop_now(&self, llm: &LlmRuntime) -> GatewayStatus {
        self.stop_with(llm, None, None)
    }

    /// Stops the gateway when the app quits: like [`GatewayManager::stop`] but drains for at
    /// most 3 s; `quit == false` is a plain [`GatewayManager::stop`].
    pub fn shutdown(&self, llm: &LlmRuntime, quit: bool) -> GatewayStatus {
        let limits = self.limits();
        let drain = if quit {
            limits.quit_drain
        } else {
            limits.drain
        };
        self.stop_with(llm, Some(drain), None)
    }

    /// Stops and starts again (the same drain rules as Stop). The report names the open
    /// connections the restart dropped; the Failover settings (which a running gateway keeps
    /// until now) are read anew.
    ///
    /// # Errors
    /// As [`GatewayManager::start`].
    pub fn restart(&self, llm: &Arc<LlmRuntime>) -> Result<RestartReport, AppError> {
        let dropped_connections = match &self.lock().phase {
            Phase::Running(live) => live.shared.open_connections.load(Ordering::Relaxed),
            _ => 0,
        };
        self.stop(llm);
        let status = self.start(llm)?;
        Ok(RestartReport {
            dropped_connections,
            status,
        })
    }

    /// `drain`: how long in-flight requests may finish; `None` aborts them now. `notice` rides
    /// on the final status event.
    fn stop_with(
        &self,
        llm: &LlmRuntime,
        drain: Option<Duration>,
        notice: Option<String>,
    ) -> GatewayStatus {
        let begin = {
            let mut inner = self.lock();
            match std::mem::replace(&mut inner.phase, Phase::Stopped) {
                Phase::Running(live) => {
                    inner.phase = Phase::Stopping(Draining {
                        control: Arc::clone(&live.control),
                        shared: Arc::clone(&live.shared),
                        started_at_ms: live.started_at_ms,
                    });
                    Begin::Mine(live)
                }
                Phase::Stopping(draining) => {
                    if drain.is_none() {
                        let _ = draining.control.send(Control::Abort);
                    }
                    inner.phase = Phase::Stopping(draining);
                    Begin::Joined
                }
                Phase::Starting => {
                    inner.phase = Phase::Starting;
                    Begin::Idle
                }
                Phase::Stopped => Begin::Idle,
            }
        };
        match begin {
            Begin::Idle => self.announce(llm, notice),
            Begin::Joined => {
                let inner = self.lock();
                let waited = self.stopped.wait_timeout_while(
                    inner,
                    self.limits().drain + STOP_GRACE,
                    |inner| matches!(inner.phase, Phase::Stopping(_)),
                );
                drop(waited);
                self.status(llm)
            }
            Begin::Mine(live) => {
                self.announce(llm, None);
                self.drain(llm, live, drain);
                self.lock().phase = Phase::Stopped;
                self.stopped.notify_all();
                self.announce(llm, notice)
            }
        }
    }

    /// Tells the accept loop to stop and waits for it, announcing the in-flight count as it
    /// falls; then releases the runtime and flushes the log writer.
    fn drain(&self, llm: &LlmRuntime, live: Live, drain: Option<Duration>) {
        let signal = drain.map_or(Control::Abort, Control::Drain);
        let _ = live.control.send(signal);
        let deadline = Instant::now() + drain.unwrap_or_default() + STOP_GRACE;
        let mut announced = live.shared.in_flight.load(Ordering::Relaxed);
        loop {
            match live.closed.recv_timeout(DRAIN_POLL) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    let now = live.shared.in_flight.load(Ordering::Relaxed);
                    if now != announced {
                        announced = now;
                        self.announce(llm, None);
                    }
                }
            }
        }
        live.runtime.shutdown_background();
        // The accept loop is gone and with it every connection: nothing can log any more, so
        // what is queued is stored before Stop reports done.
        live.shared.flush_log();
    }

    /// Restarts the gateway when it runs on a port, or on an interface scope, other than the
    /// configured one.
    ///
    /// # Errors
    /// As [`GatewayManager::start`]; the gateway stays stopped (state `error`) when the new port
    /// cannot be bound.
    pub fn apply_settings(&self, llm: &Arc<LlmRuntime>) -> Result<(), AppError> {
        let _applying = self.applying.lock().unwrap_or_else(PoisonError::into_inner);
        let bound = match &self.lock().phase {
            Phase::Running(live) => Some((live.shared.port, live.shared.lan)),
            _ => None,
        };
        let settings = llm.settings();
        let configured = settings.ai.gateway.port;
        if bound.is_some_and(|(port, lan)| {
            (configured != 0 && port != configured) || lan != settings.ai.gateway.serve_lan
        }) {
            self.stop(llm);
            self.start(llm)?;
        }
        Ok(())
    }

    /// Call after a client was created, rotated, revoked or deleted: refreshes the token cache
    /// and, when LAN serving is on but no active client is left, turns it off in the
    /// runtime's settings, restarts a LAN listener on loopback and announces the change with the
    /// notice "Local network turned off: no active clients". Returns whether LAN was turned
    /// off; the caller must then persist `ai.gateway.serve_lan = false` too. Blocks while a
    /// running gateway drains.
    pub fn on_clients_changed(&self, llm: &Arc<LlmRuntime>) -> bool {
        llm.touch();
        let settings = llm.settings();
        if !settings.ai.gateway.serve_lan {
            return false;
        }
        let Ok(hashes) = llm.client_hashes_blocking() else {
            return false;
        };
        if hashes.iter().any(|entry| entry.client.is_active()) {
            return false;
        }
        let mut next = (*settings).clone();
        next.ai.gateway.serve_lan = false;
        llm.set_settings(Arc::new(next));
        let notice = Some(LAN_OFF_NOTICE.to_owned());
        let on_lan = matches!(&self.lock().phase, Phase::Running(live) if live.shared.lan);
        if on_lan {
            self.stop_with(llm, Some(self.limits().drain), None);
            // A failed start is in the status (`last_error`) and has announced itself.
            let _ = self.start_with(llm, notice);
        } else {
            self.announce(llm, notice);
        }
        notify(llm, GatewayNoticeCode::LanOff, LAN_OFF_NOTICE.to_owned());
        true
    }
}

/// While the gateway listens on the LAN: looks for a change of the machine's addresses every
/// `address_check` (the Host guard accepts the new ones at once, the lookup being fresh) and
/// announces it with a notice naming the new address.
async fn watch_addresses(
    llm: Arc<LlmRuntime>,
    shared: Arc<Shared>,
    mut known: Arc<lan::LanSnapshot>,
    mut control: watch::Receiver<Control>,
) {
    loop {
        tokio::select! {
            () = tokio::time::sleep(shared.limits.address_check) => {}
            // Any control message (stop, abort) or the end of the gateway ends the watch.
            _ = control.changed() => return,
        }
        let Some(current) = shared.lan_names.refresh().await else {
            continue;
        };
        if current.same_addresses(&known) {
            continue;
        }
        let added: Vec<String> = current
            .added_since(&known)
            .iter()
            .map(ToString::to_string)
            .collect();
        known = current;
        let notice = if added.is_empty() {
            "Local network address changed".to_owned()
        } else {
            format!("Local network address changed: {}", added.join(", "))
        };
        let llm = Arc::clone(&llm);
        let _ = tokio::task::spawn_blocking(move || {
            llm.gateway.announce(&llm, Some(notice.clone()));
            notify(&llm, GatewayNoticeCode::LanAddressChanged, notice);
        })
        .await;
    }
}

/// Pushes a [`LlmEvent::GatewayNotice`]; the status's `notice` keeps the same words.
fn notify(llm: &LlmRuntime, code: GatewayNoticeCode, notice: String) {
    llm.emit(LlmEvent::GatewayNotice { code, notice });
}

impl Default for GatewayManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Test fakes of the gateway (fake upstream, canned replies). The app crate's tests reach them
/// through the `test-support` feature; a release build never compiles them.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod tests;
#[cfg(test)]
mod tests_guards;
#[cfg(test)]
mod tests_lifecycle;
#[cfg(test)]
mod tests_pipeline;
#[cfg(test)]
mod tests_protocol;

#[cfg(test)]
mod entropy_tests {
    use super::*;

    #[test]
    fn a_failing_entropy_source_stops_the_start_with_the_no_entropy_code() {
        let (error, detail) =
            check_entropy(4141, |_| Err(getrandom::Error::UNSUPPORTED)).unwrap_err();
        assert_eq!(detail.code, "no_entropy");
        assert_eq!(detail.port, 4141);
        assert_eq!(
            detail.message,
            "SpanBay cannot create a secure token on this system."
        );
        assert!(matches!(error, AppError::Internal { .. }));
        assert!(check_entropy(4141, getrandom::fill).is_ok());
    }
}
