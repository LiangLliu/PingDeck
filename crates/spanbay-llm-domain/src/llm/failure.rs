//! Failure taxonomy for model calls.
//!
//! Single responsibility: describe one failed call as a [`Failure`] (a [`FailureKind`] and the
//! [`FailureScope`] it is about), so the probe, the gateway, the client, the circuit, the key
//! cooldown and the UI read failures the same way. [`assess`] decides in one fixed order;
//! [`classify`] is its kind for callers that only have a status and a text. Provider error bodies
//! are usually JSON; the text is only lowercased and searched, never parsed. No I/O, no clock.

use serde::{Deserialize, Serialize};

use crate::llm::health::Outcome;

mod text;

use text::{
    POLICY_WORDS, QUOTA_CODES, QUOTA_WORDS, RATE_LIMIT_WORDS, REGION_WORDS, contains_any,
    context_limit_in, mentions_model, missing_model_in, output_cap_in, scan_text, text_kind,
};
pub use text::{names_context_limit, names_missing_model};

/// Why a model call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The key is missing, invalid or lacks permission.
    Auth,
    /// The key is valid but bound to another region or platform (or the caller's region is blocked).
    Region,
    /// Credit, balance or billing exhausted, or the model is not activated.
    Quota,
    /// Too many requests; retry later.
    RateLimit,
    /// The model does not exist or is not served to this account.
    ModelUnavailable,
    /// The call or the provider timed out.
    Timeout,
    /// The call succeeded but produced no content.
    EmptyOutput,
    /// The upstream does not speak the requested protocol or endpoint.
    ProtocolMismatch,
    /// The provider failed (5xx, overloaded).
    ProviderError,
    /// The request never completed (DNS, connect, reset).
    Network,
    /// A content or safety policy blocked the request.
    PolicyBlocked,
    /// The caller cancelled; produced only by callers, never by [`classify`].
    Cancelled,
    /// Nothing recognisable.
    Unknown,
    /// The request exceeds the model's context or output limit.
    ContextLimit,
    /// The stream closed cleanly without its end marker (the answer is incomplete).
    Truncated,
    /// The upstream says the request itself is wrong (another 4xx).
    BadRequest,
}

impl FailureKind {
    /// Every kind, used to keep [`FailureKind::parse`] and [`FailureKind::as_str`] in step.
    const ALL: [FailureKind; 16] = [
        Self::Auth,
        Self::Region,
        Self::Quota,
        Self::RateLimit,
        Self::ModelUnavailable,
        Self::Timeout,
        Self::EmptyOutput,
        Self::ProtocolMismatch,
        Self::ProviderError,
        Self::Network,
        Self::PolicyBlocked,
        Self::Cancelled,
        Self::Unknown,
        Self::ContextLimit,
        Self::Truncated,
        Self::BadRequest,
    ];

    /// The snake_case identifier used in the database and DTOs (stored strings only grow).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Region => "region",
            Self::Quota => "quota",
            Self::RateLimit => "rate_limit",
            Self::ModelUnavailable => "model_unavailable",
            Self::Timeout => "timeout",
            Self::EmptyOutput => "empty_output",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::ProviderError => "provider_error",
            Self::Network => "network",
            Self::PolicyBlocked => "policy_blocked",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
            Self::ContextLimit => "context_limit",
            Self::Truncated => "truncated",
            Self::BadRequest => "bad_request",
        }
    }

    /// Parses [`FailureKind::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }
}

/// What a failure is about, which decides where failover goes next and who pays for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureScope {
    /// The credential or the account behind it: try the provider's next key.
    Key,
    /// The model at this provider: try the next target.
    Model,
    /// The provider or the connection to it: try the next target.
    Target,
    /// The request itself: answer the caller with it.
    Request,
    /// This machine's network (DNS, no route, local proxy): try the next target, never blame the
    /// provider's circuit.
    Local,
}

/// How an attempt failed below HTTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// First byte or idle timeout before any content.
    Timeout,
    /// Connection, TLS or transfer error.
    Connection,
    /// This machine's network (DNS, or the proxy refused, unreachable or asking for credentials):
    /// never the provider's fault.
    Local,
    /// A 2xx that is not the expected stream or JSON, or whose body cannot be read.
    UnexpectedBody,
    /// The stream carried an error event before the first content event.
    StreamError,
    /// The stream closed cleanly without its end marker.
    Truncated,
}

/// Everything the failure rules look at, taken from one failed attempt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FailureObservation {
    /// HTTP status of the upstream answer, when there was one.
    pub status: Option<u16>,
    /// The failure below HTTP, when there was no failing status.
    pub transport: Option<Transport>,
    /// Start of the error body (or the in-stream error message).
    pub body: String,
    /// The upstream's `Retry-After`, in milliseconds.
    pub retry_after_ms: Option<u64>,
}

/// What to do after a failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The credential or quota of this key failed: try the provider's next enabled key.
    NextKey,
    /// The target (provider and model) failed: skip its remaining keys, try the next target.
    NextTarget,
    /// The request itself is wrong: answer the caller with it.
    Return,
}

/// How long a key rests after a failure (the durations are the key book's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRest {
    /// The upstream's `Retry-After`, in milliseconds.
    RetryAfter(u64),
    /// The rate-limit backoff: doubling per consecutive failure.
    Backoff,
    /// The long rest of a rejected credential or an exhausted account.
    Rejected,
}

/// One failed attempt, judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The category.
    pub kind: FailureKind,
    /// What it is about.
    pub scope: FailureScope,
    /// HTTP status, when the upstream answered.
    pub status: Option<u16>,
    /// The upstream's `Retry-After`, in milliseconds.
    pub retry_after_ms: Option<u64>,
    /// The attempt was an in-place retry of the same target (set by the failover driver).
    pub retried: bool,
}

impl Failure {
    /// Where failover goes next.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        match self.scope {
            FailureScope::Key => Verdict::NextKey,
            FailureScope::Model | FailureScope::Target | FailureScope::Local => Verdict::NextTarget,
            FailureScope::Request => Verdict::Return,
        }
    }

    /// How long the key rests: exhausted accounts by the header or the long rest (no 429
    /// doubling), rate limits by the header or the backoff, rejected credentials the long rest.
    /// A model-level refusal does not rest the key.
    #[must_use]
    pub fn key_rest(&self) -> Option<KeyRest> {
        let after = self.retry_after_ms.map(KeyRest::RetryAfter);
        match self.kind {
            FailureKind::Quota => Some(after.unwrap_or(KeyRest::Rejected)),
            FailureKind::RateLimit => Some(after.unwrap_or(KeyRest::Backoff)),
            FailureKind::Auth | FailureKind::Region => Some(KeyRest::Rejected),
            _ => None,
        }
    }

    /// What the failure means for the circuit of the key and model: only trouble of the target
    /// or the model counts; a key failure counts only when the provider's key cooldown is off
    /// (otherwise the cooldown already rests the key); this machine's network never does.
    #[must_use]
    pub fn health_outcome(&self, key_cooldown: bool) -> Outcome {
        let failed = match self.scope {
            FailureScope::Target | FailureScope::Model => matches!(
                self.kind,
                FailureKind::ProviderError
                    | FailureKind::Timeout
                    | FailureKind::Network
                    | FailureKind::ProtocolMismatch
                    | FailureKind::Truncated
            ),
            FailureScope::Key => !key_cooldown,
            FailureScope::Request | FailureScope::Local => false,
        };
        if failed {
            Outcome::Failure
        } else {
            Outcome::Success
        }
    }
}

impl FailureObservation {
    /// [`assess`] of this observation.
    #[must_use]
    pub fn assess(&self) -> Failure {
        assess(self)
    }

    /// The circuit outcome ([`Failure::health_outcome`]).
    #[must_use]
    pub fn health_outcome(&self, key_cooldown: bool) -> Outcome {
        assess(self).health_outcome(key_cooldown)
    }
}

/// The scope a kind found in text alone is about.
const fn scope_of(kind: FailureKind) -> FailureScope {
    match kind {
        FailureKind::Auth | FailureKind::Region | FailureKind::Quota | FailureKind::RateLimit => {
            FailureScope::Key
        }
        FailureKind::ModelUnavailable | FailureKind::ContextLimit => FailureScope::Model,
        FailureKind::PolicyBlocked | FailureKind::BadRequest => FailureScope::Request,
        _ => FailureScope::Target,
    }
}

/// The kind and scope of a failure, from its status, its transport failure and its scanned text
/// (see [`assess`] for the order).
fn judge(
    status: Option<u16>,
    transport: Option<Transport>,
    text: &str,
) -> (FailureKind, FailureScope) {
    use FailureKind as K;
    use FailureScope as S;
    match transport {
        Some(Transport::Local) => return (K::Network, S::Local),
        Some(Transport::Connection) => return (K::Network, S::Target),
        Some(Transport::Timeout) => return (K::Timeout, S::Target),
        Some(Transport::UnexpectedBody) => return (K::ProtocolMismatch, S::Target),
        Some(Transport::Truncated) => return (K::Truncated, S::Target),
        Some(Transport::StreamError) | None => {}
    }
    if contains_any(text, QUOTA_CODES) {
        return (K::Quota, S::Key);
    }
    if contains_any(text, POLICY_WORDS) {
        return (K::PolicyBlocked, S::Request);
    }
    if transport == Some(Transport::StreamError) {
        let kind = match text_kind(text) {
            K::Unknown => K::ProviderError,
            kind => kind,
        };
        return (kind, S::Target);
    }
    let Some(failing) = status.filter(|status| *status >= 400) else {
        let kind = text_kind(text);
        let scope = if status.is_some() {
            S::Request
        } else {
            scope_of(kind)
        };
        return (kind, scope);
    };
    match failing {
        400 | 413 if context_limit_in(text) => (K::ContextLimit, S::Model),
        // An output cap above this model's: another model of the chain may take it as it is.
        400 if output_cap_in(text) => (K::ContextLimit, S::Model),
        408 | 504 => (K::Timeout, S::Target),
        500..=599 => (K::ProviderError, S::Target),
        401 | 403 if contains_any(text, REGION_WORDS) => (K::Region, S::Key),
        403 if mentions_model(text) => (K::ModelUnavailable, S::Model),
        401 | 403 => (K::Auth, S::Key),
        402 => (K::Quota, S::Key),
        404 if missing_model_in(text) => (K::ModelUnavailable, S::Model),
        // Not about this model: the caller's answer (probes still word it as unavailable).
        404 => (K::ModelUnavailable, S::Request),
        // Rate-limit wording first: OpenAI's free-tier 429 links the billing page.
        429 if contains_any(text, RATE_LIMIT_WORDS) => (K::RateLimit, S::Key),
        429 if contains_any(text, QUOTA_WORDS) => (K::Quota, S::Key),
        429 => (K::RateLimit, S::Key),
        _ => (K::BadRequest, S::Request),
    }
}

/// Judges one failed attempt, in a fixed order: this machine's network, other transport
/// failures, structured quota codes (any status), policy text, context and output-cap limits,
/// timeouts, 5xx
/// (before any text rule), region, model-level 403, other 401/403 and 402, model-level 404, 429
/// (rate-limit wording before money words), other 4xx.
///
/// Without a failing status the text alone decides the kind (after the quota codes and the
/// policy text): an in-stream error ([`Transport::StreamError`]) takes the text's kind, an
/// unrecognised text being [`FailureKind::ProviderError`], and is about the target; a status that
/// is not a failure (a 200 carrying an error text) is about the request; with no status at all
/// the kind's own scope applies.
#[must_use]
pub fn assess(observation: &FailureObservation) -> Failure {
    let (kind, scope) = judge(
        observation.status,
        observation.transport,
        &scan_text(&observation.body),
    );
    Failure {
        kind,
        scope,
        status: observation.status,
        retry_after_ms: observation.retry_after_ms,
        retried: false,
    }
}

/// Whether a response status is worth retrying (`408`, `429`, any `5xx`).
#[must_use]
pub const fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500..=599)
}

/// The kind of a failed call that only has a status and a text: [`assess`]`(...).kind`, so the
/// probe, the logs and the agent read the same category as failover.
#[must_use]
pub fn classify(status: Option<u16>, text: &str) -> FailureKind {
    judge(status, None, &scan_text(text)).0
}

#[cfg(test)]
mod tests;
