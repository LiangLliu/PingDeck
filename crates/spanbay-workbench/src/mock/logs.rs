//! Bounded in-memory request log and clock helpers.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::workspace::items::{
    MOCK_LOG_BODY_LIMIT_BYTES, MockCallbackResult, MockPhaseTiming, MockServer, MockTransactionLog,
};
use crate::workspace::models::{HISTORY_HEADERS_LIMIT_BYTES, KeyValue, bounded_body_preview};

use super::fanout::Subscriber;
use super::history::hit_row;
use super::http::ParsedRequest;
use super::reply::MockReply;
use super::server::ServerState;

/// Default in-memory request log cap per running server (`mock.log_capacity` overrides it).
pub const MOCK_LOG_CAPACITY: usize = 200;

/// Milliseconds since `started`, saturating.
pub(super) fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// `duration` in whole milliseconds, saturating (a phase timing).
pub(super) fn phase_ms(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

/// Wall-clock milliseconds since the Unix epoch.
pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// `headers` as log rows, keeping rows until [`HISTORY_HEADERS_LIMIT_BYTES`] of header text is
/// reached so one request cannot park a whole header block (up to 64 KiB) in every log entry.
fn capped_headers(headers: &[(String, String)]) -> Vec<KeyValue> {
    let mut total = 0_usize;
    headers
        .iter()
        .take_while(|(key, value)| {
            total = total.saturating_add(key.len() + value.len());
            total <= HISTORY_HEADERS_LIMIT_BYTES
        })
        .map(|(key, value)| KeyValue {
            key: key.clone(),
            value: value.clone(),
            enabled: true,
            description: None,
        })
        .collect()
}

/// Records a served request: the bounded request log and its live subscribers, the history
/// row and the hit counter. Returns the id of the log row.
pub(super) fn push_log(
    state: &Arc<ServerState>,
    config: &MockServer,
    request: &ParsedRequest,
    reply: &MockReply,
    elapsed: u64,
    timing: MockPhaseTiming,
) -> String {
    let mut entry = log_entry(state, config, request, reply, elapsed);
    entry.timing = Some(timing);
    if let Some(history) = &state.history
        && let Some(row) = hit_row(
            config,
            &state.bound.to_string(),
            &entry.id,
            request,
            reply,
            elapsed,
        )
    {
        entry.history_entry_id = Some(row.id.as_str().to_owned());
        history.submit(row);
    }
    state.hits.fetch_add(1, Ordering::Relaxed);
    let id = entry.id.clone();
    publish(state, entry);
    id
}

/// Records a request the server refused before routing it (a foreign `Host`, a connection
/// beyond the limit): a log row only, no history row and no hit.
pub(super) fn push_rejection(state: &Arc<ServerState>, request: &ParsedRequest, reply: &MockReply) {
    let config = Arc::clone(
        &state
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let entry = log_entry(state, &config, request, reply, 0);
    publish(state, entry);
}

/// Adds the result of a callback to its log row and the live streams.
pub(super) fn record_callback(state: &Arc<ServerState>, log_id: &str, result: MockCallbackResult) {
    for (subscriber, at) in state.log.record_callback(log_id, &result, Instant::now()) {
        spawn_flush_timer(state, subscriber, at);
    }
}

fn log_entry(
    state: &ServerState,
    config: &MockServer,
    request: &ParsedRequest,
    reply: &MockReply,
    elapsed: u64,
) -> MockTransactionLog {
    let sequence = state
        .sequence
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1);
    let (request_body, request_body_truncated) =
        bounded_body_preview(&request.body, MOCK_LOG_BODY_LIMIT_BYTES);
    let (response_body, response_body_truncated) = bounded_body_preview(
        &String::from_utf8_lossy(&reply.body),
        MOCK_LOG_BODY_LIMIT_BYTES,
    );
    MockTransactionLog {
        // The start time keeps ids of one server unique across restarts: history rows keep them.
        id: format!("log_{}_{sequence}", state.started_at_ms),
        server_id: config.id.clone(),
        timestamp_ms: now_ms(),
        method: request.method.clone(),
        path: request.path.clone(),
        query: request.query.clone(),
        request_headers: capped_headers(&request.headers),
        request_body,
        request_body_truncated,
        matched_route_id: reply.route_id.clone(),
        matched_response_id: reply.response_id.clone(),
        status_code: reply.status,
        response_headers: capped_headers(&reply.headers),
        response_body,
        response_body_truncated,
        proxied: reply.proxied,
        elapsed_ms: elapsed,
        timing: None,
        outcome: Some(reply.outcome),
        selected_by: reply.selection.clone(),
        route_label: reply.route_label.clone(),
        response_label: reply.response_label.clone(),
        callbacks: reply.callback_results.clone(),
        history_entry_id: None,
    }
}

fn publish(state: &Arc<ServerState>, entry: MockTransactionLog) {
    for (subscriber, at) in state.log.publish(entry, Instant::now()) {
        spawn_flush_timer(state, subscriber, at);
    }
}

/// Polls `subscriber` when its batch window ends, again for as long as rows keep arriving
/// inside the next windows. Runs on the server runtime, so the stream's trailing rows leave on
/// time even when no further request comes in.
fn spawn_flush_timer(state: &Arc<ServerState>, subscriber: Arc<Subscriber>, at: Instant) {
    let state = Arc::clone(state);
    let handle = state.handle.clone();
    handle.spawn(async move {
        let mut at = at;
        loop {
            tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
            match state.log.poll_subscriber(&subscriber, Instant::now(), true) {
                Some(next) => at = next,
                None => break,
            }
        }
    });
}
