//! Fire-and-forget outbound callbacks triggered by a matched response.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use reqwest::Method;
use url::Url;

use crate::workspace::items::{MockCallback, MockCallbackResult, MockResponse, MockServer};
use crate::workspace::models::HttpMethod;

use super::http::sanitized_header;
use super::logs::{elapsed_ms, record_callback};
use super::proxy::ensure_target_allowed;
use super::reply::MAX_LATENCY_MS;
use super::server::{ServerState, lock};
use super::template::{TemplateContext, render};

/// Callbacks allowed in flight at once across all mock servers; extras are dropped.
const MAX_CALLBACKS_IN_FLIGHT: usize = 8;

/// Callbacks currently in flight, across every mock server.
static CALLBACKS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// One reserved in-flight callback slot; released on drop, so a panicking or
/// dropped callback task cannot leak it.
pub(super) struct CallbackSlot(&'static AtomicUsize);

impl CallbackSlot {
    /// Reserves a slot unless `limit` callbacks are already counted in `counter`.
    pub(super) fn acquire(counter: &'static AtomicUsize, limit: usize) -> Option<Self> {
        if counter.fetch_add(1, Ordering::SeqCst) >= limit {
            counter.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(counter))
    }
}

impl Drop for CallbackSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A rendered callback holding its in-flight slot, waiting to be fired.
pub(super) struct PendingCallback {
    callback: MockCallback,
    slot: CallbackSlot,
}

/// Renders the callbacks attached to `response` and reserves their slots. A callback beyond
/// [`MAX_CALLBACKS_IN_FLIGHT`] is not fired; its result is returned at once.
pub(super) fn prepare_callbacks(
    config: &MockServer,
    response: &MockResponse,
    context: &TemplateContext<'_>,
) -> (Vec<PendingCallback>, Vec<MockCallbackResult>) {
    let mut pending = Vec::new();
    let mut dropped = Vec::new();
    for id in &response.callback_ids {
        let Some(callback) = config.callbacks.iter().find(|callback| &callback.id == id) else {
            continue;
        };
        let mut callback = callback.clone();
        callback.url = render(&callback.url, context);
        let Some(slot) = CallbackSlot::acquire(&CALLBACKS_IN_FLIGHT, MAX_CALLBACKS_IN_FLIGHT)
        else {
            // One row records at most this many unsent results, however many ids the response lists.
            if dropped.len() >= MAX_CALLBACKS_IN_FLIGHT {
                continue;
            }
            dropped.push(result(
                &callback,
                Err(format!(
                    "not sent: {MAX_CALLBACKS_IN_FLIGHT} callbacks already in flight"
                )),
                0,
            ));
            continue;
        };
        callback.body = render(&callback.body, context);
        for header in &mut callback.headers {
            header.value = render(&header.value, context);
        }
        pending.push(PendingCallback { callback, slot });
    }
    (pending, dropped)
}

/// Fires `pending` as tasks owned by the server (`state.callbacks`), so a slow callback never
/// delays a reply and stopping the server cancels the ones still waiting out their delay,
/// releasing their slots. Each result is added to log row `log_id`. Called once the row is
/// logged, so a result can never arrive before its row.
pub(super) fn spawn_callbacks(
    state: &Arc<ServerState>,
    log_id: &str,
    pending: Vec<PendingCallback>,
) {
    if pending.is_empty() {
        return;
    }
    let mut tasks = lock(&state.callbacks);
    while tasks.try_join_next().is_some() {}
    for PendingCallback { callback, slot } in pending {
        let state = Arc::clone(state);
        let log_id = log_id.to_owned();
        tasks.spawn(async move {
            let _slot = slot;
            let outcome = fire_callback(&state.client, &callback).await;
            record_callback(&state, &log_id, outcome);
        });
    }
}

fn result(
    callback: &MockCallback,
    outcome: Result<u16, String>,
    elapsed_ms: u64,
) -> MockCallbackResult {
    let (status, error) = match outcome {
        Ok(status) => (Some(status), None),
        Err(error) => (None, Some(error)),
    };
    MockCallbackResult {
        name: callback.name.clone(),
        url: callback.url.clone(),
        status,
        error,
        elapsed_ms,
    }
}

/// Sends one callback after its delay. A callback to a metadata or link-local address, an
/// invalid URL or a network failure is reported, never retried: the mock must not fail because
/// of a callback.
async fn fire_callback(client: &reqwest::Client, callback: &MockCallback) -> MockCallbackResult {
    if callback.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(callback.delay_ms.min(MAX_LATENCY_MS))).await;
    }
    let started = Instant::now();
    let outcome = send(client, callback).await;
    result(callback, outcome, elapsed_ms(started))
}

async fn send(client: &reqwest::Client, callback: &MockCallback) -> Result<u16, String> {
    let url = Url::parse(&callback.url).map_err(|_| "invalid URL".to_owned())?;
    ensure_target_allowed(&url).await?;
    let mut builder = client.request(method_for(callback.method), url);
    for header in callback.headers.iter().filter(|header| header.enabled) {
        if let Some((name, value)) = sanitized_header(&header.key, &header.value) {
            builder = builder.header(name, value);
        }
    }
    if !callback.body.is_empty() {
        builder = builder.body(callback.body.clone());
    }
    builder
        .send()
        .await
        .map(|response| response.status().as_u16())
        .map_err(|error| error.without_url().to_string())
}

fn method_for(method: HttpMethod) -> Method {
    match method {
        HttpMethod::Get => Method::GET,
        HttpMethod::Post => Method::POST,
        HttpMethod::Put => Method::PUT,
        HttpMethod::Patch => Method::PATCH,
        HttpMethod::Delete => Method::DELETE,
        HttpMethod::Head => Method::HEAD,
        HttpMethod::Options => Method::OPTIONS,
    }
}
