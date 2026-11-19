//! Replay eligibility and "Open in API client": pure decisions over a stored capture (whether it
//! can be re-sent, and what an API-client tab opened from it holds). Sending and the tab itself
//! live elsewhere.

use serde::{Deserialize, Serialize};

use super::bound::ELISION_PREFIX;
use super::model::{CapturedExchange, RowFacts};
use super::redact::count_markers;
use super::stored::{
    CUT_WARNING, NO_BODY, StoredBody, bare_url, client_body, gateway_path, gateway_url,
    redacted_values_phrase, upstream_body,
};
use crate::llm::wire::Protocol;

/// Why a replay is not offered. With `needs_confirmation` the user may still replay after a
/// confirm dialog (redacted values).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayBlock {
    /// The reason shown as tooltip and text.
    pub reason: String,
    /// Replay is allowed once the user confirms (only for redacted values).
    pub needs_confirmation: bool,
}

impl ReplayBlock {
    fn hard(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            needs_confirmation: false,
        }
    }
}

/// Counts the elision markers and whether every one sits in image context.
fn elided_media(text: &str) -> (usize, bool) {
    let mut count = 0;
    let mut all_images = true;
    for (at, _) in text.match_indices(ELISION_PREFIX) {
        count += 1;
        let context = &text.as_bytes()[at.saturating_sub(80)..at];
        all_images &= String::from_utf8_lossy(context)
            .to_ascii_lowercase()
            .contains("image");
    }
    (count, all_images)
}

fn elided_reason(body: &StoredBody<'_>) -> String {
    let (count, all_images) = elided_media(&body.text);
    let count = count.max(1);
    match (count, all_images) {
        (1, true) => "This request had 1 image that was not stored".to_owned(),
        (n, true) => format!("This request had {n} images that were not stored"),
        (1, false) => "This request had 1 media item that was not stored".to_owned(),
        (n, false) => format!("This request had {n} media items that were not stored"),
    }
}

/// Whether the stored client request can be re-sent through the gateway.
///
/// Blocked, in this order, when: there is no capture or no body; the body was cut at 256 KiB
/// (never replayable); media was elided; the body holds redacted values (replay would send the
/// redacted text) unless `confirm_redacted`.
///
/// # Errors
/// A [`ReplayBlock`] naming the first condition that applies.
pub fn replay_eligibility(
    capture: Option<&CapturedExchange>,
    confirm_redacted: bool,
) -> Result<(), ReplayBlock> {
    let body = capture
        .and_then(client_body)
        .ok_or_else(|| ReplayBlock::hard(NO_BODY))?;
    if body.truncated {
        return Err(ReplayBlock::hard(
            "The stored body was cut at 256 KiB, so the request cannot be reproduced",
        ));
    }
    if body.elided {
        return Err(ReplayBlock::hard(elided_reason(&body)));
    }
    let redacted = count_markers(&body.text);
    if redacted > 0 && !confirm_redacted {
        return Err(ReplayBlock {
            reason: format!(
                "{}; replay would send the redacted text",
                redacted_values_phrase(redacted)
            ),
            needs_confirmation: true,
        });
    }
    Ok(())
}

/// Where the opened tab sends its request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApiClientTarget {
    /// A provider and model of the catalog (the serving provider still exists).
    Provider {
        /// The provider id.
        provider_id: String,
        /// The model id sent upstream.
        model: String,
    },
    /// A literal URL: the gateway for a client request, the stored host for a deleted provider.
    Custom {
        /// The URL, without user info or query.
        url: String,
    },
}

/// The unsaved request tab "Open in API client" creates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiClientDraft {
    /// Tab title: `POST /chat/completions · deepseek-v4-pro`.
    pub title: String,
    /// Where the request goes.
    pub target: ApiClientTarget,
    /// The protocol of the body.
    pub protocol: Protocol,
    /// The stored body text; empty when none was captured.
    pub body: String,
    /// Notes shown in the tab (not captured, cut, elided, redacted).
    pub notes: Vec<String>,
}

/// What a draft is built from before its notes are written.
struct DraftParts<'a> {
    target: ApiClientTarget,
    protocol: Protocol,
    model: String,
    path: String,
    body: Option<StoredBody<'a>>,
}

/// The post-translation upstream request: the serving provider and model when the provider still
/// exists, else the stored host as a Custom URL.
fn upstream_draft<'a>(capture: &'a CapturedExchange, facts: &RowFacts) -> DraftParts<'a> {
    let protocol = facts.protocol_out.unwrap_or(capture.client_protocol);
    let model = facts
        .upstream_model
        .clone()
        .unwrap_or_else(|| facts.requested_model.clone());
    let target = match (&facts.provider_id, facts.provider_exists) {
        (Some(provider_id), true) => ApiClientTarget::Provider {
            provider_id: provider_id.clone(),
            model: model.clone(),
        },
        _ => ApiClientTarget::Custom {
            url: bare_url(&capture.upstream_url),
        },
    };
    let mut path = protocol.suffix().replace("{model}", &model);
    if facts.stream {
        path = path.replace(":generateContent", ":streamGenerateContent");
    }
    DraftParts {
        target,
        protocol,
        model,
        path,
        body: upstream_body(capture, facts),
    }
}

/// The client request aimed at the gateway at `gateway_base`.
fn gateway_draft<'a>(
    capture: &'a CapturedExchange,
    facts: &RowFacts,
    gateway_base: &str,
) -> DraftParts<'a> {
    let protocol = capture.client_protocol;
    let model = facts.requested_model.clone();
    let path = gateway_path(protocol, &model, facts.stream);
    let target = ApiClientTarget::Custom {
        url: gateway_url(gateway_base, &path),
    };
    let path = path.split('?').next().unwrap_or(&path).to_owned();
    DraftParts {
        target,
        protocol,
        model,
        path,
        body: client_body(capture),
    }
}

/// The tab's body text and the notes about it (not captured, cut, elided, redacted, in order).
fn body_notes(body: Option<StoredBody<'_>>) -> (String, Vec<String>) {
    let Some(body) = body else {
        return (String::new(), vec!["The body was not captured".to_owned()]);
    };
    let mut notes = Vec::new();
    if body.truncated {
        notes.push(CUT_WARNING.to_owned());
    }
    if body.elided {
        notes.push(
            "The body had media that was not stored; the text holds elision markers".to_owned(),
        );
    }
    let redacted = count_markers(&body.text);
    if redacted > 0 {
        notes.push(redacted_values_phrase(redacted));
    }
    (body.text.into_owned(), notes)
}

/// Builds the draft: the post-translation upstream request when the capture has it (target = the
/// serving provider and model, `protocol_out`), else the client request aimed at the gateway
/// (`gateway_base`). A deleted provider opens in Custom URL mode with the stored host.
#[must_use]
pub fn open_in_api_client(
    capture: &CapturedExchange,
    facts: &RowFacts,
    gateway_base: &str,
) -> ApiClientDraft {
    let parts = if capture.upstream_url.is_empty() {
        gateway_draft(capture, facts, gateway_base)
    } else {
        upstream_draft(capture, facts)
    };
    let (body, notes) = body_notes(parts.body);
    ApiClientDraft {
        title: format!("POST {} · {}", parts.path, parts.model),
        target: parts.target,
        protocol: parts.protocol,
        body,
        notes,
    }
}
