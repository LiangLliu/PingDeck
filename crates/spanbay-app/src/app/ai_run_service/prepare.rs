//! Loading an AI request and turning it into the call the core runs.
//!
//! Single responsibility: variable resolution of everything a call sends (builder text, Raw
//! JSON, the model, a custom URL, headers, the custom target's key) exactly as in HTTP mode, and
//! the choice of destination: a provider target gets the core's URL and key (never a variable),
//! a custom target the user's own URL through the guarded transport.

use std::time::Duration;

use serde_json::Value;

use crate::app::dto::SaveRequestInput;
use crate::app::execution_service::{ExecutionContext, execution_context};
use crate::app::{AppState, command_error, request_from_input};
use crate::error::AppError;
use crate::executor::{ExecutionError, ExecutorLimits, auth_header_pairs, request_url};
use crate::llm::ai_run::{AiRunSpec, Destination, missing_endpoint};
use crate::llm::safe_http::client::ReqwestSafeHttp;
use crate::llm::store::resolve_price;
use crate::llm::wire::{Protocol, rewrite_model};
use crate::workspace::ai_request::{AiTarget, CompareModel, RequestMode};
use crate::workspace::variables::{VariableResolutionError, resolve_template};
use crate::workspace::{Request, RequestBody, RequestId, VariableScopes, WorkspaceId};
use spanbay_base::secrets::mask_secrets;

/// Timeout of a request that has none (the 120 s of the chat presets).
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// A request and its execution context, before any script ran.
pub(super) struct Loaded {
    pub request: Request,
    pub context: ExecutionContext,
}

/// Loads the saved request or the draft and the context it runs in.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown request or environment, [`AppError::Validation`] when
/// neither or both of `request_id` and `draft` are given, or the request is not in AI API mode.
pub(super) fn load(
    state: &AppState,
    workspace_id: &str,
    request_id: Option<&str>,
    draft: Option<SaveRequestInput>,
    environment_id: Option<&str>,
) -> Result<Loaded, AppError> {
    let mut request = match (request_id, draft) {
        (Some(id), None) => {
            let workspace = WorkspaceId::new(workspace_id).map_err(command_error)?;
            let request_id = RequestId::new(id).map_err(command_error)?;
            state
                .store()
                .get_request(&workspace, &request_id)
                .map_err(command_error)?
                .ok_or_else(|| AppError::not_found(format!("request {id} not found")))?
        }
        (None, Some(draft)) => request_from_input(draft)?,
        _ => {
            return Err(AppError::validation(
                "give either a saved request id or a draft request",
            ));
        }
    };
    if request.workspace_id.as_str() != workspace_id {
        return Err(AppError::validation(
            "the request belongs to another workspace",
        ));
    }
    state
        .settings()
        .network
        .apply_inherited(&mut request.network_options);
    if request.mode != RequestMode::AiApi || request.ai.is_none() {
        return Err(AppError::validation(
            "this request is not in AI API mode; send it with the HTTP commands",
        ));
    }
    let context = {
        let store = state.store();
        execution_context(&store, &request, environment_id)?
    };
    Ok(Loaded { request, context })
}

/// Resolves `{{variables}}`; an unresolved one is the same error as in HTTP mode.
fn resolve(text: &str, scopes: &VariableScopes) -> Result<String, AppError> {
    resolve_template(text, scopes).map_err(|error| {
        let VariableResolutionError::UnresolvedVariables { keys } = error;
        AppError::from(ExecutionError::UnresolvedVariable { keys })
    })
}

/// Resolves a body but leaves `{{attachment:<sha256 prefix>}}` placeholders alone: they are not
/// variables, the core replaces them with the attachment's data when it sends.
fn resolve_body(text: &str, scopes: &VariableScopes) -> Result<String, AppError> {
    const OPEN: &str = "{{attachment:";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let Some(len) = rest[start..].find("}}") else {
            break;
        };
        out.push_str(&resolve(&rest[..start], scopes)?);
        out.push_str(&rest[start..start + len + 2]);
        rest = &rest[start + len + 2..];
    }
    out.push_str(&resolve(rest, scopes)?);
    Ok(out)
}

/// Whether the call asks for a stream: `stream: true` in the body, or for Gemini the streaming
/// action in the URL (`stream` is the URL action there, not a body field).
fn wants_stream(protocol: Protocol, body: &str, url: &str) -> bool {
    match protocol {
        Protocol::Embeddings => false,
        Protocol::Gemini => url.contains("streamGenerateContent"),
        _ => serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|json| json.get("stream").and_then(Value::as_bool))
            .unwrap_or(false),
    }
}

/// Everything the call sends, resolved.
#[derive(Clone)]
pub(super) struct Call {
    pub protocol: Protocol,
    pub model: String,
    /// The provider of a provider target (`None`: the custom URL).
    pub provider_id: Option<String>,
    pub body: String,
    pub headers: Vec<(String, String)>,
    pub dropped: Vec<String>,
    /// The resolved URL of a custom target.
    pub custom_url: Option<String>,
    pub proxy: String,
    /// What the Raw tab shows as URL.
    pub shown_url: String,
    pub timeout: Duration,
    pub stream: bool,
    pub secrets: Vec<String>,
}

/// Resolves `request` (after its scripts ran; `request.auth` is the effective auth) against the
/// scopes of `context`.
///
/// # Errors
/// [`AppError::Validation`] for an unresolved variable, a body that is not JSON text, or a
/// custom URL that is not a URL.
pub(super) fn resolve_call(
    request: &Request,
    context: &ExecutionContext,
    dropped: Vec<String>,
) -> Result<Call, AppError> {
    let scopes = context.scopes();
    let ai = request
        .ai
        .as_ref()
        .ok_or_else(|| AppError::validation("AI API mode needs AI settings"))?;
    let model = resolve(ai.target.model(), &scopes)?;
    if model.trim().is_empty() {
        return Err(AppError::validation("Choose a model"));
    }
    let content = match &request.body {
        RequestBody::Json { content } | RequestBody::RawText { content, .. }
            if !content.trim().is_empty() =>
        {
            content
        }
        _ => {
            return Err(AppError::validation(
                "An AI API request sends a JSON body: add one in Raw JSON or the builder",
            ));
        }
    };
    let body = resolve_body(content, &scopes)?;
    let mut headers = Vec::new();
    for header in request.headers.iter().filter(|header| header.enabled) {
        let name = resolve(&header.key, &scopes)?;
        if !name.trim().is_empty() {
            headers.push((name, resolve(&header.value, &scopes)?));
        }
    }
    let secrets = scopes.secret_values();
    let timeout_ms = match request.network_options.timeout_ms {
        0 => DEFAULT_TIMEOUT_MS,
        ms => ms,
    };
    let (provider_id, custom_url, proxy, shown_url) = match &ai.target {
        AiTarget::Provider { provider_id, .. } => (
            Some(provider_id.clone()),
            None,
            String::new(),
            request.url.clone(),
        ),
        AiTarget::Custom { .. } => {
            let url = request_url(request, &scopes).map_err(AppError::from)?;
            let names: Vec<String> = headers.iter().map(|(name, _)| name.clone()).collect();
            headers
                .extend(auth_header_pairs(&request.auth, &scopes, &names).map_err(AppError::from)?);
            (
                None,
                Some(url.to_string()),
                // An inherited proxy follows the Settings rule (no_proxy, local hosts).
                if request
                    .network_options
                    .inherit
                    .contains(&spanbay_model::models::NetworkField::ProxyUrl)
                {
                    String::new()
                } else {
                    request
                        .network_options
                        .proxy_url
                        .clone()
                        .unwrap_or_default()
                },
                mask_secrets(url.as_str(), &secrets),
            )
        }
    };
    let stream = wants_stream(
        ai.protocol,
        &body,
        custom_url.as_deref().unwrap_or(&shown_url),
    );
    Ok(Call {
        protocol: ai.protocol,
        model,
        provider_id,
        body,
        headers,
        dropped,
        custom_url,
        proxy,
        shown_url,
        timeout: Duration::from_millis(timeout_ms),
        stream,
        secrets,
    })
}

impl Call {
    /// The call of one compare row: the same body with only `model` replaced (Gemini carries the
    /// model in the URL the core derives). A custom target keeps its URL.
    pub(super) fn for_model(&self, entry: &CompareModel) -> Result<Self, AppError> {
        let mut call = self.clone();
        call.model.clone_from(&entry.model);
        if self.provider_id.is_some() {
            call.provider_id = Some(entry.provider_id.clone());
        }
        if self.protocol != Protocol::Gemini {
            let mut json: Value = serde_json::from_str(&self.body).map_err(|_| {
                AppError::validation("the body must be a JSON object to run it on several models")
            })?;
            rewrite_model(self.protocol, &mut json, &entry.model);
            call.body = json.to_string();
        }
        Ok(call)
    }
}

/// The spec of the core call for `call`: the provider's target and effective price, or the custom
/// URL with its guarded transport.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown or disabled provider, [`AppError::Validation`] for a
/// provider without an enabled key, [`AppError::Unsupported`] naming the missing endpoint
/// (`DeepSeek has no Responses endpoint`).
pub(super) async fn make_spec(
    state: &AppState,
    call: &Call,
    run_id: &str,
    keep_exchange: bool,
) -> Result<AiRunSpec, AppError> {
    let (destination, price) = match (&call.provider_id, &call.custom_url) {
        (Some(provider_id), _) => {
            let snapshot = state.llm.snapshot().await?;
            let name = snapshot
                .providers
                .get(provider_id)
                .map(|provider| provider.name.clone())
                .ok_or_else(|| AppError::not_found(format!("provider {provider_id} not found")))?;
            let target = snapshot
                .target_for(provider_id, None, &call.model, Some(call.protocol))
                .map_err(|error| match error {
                    AppError::Unsupported { .. } => {
                        AppError::unsupported(missing_endpoint(&name, call.protocol))
                    }
                    other => other,
                })?;
            let price = {
                let store = state.store();
                resolve_price(store.connection(), provider_id, &call.model)?
            };
            (Destination::Provider(Box::new(target)), price)
        }
        (None, Some(url)) => {
            let origin = url::Url::parse(url)
                .map_err(|error| AppError::validation(format!("invalid request URL: {error}")))?
                .origin()
                .ascii_serialization();
            let http =
                ReqwestSafeHttp::for_network(&state.settings().network, &origin, &call.proxy)?;
            (
                Destination::Custom {
                    url: url.clone(),
                    http,
                },
                crate::llm::usage::PriceResolution::default(),
            )
        }
        (None, None) => return Err(AppError::validation("Enter a URL")),
    };
    Ok(AiRunSpec {
        run_id: run_id.to_owned(),
        protocol: call.protocol,
        model: call.model.clone(),
        stream: call.stream,
        destination,
        headers: call.headers.clone(),
        dropped_headers: call.dropped.clone(),
        body: call.body.clone(),
        shown_url: call.shown_url.clone(),
        timeout: call.timeout,
        keep_exchange,
        price,
        secrets: call.secrets.clone(),
        max_response_bytes: ExecutorLimits::from(&state.settings().data).max_body_bytes,
        attachments_dir: state.data_dirs().map(|dirs| dirs.attachments.clone()),
    })
}
