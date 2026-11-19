//! The request log: paged list, the drawer's detail, body search, count, clear, capture
//! deletion, "Copy as curl" and "Open in API client".
//!
//! Single responsibility: shell-facing reads and deletes over `llm::store::{log, captures}` and
//! the pure builders of `llm::capture`. Search, chips and sorting run in the webview over the
//! rows these calls page in; the core only pages, filters by source and provider,
//! searches bodies with a bound parameter, and rebuilds what the drawer shows from a stored
//! capture — the merged reply is decoded here with the hub decoders, never in the webview.
//! Replay ([`llm_log_replay_impl`]) re-sends a stored client request through the gateway's own
//! request handler in-process: the pipeline is `llm::gateway::replay`, this module
//! checks eligibility, registers the cancel id and spawns the task.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::client_writers::gateway_url;
use super::{with_connection, with_db_blocking};
use crate::app::AppState;
use crate::cancel::CancelToken;
use crate::error::AppError;
use crate::events::EventSink;
use crate::llm::capture::{
    ApiClientDraft, CurlKind, CurlOutput, MergedReply, MergedSource, RowFacts, build_curl,
    merged_reply, open_in_api_client, replay_eligibility,
};
use crate::llm::capture_record::{CapturedBody, CapturedExchange, CapturedFrames, TimingJson};
use crate::llm::gateway::replay::{ReplayEvent, ReplayMode, ReplayRequest, replay};
use crate::llm::store::{
    self, ClearReport, DEFAULT_LOG_PAGE, LogDetail, LogFilter, LogPage, MAX_LOG_PAGE,
    RequestLogEntry,
};
use crate::llm::wire::Protocol;

/// Whether an action on a row is offered, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Availability {
    /// The button is enabled.
    pub available: bool,
    /// The tooltip and in-text reason when it is not.
    pub reason: Option<String>,
    /// Visible warnings of an enabled action ("Body was cut at 256 KiB").
    pub warnings: Vec<String>,
    /// Replay only: allowed once the user confirms (the body holds redacted values).
    pub needs_confirmation: bool,
}

impl Availability {
    fn yes(warnings: Vec<String>) -> Self {
        Self {
            available: true,
            reason: None,
            warnings,
            needs_confirmation: false,
        }
    }

    fn no(reason: String, needs_confirmation: bool) -> Self {
        Self {
            available: false,
            reason: Some(reason),
            warnings: Vec::new(),
            needs_confirmation,
        }
    }
}

/// Everything the debug drawer shows for one row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogDetailDto {
    /// The row.
    pub entry: RequestLogEntry,
    /// Its stored, redacted capture; `None` when capture was off for it or it was deleted.
    pub capture: Option<CapturedExchange>,
    /// The reply the client received, reassembled by the hub decoders.
    pub merged_reply: Option<MergedReply>,
    /// For a translated row, the reply the provider sent; `None` otherwise.
    pub merged_upstream_reply: Option<MergedReply>,
    /// "Copy as curl" of the gateway request.
    pub curl_gateway: Availability,
    /// "Copy as curl" of the upstream request.
    pub curl_upstream: Availability,
    /// "Replay".
    pub replay: Availability,
    /// "Open in API client" is always offered (an empty body opens with a note).
    pub open_in_api_client: bool,
}

/// The input of [`super::llm_diagnostics_bundle_impl`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsBundleInput {
    /// The preview switch "Include provider addresses" (off by default).
    #[serde(default)]
    pub include_provider_addresses: bool,
    /// The drawer's switch "Include this request's bodies": the row whose stored (already
    /// redacted) capture is added; the bundle then lists that row and the 20 before it.
    #[serde(default)]
    pub include_bodies_of: Option<i64>,
}

/// One page of rows, newest first: those older than `before_id`, at most `limit` (default 200,
/// at most 500). Of `filter` only `source` and `provider_id` apply to a page (the rest of the
/// search runs in the webview); [`llm_log_count_impl`] and [`llm_log_clear_impl`] honour
/// every field.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_log_list_impl(
    state: &AppState,
    filter: Option<LogFilter>,
    limit: Option<u32>,
    before_id: Option<i64>,
) -> Result<Vec<RequestLogEntry>, AppError> {
    let filter = filter.unwrap_or_default();
    let limit = limit.map_or(DEFAULT_LOG_PAGE, |limit| limit as usize);
    with_connection(state, |connection| {
        store::list_log(
            connection,
            &LogPage {
                before_id,
                limit: limit.clamp(1, MAX_LOG_PAGE),
                source: filter.source,
                provider_id: filter.provider_id,
            },
        )
    })
}

/// How many rows `filter` selects (the "Delete 3,412 log rows" confirm).
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_log_count_impl(state: &AppState, filter: Option<LogFilter>) -> Result<u64, AppError> {
    with_connection(state, |connection| {
        store::count_log(connection, &filter.unwrap_or_default())
    })
}

/// Cancels its token when dropped: a search whose caller stopped waiting also stops reading.
struct CancelOnDrop(CancelToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Ids of the rows whose stored request or response bodies contain `text` (`body:`
/// terms): case-insensitive, newest first, at most 5,000. `text` is only ever a bound parameter.
/// Runs on its own connection, so it never holds the store lock. The search reads the newest
/// [`store::MAX_SEARCH_ROWS`] captures for at most [`store::SEARCH_TIME_BUDGET`]; when either
/// runs out, or the caller drops the future, it returns the matches found so far (no marker: the
/// result type has none).
///
/// # Errors
/// [`AppError::Database`] on failure.
pub async fn llm_log_search_bodies_impl(
    state: &AppState,
    text: &str,
) -> Result<Vec<i64>, AppError> {
    let text = text.to_owned();
    let cancel = CancelOnDrop(CancelToken::new());
    let budget = store::SearchBudget::new(cancel.0.clone());
    with_db_blocking(state, move |connection| {
        store::search_log_bodies(connection, &text, &budget)
    })
    .await
}

/// Deletes the rows `filter` selects (every row without one) and their captures, in bounded
/// batches on its own connection; the daily usage rollup stays, so Today is unchanged.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub async fn llm_log_clear_impl(
    state: &AppState,
    filter: Option<LogFilter>,
) -> Result<ClearReport, AppError> {
    let filter = filter.unwrap_or_default();
    with_db_blocking(state, move |connection| {
        store::clear_log(connection, &filter)
    })
    .await
}

/// "Delete captures only": removes every stored capture, keeps rows and the rollup, in
/// bounded batches on its own connection. Returns the bytes freed.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub async fn llm_log_delete_captures_impl(state: &AppState) -> Result<u64, AppError> {
    with_db_blocking(state, store::delete_captures).await
}

/// How many captures are stored and the bytes "Delete captures only" would free. Runs
/// on its own connection.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub async fn llm_log_capture_stats_impl(state: &AppState) -> Result<store::CaptureStats, AppError> {
    with_db_blocking(state, store::capture_stats).await
}

/// "Delete this capture": removes one row's capture, keeps the row. Returns the bytes
/// freed, 0 when the row had none.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_log_delete_capture_impl(state: &AppState, log_id: i64) -> Result<u64, AppError> {
    with_connection(state, |connection| {
        store::delete_capture(connection, log_id)
    })
}

/// The row and what the drawer derives from it.
///
/// # Errors
/// [`AppError::NotFound`] when the row was pruned ("This request was removed by log retention"),
/// [`AppError::Database`] on failure.
pub fn llm_log_get_impl(state: &AppState, id: i64) -> Result<LogDetailDto, AppError> {
    let (detail, provider_exists) = row_with_provider(state, id)?;
    let base = gateway_url(state);
    Ok(detail_dto(detail, provider_exists, &base))
}

/// "Copy as curl": a runnable command from the stored capture, credentials as placeholders
/// (`$PINGDECK_TOKEN`, `$PROVIDER_API_KEY`) — never the stored masked text.
///
/// # Errors
/// [`AppError::NotFound`] for a pruned row, [`AppError::Validation`] with the reason
/// when no command can be built.
pub fn llm_log_curl_impl(
    state: &AppState,
    id: i64,
    kind: CurlKind,
) -> Result<CurlOutput, AppError> {
    let (detail, provider_exists) = row_with_provider(state, id)?;
    let capture = detail
        .capture
        .as_ref()
        .ok_or_else(|| AppError::validation("No request body was stored"))?;
    build_curl(
        capture,
        &row_facts(&detail.entry, provider_exists),
        kind,
        &gateway_url(state),
    )
    .map_err(|refusal| AppError::validation(refusal.reason))
}

/// "Open in API client": the unsaved AI-mode request tab the row creates: the
/// post-translation upstream request when the capture has it, else the client request aimed at
/// the gateway; with no capture an empty body and the note "The body was not captured".
///
/// # Errors
/// [`AppError::NotFound`] for a pruned row, [`AppError::Database`] on failure.
pub fn llm_log_open_in_api_client_impl(
    state: &AppState,
    id: i64,
) -> Result<ApiClientDraft, AppError> {
    let (detail, provider_exists) = row_with_provider(state, id)?;
    let capture = detail
        .capture
        .clone()
        .unwrap_or_else(|| empty_capture(&detail.entry));
    Ok(open_in_api_client(
        &capture,
        &row_facts(&detail.entry, provider_exists),
        &gateway_url(state),
    ))
}

fn row_with_provider(state: &AppState, id: i64) -> Result<(LogDetail, bool), AppError> {
    with_connection(state, |connection| {
        let detail = store::get_log(connection, id)?.ok_or_else(|| {
            AppError::not_found(format!(
                "request {id} was removed by log retention or never existed"
            ))
        })?;
        let provider_exists = match &detail.entry.provider_id {
            Some(provider_id) => store::get_provider(connection, provider_id)?.is_some(),
            None => false,
        };
        Ok((detail, provider_exists))
    })
}

fn protocol_of(text: &str) -> Option<Protocol> {
    Protocol::parse(text)
}

/// The facts of a row curl, replay and "Open in API client" need beyond the capture.
fn row_facts(entry: &RequestLogEntry, provider_exists: bool) -> RowFacts {
    RowFacts {
        protocol_out: entry.protocol_out.as_deref().and_then(protocol_of),
        requested_model: entry.requested_model.clone(),
        upstream_model: entry.upstream_model.clone(),
        stream: entry.stream,
        provider_id: entry.provider_id.clone(),
        provider_exists,
    }
}

/// A capture with nothing in it, for a row stored without one.
fn empty_capture(entry: &RequestLogEntry) -> CapturedExchange {
    CapturedExchange {
        request_log_id: entry.id,
        ts_ms: entry.ts_ms,
        client_protocol: protocol_of(&entry.protocol_in).unwrap_or(Protocol::OpenaiChat),
        client_req_headers: Vec::new(),
        client_req_body: None,
        hub_request: None,
        upstream_url: String::new(),
        upstream_req_headers: Vec::new(),
        upstream_req_body: None,
        upstream_status: None,
        upstream_resp_headers: Vec::new(),
        upstream_resp_body: None,
        client_resp_headers: Vec::new(),
        client_resp_body: None,
        upstream_frames: None,
        client_frames: None,
        translation_notes: Vec::new(),
        timing: TimingJson::default(),
        bytes_total: 0,
        redacted_count: 0,
    }
}

/// The reply one side holds: its frames when the stream was stored, else its body.
fn side_reply(
    protocol: Protocol,
    frames: Option<&CapturedFrames>,
    body: Option<&CapturedBody>,
) -> Option<MergedReply> {
    match (frames, body) {
        (Some(frames), _) => Some(merged_reply(protocol, MergedSource::Frames(frames))),
        (None, Some(body)) if body.text.is_some() => {
            Some(merged_reply(protocol, MergedSource::Body(body)))
        }
        _ => None,
    }
}

fn availability_of_curl(
    capture: Option<&CapturedExchange>,
    facts: &RowFacts,
    kind: CurlKind,
    base: &str,
) -> Availability {
    let Some(capture) = capture else {
        return Availability::no("No request body was stored".to_owned(), false);
    };
    match build_curl(capture, facts, kind, base) {
        Ok(output) => Availability::yes(output.warnings),
        Err(refusal) => Availability::no(refusal.reason, false),
    }
}

fn detail_dto(detail: LogDetail, provider_exists: bool, gateway_base: &str) -> LogDetailDto {
    let LogDetail { entry, capture } = detail;
    let facts = row_facts(&entry, provider_exists);
    let (merged, merged_upstream) = capture.as_ref().map_or((None, None), |capture| {
        let client = side_reply(
            capture.client_protocol,
            capture.client_frames.as_ref(),
            capture.client_resp_body.as_ref(),
        );
        let upstream_protocol = facts.protocol_out.unwrap_or(capture.client_protocol);
        let upstream = side_reply(
            upstream_protocol,
            capture.upstream_frames.as_ref(),
            capture.upstream_resp_body.as_ref(),
        );
        if entry.translated {
            (client, upstream)
        } else {
            // Same protocol on both sides: one reply, whichever side was stored.
            (client.or(upstream), None)
        }
    });
    let replay = match replay_eligibility(capture.as_ref(), false) {
        Ok(()) => Availability::yes(Vec::new()),
        Err(block) => Availability::no(block.reason, block.needs_confirmation),
    };
    LogDetailDto {
        curl_gateway: availability_of_curl(
            capture.as_ref(),
            &facts,
            CurlKind::Gateway,
            gateway_base,
        ),
        curl_upstream: availability_of_curl(
            capture.as_ref(),
            &facts,
            CurlKind::Upstream,
            gateway_base,
        ),
        replay,
        open_in_api_client: true,
        merged_reply: merged,
        merged_upstream_reply: merged_upstream,
        entry,
        capture,
    }
}

/// What starting a replay returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayStarted {
    /// The id [`llm_log_replay_cancel_impl`] takes.
    pub replay_id: String,
    /// The row the replay will write, when it is already known; it is stored when the answer
    /// ends, so the end event ([`ReplayEvent::Done`]) carries it.
    pub new_log_id: Option<i64>,
}

/// Re-sends the stored client request of row `log_id` through the gateway's own handler, in
/// process (no HTTP hop, no token, no Host or scope check: it tests routing and the upstream), as
/// a new `gateway` row of the client "PingDeck replay" linked to the original. `mode`
/// either resolves the requested name again or pins the `provider/model` that served the original.
/// The answer streams into `sink` and ends with [`ReplayEvent::Done`]. It works with the gateway
/// stopped. Call it inside a Tokio runtime: the replay runs as a spawned task.
///
/// # Errors
/// [`AppError::NotFound`] for a pruned row, [`AppError::Validation`] with the reason when
/// the request cannot be reproduced (a body with redacted values needs `confirm_redacted`) or
/// when the original had no serving target for `ReplayOnServedTarget`,
/// [`AppError::Internal`] without a Tokio runtime.
pub fn llm_log_replay_impl(
    state: &AppState,
    log_id: i64,
    mode: ReplayMode,
    confirm_redacted: bool,
    sink: Arc<dyn EventSink<ReplayEvent>>,
) -> Result<ReplayStarted, AppError> {
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| AppError::internal("a replay needs a Tokio runtime"))?;
    let LogDetail { entry, capture } = with_connection(state, |connection| {
        store::get_log(connection, log_id)?.ok_or_else(|| {
            AppError::not_found(format!(
                "request {log_id} was removed by log retention or never existed"
            ))
        })
    })?;
    replay_eligibility(capture.as_ref(), confirm_redacted)
        .map_err(|block| AppError::validation(block.reason))?;
    let body = capture
        .as_ref()
        .and_then(|capture| capture.client_req_body.as_ref())
        .and_then(|body| body.text.clone())
        .ok_or_else(|| AppError::validation("No request body was stored"))?;
    let pin = match mode {
        ReplayMode::Replay => None,
        ReplayMode::ReplayOnServedTarget => Some(
            entry
                .provider_id
                .clone()
                .zip(entry.upstream_model.clone())
                .ok_or_else(|| AppError::validation("This request was not served by any target"))?,
        ),
    };
    let request = ReplayRequest {
        of: log_id,
        protocol: protocol_of(&entry.protocol_in)
            .or_else(|| capture.as_ref().map(|capture| capture.client_protocol))
            .unwrap_or(Protocol::OpenaiChat),
        requested_model: entry.requested_model,
        stream: entry.stream,
        body: body.into(),
        pin,
    };
    if serde_json::from_slice::<serde_json::Value>(&request.body).is_err() {
        return Err(AppError::validation(
            "The stored request body is not valid JSON, so it cannot be replayed",
        ));
    }
    let replay_id = format!("rp_{}", crate::workspace::variables::uuid_v4());
    let guard = state.cancellations.register_owned(Some(&replay_id))?;
    let llm = Arc::clone(&state.llm);
    runtime.spawn(async move {
        let outcome = replay(&llm, request, sink.as_ref(), guard.token()).await;
        if outcome.is_err() {
            sink.emit(ReplayEvent::Done { new_log_id: None });
        }
        drop(guard);
    });
    Ok(ReplayStarted {
        replay_id,
        new_log_id: None,
    })
}

/// Stops a running replay (the Stop button): the upstream request closes within a second and the
/// row is stored `canceled`. Returns whether the replay was still running.
pub fn llm_log_replay_cancel_impl(state: &AppState, replay_id: &str) -> bool {
    state.cancellations.cancel(replay_id)
}
