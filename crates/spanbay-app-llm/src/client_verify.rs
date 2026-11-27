//! Verify: did the tool's setup work from the client's side?
//!
//! Single responsibility: the two answers of the Verify step. (a) "Waiting for the first request
//! from <tool>": the gateway records the first authenticated request of a client armed after
//! Connect ([`llm_client_first_request_impl`], free, needs no token). (b) "Send a test request"
//! ([`llm_client_verify_impl`], file writers only): the core reads the token it wrote from the
//! tool's config file and sends **one real HTTP request over loopback** with the tool's own
//! protocol to the chosen alias, then reports status, serving target and time to first token, or
//! the exact refusal. The request carries a one-shot secret the core minted, so the gateway
//! logs it as `source = probe`: it never counts in Today and never satisfies (a). The token
//! stays inside this function: it is not returned, logged or put in an error.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::client_writers::llm_client_inspect_in;
use super::{
    current_time_ms, llm_subscribe_impl, llm_unsubscribe_impl, with_connection, with_db_blocking,
};
use crate::app::AppState;
use crate::client_config::{self, ClientApp, DetectEnv};
use crate::crypto::sha256_hex;
use crate::error::AppError;
use crate::events::Sequenced;
use crate::llm::client_watch::VERIFY_HEADER;
use crate::llm::events::LlmEvent;
use crate::llm::gateway::GatewayState;
use crate::llm::store::{self, RequestLogEntry};
use crate::secrets::redact_secrets;

/// How long the loopback connect may take: a stopped gateway answers "stopped" within 3 s.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// The longest a test request may take end to end.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to wait for the log row of the test request after its answer.
const ROW_WAIT: Duration = Duration::from_secs(3);
/// Longest provider message kept in a result.
const MAX_MESSAGE_CHARS: usize = 300;
/// The prompt of the test request: about one token in, one token out.
const PROMPT: &str = "ping";
/// Header of every gateway response that carries the log row's request id.
const REQUEST_ID_HEADER: &str = "x-spanbay-request-id";

/// The first answered request of a client since Connect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirstRequest {
    /// `rq_<16 hex>` of the request.
    pub request_id: String,
    /// The log row (the "Open request" link filtered to this client); `None` while the row is
    /// still being written.
    pub log_id: Option<i64>,
    /// The HTTP status the gateway answered with: a refusal shows as "received but rejected"
    /// until a later request succeeds.
    pub status: u16,
}

/// What the test request found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyOutcome {
    /// The gateway answered with a model reply.
    Served,
    /// The gateway is not running (or nothing listens on its port).
    Stopped,
    /// The gateway refused the `Host` it was called with.
    HostNotAllowed,
    /// The client's allow-list does not include the alias the tool uses.
    ModelNotAllowed,
    /// No target of the alias can take a request (circuit open, disabled, no key).
    NoHealthyTarget,
    /// The token in the tool's config is not (or no longer) a client's.
    TokenRejected,
    /// Anything else; `message` carries the gateway's or provider's words.
    Failed,
}

/// The answer of the test request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyResult {
    /// What happened.
    pub outcome: VerifyOutcome,
    /// The alias (or `provider/model`) the request named.
    pub alias: String,
    /// HTTP status; `None` when no answer came (stopped).
    pub status: Option<u16>,
    /// `provider name/model` that served the request (from the stored `probe` row).
    pub target: Option<String>,
    /// Time to first token in ms.
    pub ttft_ms: Option<u64>,
    /// Time to the end of the answer in ms.
    pub latency_ms: u64,
    /// The refusal or failure in words; never a token.
    pub message: Option<String>,
    /// `rq_<16 hex>` of the test request.
    pub request_id: Option<String>,
    /// The test request's log row (source `probe`), once stored.
    pub log_id: Option<i64>,
}

/// The first request seen from `client_id` since it was armed (Connect, create or rotate), or
/// `None` while the tool has not called the gateway yet. Read it when the dialog opens its
/// Verify step and again on each [`LlmEvent::GatewayLogBatch`]; after 60 s without an answer the
/// dialog shows its "Restart <tool>" hint.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_client_first_request_impl(
    state: &AppState,
    client_id: &str,
) -> Result<Option<FirstRequest>, AppError> {
    let Some(seen) = state
        .llm
        .services
        .client_watch
        .first_requests
        .seen(client_id)
    else {
        return Ok(None);
    };
    let log_id = with_connection(state, |connection| {
        store::log_id_for_request_id(connection, &seen.request_id)
    })?;
    Ok(Some(FirstRequest {
        request_id: seen.request_id,
        log_id,
        status: seen.status,
    }))
}

/// The request the tool's own protocol would send: path, extra headers, body.
struct TestRequest {
    path: String,
    headers: Vec<(&'static str, String)>,
    body: Value,
}

fn test_request(app: ClientApp, alias: &str, token: &str) -> TestRequest {
    match app {
        ClientApp::ClaudeCode => TestRequest {
            path: "/v1/messages".to_owned(),
            headers: vec![
                ("authorization", format!("Bearer {token}")),
                ("anthropic-version", "2023-06-01".to_owned()),
            ],
            body: json!({"model": alias, "max_tokens": 1, "stream": true,
                "messages": [{"role": "user", "content": PROMPT}]}),
        },
        ClientApp::Codex => TestRequest {
            path: "/v1/responses".to_owned(),
            headers: vec![("authorization", format!("Bearer {token}"))],
            body: json!({"model": alias, "input": PROMPT, "max_output_tokens": 16, "stream": true}),
        },
        ClientApp::OpenCode => TestRequest {
            path: "/v1/chat/completions".to_owned(),
            headers: vec![("authorization", format!("Bearer {token}"))],
            body: json!({"model": alias, "max_tokens": 1, "stream": true,
                "messages": [{"role": "user", "content": PROMPT}]}),
        },
        ClientApp::GeminiCli => TestRequest {
            path: format!("/v1beta/models/{alias}:streamGenerateContent?alt=sse"),
            headers: vec![("x-goog-api-key", token.to_owned())],
            body: json!({"contents": [{"role": "user", "parts": [{"text": PROMPT}]}],
                "generationConfig": {"maxOutputTokens": 1}}),
        },
    }
}

/// The gateway's own words of an error body in any of the three dialects.
fn error_text(body: &str) -> String {
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let text = value
        .pointer("/error/message")
        .or_else(|| value.pointer("/message"))
        .and_then(Value::as_str)
        .unwrap_or(body);
    redact_secrets(text, &[])
        .chars()
        .take(MAX_MESSAGE_CHARS)
        .collect()
}

/// Names a non-2xx answer of the gateway (the refusals of `gateway::errors`, in any dialect).
fn classify(status: u16, message: &str) -> VerifyOutcome {
    let lower = message.to_ascii_lowercase();
    match status {
        401 => VerifyOutcome::TokenRejected,
        403 if lower.starts_with("host ") && lower.contains("not allowed") => {
            VerifyOutcome::HostNotAllowed
        }
        403 if lower.contains("cannot use model") => VerifyOutcome::ModelNotAllowed,
        503 => VerifyOutcome::NoHealthyTarget,
        400 if lower.contains("no enabled provider key") => VerifyOutcome::NoHealthyTarget,
        _ => VerifyOutcome::Failed,
    }
}

/// The model the tool uses: the one in its config, else the first model the client may use (its
/// first allowed model, or for an allow-all client the first model the gateway lists).
fn alias_to_use(
    state: &AppState,
    configured: Option<String>,
    client: &store::GatewayClient,
) -> Result<String, AppError> {
    if let Some(model) = configured {
        return Ok(model);
    }
    let first = if client.allow_all {
        state
            .llm
            .snapshot_blocking()?
            .listed_models()
            .into_iter()
            .next()
    } else {
        client.allowed_models.first().cloned()
    };
    first.ok_or_else(|| AppError::validation("The client has no model to call: allow one first"))
}

/// Sends one real request to the gateway over loopback with the token the core wrote into the
/// tool's config, using the tool's protocol, and reports what came back. It costs
/// about one token and is logged as `source = probe`.
///
/// Only the four file writers have a stored token; snippet clients get no verify.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown or revoked client; [`AppError::Validation`] when the
/// tool's config holds no token, holds another client's token, or no model can be chosen;
/// [`AppError::Internal`] without OS randomness. Everything the gateway or a provider answers is
/// a [`VerifyResult`], not an error.
pub async fn llm_client_verify_impl(
    state: &AppState,
    client_id: &str,
    app: ClientApp,
) -> Result<VerifyResult, AppError> {
    let home = client_config::home_dir()?;
    llm_client_verify_in(state, client_id, app, &home).await
}

/// [`llm_client_verify_impl`] for an explicit home directory.
pub(crate) async fn llm_client_verify_in(
    state: &AppState,
    client_id: &str,
    app: ClientApp,
    home: &std::path::Path,
) -> Result<VerifyResult, AppError> {
    let id = client_id.to_owned();
    let client =
        with_db_blocking(state, move |connection| store::get_client(connection, &id)).await?;
    if !client.is_active() {
        return Err(AppError::not_found(format!(
            "gateway client {client_id} is revoked"
        )));
    }
    let token = client_config::read_written_token(home, app)?.ok_or_else(|| {
        AppError::validation(format!(
            "{}'s config holds no gateway token: connect it first",
            app.display_name()
        ))
    })?;
    let hashes = with_db_blocking(state, store::client_hashes_snapshot).await?;
    let own = hashes.iter().any(|entry| {
        entry.client.id == client.id && entry.token_sha256 == sha256_hex(token.as_bytes())
    });
    if !own {
        return Err(AppError::validation(format!(
            "The token in {}'s config is not this client's",
            app.display_name()
        )));
    }
    let configured = llm_client_inspect_in(state, app, &DetectEnv::home_only(home))?
        .inspection
        .alias;
    let alias = alias_to_use(state, configured, &client)?;

    let status = state.llm.gateway.status(&state.llm);
    let started = Instant::now();
    let result = |outcome, status: Option<u16>, message: Option<String>| VerifyResult {
        outcome,
        alias: alias.clone(),
        status,
        target: None,
        ttft_ms: None,
        latency_ms: elapsed_ms(started),
        message,
        request_id: None,
        log_id: None,
    };
    if status.state != GatewayState::Running {
        return Ok(result(
            VerifyOutcome::Stopped,
            None,
            Some("The gateway is not running".to_owned()),
        ));
    }
    let request = test_request(app, &alias, &token);
    let secret = state.llm.services.client_watch.verify.mint()?;
    let (rows, subscription) = watch_probe_rows(state);
    let sent = send(status.port, &request, &secret).await;
    let outcome = match sent {
        Err(SendError::Unreachable) => {
            state.llm.services.client_watch.verify.revoke(&secret);
            Ok(result(
                VerifyOutcome::Stopped,
                None,
                Some("Nothing is listening on the gateway's address".to_owned()),
            ))
        }
        Err(SendError::Failed(message)) => {
            state.llm.services.client_watch.verify.revoke(&secret);
            Ok(result(VerifyOutcome::Failed, None, Some(message)))
        }
        Ok(answer) => {
            let mut verdict = result(
                if (200..300).contains(&answer.status) {
                    VerifyOutcome::Served
                } else {
                    classify(answer.status, &answer.message)
                },
                Some(answer.status),
                (!(200..300).contains(&answer.status)).then(|| answer.message.clone()),
            );
            verdict.ttft_ms = answer.ttft_ms;
            verdict.request_id.clone_from(&answer.request_id);
            if let (Some(rows), Some(request_id)) = (rows, &answer.request_id)
                && let Some(row) = wait_for_row(rows, request_id).await
            {
                verdict.target = served_target(&row);
                verdict.ttft_ms = row.ttft_ms.or(verdict.ttft_ms);
                verdict.log_id = Some(row.id);
            }
            if verdict.outcome == VerifyOutcome::Served {
                // The token the file holds passed: the status says "Verified" from now on.
                // A failed write only leaves the status at "Configured".
                let verified_id = client.id.clone();
                let marked = with_db_blocking(state, move |connection| {
                    store::mark_client_verified(connection, &verified_id, current_time_ms())
                })
                .await
                .unwrap_or(false);
                if marked {
                    state.llm.emit(LlmEvent::ClientsChanged);
                }
            }
            Ok(verdict)
        }
    };
    if let Some(id) = subscription {
        llm_unsubscribe_impl(state, id);
    }
    outcome
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// `provider name/model` of the target that served a row.
fn served_target(row: &RequestLogEntry) -> Option<String> {
    let model = row.upstream_model.as_deref()?;
    let provider = row
        .provider_name
        .as_deref()
        .or(row.provider_id.as_deref())?;
    Some(format!("{provider}/{model}"))
}

/// Subscribes to the stored `probe` rows (before the request, so none is missed); `None` when
/// the hub has no free slot (the result then has no target).
fn watch_probe_rows(
    state: &AppState,
) -> (
    Option<tokio::sync::mpsc::UnboundedReceiver<RequestLogEntry>>,
    Option<u64>,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let sink = move |event: Sequenced<LlmEvent>| -> bool {
        if let LlmEvent::GatewayLogBatch { entries } = event.event {
            for entry in entries.into_iter().filter(|entry| entry.source == "probe") {
                if sender.send(entry).is_err() {
                    return false;
                }
            }
        }
        true
    };
    match llm_subscribe_impl(state, Arc::new(sink), None) {
        Ok(id) => (Some(receiver), Some(id)),
        Err(_) => (None, None),
    }
}

async fn wait_for_row(
    mut rows: tokio::sync::mpsc::UnboundedReceiver<RequestLogEntry>,
    request_id: &str,
) -> Option<RequestLogEntry> {
    tokio::time::timeout(ROW_WAIT, async {
        while let Some(row) = rows.recv().await {
            if row.request_id.as_deref() == Some(request_id) {
                return Some(row);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// What came back from the gateway.
struct Answer {
    status: u16,
    /// The error text of a non-2xx answer.
    message: String,
    ttft_ms: Option<u64>,
    request_id: Option<String>,
}

enum SendError {
    /// The connection was refused or timed out: nothing listens.
    Unreachable,
    Failed(String),
}

/// One request over real loopback HTTP; a 2xx stream is read to its end, so the gateway logs the
/// finished request before this returns.
async fn send(port: u16, request: &TestRequest, secret: &str) -> Result<Answer, SendError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| SendError::Failed(format!("cannot build the HTTP client: {error}")))?;
    let mut call = client
        .post(format!("http://127.0.0.1:{port}{}", request.path))
        .header(VERIFY_HEADER, secret)
        .header("content-type", "application/json")
        .body(request.body.to_string());
    for (name, value) in &request.headers {
        call = call.header(*name, value);
    }
    let sent = Instant::now();
    let mut response = call.send().await.map_err(|error| {
        if error.is_connect() {
            SendError::Unreachable
        } else {
            SendError::Failed(redact_secrets(&error.to_string(), &[]))
        }
    })?;
    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if (200..300).contains(&status) {
        let mut ttft_ms = None;
        while let Ok(Some(_chunk)) = response.chunk().await {
            ttft_ms.get_or_insert_with(|| elapsed_ms(sent));
        }
        return Ok(Answer {
            status,
            message: String::new(),
            ttft_ms,
            request_id,
        });
    }
    let body = response.text().await.unwrap_or_default();
    Ok(Answer {
        status,
        message: error_text(&body),
        ttft_ms: None,
        request_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_are_told_apart_in_every_dialect() {
        let host = error_text(
            r#"{"error":{"message":"Host evil.test is not allowed. Add it under Extra allowed hosts in Settings.","type":"permission_error","code":"host_not_allowed"}}"#,
        );
        assert_eq!(classify(403, &host), VerifyOutcome::HostNotAllowed);
        let model = error_text(
            r#"{"type":"error","error":{"type":"permission_error","message":"This token cannot use model fast."}}"#,
        );
        assert_eq!(classify(403, &model), VerifyOutcome::ModelNotAllowed);
        assert_eq!(
            classify(401, "Missing or invalid gateway token."),
            VerifyOutcome::TokenRejected
        );
        assert_eq!(
            classify(
                503,
                "every upstream for this model is unavailable (circuit open)"
            ),
            VerifyOutcome::NoHealthyTarget
        );
        assert_eq!(
            classify(400, "model fast has no enabled provider key to call"),
            VerifyOutcome::NoHealthyTarget
        );
        assert_eq!(classify(500, "boom"), VerifyOutcome::Failed);
        assert_eq!(
            classify(403, "Browser requests are not allowed."),
            VerifyOutcome::Failed
        );
    }

    #[test]
    fn each_tool_sends_its_own_protocol_with_its_own_auth_carrier() {
        let claude = test_request(ClientApp::ClaudeCode, "claude-code", "tok");
        assert_eq!(claude.path, "/v1/messages");
        assert!(
            claude
                .headers
                .iter()
                .any(|(n, v)| *n == "anthropic-version" && v == "2023-06-01")
        );
        assert_eq!(claude.body["model"], "claude-code");
        let codex = test_request(ClientApp::Codex, "x", "tok");
        assert_eq!(codex.path, "/v1/responses");
        assert_eq!(codex.body["input"], PROMPT);
        let opencode = test_request(ClientApp::OpenCode, "x", "tok");
        assert_eq!(opencode.path, "/v1/chat/completions");
        let gemini = test_request(ClientApp::GeminiCli, "my-alias", "tok");
        assert_eq!(
            gemini.path,
            "/v1beta/models/my-alias:streamGenerateContent?alt=sse"
        );
        assert!(
            gemini
                .headers
                .iter()
                .any(|(n, v)| *n == "x-goog-api-key" && v == "tok")
        );
    }

    #[test]
    fn error_text_is_redacted_and_bounded() {
        let long = format!(r#"{{"error":{{"message":"{}"}}}}"#, "x".repeat(1000));
        assert_eq!(error_text(&long).chars().count(), MAX_MESSAGE_CHARS);
        assert_eq!(error_text("plain text"), "plain text");
    }
}
