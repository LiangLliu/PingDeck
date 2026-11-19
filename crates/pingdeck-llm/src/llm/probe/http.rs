//! Raw HTTP helpers of the probes: the paginated model list and credentialed GETs.
//!
//! Single responsibility: GET JSON from a provider (no gateway, no breaker), follow model-list
//! pagination and parse the list shapes the supported providers use.

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::Value;

use super::RequestLog;
use crate::llm::router::Target;
use crate::llm::runtime::{LlmRuntime, now_ms};
use crate::llm::upstream::{AttemptError, map_reqwest, read_json};
use crate::llm::wire::{Protocol, models_url, request_headers};
use pingdeck_base::cancel::{CancelToken, until_cancelled};
use pingdeck_base::error::AppError;

/// Pages followed when listing models.
pub(super) const MAX_LIST_PAGES: usize = 5;
/// Largest error body kept from a rejected list request.
const MAX_ERROR_BYTES: usize = 64 * 1024;

/// Timeout of one model-list request outside a probe run.
const LIST_TIMEOUT: Duration = Duration::from_secs(60);

/// A credentialed GET to the provider that is recorded in the request log of its check.
pub(super) struct Get<'a> {
    pub(super) runtime: &'a LlmRuntime,
    pub(super) target: &'a Target,
    pub(super) cancel: Option<&'a CancelToken>,
    /// `None` outside a probe run (nothing is logged).
    pub(super) log: Option<&'a RequestLog>,
    /// The log row's endpoint label (`base` or `models`).
    pub(super) endpoint: &'static str,
    pub(super) timeout: Duration,
}

impl Get<'_> {
    /// GETs `url` and returns the response whatever its status.
    pub(super) async fn raw(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<reqwest::Response, AttemptError> {
        let started_ms = now_ms();
        let started = Instant::now();
        let outcome = raw_get(
            self.runtime,
            &self.target.proxy_url,
            self.cancel,
            url,
            headers,
            self.timeout,
        )
        .await;
        if let Some(log) = self.log {
            log.record(
                self.target,
                self.endpoint,
                started_ms,
                started,
                outcome
                    .as_ref()
                    .ok()
                    .map(|response| response.status().as_u16()),
                None,
                outcome.as_ref().err(),
                false,
            );
        }
        outcome
    }

    /// GETs a JSON document; a non-2xx status is an [`AttemptError::Status`].
    async fn json(&self, url: &str, headers: &[(String, String)]) -> Result<Value, AttemptError> {
        let response = self.raw(url, headers).await?;
        let status = response.status();
        if !status.is_success() {
            let retry_after_ms = crate::llm::upstream::retry_after_ms(response.headers());
            let body = read_capped(response, self.cancel).await;
            return Err(AttemptError::Status {
                status: status.as_u16(),
                body,
                retry_after_ms,
            });
        }
        read_json(response, self.cancel).await
    }
}

/// Lists the model ids at `models_endpoint`, following the provider's pagination (at most
/// five pages): Anthropic `has_more`/`after_id`, Gemini `nextPageToken`.
///
/// # Errors
/// [`AttemptError`] for a transport failure, a non-2xx status, or a body that is not a model list.
pub async fn fetch_model_ids(
    runtime: &LlmRuntime,
    target: &Target,
    models_endpoint: &str,
    cancel: Option<&CancelToken>,
) -> Result<Vec<String>, AttemptError> {
    let get = Get {
        runtime,
        target,
        cancel,
        log: None,
        endpoint: "models",
        timeout: LIST_TIMEOUT,
    };
    list_model_ids(&get, models_endpoint).await
}

/// [`fetch_model_ids`] over an explicit request context (a probe run logs and times it).
pub(super) async fn list_model_ids(
    get: &Get<'_>,
    models_endpoint: &str,
) -> Result<Vec<String>, AttemptError> {
    let target = get.target;
    let protocol = list_protocol(target);
    let headers = request_headers(
        protocol,
        target.auth_for(protocol),
        &target.secret,
        &target.quirks,
    );
    let base = models_url(&target.base_url, models_endpoint);
    let mut ids: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..MAX_LIST_PAGES {
        let url = match (protocol, cursor.as_deref()) {
            (Protocol::Gemini, None) => with_query(&base, &[("pageSize", "1000")]),
            (Protocol::Gemini, Some(token)) => {
                with_query(&base, &[("pageSize", "1000"), ("pageToken", token)])
            }
            (Protocol::AnthropicMessages, None) => with_query(&base, &[("limit", "1000")]),
            (Protocol::AnthropicMessages, Some(after)) => {
                with_query(&base, &[("limit", "1000"), ("after_id", after)])
            }
            _ => base.clone(),
        };
        let body = get.json(&url, &headers).await?;
        if model_entries(&body).is_none() {
            if page == 0 {
                return Err(AttemptError::Invalid(AppError::network(
                    "the model list has an unrecognised shape",
                )));
            }
            break;
        }
        let page_ids = parse_model_list(&body);
        cursor = match protocol {
            Protocol::Gemini => body["nextPageToken"]
                .as_str()
                .filter(|token| !token.is_empty())
                .map(str::to_owned),
            Protocol::AnthropicMessages if body["has_more"].as_bool() == Some(true) => {
                body["last_id"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| page_ids.last().cloned())
            }
            _ => None,
        };
        ids.extend(page_ids);
        if cursor.is_none() {
            break;
        }
    }
    Ok(ids)
}

/// The model ids of a list response; empty when the shape is not a model list.
///
/// Understands OpenAI and Anthropic `{data:[{id}]}`, Gemini `{models:[{name:"models/x"}]}`,
/// Ollama `{models:[{name|model}]}`, and plain arrays of strings or objects.
pub(crate) fn parse_model_list(body: &Value) -> Vec<String> {
    model_entries(body).map_or_else(Vec::new, |entries| {
        entries.iter().filter_map(model_id).collect()
    })
}

/// The array of a model-list body, or `None` when the body has another shape.
pub(super) fn model_entries(body: &Value) -> Option<&Vec<Value>> {
    match body {
        Value::Array(entries) => Some(entries),
        Value::Object(object) => object
            .get("data")
            .or_else(|| object.get("models"))?
            .as_array(),
        _ => None,
    }
}

fn model_id(entry: &Value) -> Option<String> {
    let raw = match entry {
        Value::String(text) => text.as_str(),
        Value::Object(_) => ["id", "name", "model"]
            .iter()
            .find_map(|key| entry.get(*key).and_then(Value::as_str))?,
        _ => return None,
    };
    let id = raw.strip_prefix("models/").unwrap_or(raw).trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// Protocol whose headers authenticate the model-list request: the target's preferred chat
/// protocol (so a Gemini-only provider gets Gemini headers), OpenAI-style otherwise.
pub(super) fn list_protocol(target: &Target) -> Protocol {
    target.chat_protocol(None).unwrap_or(Protocol::OpenaiChat)
}

fn with_query(url: &str, pairs: &[(&str, &str)]) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.query_pairs_mut().extend_pairs(pairs);
            parsed.into()
        }
        Err(_) => url.to_owned(),
    }
}

/// GETs `url` and returns the response whatever its status.
async fn raw_get(
    runtime: &LlmRuntime,
    proxy_url: &str,
    cancel: Option<&CancelToken>,
    url: &str,
    headers: &[(String, String)],
    timeout: Duration,
) -> Result<reqwest::Response, AttemptError> {
    let client = runtime
        .http_for(url, proxy_url)
        .map_err(AttemptError::Invalid)?;
    let mut request = client.get(url).timeout(timeout);
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let proxied = runtime.proxied_for(url, proxy_url);
    let sending = async {
        request
            .send()
            .await
            .map_err(|error| map_reqwest(error, proxied))
    };
    until_cancelled(cancel, sending)
        .await
        .ok_or(AttemptError::Cancelled)?
}

/// At most [`MAX_ERROR_BYTES`] of a body as lossy UTF-8; transfer errors end the text.
pub(super) async fn read_capped(
    response: reqwest::Response,
    cancel: Option<&CancelToken>,
) -> String {
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let reading = async {
        while let Some(Ok(chunk)) = stream.next().await {
            buffer.extend_from_slice(&chunk);
            if buffer.len() >= MAX_ERROR_BYTES {
                break;
            }
        }
    };
    let _ = until_cancelled(cancel, reading).await;
    buffer.truncate(MAX_ERROR_BYTES);
    String::from_utf8_lossy(&buffer).into_owned()
}
