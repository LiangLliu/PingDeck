//! Live mock-server state and the tokio/hyper listener that serves it.
//!
//! Single responsibility: own the lifecycle of the running servers. The manager keeps one small
//! dedicated tokio runtime; every server is an accept task on it that serves HTTP/1.1 connections
//! (keep-alive, chunked request bodies) with hyper, bounded by a connection semaphore. A server
//! listens on loopback only: `127.0.0.1`, and `[::1]` on the same port when the machine has
//! IPv6, so a client that resolves `localhost` to `::1` first reaches it too (and a foreign
//! listener on `[::1]` is reported as a port conflict instead of silently answering). Stopping a
//! server wakes its accept task, which drops the listeners (releasing the port) before `stop`
//! returns.

use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;

use crate::events::{EventSink, MockLogEvent};
use crate::workspace::items::{
    MockFault, MockLogOutcome, MockPhaseTiming, MockServer, MockTransactionLog,
};

use super::callbacks::spawn_callbacks;
use super::crud::LiveBuckets;
use super::fanout::LogHub;
use super::history::MockHistorySink;
use super::http::{MAX_HEADER_BYTES, ParsedRequest, build_response, json_headers, read_request};
use super::live_data::initial_value;
use super::logs::{MOCK_LOG_CAPACITY, elapsed_ms, now_ms, phase_ms, push_log, push_rejection};
use super::recording::{MockRecording, Recordings};
use super::reply::{MAX_STREAM_MS, MockReply, ReplyBody, build_reply, stream_body};
use super::routing::{normalized_prefix, strip_prefix};

/// Pause after a failed `accept()` so a persistent error (e.g. `EMFILE`) cannot spin the CPU.
pub(super) const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(50);

/// Connections served at once per server; further ones get a JSON `503`.
pub(super) const MAX_CONNECTIONS: usize = 32;

/// A client that sends no request headers (idle keep-alive included) or no request body within
/// this window is disconnected, so a slowloris client cannot hold a connection slot forever.
#[cfg(not(test))]
pub(super) const CONNECTION_IO_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
pub(super) const CONNECTION_IO_TIMEOUT: Duration = Duration::from_millis(500);

/// How long `stop` waits for the accept task to release the listener.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// Log subscriptions a running server accepts at once.
pub(super) const MAX_LOG_SUBSCRIBERS: usize = 16;

/// Connections beyond [`MAX_CONNECTIONS`] that are answered `503` at once; further ones are
/// closed without a reply, so a flood cannot pile up tasks or sockets.
const MAX_REJECTIONS: usize = 8;

/// How long a rejected connection lingers (to deliver the `503` before closing) at most.
const REJECT_LINGER: Duration = Duration::from_millis(250);

/// A shared consumer of a server's log events.
pub type LogSink = Arc<dyn EventSink<MockLogEvent>>;

/// Why a mock server could not be started.
#[derive(Debug, Error)]
pub enum MockStartError {
    /// The requested port is bound by another process.
    #[error("port {port} is already in use")]
    PortInUse { port: u16 },
    /// Another process listens on `[::1]` at the port, so clients that resolve `localhost` to
    /// `::1` would reach it instead of the mock.
    #[error(
        "port {port} is already in use on [::1] by another process; clients that resolve localhost to ::1 would reach it instead of the mock"
    )]
    Ipv6PortInUse { port: u16 },
    /// Binding the listener failed for another reason.
    #[error("failed to bind mock server on 127.0.0.1:{port}: {source}")]
    Bind { port: u16, source: std::io::Error },
    /// The server runtime could not be started.
    #[error("failed to start the mock server thread: {source}")]
    Thread { source: std::io::Error },
}

/// Why a log subscription was refused.
#[derive(Debug, Error)]
pub enum MockSubscribeError {
    /// No server with this id is running.
    #[error("mock server is not running")]
    NotRunning,
    /// The server already has [`MAX_LOG_SUBSCRIBERS`] live subscriptions.
    #[error("too many log subscribers on this mock server (limit {MAX_LOG_SUBSCRIBERS})")]
    TooMany,
}

/// Owns every running mock server and its hot-swappable configuration.
pub struct MockServerManager {
    pub(super) running: Mutex<HashMap<String, RunningMockServer>>,
    /// Runtime every server task runs on; built on the first start.
    runtime: Mutex<Option<Runtime>>,
    /// One client (and connection pool) for every proxied request and callback.
    client: Mutex<Option<reqwest::Client>>,
    /// Serialises [`MockServerManager::start`].
    start_lock: Mutex<()>,
    /// When each server was last stopped, in Unix milliseconds; in memory only, so a restart of
    /// the app forgets it. A start clears the entry.
    stopped_at: Mutex<HashMap<String, i64>>,
    /// Request log size of running servers and of those started from now on
    /// (`mock.log_capacity`).
    log_capacity: AtomicUsize,
    /// Receives one history row per served request; `None` records nothing.
    history: Option<Arc<dyn MockHistorySink>>,
    /// Host names besides loopback the Host guard of every server accepts
    /// (`mock.extra_allowed_hosts`); shared with the running servers, so a change applies at once.
    extra_hosts: Arc<RwLock<Vec<String>>>,
}

impl Default for MockServerManager {
    fn default() -> Self {
        Self::build(None)
    }
}

/// Counters of a running server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MockServerStats {
    /// When the listener was bound, in Unix milliseconds.
    pub started_at_ms: i64,
    /// Requests served since it started; unlike the log ring, never trimmed.
    pub hit_count: u64,
}

/// One bound listener and its mutable server configuration.
pub struct RunningMockServer {
    pub(super) state: Arc<ServerState>,
    /// Wakes the accept task; it then drops the listeners.
    shutdown: watch::Sender<bool>,
    /// Disconnects once the accept task has dropped the listeners.
    closed: std::sync::mpsc::Receiver<()>,
    pub(super) bound: SocketAddr,
}

impl RunningMockServer {
    /// Address the listener is bound to.
    #[must_use]
    pub fn bound_addr(&self) -> SocketAddr {
        self.bound
    }
}

impl Drop for RunningMockServer {
    /// Stops the accept task and waits until the listener is closed, so a removed or displaced
    /// entry has always released its port.
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        let _ = self.closed.recv_timeout(STOP_TIMEOUT);
    }
}

impl ServerState {
    /// Builds the live state of a freshly bound server, seeding its data buckets.
    pub(super) fn new(
        server: MockServer,
        bound: SocketAddr,
        client: reqwest::Client,
        log_capacity: usize,
        handle: Handle,
        history: Option<Arc<dyn MockHistorySink>>,
        extra_hosts: Arc<RwLock<Vec<String>>>,
    ) -> Self {
        let endpoint_prefix = normalized_prefix(&server.endpoint_prefix);
        Self {
            buckets: Mutex::new(LiveBuckets::new(initial_buckets(&server))),
            config: RwLock::new(Arc::new(server)),
            log: LogHub::new(log_capacity),
            counters: Mutex::new(HashMap::new()),
            route_hits: Mutex::new(HashMap::new()),
            sequence: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            #[cfg(test)]
            accepted: AtomicU64::new(0),
            started_at_ms: now_ms(),
            endpoint_prefix,
            callbacks: Mutex::new(JoinSet::new()),
            bound,
            client,
            handle,
            history,
            extra_hosts,
            recordings: Mutex::default(),
        }
    }
}

/// Live state of one running server: hot-swappable config, logs, counters and buckets.
pub(super) struct ServerState {
    /// Hot-swappable configuration; requests clone the `Arc`, never the struct.
    pub(super) config: RwLock<Arc<MockServer>>,
    /// The request log ring and its batched subscribers.
    pub(super) log: LogHub,
    /// Sequential response cursor per route.
    pub(super) counters: Mutex<HashMap<String, u64>>,
    /// 1-based request count per route, used by `RequestNumber` rules.
    pub(super) route_hits: Mutex<HashMap<String, u64>>,
    /// Live JSON of every data bucket, keyed by bucket id. `Crud` routes mutate
    /// these; they are seeded from `MockServer::data_buckets` on start.
    pub(super) buckets: Mutex<LiveBuckets>,
    pub(super) sequence: AtomicU64,
    /// Requests served since the server started (`MockServerStats::hit_count`).
    pub(super) hits: AtomicU64,
    /// Connections the accept loop has dispatched (served or rejected); lets tests wait for a
    /// known number of sockets to be accepted instead of sleeping.
    #[cfg(test)]
    pub(super) accepted: AtomicU64,
    /// When the server started, in Unix milliseconds.
    pub(super) started_at_ms: i64,
    /// The endpoint prefix this listener serves, normalized and fixed at start: a saved edit
    /// applies after a stop and start, not to the running listener.
    pub(super) endpoint_prefix: String,
    /// Callback tasks in flight; the accept task cancels them when the server stops.
    pub(super) callbacks: Mutex<JoinSet<()>>,
    pub(super) bound: SocketAddr,
    /// Shared client for proxying and callbacks.
    pub(super) client: reqwest::Client,
    /// The server runtime, for the timers that flush log batches.
    pub(super) handle: Handle,
    /// Where the history row of every served request goes.
    pub(super) history: Option<Arc<dyn MockHistorySink>>,
    /// The manager's extra allowed hosts.
    pub(super) extra_hosts: Arc<RwLock<Vec<String>>>,
    /// Proxied exchanges kept while recording is on.
    pub(super) recordings: Mutex<Recordings>,
}

impl MockServerManager {
    /// Creates a manager with no running servers that records no history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a manager whose servers submit one history row per served request to `history`.
    #[must_use]
    pub fn with_history(history: Arc<dyn MockHistorySink>) -> Self {
        Self::build(Some(history))
    }

    fn build(history: Option<Arc<dyn MockHistorySink>>) -> Self {
        Self {
            running: Mutex::default(),
            runtime: Mutex::default(),
            client: Mutex::default(),
            start_lock: Mutex::default(),
            stopped_at: Mutex::default(),
            log_capacity: AtomicUsize::new(MOCK_LOG_CAPACITY),
            history,
            extra_hosts: Arc::default(),
        }
    }

    /// Sets the host names (`host` or `host:port`, compared without case) the Host guard of
    /// every server accepts besides loopback; running servers apply them to their next request.
    pub fn set_extra_allowed_hosts(&self, hosts: Vec<String>) {
        *write_lock(&self.extra_hosts) = hosts;
    }

    /// Sets the request log size of every running server and of servers started later; a
    /// running log shrinks on its next entry.
    pub fn set_log_capacity(&self, capacity: usize) {
        self.log_capacity.store(capacity, Ordering::Relaxed);
        for server in lock(&self.running).values() {
            server.state.log.set_capacity(capacity);
        }
    }

    /// Handle to the server runtime, building the runtime on first use.
    fn handle(&self) -> Result<Handle, MockStartError> {
        let mut runtime = lock(&self.runtime);
        if let Some(runtime) = runtime.as_ref() {
            return Ok(runtime.handle().clone());
        }
        let built = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("spanbay-mock")
            .enable_all()
            .build()
            .map_err(|source| MockStartError::Thread { source })?;
        let handle = built.handle().clone();
        *runtime = Some(built);
        Ok(handle)
    }

    /// The shared outbound client (see [`super::proxy::outbound_client`]).
    fn client(&self) -> Result<reqwest::Client, MockStartError> {
        let mut client = lock(&self.client);
        if let Some(client) = client.as_ref() {
            return Ok(client.clone());
        }
        let built = super::proxy::outbound_client().map_err(|error| MockStartError::Thread {
            source: std::io::Error::other(error),
        })?;
        *client = Some(built.clone());
        Ok(built)
    }

    /// Binds `127.0.0.1` and starts `server`. A port of `0` asks the OS for a free port and
    /// the runtime configuration is updated to the bound port.
    pub fn start(&self, mut server: MockServer) -> Result<SocketAddr, MockStartError> {
        // Starts are serialised, so two concurrent starts of one id (or port) run one after the
        // other: the second stops the first and rebinds its port instead of failing with
        // `PortInUse`.
        let _starting = lock(&self.start_lock);
        // Stopped first so a restart can rebind the port the old listener still holds.
        let id = server.id.clone();
        self.stop(&id);

        let handle = self.handle()?;
        let client = self.client()?;
        let requested_port = server.port;
        let bind_error = |error: std::io::Error| {
            if error.kind() == std::io::ErrorKind::AddrInUse {
                MockStartError::PortInUse {
                    port: requested_port,
                }
            } else {
                MockStartError::Bind {
                    port: requested_port,
                    source: error,
                }
            }
        };
        // Bound synchronously so "port in use" is reported to the caller, then handed to tokio.
        let (listener, ipv6) = bind_loopback(requested_port).map_err(|error| match error {
            BindFailure::Ipv4(error) => bind_error(error),
            BindFailure::Ipv6InUse(port) => MockStartError::Ipv6PortInUse { port },
        })?;
        let bound = listener.local_addr().map_err(bind_error)?;
        if server.port == 0 {
            server.port = bound.port();
        }

        let state = Arc::new(ServerState::new(
            server,
            bound,
            client,
            self.log_capacity.load(Ordering::Relaxed),
            handle.clone(),
            self.history.clone(),
            Arc::clone(&self.extra_hosts),
        ));
        let (shutdown, shutdown_signal) = watch::channel(false);
        let (closed_sender, closed) = std::sync::mpsc::channel();
        handle.spawn(serve(
            listener,
            ipv6,
            Arc::clone(&state),
            shutdown_signal,
            closed_sender,
        ));

        lock(&self.stopped_at).remove(&id);
        let displaced = lock(&self.running).insert(
            id,
            RunningMockServer {
                state,
                shutdown,
                closed,
                bound,
            },
        );
        // Unreachable while `_starting` is held (the old entry was stopped above); dropping a
        // server waits for its listener, which must never happen inside the `running` lock.
        drop(displaced);

        Ok(bound)
    }

    /// Stops the listener and waits until it is closed.
    pub fn stop(&self, id: &str) -> bool {
        // The entry is dropped (stopped and awaited) after the `running` lock is released.
        let removed = lock(&self.running).remove(id);
        if removed.is_some() {
            lock(&self.stopped_at).insert(id.to_owned(), now_ms());
        }
        removed.is_some()
    }

    /// Replaces the configuration of a running server. Port/prefix listener
    /// changes still need a stop/start, everything else applies immediately.
    ///
    /// Data buckets keep their live contents across a hot reload unless a
    /// bucket is newly added or its configured `value_json` changed.
    pub fn update(&self, server: MockServer) -> bool {
        let running = lock(&self.running);
        let Some(running) = running.get(&server.id) else {
            return false;
        };
        let mut config = write_lock(&running.state.config);
        let mut buckets = lock(&running.state.buckets);
        for bucket in &server.data_buckets {
            let value_changed = config
                .data_buckets
                .iter()
                .find(|previous| previous.id == bucket.id)
                .is_none_or(|previous| previous.value_json != bucket.value_json);
            if value_changed || !buckets.contains_key(&bucket.id) {
                buckets.insert(bucket.id.clone(), initial_value(bucket));
            }
        }
        buckets.retain(|id| server.data_buckets.iter().any(|bucket| bucket.id == id));
        *config = Arc::new(server);
        true
    }

    /// Stops every running server, returning how many listeners were stopped.
    pub fn stop_all(&self) -> usize {
        let ids: Vec<String> = lock(&self.running).keys().cloned().collect();
        ids.iter().filter(|id| self.stop(id)).count()
    }

    /// How many servers are listening right now.
    #[must_use]
    pub fn running_count(&self) -> usize {
        lock(&self.running).len()
    }

    /// Whether a server with this id is currently listening.
    #[must_use]
    pub fn is_running(&self, id: &str) -> bool {
        lock(&self.running).contains_key(id)
    }

    /// Address a running server is bound to, if any.
    #[must_use]
    pub fn bound_addr(&self, id: &str) -> Option<SocketAddr> {
        lock(&self.running)
            .get(id)
            .map(RunningMockServer::bound_addr)
    }

    /// Snapshot of a running server's request log, oldest first.
    #[must_use]
    pub fn logs(&self, id: &str) -> Vec<MockTransactionLog> {
        let running = lock(&self.running);
        let Some(running) = running.get(id) else {
            return Vec::new();
        };
        running.state.log.snapshot()
    }

    /// The endpoint prefix a running listener serves (normalized: empty or `/segment`), which a
    /// saved edit does not change until the next start; `None` when it is not running.
    #[must_use]
    pub fn endpoint_prefix(&self, id: &str) -> Option<String> {
        lock(&self.running)
            .get(id)
            .map(|running| running.state.endpoint_prefix.clone())
    }

    /// When the server was last stopped in this session, in Unix milliseconds; `None` while it
    /// runs or when it has not been stopped since the app started.
    #[must_use]
    pub fn stopped_at_ms(&self, id: &str) -> Option<i64> {
        lock(&self.stopped_at).get(id).copied()
    }

    /// Counters of a running server; `None` when it is not running.
    #[must_use]
    pub fn stats(&self, id: &str) -> Option<MockServerStats> {
        lock(&self.running).get(id).map(|running| MockServerStats {
            started_at_ms: running.state.started_at_ms,
            hit_count: running.state.hits.load(Ordering::Relaxed),
        })
    }

    /// Pushes the new log entries of a running server to `sink` as batches (at most one
    /// [`MockLogEvent::Batch`] per 33 ms window, the first entry after a quiet spell at once),
    /// returning the subscription id. The subscription ends on [`unsubscribe_logs`], when the
    /// server stops (its last pending batch is delivered first), or when the sink reports
    /// `false`. At most [`MAX_LOG_SUBSCRIBERS`] subscriptions are live per server; a
    /// sink that went away is only noticed on its next emit, so a leaked subscription holds a
    /// place until the server logs again or stops.
    ///
    /// Entries logged before this call are not replayed: read [`logs`](Self::logs) afterwards and
    /// de-duplicate by entry id.
    pub fn subscribe_logs(
        &self,
        id: &str,
        sink: Arc<dyn EventSink<MockLogEvent>>,
    ) -> Result<u64, MockSubscribeError> {
        static NEXT_SUBSCRIPTION: AtomicU64 = AtomicU64::new(1);
        let running = lock(&self.running);
        let running = running.get(id).ok_or(MockSubscribeError::NotRunning)?;
        let subscription = NEXT_SUBSCRIPTION.fetch_add(1, Ordering::Relaxed);
        if running.state.log.subscribe(subscription, sink) {
            Ok(subscription)
        } else {
            Err(MockSubscribeError::TooMany)
        }
    }

    /// Ends a log subscription; returns whether it was still active.
    pub fn unsubscribe_logs(&self, subscription: u64) -> bool {
        let running = lock(&self.running);
        running
            .values()
            .any(|running| running.state.log.unsubscribe(subscription))
    }

    /// Resets the counters of a running server: sequential cursors, the per-route
    /// request numbers `RequestNumber` rules read, and the hit count. Data buckets and the log
    /// are left alone. Returns whether the server is running.
    pub fn reset_state(&self, id: &str) -> bool {
        let running = lock(&self.running);
        let Some(running) = running.get(id) else {
            return false;
        };
        lock(&running.state.counters).clear();
        lock(&running.state.route_hits).clear();
        running.state.hits.store(0, Ordering::Relaxed);
        true
    }

    /// Clears the request log and the batches subscribers have not been sent yet,
    /// returning whether the server is running.
    pub fn clear_logs(&self, id: &str) -> bool {
        let running = lock(&self.running);
        let Some(running) = running.get(id) else {
            return false;
        };
        running.state.log.clear();
        true
    }

    /// Switches recording of proxied exchanges on a running server (switching it on starts an
    /// empty session) and returns how many it holds; `None` when the server is not running.
    pub fn set_recording(&self, id: &str, enabled: bool) -> Option<usize> {
        let running = lock(&self.running);
        let mut recordings = lock(&running.get(id)?.state.recordings);
        recordings.set_enabled(enabled);
        Some(recordings.len())
    }

    /// Whether a running server records proxied exchanges and how many it holds; `None` when it
    /// is not running.
    #[must_use]
    pub fn recording(&self, id: &str) -> Option<(bool, usize)> {
        let running = lock(&self.running);
        let recordings = lock(&running.get(id)?.state.recordings);
        Some((recordings.enabled, recordings.len()))
    }

    /// The recordings of a running server, oldest first (empty when it is not running).
    #[must_use]
    pub fn recordings(&self, id: &str) -> Vec<MockRecording> {
        let running = lock(&self.running);
        running
            .get(id)
            .map(|running| lock(&running.state.recordings).snapshot())
            .unwrap_or_default()
    }

    /// The recording of the exchange logged as `log_id` on a running server, with its whole
    /// response body.
    #[must_use]
    pub fn recorded_exchange(&self, id: &str, log_id: &str) -> Option<MockRecording> {
        let running = lock(&self.running);
        lock(&running.get(id)?.state.recordings).by_log_id(log_id)
    }
}

impl Drop for MockServerManager {
    /// Stops every server, then abandons the runtime without blocking (it may be dropped from
    /// inside another runtime's task).
    fn drop(&mut self) {
        self.stop_all();
        if let Some(runtime) = lock(&self.runtime).take() {
            runtime.shutdown_background();
        }
    }
}

/// Why the loopback listeners could not be bound.
enum BindFailure {
    /// Binding `127.0.0.1` failed.
    Ipv4(std::io::Error),
    /// Another process holds the port on `[::1]`.
    Ipv6InUse(u16),
}

/// How many fresh ports a start on port `0` tries before giving up the IPv6 twin.
const EPHEMERAL_ATTEMPTS: usize = 8;

/// Binds `127.0.0.1:port` and, when the machine has IPv6 loopback, `[::1]` on the same port
/// (both non-blocking). A `[::1]` port held by another process is a conflict; a machine without
/// IPv6 loopback serves IPv4 only. For port `0` the OS picks the IPv4 port and a pick whose
/// IPv6 twin is taken is retried a few times, then served on IPv4 only.
fn bind_loopback(port: u16) -> Result<(TcpListener, Option<TcpListener>), BindFailure> {
    let attempts = if port == 0 { EPHEMERAL_ATTEMPTS } else { 1 };
    for attempt in 1..=attempts {
        let ipv4 = TcpListener::bind(("127.0.0.1", port)).map_err(BindFailure::Ipv4)?;
        ipv4.set_nonblocking(true).map_err(BindFailure::Ipv4)?;
        let bound = ipv4.local_addr().map_err(BindFailure::Ipv4)?.port();
        match TcpListener::bind((Ipv6Addr::LOCALHOST, bound)) {
            Ok(ipv6) => {
                if ipv6.set_nonblocking(true).is_err() {
                    return Ok((ipv4, None));
                }
                return Ok((ipv4, Some(ipv6)));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                if port != 0 {
                    return Err(BindFailure::Ipv6InUse(bound));
                }
                if attempt == attempts {
                    return Ok((ipv4, None));
                }
            }
            // No IPv6 loopback on this machine (or it is switched off): IPv4 only.
            Err(_) => return Ok((ipv4, None)),
        }
    }
    Err(BindFailure::Ipv4(std::io::Error::other(
        "no loopback port could be bound",
    )))
}

/// Accepts the next connection on either listener.
async fn accept_any(
    ipv4: &tokio::net::TcpListener,
    ipv6: Option<&tokio::net::TcpListener>,
) -> std::io::Result<(tokio::net::TcpStream, SocketAddr)> {
    match ipv6 {
        Some(ipv6) => tokio::select! {
            accepted = ipv4.accept() => accepted,
            accepted = ipv6.accept() => accepted,
        },
        None => ipv4.accept().await,
    }
}

/// Parses every bucket's JSON once, so live CRUD mutations start from the
/// configuration. Invalid JSON becomes an empty array rather than an error, so a
/// half-written bucket cannot stop the server from starting.
fn initial_buckets(server: &MockServer) -> HashMap<String, Value> {
    server
        .data_buckets
        .iter()
        .map(|bucket| (bucket.id.clone(), initial_value(bucket)))
        .collect()
}

/// Locks a mutex, recovering the data if a handler panicked while holding it.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Accepts connections until `shutdown` fires, then drops the listener, cancels every open
/// connection and pending callback, waits for them to finish and signals `closed`: once `stop`
/// returns, no task of this server can log, emit or write anymore. A failed `accept()` is
/// transient: back off and keep serving.
async fn serve(
    listener: TcpListener,
    ipv6: Option<TcpListener>,
    state: Arc<ServerState>,
    mut shutdown: watch::Receiver<bool>,
    closed: std::sync::mpsc::Sender<()>,
) {
    let listener = match tokio::net::TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(error) => {
            crate::logging::log_line(
                crate::logging::LogLevel::Error,
                "mock.server",
                &format!("could not register the listener: {error}"),
            );
            return;
        }
    };
    let ipv6 = ipv6.and_then(|ipv6| match tokio::net::TcpListener::from_std(ipv6) {
        Ok(ipv6) => Some(ipv6),
        Err(error) => {
            crate::logging::log_line(
                crate::logging::LogLevel::Warn,
                "mock.server",
                &format!("could not register the IPv6 listener: {error}"),
            );
            None
        }
    });
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let reject_permits = Arc::new(Semaphore::new(MAX_REJECTIONS));
    let mut connections = JoinSet::new();

    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            // Finished connections are reaped as they end, not only when the next one arrives.
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = accept_any(&listener, ipv6.as_ref()) => match accepted {
                Ok((stream, peer)) => {
                    let _ = stream.set_nodelay(true);
                    if let Ok(permit) = Arc::clone(&permits).try_acquire_owned() {
                        let state = Arc::clone(&state);
                        connections.spawn(async move {
                            serve_connection(stream, peer.ip().to_string(), state).await;
                            drop(permit);
                        });
                    } else if let Ok(permit) = Arc::clone(&reject_permits).try_acquire_owned() {
                        let busy = MockReply::json(503, &serde_json::json!({ "error": "too_many_connections" }), None)
                            .with_outcome(MockLogOutcome::Busy);
                        push_rejection(&state, &ParsedRequest::default(), &busy);
                        connections.spawn(reject_busy(stream, permit));
                    }
                    // Otherwise `stream` drops here: a flood beyond both bounds is simply closed.
                    #[cfg(test)]
                    state.accepted.fetch_add(1, Ordering::SeqCst);
                }
                Err(error) => {
                    crate::logging::log_line(
                        crate::logging::LogLevel::Warn,
                        "mock.server",
                        &format!("accept failed: {error}"),
                    );
                    tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                }
            },
        }
    }

    drop(listener);
    drop(ipv6);
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    // Connections are gone, so nothing can spawn another callback.
    let mut callbacks = std::mem::take(&mut *lock(&state.callbacks));
    callbacks.abort_all();
    while callbacks.join_next().await.is_some() {}
    // Rows still waiting for their window leave now; the subscriptions end with the server.
    state.log.close();
    let _ = closed.send(());
}

/// Answers a connection beyond [`MAX_CONNECTIONS`] with a JSON `503`, then half-closes it and
/// drains it for at most [`REJECT_LINGER`].
///
/// The reply is written without waiting for the request, and the socket is read until the
/// client hangs up or the linger ends: closing with unread request bytes would make the kernel
/// send a reset that discards the `503` on the client side. `_permit` bounds how many sockets
/// can be lingering at once.
async fn reject_busy(mut stream: tokio::net::TcpStream, _permit: OwnedSemaphorePermit) {
    const BODY: &str = r#"{"error":"too_many_connections"}"#;
    let reply = format!(
        "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{BODY}",
        BODY.len()
    );
    let _ = tokio::time::timeout(REJECT_LINGER, async {
        stream.write_all(reply.as_bytes()).await?;
        stream.shutdown().await?;
        let mut scratch = [0_u8; 4096];
        while stream.read(&mut scratch).await? > 0 {}
        Ok::<(), std::io::Error>(())
    })
    .await;
}

/// Serves one keep-alive HTTP/1.1 connection until the client leaves or goes quiet.
async fn serve_connection(stream: tokio::net::TcpStream, peer_ip: String, state: Arc<ServerState>) {
    let service =
        service_fn(move |request| handle_request(Arc::clone(&state), peer_ip.clone(), request));
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(CONNECTION_IO_TIMEOUT)
        .max_buf_size(MAX_HEADER_BYTES)
        .keep_alive(true)
        .serve_connection(TokioIo::new(stream), service);
    // Protocol errors (a client hanging up mid-request, a malformed head) end the connection.
    let _ = connection.await;
}

/// Whether the `Host` header names this server: a loopback name or address, the bound address,
/// or one of the `extra` allowed hosts. A request without `Host` (HTTP/1.0, not a browser) is
/// let through.
fn host_allowed(headers: &http::HeaderMap, bound: SocketAddr, extra: &[String]) -> bool {
    let Some(value) = headers.get(http::header::HOST) else {
        return true;
    };
    let Some(authority) = value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<http::uri::Authority>().ok())
    else {
        return false;
    };
    let host = authority.host();
    crate::net_guard::classify_host_literal(host) == Some(crate::net_guard::HostClass::Loopback)
        || host.trim_matches(['[', ']']) == bound.ip().to_string()
        || extra
            .iter()
            .any(|entry| extra_host_matches(entry, host, authority.port_u16(), bound.port()))
}

/// Whether the extra allowed host `entry` (`host` or `host:port`, IPv6 in brackets) names
/// `host` with `port` (`None`: no port in the header). An entry without a port accepts no port or
/// the bound port; one with a port (a forwarded port in front of the mock) accepts only it.
fn extra_host_matches(entry: &str, host: &str, port: Option<u16>, bound_port: u16) -> bool {
    let entry = entry.trim();
    let (entry_host, entry_port) = match entry.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((name, tail)) => (name, tail.strip_prefix(':')),
            None => return false,
        },
        None => match entry.rsplit_once(':') {
            Some((name, port)) if !name.contains(':') => (name, Some(port)),
            _ => (entry, None),
        },
    };
    let port_ok = match entry_port {
        Some(wanted) => wanted
            .parse::<u16>()
            .is_ok_and(|wanted| port == Some(wanted)),
        None => port.is_none_or(|port| port == bound_port),
    };
    port_ok && entry_host.eq_ignore_ascii_case(host.trim_matches(['[', ']']))
}

/// A buffered response as a [`ReplyBody`].
fn buffered(response: Response<http_body_util::Full<bytes::Bytes>>) -> Response<ReplyBody> {
    use http_body_util::BodyExt;
    response.map(|body| body.map_err(|never| match never {}).boxed_unsync())
}

/// Ends a connection without a response; returned for a response with a [`MockFault`].
#[derive(Debug, Error)]
#[error("mock fault: the connection was closed without a response")]
pub(super) struct FaultClose;

/// Reads one request, builds the reply, waits out the configured latency and logs the exchange.
/// A stream response is logged when its headers go out, with the whole stream as its body. A
/// response with a fault is logged, then the connection is cut ([`MockFault::Drop`]) or held
/// silent until [`MAX_STREAM_MS`] and then cut ([`MockFault::Hang`]).
async fn handle_request(
    state: Arc<ServerState>,
    peer_ip: String,
    request: Request<hyper::body::Incoming>,
) -> Result<Response<ReplyBody>, FaultClose> {
    let started = Instant::now();
    let host_ok = host_allowed(
        request.headers(),
        state.bound,
        &read_lock(&state.extra_hosts),
    );
    if !host_ok {
        // A page reaching this server through a rebound DNS name sends that name as `Host`.
        let reply = MockReply::json(
            403,
            &serde_json::json!({ "error": "host not allowed" }),
            None,
        )
        .with_outcome(MockLogOutcome::RejectedHost);
        push_rejection(&state, &ParsedRequest::head_of(&request), &reply);
        return Ok(buffered(build_response(
            403,
            &json_headers(),
            reply.body,
            true,
        )));
    }
    let request = match read_request(request, CONNECTION_IO_TIMEOUT).await {
        Ok(request) => request,
        Err(error) => {
            let body = serde_json::json!({ "error": error.to_string() }).to_string();
            // The unread body makes this connection unusable for another request.
            return Ok(buffered(build_response(
                error.status(),
                &json_headers(),
                body,
                true,
            )));
        }
    };
    let mut timing = MockPhaseTiming {
        read_ms: Some(phase_ms(started.elapsed())),
        ..MockPhaseTiming::default()
    };

    let config = Arc::clone(&read_lock(&state.config));
    let prefix = state.endpoint_prefix.as_str();
    let relative_path = strip_prefix(&request.path, prefix);
    let base_url = format!("http://{}{}", state.bound, prefix);

    let route_started = Instant::now();
    let mut proxy_time = None;
    let mut reply = build_reply(
        &config,
        &state,
        &request,
        relative_path,
        &base_url,
        &peer_ip,
        &mut proxy_time,
    )
    .await;
    timing.route_ms = Some(phase_ms(
        route_started
            .elapsed()
            .saturating_sub(proxy_time.unwrap_or_default()),
    ));
    timing.proxy_ms = proxy_time.map(phase_ms);
    if reply.outcome == MockLogOutcome::RejectedOrigin {
        // Refused like a foreign Host: a traffic row only, no hit and no history row.
        push_rejection(&state, &request, &reply);
        return Ok(buffered(build_response(
            reply.status,
            &reply.headers,
            reply.body,
            false,
        )));
    }
    if !reply.latency.is_zero() {
        let latency_started = Instant::now();
        tokio::time::sleep(reply.latency).await;
        timing.latency_ms = Some(phase_ms(latency_started.elapsed()));
    }

    let callbacks = std::mem::take(&mut reply.callbacks);
    let log_id = push_log(
        &state,
        &config,
        &request,
        &reply,
        elapsed_ms(started),
        timing,
    );
    // Fired after the row exists, so each result lands on it.
    spawn_callbacks(&state, &log_id, callbacks);
    if reply.proxied {
        lock(&state.recordings).record(&log_id, &request, &reply);
    }
    match reply.fault {
        Some(MockFault::Drop) => return Err(FaultClose),
        Some(MockFault::Hang) => {
            tokio::time::sleep(Duration::from_millis(MAX_STREAM_MS)).await;
            return Err(FaultClose);
        }
        None => {}
    }
    let Some(plan) = reply.stream.take() else {
        return Ok(buffered(build_response(
            reply.status,
            &reply.headers,
            reply.body,
            false,
        )));
    };
    let head = build_response(reply.status, &reply.headers, bytes::Bytes::new(), false);
    Ok(head.map(|_| stream_body(plan)))
}
