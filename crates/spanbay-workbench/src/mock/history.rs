//! Mock hits as history rows: row construction and the background writer.
//!
//! Single responsibility: turn every request a running mock served into an
//! [`ExecutionHistoryEntry`] (`source = Mock`) and hand it to a [`MockHistorySink`]. The shipped
//! sink, [`MockHistoryWriter`], never blocks the mock's reply: rows go through a bounded channel
//! with `try_send` (a full queue drops the row and counts it) to ONE dedicated thread, which
//! writes them to the store in batches of at most [`MAX_BATCH_ROWS`] rows or [`MAX_BATCH_WAIT`].
//! A plain thread is used because no tokio runtime is guaranteed to exist next to a store (the
//! in-memory state of sync tests has none). Retention per source is the store's job.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::logging::{LogLevel, log_line};
use crate::workspace::WorkspaceStore;
use crate::workspace::items::ExecutedRequestSummary;
use crate::workspace::items::MockServer;
use crate::workspace::models::{
    ExecutionHistoryEntry, ExecutionHistoryResultKind, HISTORY_BODY_PREVIEW_LIMIT_BYTES,
    HistoryEntryId, HistoryOriginRef, HistorySource, HttpMethod, KeyValue, WorkspaceId,
    bounded_body_preview, capped_masked_headers,
};

use super::http::ParsedRequest;
use super::logs::now_ms;
use super::reply::MockReply;

/// Rows the writer queue holds before further rows are dropped.
pub const HISTORY_QUEUE_CAPACITY: usize = 1000;

/// Most rows one store transaction carries.
const MAX_BATCH_ROWS: usize = 100;

/// Longest a row waits for companions before its batch is written.
const MAX_BATCH_WAIT: Duration = Duration::from_millis(50);

/// Protocol recorded on every mock row: the listener speaks HTTP/1.1 only.
const MOCK_PROTOCOL: &str = "HTTP/1.1";

/// Receives the history row of every request a mock served. Implementations must return
/// quickly: the call sits on the path of the mock's reply.
pub trait MockHistorySink: Send + Sync {
    /// Queues `entry`; a sink that cannot keep up drops it rather than waiting.
    fn submit(&self, entry: ExecutionHistoryEntry);
}

/// What travels to the writer thread.
enum Message {
    Row(Box<ExecutionHistoryEntry>),
    /// Barrier: acknowledged once every row queued before it has been written.
    Flush(mpsc::Sender<()>),
}

/// Writes mock-hit rows on a dedicated thread; see the module docs.
///
/// Dropping the writer closes the queue, lets the thread write what remains and joins it, so no
/// accepted row is lost at shutdown.
pub struct MockHistoryWriter {
    /// `None` only while dropping.
    sender: Option<SyncSender<Message>>,
    thread: Option<JoinHandle<()>>,
    dropped: AtomicU64,
}

impl MockHistoryWriter {
    /// A writer persisting into `store` (short lock per batch).
    ///
    /// # Errors
    /// The writer thread could not be spawned.
    pub fn with_store(store: std::sync::Arc<Mutex<WorkspaceStore>>) -> std::io::Result<Self> {
        Self::spawn(HISTORY_QUEUE_CAPACITY, move |rows| {
            persist_rows(&store, rows);
        })
    }

    /// A writer with `capacity` queue slots handing each batch to `persist` (the store-backed
    /// writer and the tests share this path).
    pub(super) fn spawn(
        capacity: usize,
        persist: impl FnMut(&[ExecutionHistoryEntry]) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let thread = std::thread::Builder::new()
            .name("spanbay-mock-history".into())
            .spawn(move || run(&receiver, persist))?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
            dropped: AtomicU64::new(0),
        })
    }

    /// Rows dropped so far because the queue was full (or the writer thread had died).
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Blocks until every row accepted before this call has been written; for shutdown and
    /// tests. Never call it from the mock's request path.
    pub fn flush(&self) {
        let Some(sender) = &self.sender else { return };
        let (ack, done) = mpsc::channel();
        // A blocking send: the barrier must not be dropped when the queue is full.
        if sender.send(Message::Flush(ack)).is_ok() {
            let _ = done.recv();
        }
    }
}

impl MockHistorySink for MockHistoryWriter {
    fn submit(&self, entry: ExecutionHistoryEntry) {
        let Some(sender) = &self.sender else { return };
        match sender.try_send(Message::Row(Box::new(entry))) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for MockHistoryWriter {
    fn drop(&mut self) {
        // The thread leaves its loop once the queue is closed and drained.
        drop(self.sender.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The writer thread: collect rows into a batch (until it is full, its wait is over or a
/// barrier arrives), persist it, acknowledge the barrier, repeat; exit when the queue is closed
/// and empty.
fn run(receiver: &Receiver<Message>, mut persist: impl FnMut(&[ExecutionHistoryEntry])) {
    let mut batch: Vec<ExecutionHistoryEntry> = Vec::with_capacity(MAX_BATCH_ROWS);
    let mut open = true;
    while open {
        let mut barrier = None;
        match receiver.recv() {
            Ok(Message::Row(row)) => batch.push(*row),
            Ok(Message::Flush(ack)) => barrier = Some(ack),
            Err(_) => break,
        }
        let deadline = Instant::now() + MAX_BATCH_WAIT;
        while barrier.is_none() && batch.len() < MAX_BATCH_ROWS {
            let wait = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(wait) {
                Ok(Message::Row(row)) => batch.push(*row),
                Ok(Message::Flush(ack)) => barrier = Some(ack),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => open = false,
            }
            if !open {
                break;
            }
        }
        if !batch.is_empty() {
            persist(&batch);
            batch.clear();
        }
        if let Some(ack) = barrier {
            let _ = ack.send(());
        }
    }
}

/// Writes one batch in one transaction, skipping rows whose workspace was deleted meanwhile
/// (their foreign key would fail the whole batch). A failed write is logged and dropped: history
/// is best effort and must never disturb the mock.
fn persist_rows(store: &Mutex<WorkspaceStore>, rows: &[ExecutionHistoryEntry]) {
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    let mut known: Vec<(&WorkspaceId, bool)> = Vec::new();
    for row in rows {
        if known.iter().all(|(id, _)| *id != &row.workspace_id) {
            let exists = store
                .get_workspace(&row.workspace_id)
                .ok()
                .flatten()
                .is_some();
            known.push((&row.workspace_id, exists));
        }
    }
    let is_live = |row: &ExecutionHistoryEntry| {
        known
            .iter()
            .any(|(id, exists)| *exists && *id == &row.workspace_id)
    };
    let result = if rows.iter().all(is_live) {
        store.save_execution_history_batch(rows)
    } else {
        let live: Vec<ExecutionHistoryEntry> =
            rows.iter().filter(|row| is_live(row)).cloned().collect();
        store.save_execution_history_batch(&live)
    };
    if let Err(error) = result {
        log_line(
            LogLevel::Warn,
            "mock.history",
            &format!("could not record {} mock hits: {error}", rows.len()),
        );
    }
}

/// The `HttpMethod` spelled by an HTTP method token, if it is one of the seven SpanBay knows.
fn history_method(method: &str) -> Option<HttpMethod> {
    Some(match method {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        _ => return None,
    })
}

/// The history row of one served request, or `None` when it cannot be recorded: a method that
/// is not one of the seven [`HttpMethod`] variants (`TRACE`, `CONNECT`, custom verbs) has no
/// representation in a history row, so such hits only appear in the live mock log.
///
/// `request_name` is the matched route's `METHOD pattern`; an unmatched or proxied request is
/// named by its path. The URL carries the listener's address (`authority`), so a copied URL
/// reaches the mock again; `log_id` is the traffic row the history row points back to.
pub(super) fn hit_row(
    config: &MockServer,
    authority: &str,
    log_id: &str,
    request: &ParsedRequest,
    reply: &MockReply,
    elapsed_ms: u64,
) -> Option<ExecutionHistoryEntry> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let method = history_method(&request.method)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    // The sequence keeps ids unique when two hits share a nanosecond reading.
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let id = HistoryEntryId::new(format!("hist_mock_{}_{nanos}_{sequence}", config.id)).ok()?;
    let request_name = reply
        .route_id
        .as_deref()
        .and_then(|route_id| config.routes.iter().find(|route| route.id == route_id))
        .map_or_else(
            || request.path.clone(),
            |route| format!("{} {}", route.method, route.path),
        );
    let request_url = if request.query.is_empty() {
        format!("http://{authority}{}", request.path)
    } else {
        format!("http://{authority}{}?{}", request.path, request.query)
    };
    let (response_body_preview, response_body_truncated) = bounded_body_preview(
        &String::from_utf8_lossy(&reply.body),
        HISTORY_BODY_PREVIEW_LIMIT_BYTES,
    );
    let headers: Vec<KeyValue> = reply
        .headers
        .iter()
        .map(|(key, value)| KeyValue {
            key: key.clone(),
            value: value.clone(),
            enabled: true,
            description: None,
        })
        .collect();
    let (response_headers, response_headers_truncated) = capped_masked_headers(&headers);
    // What was sent, masked like every other stored header, so "Replay" can resend it instead of
    // dropping the headers and the body.
    let sent: Vec<KeyValue> = request
        .headers
        .iter()
        .map(|(key, value)| KeyValue {
            key: key.clone(),
            value: value.clone(),
            enabled: true,
            description: None,
        })
        .collect();
    let (sent_headers, _) = capped_masked_headers(&sent);
    let (sent_body, _) = bounded_body_preview(&request.body, HISTORY_BODY_PREVIEW_LIMIT_BYTES);
    Some(ExecutionHistoryEntry {
        id,
        workspace_id: config.workspace_id.clone(),
        request_id: None,
        environment_id: None,
        result_kind: ExecutionHistoryResultKind::Success,
        request_name,
        method,
        final_url: request_url.clone(),
        request_url: request_url.clone(),
        status_code: reply.status,
        status_text: hyper::StatusCode::from_u16(reply.status)
            .ok()
            .and_then(|status| status.canonical_reason())
            .unwrap_or_default()
            .to_owned(),
        elapsed_ms: u128::from(elapsed_ms),
        body_size_bytes: reply.body.len(),
        attempt_count: 1,
        response_body_preview,
        response_body_truncated,
        error_kind: None,
        error_message: None,
        executed_at: now_ms().to_string(),
        request_snapshot: None,
        source: HistorySource::Mock,
        source_name: config.name.clone(),
        source_id: config.id.clone(),
        run_id: None,
        protocol: MOCK_PROTOCOL.to_owned(),
        timing: None,
        environment_name: String::new(),
        response_headers,
        response_headers_truncated,
        sent_request: Some(ExecutedRequestSummary {
            method: request.method.clone(),
            url: request_url,
            headers: sent_headers,
            body_preview: sent_body,
        }),
        call_chain: None,
        collection_path: Vec::new(),
        connect_phase: None,
        // A mock hit runs no checks: NULL, never 0 passed.
        check_summary: None,
        origin_ref: Some(HistoryOriginRef::MockTxn {
            id: log_id.to_owned(),
        }),
        origin_label: None,
        origin_turn_id: None,
    })
}
