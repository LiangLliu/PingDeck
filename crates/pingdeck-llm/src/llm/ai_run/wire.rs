//! What goes on the wire: the URL, the final header list and its masked view.
//!
//! Single responsibility: for a provider target the core fixes the URL (the provider's endpoint
//! for the call's protocol, never translated) and the credential (the injected key, auth style
//! and extra headers of the provider); the user's own headers follow, except where they would
//! collide with an injected one. A custom target sends exactly what the user wrote. Nothing here
//! touches the network.

use std::collections::HashSet;

use super::types::{AiHeader, AiRunSpec, AiRunStatus, Destination};
use crate::llm::router::Target;
use crate::llm::wire::{Protocol, request_headers};
use pingdeck_model::models::mask_sensitive_header;

/// A refusal before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Refusal {
    pub status: AiRunStatus,
    pub message: String,
}

impl Refusal {
    fn unsupported(message: impl Into<String>) -> Self {
        Self {
            status: AiRunStatus::Unsupported,
            message: message.into(),
        }
    }
}

/// The planned request.
pub(super) struct Planned {
    /// The URL that is requested (the provider's real address for a provider target).
    pub url: String,
    /// Headers in send order.
    pub headers: Vec<(String, String)>,
    /// Lower-case names of headers whose values are credentials of the provider.
    pub injected: HashSet<String>,
    /// Secret texts to mask wherever the sent request is shown.
    pub secrets: Vec<String>,
    /// The provider's own credential texts: they never appear in a result, even when the
    /// upstream's reply echoes them.
    pub hidden: Vec<String>,
}

/// The name the UI uses for a protocol in messages (`DeepSeek has no Responses endpoint`).
#[must_use]
pub fn protocol_label(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiChat => "Chat Completions",
        Protocol::OpenaiResponses => "Responses",
        Protocol::AnthropicMessages => "Messages",
        Protocol::Gemini => "Gemini",
        Protocol::Embeddings => "Embeddings",
        Protocol::DecisionSystemone | Protocol::DecisionOpenai => "Decision",
    }
}

/// The message of a provider that lacks the protocol's endpoint.
#[must_use]
pub fn missing_endpoint(provider: &str, protocol: Protocol) -> String {
    format!("{provider} has no {} endpoint", protocol_label(protocol))
}

/// Plans the request of `spec`.
///
/// # Errors
/// A [`Refusal`] when a provider lacks the protocol's endpoint or a header cannot be sent.
pub(super) fn plan(spec: &AiRunSpec) -> Result<Planned, Refusal> {
    let planned = match &spec.destination {
        Destination::Provider(target) => plan_provider(spec, target)?,
        Destination::Custom { url, .. } => Planned {
            url: url.clone(),
            headers: with_defaults(spec.headers.clone(), spec.stream),
            injected: HashSet::new(),
            hidden: Vec::new(),
            secrets: spec.secrets.clone(),
        },
    };
    for (name, value) in &planned.headers {
        if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
            return Err(Refusal::unsupported(format!(
                "`{name}` is not a valid header name"
            )));
        }
        if reqwest::header::HeaderValue::from_str(value).is_err() {
            return Err(Refusal::unsupported(format!(
                "The value of header `{name}` cannot be sent"
            )));
        }
    }
    Ok(planned)
}

fn plan_provider(spec: &AiRunSpec, target: &Target) -> Result<Planned, Refusal> {
    if target.endpoint(spec.protocol).is_none() {
        return Err(Refusal::unsupported(missing_endpoint(
            &target.provider_name,
            spec.protocol,
        )));
    }
    let url = target
        .url(spec.protocol, spec.stream)
        .map_err(|error| Refusal::unsupported(error.message().to_owned()))?;
    let injected = request_headers(
        spec.protocol,
        target.auth_for(spec.protocol),
        &target.secret,
        &target.quirks,
    );
    let names: HashSet<String> = injected
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    let credentials = injected
        .iter()
        .filter(|(name, _)| name != "anthropic-version")
        .map(|(name, _)| name.clone())
        .collect();
    let mut secrets = spec.secrets.clone();
    secrets.push(target.secret.trim().to_owned());
    secrets.extend(target.quirks.extra_headers.values().cloned());
    secrets.retain(|secret| !secret.is_empty());
    let mut headers = injected;
    // The injected headers win; the user keeps every other header.
    headers.extend(
        spec.headers
            .iter()
            .filter(|(name, _)| !names.contains(&name.to_ascii_lowercase()))
            .cloned(),
    );
    Ok(Planned {
        url,
        headers: with_defaults(headers, spec.stream),
        injected: credentials,
        hidden: secrets
            .iter()
            .skip(spec.secrets.len())
            .filter(|text| text.chars().count() >= pingdeck_base::secrets::MIN_REDACTION_CHARS)
            .cloned()
            .collect(),
        secrets,
    })
}

/// Adds `content-type`, `accept` and (so every send opens its own connection)
/// `connection: close` unless the headers already have them.
fn with_defaults(mut headers: Vec<(String, String)>, stream: bool) -> Vec<(String, String)> {
    let mut add = |name: &str, value: &str| {
        if !headers
            .iter()
            .any(|(own, _)| own.eq_ignore_ascii_case(name))
        {
            headers.push((name.to_owned(), value.to_owned()));
        }
    };
    add("content-type", "application/json");
    add(
        "accept",
        if stream {
            "text/event-stream"
        } else {
            "application/json"
        },
    );
    add("connection", "close");
    headers
}

/// The headers as the Raw tab shows them: credentials and injected provider headers masked, the
/// user's secret variable values masked everywhere.
pub(super) fn masked_view(planned: &Planned) -> Vec<AiHeader> {
    planned
        .headers
        .iter()
        .map(|(name, value)| {
            let lower = name.to_ascii_lowercase();
            let mut shown = mask_sensitive_header(name, value);
            if shown == *value && planned.injected.contains(&lower) {
                "[redacted]".clone_into(&mut shown);
            }
            AiHeader {
                name: name.clone(),
                value: pingdeck_base::secrets::mask_secrets(&shown, &planned.secrets),
            }
        })
        .collect()
}
