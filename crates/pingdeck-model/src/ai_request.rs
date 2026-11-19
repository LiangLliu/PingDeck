//! AI API mode of a request: mode and settings data model, provider-target destination rules,
//! mode switching and the data behind `pm.response.ai`.
//!
//! A request is stored as a complete plain HTTP request plus, for [`RequestMode::AiApi`], an
//! [`AiRequestConfig`]. Code that ignores the AI settings (runner, flows, Postman export,
//! scripts) therefore still sees a valid HTTP request. For a provider target the
//! URL field only holds a non-routable label and the core fixes URL, host and credential at
//! run time (D-200); the helpers here are the pure rules every layer shares.
//!
//! Decisions:
//! * `ai` is `Some` exactly when `mode` is `ai_api`. A request in `http` mode that still carries
//!   `ai` is normalised to `None` on save ([`Request::normalize_ai`]); the frontend keeps the AI
//!   settings of an open tab for "Restore", the stored document stays unambiguous.
//! * Only the five API-client protocols are valid; the gateway-only decision protocols are not.
//! * `{{provider key}}` and `{{provider_key}}` are ordinary, undefined variable names: provider
//!   keys are never variables, so nothing here or in the resolver can turn them into a key.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::models::{AuthConfig, HttpMethod, KeyValue, Request, RequestBody};
use crate::validation::WorkspaceError;
use crate::wire::{DEFAULT_ANTHROPIC_VERSION, Protocol};
use pingdeck_base::error::AppError;

/// Most models one compare run may list.
pub const MAX_COMPARE_MODELS: usize = 12;

/// Protocols an AI API request may use, in UI order.
pub const AI_PROTOCOLS: [Protocol; 5] = [
    Protocol::OpenaiChat,
    Protocol::OpenaiResponses,
    Protocol::AnthropicMessages,
    Protocol::Gemini,
    Protocol::Embeddings,
];

/// Text a provider request's credential becomes when it is switched to HTTP mode.
pub const PROVIDER_KEY_PLACEHOLDER: &str = "{{provider key}}";

/// Credential placeholder of an exported (Postman) provider request.
pub const EXPORT_KEY_PLACEHOLDER: &str = "{{api_key}}";

/// Headers a user or script may not set on a provider-target request; they are dropped.
pub const PROTECTED_HEADERS: [&str; 4] = ["host", "authorization", "x-api-key", "x-goog-api-key"];

/// Scheme of the label a provider request's URL field holds; no executor sends it.
const LABEL_SCHEME: &str = "pingdeck-provider";

/// How a request is edited and run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestMode {
    /// Plain HTTP request.
    #[default]
    Http,
    /// AI API request: the request carries [`AiRequestConfig`].
    AiApi,
}

impl RequestMode {
    /// The identifier stored in the `requests.mode` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::AiApi => "ai_api",
        }
    }

    /// Parses [`RequestMode::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "http" => Some(Self::Http),
            "ai_api" => Some(Self::AiApi),
            _ => None,
        }
    }

    /// Whether this is [`RequestMode::Http`].
    #[must_use]
    pub const fn is_http(self) -> bool {
        matches!(self, Self::Http)
    }
}

/// One entry of a compare run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareModel {
    /// Provider that serves the model.
    pub provider_id: String,
    /// Model name.
    pub model: String,
}

/// Where an AI request is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiTarget {
    /// A configured provider: the core adds base URL and credential, never shown or stored.
    Provider {
        /// Provider id.
        provider_id: String,
        /// Provider name, kept so a request synced to another machine can say what it used.
        #[serde(default)]
        provider_name: String,
        /// Base URL of the provider when the request was saved, used only to re-link on import.
        #[serde(default)]
        base_url_hint: String,
        /// Model name.
        model: String,
    },
    /// A URL the user typed in the request's URL field; key and headers live in the request.
    Custom {
        /// Model name.
        model: String,
    },
}

impl AiTarget {
    /// The model the request names.
    #[must_use]
    pub fn model(&self) -> &str {
        match self {
            Self::Provider { model, .. } | Self::Custom { model } => model,
        }
    }
}

/// AI settings of a request in [`RequestMode::AiApi`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRequestConfig {
    /// Wire protocol of the request body.
    pub protocol: Protocol,
    /// Provider or custom URL.
    pub target: AiTarget,
    /// Structured builder state, owned by the frontend; absent when the body was last edited as
    /// Raw JSON and cannot be represented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder: Option<Value>,
    /// Models of "Run on N models…".
    #[serde(default)]
    pub compare_models: Vec<CompareModel>,
}

impl Default for AiRequestConfig {
    /// Chat Completions to a custom URL with no model chosen yet.
    fn default() -> Self {
        Self {
            protocol: Protocol::OpenaiChat,
            target: AiTarget::Custom {
                model: String::new(),
            },
            builder: None,
            compare_models: Vec::new(),
        }
    }
}

fn invalid(reason: impl Into<String>) -> WorkspaceError {
    WorkspaceError::InvalidAiRequest {
        reason: reason.into(),
    }
}

impl AiRequestConfig {
    /// Checks protocol, model and compare list.
    ///
    /// A provider target may still be unchosen (an empty id): an imported request whose provider
    /// does not exist on this machine keeps its settings and asks the user to choose one.
    /// Sending stays blocked until then.
    pub fn validate(&self) -> Result<(), WorkspaceError> {
        if !AI_PROTOCOLS.contains(&self.protocol) {
            return Err(invalid(format!(
                "protocol {} is not available in AI API mode",
                self.protocol.as_str()
            )));
        }
        if self.target.model().trim().is_empty() {
            return Err(invalid("choose a model"));
        }
        if self.compare_models.len() > MAX_COMPARE_MODELS {
            return Err(invalid(format!(
                "at most {MAX_COMPARE_MODELS} models can be compared"
            )));
        }
        let mut seen = HashSet::new();
        for entry in &self.compare_models {
            if entry.provider_id.trim().is_empty() || entry.model.trim().is_empty() {
                return Err(invalid(
                    "a compared model needs a provider and a model name",
                ));
            }
            if !seen.insert((entry.provider_id.as_str(), entry.model.as_str())) {
                return Err(invalid(format!(
                    "model {} is listed twice in the compare list",
                    entry.model
                )));
            }
        }
        Ok(())
    }
}

impl Request {
    /// Whether the request runs as an AI API request with a provider target.
    #[must_use]
    pub fn is_provider_target(&self) -> bool {
        self.mode == RequestMode::AiApi
            && matches!(
                self.ai.as_ref().map(|ai| &ai.target),
                Some(AiTarget::Provider { .. })
            )
    }

    /// Whether the request runs through the AI run service: AI API mode with its settings, either
    /// target.
    #[must_use]
    pub fn runs_as_ai(&self) -> bool {
        self.mode == RequestMode::AiApi && self.ai.is_some()
    }

    /// The method the production guard checks: an AI request always sends a POST.
    #[must_use]
    pub fn guard_method(&self) -> HttpMethod {
        if self.runs_as_ai() {
            HttpMethod::Post
        } else {
            self.method
        }
    }

    /// Drops stale AI settings of an HTTP request so `ai` is `Some` exactly in AI API mode.
    pub fn normalize_ai(&mut self) {
        if self.mode.is_http() {
            self.ai = None;
        }
    }

    /// Checks that an AI API request carries valid AI settings.
    pub fn validate_ai(&self) -> Result<(), WorkspaceError> {
        match (self.mode, &self.ai) {
            (RequestMode::AiApi, None) => Err(invalid("AI API mode needs AI settings")),
            (RequestMode::AiApi, Some(ai)) => ai.validate(),
            (RequestMode::Http, _) => Ok(()),
        }
    }
}

/// Outcome of [`relink_provider`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relink {
    /// The target's provider exists here under this id.
    Linked(String),
    /// No provider matches: the user must choose one ("Choose a provider").
    NeedsChoice,
    /// A custom target has no provider to link.
    NotApplicable,
}

/// Matches an imported provider target to a local provider: by id, then by base URL.
/// `providers` holds `(id, base_url)` pairs. Linking never changes where a request is sent: the
/// destination rules apply to the matched provider like to any other.
#[must_use]
pub fn relink_provider(target: &AiTarget, providers: &[(&str, &str)]) -> Relink {
    let AiTarget::Provider {
        provider_id,
        base_url_hint,
        ..
    } = target
    else {
        return Relink::NotApplicable;
    };
    if let Some((id, _)) = providers.iter().find(|(id, _)| id == provider_id) {
        return Relink::Linked((*id).to_owned());
    }
    let hint = normalize_base_url(base_url_hint);
    if hint.is_empty() {
        return Relink::NeedsChoice;
    }
    providers
        .iter()
        .find(|(_, base)| normalize_base_url(base) == hint)
        .map_or(Relink::NeedsChoice, |(id, _)| {
            Relink::Linked((*id).to_owned())
        })
}

/// Scheme, host, port and path without a trailing slash; blank for blank input.
fn normalize_base_url(text: &str) -> String {
    let text = text.trim();
    match Url::parse(text) {
        Ok(url) if url.host_str().is_some() => format!(
            "{}://{}{}{}",
            url.scheme(),
            url.host_str().unwrap_or_default(),
            url.port()
                .map(|port| format!(":{port}"))
                .unwrap_or_default(),
            url.path().trim_end_matches('/')
        ),
        _ => text.trim_end_matches('/').to_ascii_lowercase(),
    }
}

/// The text a provider request's URL field holds instead of the provider's address: a label in
/// the non-routable `pingdeck-provider` scheme made of the provider name (reduced to
/// `[a-z0-9-]`, so it is never host-shaped) and the endpoint `path`.
#[must_use]
pub fn non_routable_label(provider_name: &str, path: &str) -> String {
    let mut slug = String::new();
    for ch in provider_name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "provider" } else { slug };
    let path = path.trim();
    let slash = if path.starts_with('/') { "" } else { "/" };
    format!("{LABEL_SCHEME}://{slug}{slash}{path}")
}

/// Whether `url` is a label made by [`non_routable_label`].
#[must_use]
pub fn is_non_routable_label(url: &str) -> bool {
    url.trim_start()
        .strip_prefix(LABEL_SCHEME)
        .is_some_and(|rest| rest.starts_with("://"))
}

/// Splits `headers` into those that may be sent and the names of the enabled protected ones
/// ([`PROTECTED_HEADERS`], any case) that were dropped. Names are listed once, as written.
#[must_use]
pub fn strip_user_credentials(headers: &[KeyValue]) -> (Vec<KeyValue>, Vec<String>) {
    let mut kept = Vec::with_capacity(headers.len());
    let mut dropped: Vec<String> = Vec::new();
    for header in headers {
        let name = header.key.trim();
        let protected = PROTECTED_HEADERS
            .iter()
            .any(|protected| protected.eq_ignore_ascii_case(name));
        if !protected || !header.enabled {
            kept.push(header.clone());
        } else if !dropped.iter().any(|seen| seen.eq_ignore_ascii_case(name)) {
            dropped.push(name.to_owned());
        }
    }
    (kept, dropped)
}

/// What a pre-request or test script may change of the request it runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptRequestPolicy {
    /// URL, query, path variables and auth are fixed by the core (provider targets).
    pub lock_destination: bool,
}

impl ScriptRequestPolicy {
    /// The policy of `request`: provider targets are locked, everything else is the user's.
    #[must_use]
    pub fn for_request(request: &Request) -> Self {
        Self {
            lock_destination: request.is_provider_target(),
        }
    }

    /// Reverts what the policy forbids in `scripted` (the request after the scripts ran) using
    /// `original` and returns the names of the protected headers it dropped. A policy that does
    /// not lock the destination changes nothing.
    pub fn enforce(self, original: &Request, scripted: &mut Request) -> Vec<String> {
        if !self.lock_destination {
            return Vec::new();
        }
        scripted.url.clone_from(&original.url);
        scripted.query_params.clone_from(&original.query_params);
        scripted.path_variables.clone_from(&original.path_variables);
        scripted.auth.clone_from(&original.auth);
        let (kept, dropped) = strip_user_credentials(&scripted.headers);
        scripted.headers = kept;
        dropped
    }
}

/// The credential header of `protocol` carrying `secret` (a placeholder text here).
fn credential_header(protocol: Protocol, secret: &str) -> KeyValue {
    let (key, value) = match protocol {
        Protocol::AnthropicMessages => ("x-api-key", secret.to_owned()),
        Protocol::Gemini => ("x-goog-api-key", secret.to_owned()),
        _ => ("Authorization", format!("Bearer {secret}")),
    };
    KeyValue {
        key: key.to_owned(),
        value,
        enabled: true,
        description: None,
    }
}

/// The plain HTTP headers of a provider request: the user's headers without protected ones,
/// then the credential header carrying `secret`, `anthropic-version` and a JSON content type
/// unless already present.
pub fn plain_http_headers(headers: &[KeyValue], protocol: Protocol, secret: &str) -> Vec<KeyValue> {
    let mut out = strip_user_credentials(headers).0;
    out.push(credential_header(protocol, secret));
    let has = |out: &[KeyValue], name: &str| {
        out.iter()
            .any(|header| header.enabled && header.key.trim().eq_ignore_ascii_case(name))
    };
    let mut extra = |name: &str, value: &str| {
        if !has(&out, name) {
            out.push(KeyValue {
                key: name.to_owned(),
                value: value.to_owned(),
                enabled: true,
                description: None,
            });
        }
    };
    if protocol == Protocol::AnthropicMessages {
        extra("anthropic-version", DEFAULT_ANTHROPIC_VERSION);
    }
    extra("Content-Type", "application/json");
    out
}

/// Switches `request` to `to` without touching URL, headers, auth or body.
///
/// To AI API mode the request keeps its HTTP fields as they are and uses its `ai` settings, or
/// [`AiRequestConfig::default`] when it has none; applying the protocol preset to empty fields
/// is left to the frontend. To HTTP mode a provider request becomes plain HTTP
/// ([`to_http_mode`]) and `ai` is cleared.
#[must_use]
pub fn switch_mode(mut request: Request, to: RequestMode, resolved_url: Option<&str>) -> Request {
    match (request.mode, to) {
        (RequestMode::Http, RequestMode::Http) | (RequestMode::AiApi, RequestMode::AiApi) => {}
        (RequestMode::Http, RequestMode::AiApi) => {
            request.mode = RequestMode::AiApi;
            request.ai.get_or_insert_with(AiRequestConfig::default);
        }
        (RequestMode::AiApi, RequestMode::Http) => request = to_http_mode(request, resolved_url),
    }
    request
}

/// Turns an AI API request into the plain HTTP request it stands for.
///
/// A provider request gets `resolved_url` (the provider's address, known to the core only; the
/// label stays when `None`) and its injected credential becomes the text
/// [`PROVIDER_KEY_PLACEHOLDER`] in the protocol's credential header, which [`ensure_sendable`]
/// refuses until the user replaces it with real authentication. Protected headers the user set
/// are dropped. A custom-target request already is a complete HTTP request.
#[must_use]
pub fn to_http_mode(mut request: Request, resolved_url: Option<&str>) -> Request {
    if let Some(ai) = request.ai.take()
        && let AiTarget::Provider { .. } = ai.target
    {
        if let Some(url) = resolved_url {
            url.clone_into(&mut request.url);
        }
        request.headers =
            plain_http_headers(&request.headers, ai.protocol, PROVIDER_KEY_PLACEHOLDER);
    }
    request.mode = RequestMode::Http;
    request
}

/// Refuses a request that still holds a provider-key placeholder: nothing may be sent until the
/// user adds authentication.
pub fn ensure_sendable(request: &Request) -> Result<(), AppError> {
    let holds_placeholder =
        |text: &str| text.contains(PROVIDER_KEY_PLACEHOLDER) || text.contains("{{provider_key}}");
    let in_pairs = |pairs: &[KeyValue]| {
        pairs
            .iter()
            .any(|pair| holds_placeholder(&pair.key) || holds_placeholder(&pair.value))
    };
    let structured = serde_json::to_string(&(&request.auth, &request.body)).unwrap_or_default();
    if holds_placeholder(&request.url)
        || in_pairs(&request.headers)
        || in_pairs(&request.query_params)
        || holds_placeholder(&structured)
    {
        return Err(AppError::validation(format!(
            "Add authentication before sending: the request still contains {PROVIDER_KEY_PLACEHOLDER} \
             in place of the provider key"
        )));
    }
    Ok(())
}

/// Start of an attachment placeholder in a Raw JSON body (`{{attachment:<sha256>}}`).
const ATTACHMENT_PLACEHOLDER: &str = "{{attachment:";

/// An AI request as a plain HTTP request for export.
#[derive(Debug, Clone, PartialEq)]
pub struct PlainExport {
    /// The plain HTTP request to write.
    pub request: Request,
    /// Attachment placeholders left in the body: the blobs are local and not exported.
    pub attachments: usize,
    /// A provider target whose address was not given, so the URL is still the label.
    pub unresolved_provider: bool,
}

/// The plain HTTP request an exporter writes for `request`, or `None` for an HTTP request.
///
/// A provider target exports `resolved_url` (the provider's base URL and endpoint path, which
/// only the caller can look up) with generated headers whose credential is the
/// [`EXPORT_KEY_PLACEHOLDER`], never a key, and no auth block. A custom target already is a
/// complete HTTP request and is exported as it is. The body is the Raw JSON, attachments
/// included as unresolved placeholders.
#[must_use]
pub fn plain_http_for_export(request: &Request, resolved_url: Option<&str>) -> Option<PlainExport> {
    let ai = request
        .ai
        .as_ref()
        .filter(|_| request.mode == RequestMode::AiApi)?;
    let mut plain = request.clone();
    plain.mode = RequestMode::Http;
    plain.ai = None;
    let is_provider = matches!(ai.target, AiTarget::Provider { .. });
    if is_provider {
        if let Some(url) = resolved_url {
            url.clone_into(&mut plain.url);
        }
        plain.headers = plain_http_headers(&request.headers, ai.protocol, EXPORT_KEY_PLACEHOLDER);
        plain.auth = AuthConfig::None;
    }
    let body = match &request.body {
        RequestBody::Json { content } | RequestBody::RawText { content, .. } => content,
        _ => "",
    };
    Some(PlainExport {
        attachments: body.matches(ATTACHMENT_PLACEHOLDER).count(),
        unresolved_provider: is_provider && resolved_url.is_none(),
        request: plain,
    })
}

/// A tool call of the finished AI reply, as `pm.response.ai.toolCalls` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiScriptToolCall {
    /// Call id.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Arguments as the model wrote them (JSON text).
    pub arguments: String,
}

/// Token counts of `pm.response.ai.usage`; `None` is `null` in the script.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiScriptUsage {
    /// Input tokens.
    pub input_tokens: Option<u64>,
    /// Output tokens.
    pub output_tokens: Option<u64>,
    /// Cached input tokens.
    pub cached_tokens: Option<u64>,
    /// Reasoning tokens.
    pub reasoning_tokens: Option<u64>,
}

/// The plain data behind the read-only `pm.response.ai` object. It holds no
/// provider id, key or base URL; the run pipeline fills it from the finished run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiScriptResult {
    /// Merged reply text.
    pub reply: String,
    /// Merged reasoning text.
    pub reasoning: String,
    /// Tool calls the model made.
    pub tool_calls: Vec<AiScriptToolCall>,
    /// Finish reason, when the stream reported one.
    pub finish_reason: Option<String>,
    /// Model that answered.
    pub model: Option<String>,
    /// Token counts.
    pub usage: AiScriptUsage,
    /// Milliseconds from Send to the first token.
    pub ttft_ms: Option<u64>,
    /// Milliseconds from Send to the end of the response.
    pub total_ms: u64,
    /// Output tokens per second; `None` below 300 tokens.
    pub tokens_per_second: Option<f64>,
    /// Number of stream events.
    pub event_count: u64,
}

#[cfg(test)]
mod tests;
