//! The request data a stored capture holds, as curl, replay, "Open in API client" and the
//! diagnostics bundle read it: the request bodies, the gateway route of a protocol, the bare
//! upstream URL and the shared warning texts.

use std::borrow::Cow;

use serde_json::Value;

use super::model::{CapturedBody, CapturedExchange, RowFacts};
use super::redact::userinfo_at;
use crate::llm::wire::{Protocol, rewrite_model};
use pingdeck_base::secrets::redact_url_path;

/// Why a row has no request body to copy or replay.
pub(super) const NO_BODY: &str = "No request body was stored";
/// The warning for a body cut at the cap.
pub(super) const CUT_WARNING: &str = "Body was cut at 256 KiB";

/// A request body found in a capture; borrowed from it unless the model was rewritten.
pub(super) struct StoredBody<'a> {
    pub text: Cow<'a, str>,
    pub truncated: bool,
    pub elided: bool,
}

fn stored(body: &CapturedBody) -> Option<StoredBody<'_>> {
    Some(StoredBody {
        text: Cow::Borrowed(body.text.as_deref()?),
        truncated: body.truncated,
        elided: body.elided,
    })
}

/// The client request body as stored.
pub(super) fn client_body(capture: &CapturedExchange) -> Option<StoredBody<'_>> {
    capture.client_req_body.as_ref().and_then(stored)
}

/// The body sent upstream. A capture without its own upstream body means "equal to the client's
/// except `model`", so the client body is returned with the upstream model put in.
pub(super) fn upstream_body<'a>(
    capture: &'a CapturedExchange,
    facts: &RowFacts,
) -> Option<StoredBody<'a>> {
    if let Some(body) = capture.upstream_req_body.as_ref().and_then(stored) {
        return Some(body);
    }
    if capture.upstream_url.is_empty() {
        return None;
    }
    let mut body = client_body(capture)?;
    let protocol = facts.protocol_out.unwrap_or(capture.client_protocol);
    if let (false, Some(model), Ok(mut json)) = (
        body.truncated,
        facts.upstream_model.as_deref(),
        serde_json::from_str::<Value>(&body.text),
    ) {
        rewrite_model(protocol, &mut json, model);
        body.text = Cow::Owned(json.to_string());
    }
    Some(body)
}

/// The path (and query) a client uses on the gateway for `protocol`. An explicit table of the
/// gateway's routes: `Protocol::suffix` names the upstream provider paths, which only happen to
/// look alike.
pub(super) fn gateway_path(protocol: Protocol, model: &str, stream: bool) -> String {
    match protocol {
        Protocol::OpenaiChat => "/v1/chat/completions".to_owned(),
        Protocol::OpenaiResponses => "/v1/responses".to_owned(),
        Protocol::AnthropicMessages => "/v1/messages".to_owned(),
        Protocol::Embeddings => "/v1/embeddings".to_owned(),
        Protocol::DecisionSystemone => "/v1/systemone".to_owned(),
        Protocol::DecisionOpenai => "/v1/decisions".to_owned(),
        Protocol::Gemini if stream => {
            format!("/v1beta/models/{model}:streamGenerateContent?alt=sse")
        }
        Protocol::Gemini => format!("/v1beta/models/{model}:generateContent"),
    }
}

/// `path` on the gateway at `base` (a trailing `/` of `base` is dropped).
pub(super) fn gateway_url(base: &str, path: &str) -> String {
    format!("{}{path}", base.trim_end_matches('/'))
}

/// "The body contains N redacted value(s)", the start of every note about redacted body text.
pub(super) fn redacted_values_phrase(count: usize) -> String {
    format!(
        "The body contains {count} redacted value{}",
        if count == 1 { "" } else { "s" }
    )
}

/// The URL without fragment, query and user info; opaque path segments (R10) are elided.
pub(super) fn bare_url(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    let Some((scheme, rest)) = url.split_once("://") else {
        return redact_url_path(url);
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = userinfo_at(rest).map_or(authority, |at| &authority[at + 1..]);
    format!("{scheme}://{host}{}", redact_url_path(path))
}
