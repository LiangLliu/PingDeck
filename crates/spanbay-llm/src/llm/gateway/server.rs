//! The gateway's HTTP plumbing: accept loop, drain, per-request context, response helpers.
//!
//! Single responsibility: move bytes between sockets and the request handlers. It follows the
//! mock server's connection discipline (bounded connections, header-read timeout, keep-alive) but
//! streams response bodies instead of buffering them and accepts large request bodies (bounded
//! together by a global byte budget). A client that stops accepting response bytes loses its
//! connection (`socket::StallGuard`). A connection over the cap is answered `503` and closed;
//! on shutdown the listener closes at once, open connections finish their request for a
//! bounded time and the rest are aborted.
//! Every request gets a [`Context`]: its `rq_<16 hex>` id, the peer, who is
//! calling, and the way to log.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;

use crate::llm::failure::FailureKind;
use crate::llm::health::CircuitConfig;
use crate::llm::runtime::{LlmRuntime, circuit_config, now_ms};
use crate::llm::store::{self, RequestLogEntry};
use spanbay_base::error::AppError;

use super::Limits;
use super::auth::{ANONYMOUS, Identity, SeenTokens, TouchBook};
use super::errors::Refusal;
use super::handlers::Route;
use super::lan::LanNames;
use super::limiter::AuthThrottle;
use super::logwriter::{LogJob, LogQueue};
use super::quota::ClientQuotas;
use super::socket::StallGuard;

/// Largest request body the gateway accepts (32 MiB: base64 images add up).
pub(super) const GATEWAY_MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Connections served at once.
pub(super) const MAX_CONNECTIONS: usize = 256;

/// Request-body bytes the gateway holds at once (8 maximal bodies): each body is parsed to a JSON
/// value, decoded and re-encoded, several times its size, so 256 connections must not all hold a
/// maximal one.
pub(super) const BODY_BUDGET_BYTES: usize = 8 * GATEWAY_MAX_BODY_BYTES;

/// A client that sends no complete request head within this window is dropped.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Over-cap connections answered at once; beyond this they are closed without a word (still
/// counted), so a flood cannot pile up writer tasks.
const REJECT_WRITERS: usize = 64;

/// How long the `503` of an over-cap connection may take.
const REJECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Message of the log row of a request that was aborted when the gateway stopped.
const STOPPED_MESSAGE: &str = "gateway stopped";

/// The client name of the rows a replay writes.
pub(super) const REPLAY_CLIENT: &str = "SpanBay replay";

/// The body type of every gateway response (fixed or streamed).
pub type GatewayBody = UnsyncBoxBody<Bytes, Infallible>;

/// What the manager tells the accept loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Control {
    /// Keep serving.
    Run,
    /// Stop accepting; open connections may finish for this long.
    Drain(Duration),
    /// Stop accepting and abort everything now.
    Abort,
}

/// The failover settings the gateway started with: a change of the Failover group applies after
/// a restart only, so the request path reads these, never the live
/// settings.
#[derive(Debug, Clone, Copy)]
pub(super) struct Failover {
    pub(super) circuit: CircuitConfig,
    /// `1 + max_retries`.
    pub(super) max_attempts: usize,
    pub(super) first_byte: Duration,
    pub(super) idle: Duration,
}

/// State shared by every connection of one started gateway.
pub(super) struct Shared {
    pub(super) llm: Arc<LlmRuntime>,
    /// The port this gateway is bound to (what a legitimate `Host` header names).
    pub(super) port: u16,
    /// Listening on all interfaces: the Host guard also accepts the machine's LAN names.
    pub(super) lan: bool,
    pub(super) limits: Limits,
    pub(super) failover: Failover,
    pub(super) requests: AtomicU64,
    pub(super) in_flight: AtomicU64,
    pub(super) open_connections: AtomicU64,
    pub(super) rejected_connections: AtomicU64,
    pub(super) auth_failures: AtomicU64,
    /// The gateway is aborting its open requests: their canceled rows say why.
    pub(super) aborting: AtomicBool,
    pub(super) throttle: AuthThrottle,
    /// The request-body bytes still available to requests being read ([`Limits::body_budget`]).
    pub(super) body_budget: Arc<Semaphore>,
    pub(super) touches: TouchBook,
    /// The clients' limits state (rolling minutes, requests in flight, spend readings).
    pub(super) quotas: Arc<ClientQuotas>,
    pub(super) seen_tokens: Arc<SeenTokens>,
    pub(super) lan_names: Arc<LanNames>,
    /// The runtime's log writer, shared with the chat client; the gateway only queues into it.
    log: Arc<LogQueue>,
    /// What the writer had lost when this gateway started: status counts from there.
    dropped_at_start: (u64, u64),
}

impl Shared {
    /// Builds the state over the runtime's log writer (started on first use).
    pub(super) fn new(
        llm: Arc<LlmRuntime>,
        port: u16,
        lan: bool,
        limits: Limits,
        lan_names: Arc<LanNames>,
    ) -> Result<Self, AppError> {
        let settings = llm.settings();
        let gateway = &settings.ai.gateway;
        let failover = Failover {
            circuit: circuit_config(gateway),
            max_attempts: usize::from(gateway.max_retries) + 1,
            first_byte: Duration::from_secs(u64::from(gateway.first_byte_timeout_s)),
            idle: Duration::from_secs(u64::from(gateway.idle_timeout_s)),
        };
        let log = llm.log_with(limits.log_queue)?;
        let seen_tokens = Arc::clone(log.seen());
        Ok(Self {
            llm,
            port,
            lan,
            limits,
            failover,
            requests: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            open_connections: AtomicU64::new(0),
            rejected_connections: AtomicU64::new(0),
            auth_failures: AtomicU64::new(0),
            aborting: AtomicBool::new(false),
            throttle: AuthThrottle::new(limits.auth_failures_per_minute, limits.auth_row_interval),
            body_budget: Arc::new(Semaphore::new(limits.body_budget)),
            touches: TouchBook::default(),
            quotas: Arc::new(ClientQuotas::default()),
            seen_tokens,
            lan_names,
            dropped_at_start: (log.dropped(), log.captures_dropped()),
            log,
        })
    }

    /// Log jobs lost so far.
    pub(super) fn log_dropped(&self) -> u64 {
        self.log.dropped().saturating_sub(self.dropped_at_start.0)
    }

    /// Captures lost so far (their rows are kept).
    pub(super) fn captures_dropped(&self) -> u64 {
        self.log
            .captures_dropped()
            .saturating_sub(self.dropped_at_start.1)
    }

    /// Waits until what is queued is stored (once no request can log any more); the writer
    /// stays up for the chat client.
    pub(super) fn flush_log(&self) {
        self.log.flush();
    }
}

/// What is known about one request and shared by every clone of its [`Context`].
struct RequestState {
    /// A row for this request was queued.
    logged: AtomicBool,
    /// The request is the Verify test request (`source = probe`).
    probe: AtomicBool,
    identity: Mutex<Identity>,
    /// `provider_id/model` of the latest attempt and how many attempts were made.
    target: Mutex<Option<String>>,
    attempts: std::sync::atomic::AtomicU32,
}

/// Everything a request handler needs.
#[derive(Clone)]
pub(super) struct Context {
    pub(super) llm: Arc<LlmRuntime>,
    pub(super) shared: Arc<Shared>,
    /// `rq_<16 hex>`: sent to the client and stored in the row.
    pub(super) request_id: String,
    pub(super) peer: SocketAddr,
    started: Instant,
    ts_ms: i64,
    state: Arc<RequestState>,
}

impl Context {
    /// The context of a request just accepted from `peer`.
    pub(super) fn new(shared: Arc<Shared>, peer: SocketAddr) -> Self {
        Self {
            llm: Arc::clone(&shared.llm),
            request_id: store::new_request_id(),
            peer,
            started: Instant::now(),
            ts_ms: now_ms(),
            state: Arc::new(RequestState {
                logged: AtomicBool::new(false),
                probe: AtomicBool::new(false),
                identity: Mutex::new(Identity::Pending),
                target: Mutex::new(None),
                attempts: std::sync::atomic::AtomicU32::new(0),
            }),
            shared,
        }
    }

    /// Who is calling.
    pub(super) fn identity(&self) -> Identity {
        self.state
            .identity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// What the client must not read in text of upstream origin: every stored secret and the
    /// client tokens seen so far (the same needles the log writer redacts with).
    pub(super) async fn redaction_needles(&self) -> Vec<String> {
        let llm = Arc::clone(&self.llm);
        let mut needles =
            tokio::task::spawn_blocking(move || llm.secrets().active_redaction_needles())
                .await
                .unwrap_or_default();
        needles.extend(self.shared.seen_tokens.snapshot());
        needles
    }

    /// Notes an attempt at `provider_id`/`model` (the response headers name the last one).
    pub(super) fn note_attempt(&self, provider_id: &str, model: &str) {
        self.state.attempts.fetch_add(1, Ordering::Relaxed);
        *self
            .state
            .target
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(format!("{provider_id}/{model}"));
    }

    /// The last attempted target and the number of attempts, once one was made.
    pub(super) fn attempted(&self) -> Option<(String, u32)> {
        let target = self
            .state
            .target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        Some((target, self.state.attempts.load(Ordering::Relaxed)))
    }

    /// Records who is calling.
    pub(super) fn set_identity(&self, identity: Identity) {
        *self
            .state
            .identity
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = identity;
    }

    /// Whether a row of this request was queued.
    pub(super) fn logged(&self) -> bool {
        self.state.logged.load(Ordering::Relaxed)
    }

    /// Marks the request as the Verify test request: its row is stored with `source = probe`.
    pub(super) fn mark_probe(&self) {
        self.state.probe.store(true, Ordering::Relaxed);
    }

    /// Queues the log job of a handled request and counts it in the request total. Never blocks;
    /// usable from `Drop`. When the writer falls a queue behind, the newest job is dropped (it
    /// is still counted here).
    pub(super) fn submit(&self, job: LogJob) {
        self.shared.requests.fetch_add(1, Ordering::Relaxed);
        self.enqueue(job);
    }

    fn enqueue(&self, mut job: LogJob) {
        let entry = &mut job.entry;
        entry.request_id = Some(self.request_id.clone());
        match self.identity() {
            Identity::Client(client) => {
                entry.client_id = Some(client.id.clone());
                entry.client_name = Some(client.name.clone());
            }
            Identity::Anonymous => entry.client_name = Some(ANONYMOUS.to_owned()),
            Identity::Replay { of } => {
                entry.client_name = Some(REPLAY_CLIENT.to_owned());
                entry.replay_of = Some(of);
            }
            Identity::Pending => {}
        }
        if self.state.probe.load(Ordering::Relaxed) {
            entry.source = "probe".to_owned();
        }
        if !self.peer.ip().is_loopback() {
            entry.peer = Some(self.peer.to_string());
        }
        if entry.outcome == "canceled"
            && entry.error.is_none()
            && self.shared.aborting.load(Ordering::Relaxed)
        {
            entry.error = Some(STOPPED_MESSAGE.to_owned());
        }
        self.state.logged.store(true, Ordering::Relaxed);
        self.shared.log.submit(job);
    }

    /// Answers a request the gateway refuses itself, in the dialect of `route`, and (when `log`)
    /// writes its row: status, the reason as `error`, a `failure_kind`, the peer, no body.
    pub(super) fn refuse(
        &self,
        refusal: &Refusal,
        route: &Route,
        model: Option<&str>,
        log: bool,
    ) -> Response<GatewayBody> {
        if log {
            self.enqueue(LogJob::from_entry(RequestLogEntry {
                ts_ms: self.ts_ms,
                protocol_in: route.protocol_in().to_owned(),
                requested_model: model.unwrap_or_default().to_owned(),
                status: refusal.status(),
                outcome: "failure".to_owned(),
                latency_ms: elapsed_ms(self.started),
                error: Some(refusal.reason().to_owned()),
                failure_kind: Some(refusal.failure_kind()),
                ..RequestLogEntry::default()
            }));
        }
        refusal.response(route.family())
    }

    /// The row of a request that was in flight when the gateway aborted it (non-streaming calls
    /// and heads that never reached a stream; streams log themselves when dropped).
    fn log_aborted(&self, route: &Route) {
        self.submit(LogJob::from_entry(RequestLogEntry {
            ts_ms: self.ts_ms,
            protocol_in: route.protocol_in().to_owned(),
            status: 503,
            outcome: "canceled".to_owned(),
            latency_ms: elapsed_ms(self.started),
            error: Some(STOPPED_MESSAGE.to_owned()),
            failure_kind: Some(FailureKind::Cancelled),
            ..RequestLogEntry::default()
        }));
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Counts a request as in flight until it is dropped (with its response body).
pub(super) struct InFlight(Arc<Shared>);

impl InFlight {
    pub(super) fn enter(shared: &Arc<Shared>) -> Self {
        shared.in_flight.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(shared))
    }

    /// `response` with its body holding this guard: the request stays in flight while the body
    /// streams, however long that takes.
    pub(super) fn follow(self, response: Response<GatewayBody>) -> Response<GatewayBody> {
        response.map(|body| {
            body.map_frame(move |frame| {
                let _alive = &self;
                frame
            })
            .boxed_unsync()
        })
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Counts a connection as open until it is dropped (also when its task is aborted).
struct OpenConnection(Arc<Shared>);

impl Drop for OpenConnection {
    fn drop(&mut self) {
        self.0.open_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Logs the request as canceled ("gateway stopped") when the handler future is dropped by an
/// abort before it produced a response. Disarmed once the handler returns.
pub(super) struct AbortLog<'a> {
    pub(super) context: &'a Context,
    pub(super) route: &'a Route,
    pub(super) armed: bool,
}

impl Drop for AbortLog<'_> {
    fn drop(&mut self) {
        if self.armed
            && !self.context.logged()
            && self.context.shared.aborting.load(Ordering::Relaxed)
        {
            self.context.log_aborted(self.route);
        }
    }
}

/// Accepts connections until `control` says to stop, then drops the listener and lets the open
/// connections finish their request for the drain time before aborting the rest.
pub(super) async fn accept_loop(
    listener: tokio::net::TcpListener,
    shared: Arc<Shared>,
    mut control: watch::Receiver<Control>,
    closed: std::sync::mpsc::Sender<()>,
) {
    let permits = Arc::new(Semaphore::new(shared.limits.max_connections));
    let rejecters = Arc::new(Semaphore::new(REJECT_WRITERS));
    let (drain_tx, drain_rx) = watch::channel(false);
    let mut connections = JoinSet::new();
    let drain_for = loop {
        tokio::select! {
            changed = control.changed() => {
                if changed.is_err() {
                    break Duration::ZERO;
                }
                match *control.borrow_and_update() {
                    Control::Run => {}
                    Control::Drain(limit) => break limit,
                    Control::Abort => break Duration::ZERO,
                }
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let _ = stream.set_nodelay(true);
                    if let Ok(permit) = Arc::clone(&permits).try_acquire_owned() {
                        let shared = Arc::clone(&shared);
                        let drain = drain_rx.clone();
                        shared.open_connections.fetch_add(1, Ordering::Relaxed);
                        connections.spawn(async move {
                            let _open = OpenConnection(Arc::clone(&shared));
                            serve_connection(stream, peer, shared, drain).await;
                            drop(permit);
                        });
                    } else {
                        shared.rejected_connections.fetch_add(1, Ordering::Relaxed);
                        if let Ok(permit) = Arc::clone(&rejecters).try_acquire_owned() {
                            connections.spawn(async move {
                                reject(stream).await;
                                drop(permit);
                            });
                        }
                    }
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            },
        }
    };
    drop(listener);
    // Open connections finish the request they are on and close; idle ones close at once.
    let _ = drain_tx.send(true);
    let drained = tokio::time::timeout(drain_for, async {
        loop {
            tokio::select! {
                next = connections.join_next() => {
                    if next.is_none() {
                        break;
                    }
                }
                changed = control.changed() => {
                    if changed.is_err() || *control.borrow_and_update() == Control::Abort {
                        break;
                    }
                }
            }
        }
    })
    .await;
    if drained.is_err() || !connections.is_empty() {
        shared.aborting.store(true, Ordering::Relaxed);
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    let _ = closed.send(());
}

async fn serve_connection(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    shared: Arc<Shared>,
    mut drain: watch::Receiver<bool>,
) {
    let stall = shared.limits.write_stall;
    let service = service_fn(move |request| {
        let context = Context::new(Arc::clone(&shared), peer);
        async move { Ok::<_, Infallible>(super::handlers::handle(context, request).await) }
    });
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT)
        .keep_alive(true)
        .serve_connection(TokioIo::new(StallGuard::new(stream, stall)), service);
    tokio::pin!(connection);
    tokio::select! {
        // A client hanging up mid-request or a malformed head just ends the connection.
        _ = connection.as_mut() => {}
        // The guard `wait_for` returns must not live across the await below.
        () = async { let _ = drain.wait_for(|draining| *draining).await; } => {
            connection.as_mut().graceful_shutdown();
            let _ = connection.await;
        }
    }
}

/// Answers a connection over the cap with `503 overloaded_error` and `Retry-After: 1`, then
/// closes it.
async fn reject(mut stream: tokio::net::TcpStream) {
    let body = serde_json::to_vec(&serde_json::json!({"error": {
        "message": "The gateway has too many open connections. Try again shortly.",
        "type": "overloaded_error", "param": null, "code": null,
    }}))
    .unwrap_or_default();
    let head = format!(
        "HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nretry-after: 1\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let answer = async {
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(&body).await?;
        stream.shutdown().await?;
        // Read what the client already sent so closing does not reset the connection and eat
        // the answer.
        let mut sink = [0_u8; 1024];
        let _ = stream.read(&mut sink).await;
        Ok::<_, std::io::Error>(())
    };
    let _ = tokio::time::timeout(REJECT_TIMEOUT, answer).await;
}

/// A fixed-body response.
pub(super) fn fixed(
    status: StatusCode,
    content_type: &str,
    body: Vec<u8>,
) -> Response<GatewayBody> {
    let mut response = Response::new(Full::new(Bytes::from(body)).boxed_unsync());
    *response.status_mut() = status;
    if let Ok(value) = content_type.parse() {
        response
            .headers_mut()
            .insert(hyper::header::CONTENT_TYPE, value);
    }
    response
}

/// A JSON response.
pub(super) fn json(status: u16, value: &serde_json::Value) -> Response<GatewayBody> {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    fixed(
        status,
        "application/json",
        serde_json::to_vec(value).unwrap_or_default(),
    )
}

/// The gateway's own HTTP status for an [`AppError`].
pub(super) fn status_of(error: &AppError) -> u16 {
    match error {
        AppError::NotFound { .. } => 404,
        AppError::Validation { .. } | AppError::Unsupported { .. } => 400,
        AppError::Conflict { .. } => 409,
        AppError::Cancelled { .. } => 499,
        AppError::Network { .. } => 502,
        _ => 500,
    }
}
