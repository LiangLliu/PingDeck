//! The Test bench's status vocabulary and the one-line reasons shown for it.
//!
//! Single responsibility: map a stored [`ProbeResult`] to one of the nine status words and
//! describe a failure in plain words. Pure: no I/O, no secrets, so the webview never re-derives
//! either.

use serde::{Deserialize, Serialize};

use super::timeouts::DEFAULT_SETTING_S;
use super::{ProbeResult, ProbeTest};
use crate::llm::failure::FailureKind;

/// Longest reason line.
const MAX_REASON_CHARS: usize = 160;

/// The closed set of cell and result states (D-44). No other status exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    /// The check passed.
    Ready,
    /// The provider refused the request for this key or model.
    Rejected,
    /// HTTP 429 or a rate-limit failure: neither a pass nor a refusal, worth running again.
    RateLimited,
    /// No usable answer: transport errors or a provider outage.
    Unreachable,
    /// A 2xx answer that is not valid for the protocol or the check.
    InvalidResponse,
    /// The model or provider says the feature does not exist, or the capability is absent.
    Unsupported,
    /// The model answered but did not show the capability (no tool call, no reasoning, no cache
    /// hit, no search event). Neither a pass nor a failure: a model can have a capability it does
    /// not show, so the cell may be run again and never counts as failed.
    NotObserved,
    /// The check did not run.
    Skipped,
    /// No answer within the check's timeout.
    TimedOut,
}

impl ProbeStatus {
    /// Every status, in legend order.
    pub const ALL: [Self; 9] = [
        Self::Ready,
        Self::Rejected,
        Self::RateLimited,
        Self::Unreachable,
        Self::InvalidResponse,
        Self::Unsupported,
        Self::NotObserved,
        Self::Skipped,
        Self::TimedOut,
    ];

    /// The word shown in the legend and the cell names.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Rejected => "Rejected",
            Self::RateLimited => "Rate limited",
            Self::Unreachable => "Unreachable",
            Self::InvalidResponse => "Invalid response",
            Self::Unsupported => "Unsupported",
            Self::NotObserved => "Not observed",
            Self::Skipped => "Skipped",
            Self::TimedOut => "Timed out",
        }
    }

    /// The snake_case identifier (database, export).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Rejected => "rejected",
            Self::RateLimited => "rate_limited",
            Self::Unreachable => "unreachable",
            Self::InvalidResponse => "invalid_response",
            Self::Unsupported => "unsupported",
            Self::NotObserved => "not_observed",
            Self::Skipped => "skipped",
            Self::TimedOut => "timed_out",
        }
    }

    /// Parses [`ProbeStatus::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == text)
    }

    /// Whether the "Failed" filter and the "N failed" counts include this status.
    #[must_use]
    pub(crate) const fn is_failure(self) -> bool {
        matches!(
            self,
            Self::Rejected
                | Self::RateLimited
                | Self::Unreachable
                | Self::InvalidResponse
                | Self::TimedOut
        )
    }

    /// Whether a retry runs the cell again: every failure, and Not observed (the capability may
    /// show on the next attempt). Ready and Unsupported are final.
    #[must_use]
    pub(crate) const fn is_retryable(self) -> bool {
        self.is_failure() || matches!(self, Self::NotObserved)
    }

    /// The status of a stored result. `None` for a cancelled check: nothing is stored and the
    /// cell stays as it was. The first matching rule wins:
    ///
    /// 1. cancelled: `None`;
    /// 2. passed: Ready;
    /// 3. HTTP 429 or `RateLimit`: Rate limited;
    /// 4. `Timeout`, HTTP 408 or 504: Timed out;
    /// 5. `Network`, or HTTP 500/502/503 with `ProviderError`: Unreachable;
    /// 6. HTTP 401-403, HTTP 404 on Chat or Stream, or an `Auth`, `Region`, `Quota`,
    ///    `ModelUnavailable` or `PolicyBlocked` failure: Rejected;
    /// 7. HTTP 400 or 422 on any check: Invalid response, the "request refused" case (often a
    ///    parameter this model or relay does not take; it says nothing about the capability);
    /// 8. a capability check with a protocol mismatch and no 2xx, or HTTP 415/501: Unsupported;
    /// 9. any other 2xx: Not observed for Tools, Reasoning, Cache and Search (a model can have a
    ///    capability it does not show), Unsupported for Vision, else Invalid response;
    /// 10. any other 4xx: Rejected; 5xx: Unreachable; otherwise Invalid response.
    #[must_use]
    pub fn from_result(result: &ProbeResult) -> Option<Self> {
        let kind = result.failure_kind;
        if kind == Some(FailureKind::Cancelled) {
            return None;
        }
        if result.ok {
            return Some(Self::Ready);
        }
        let test = ProbeTest::parse(&result.test);
        let status = result.status;
        // A 429 is never a refusal (D-203): the key or model may well work a moment later.
        if status == Some(429) || kind == Some(FailureKind::RateLimit) {
            return Some(Self::RateLimited);
        }
        if kind == Some(FailureKind::Timeout) || matches!(status, Some(408 | 504)) {
            return Some(Self::TimedOut);
        }
        if kind == Some(FailureKind::Network)
            || (matches!(status, Some(500 | 502 | 503)) && kind == Some(FailureKind::ProviderError))
        {
            return Some(Self::Unreachable);
        }
        let chat_route = matches!(test, Some(ProbeTest::Chat | ProbeTest::Stream));
        if matches!(status, Some(401..=403))
            || (status == Some(404) && chat_route)
            || is_rejected_kind(kind)
        {
            return Some(Self::Rejected);
        }
        // The request itself was refused (often a parameter the model or relay does not take):
        // the model may be fine and may well have the capability, so this is neither a pass nor
        // a verdict on the feature. Apply leaves the model and its capabilities alone.
        if matches!(status, Some(400 | 422)) {
            return Some(Self::InvalidResponse);
        }
        let answered = answered(status);
        let capability = test.is_some_and(is_capability_check);
        if capability
            && ((kind == Some(FailureKind::ProtocolMismatch) && !answered)
                || matches!(status, Some(415 | 501)))
        {
            return Some(Self::Unsupported);
        }
        if answered {
            return Some(match test {
                Some(test) if absence_is_not_observed(test) => Self::NotObserved,
                Some(ProbeTest::Vision) => Self::Unsupported,
                _ => Self::InvalidResponse,
            });
        }
        Some(match status {
            Some(400..=499) => Self::Rejected,
            Some(500..=599) => Self::Unreachable,
            _ => Self::InvalidResponse,
        })
    }
}

/// Whether the provider answered with HTTP 2xx.
pub(crate) fn answered(status: Option<u16>) -> bool {
    status.is_some_and(|code| (200..300).contains(&code))
}

/// Failure kinds that say the provider refused this key or model.
const fn is_rejected_kind(kind: Option<FailureKind>) -> bool {
    matches!(
        kind,
        Some(
            FailureKind::Auth
                | FailureKind::Region
                | FailureKind::Quota
                | FailureKind::ModelUnavailable
                | FailureKind::PolicyBlocked
        )
    )
}

/// Whether a result is the "request refused" case: HTTP 400 or 422 on any check. Apply leaves
/// such a model and its capabilities unchanged.
#[must_use]
pub(crate) fn is_request_refused(result: &ProbeResult) -> bool {
    matches!(result.status, Some(400 | 422))
        && ProbeStatus::from_result(result) == Some(ProbeStatus::InvalidResponse)
}

/// Checks of a capability: an explicit refusal of the feature (HTTP 415, 501 or a protocol
/// mismatch) means the feature does not exist.
const fn is_capability_check(test: ProbeTest) -> bool {
    matches!(
        test,
        ProbeTest::Models
            | ProbeTest::Tools
            | ProbeTest::JsonSchema
            | ProbeTest::Vision
            | ProbeTest::Reasoning
            | ProbeTest::Cache
            | ProbeTest::Embeddings
            | ProbeTest::Search
            | ProbeTest::Pdf
            | ProbeTest::Decision
    )
}

/// Checks whose failed content test on an answering model is only "not observed": the model
/// may have the capability without showing it on this prompt (Tools, Reasoning, Cache and
/// Search). [`super::capability::derive_capability`] records the same cases as
/// `Measurement::NotObserved`.
pub(crate) const fn absence_is_not_observed(test: ProbeTest) -> bool {
    matches!(
        test,
        ProbeTest::Tools | ProbeTest::Reasoning | ProbeTest::Cache | ProbeTest::Search
    )
}

/// What [`reason_line`] needs besides the result.
#[derive(Debug, Clone, Copy)]
pub struct ReasonContext<'a> {
    /// The timeout that expired, in seconds (the number "Timed out 60 s" names).
    pub timeout_s: u64,
    /// `Retry-After` of a 429, in seconds, when the provider sent one.
    pub retry_after_s: Option<u64>,
    /// How the provider is called in a sentence: "the provider", "the relay".
    pub host: &'a str,
}

impl Default for ReasonContext<'_> {
    fn default() -> Self {
        Self {
            timeout_s: u64::from(DEFAULT_SETTING_S),
            retry_after_s: None,
            host: "the provider",
        }
    }
}

/// The one-line explanation under a failed result: the status word, what happened in plain
/// words and what to do. Built only from fixed text, the status code and the failure kind, so it
/// can hold no secret; at most 160 characters. Empty for Ready and Skipped (a skip carries its
/// own reason).
#[must_use]
pub fn reason_line(
    result: &ProbeResult,
    status: ProbeStatus,
    context: &ReasonContext<'_>,
) -> String {
    let test = ProbeTest::parse(&result.test);
    let host = context.host;
    let code = result.status;
    let suffix = code.map_or_else(String::new, |code| format!(" ({code})"));
    let mut text = match status {
        ProbeStatus::Ready | ProbeStatus::Skipped => return String::new(),
        ProbeStatus::Rejected => rejected_reason(result.failure_kind, code, host, &suffix),
        ProbeStatus::RateLimited => context.retry_after_s.map_or_else(
            || "Rate limited. Wait a moment, then test again.".to_owned(),
            |seconds| format!("Rate limited, retry in {seconds} s."),
        ),
        ProbeStatus::Unreachable => match code {
            Some(code) => format!("Unreachable: {host} answered HTTP {code}. Test again later."),
            None => format!(
                "Unreachable: no answer from {host}. Check the address and your network, then test again."
            ),
        },
        ProbeStatus::TimedOut => timed_out_reason(test, context.timeout_s, host),
        ProbeStatus::InvalidResponse => invalid_reason(result, test, &suffix),
        ProbeStatus::Unsupported => unsupported_reason(test, code, host, &suffix),
        ProbeStatus::NotObserved => not_observed_reason(test),
    };
    if let Some((cut, _)) = text.char_indices().nth(MAX_REASON_CHARS) {
        text.truncate(cut);
    }
    text
}

fn rejected_reason(
    kind: Option<FailureKind>,
    code: Option<u16>,
    host: &str,
    suffix: &str,
) -> String {
    match kind {
        Some(FailureKind::Auth) if code == Some(403) => format!(
            "Rejected: this key cannot use the model (403). Enable it on {host}, then test again."
        ),
        Some(FailureKind::Auth) => format!(
            "Rejected: {host} did not accept this key{suffix}. Check the key, then test again."
        ),
        Some(FailureKind::Region) => format!(
            "Rejected: this key is bound to another region or your location is blocked{suffix}."
        ),
        Some(FailureKind::Quota) => format!(
            "Rejected: the account has no credit left for this model{suffix}. Top up, then test again."
        ),
        Some(FailureKind::ModelUnavailable) => format!(
            "Rejected: {host} does not serve this model to this key{suffix}. Check the model id."
        ),
        Some(FailureKind::PolicyBlocked) => {
            "Rejected: a content policy blocked the request. Try another model.".to_owned()
        }
        _ => format!("Rejected: request refused{suffix}."),
    }
}

fn timed_out_reason(test: Option<ProbeTest>, timeout_s: u64, host: &str) -> String {
    let first_token = matches!(test, Some(ProbeTest::Chat | ProbeTest::Stream));
    let wait = if first_token {
        "no first token"
    } else {
        "no answer"
    };
    format!(
        "Timed out: {wait} in {timeout_s} s. {} may be queueing it upstream. Test again.",
        capitalize(host)
    )
}

fn invalid_reason(result: &ProbeResult, test: Option<ProbeTest>, suffix: &str) -> String {
    if is_request_refused(result) {
        return format!(
            "Request refused{suffix}; often a parameter this model does not accept. Says nothing about support."
        );
    }
    format!(
        "Invalid response: {}.",
        match test {
            Some(ProbeTest::Tools) => "the model answered without calling the tool",
            Some(ProbeTest::Stream) => "the stream ended without usable content",
            Some(ProbeTest::JsonSchema) => "the answer does not follow the schema",
            Some(ProbeTest::Embeddings) => "the answer has no embedding vector",
            Some(ProbeTest::Chat) => "the model returned no content",
            _ => "the answer is not valid for this protocol",
        }
    )
}

fn unsupported_reason(
    test: Option<ProbeTest>,
    code: Option<u16>,
    host: &str,
    suffix: &str,
) -> String {
    if answered(code) {
        format!("Unsupported: {}.", absence(test))
    } else {
        format!(
            "Unsupported: {host} says this model cannot do {}{suffix}.",
            feature(test)
        )
    }
}

fn not_observed_reason(test: Option<ProbeTest>) -> String {
    format!(
        "Not observed: {}. The model may still support {}; test again.",
        absence(test),
        feature(test)
    )
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

const fn feature(test: Option<ProbeTest>) -> &'static str {
    match test {
        Some(ProbeTest::Tools) => "tool calls",
        Some(ProbeTest::JsonSchema) => "JSON schema output",
        Some(ProbeTest::Vision) => "image input",
        Some(ProbeTest::Reasoning) => "reasoning",
        Some(ProbeTest::Cache) => "prompt caching",
        Some(ProbeTest::Embeddings) => "embeddings",
        Some(ProbeTest::Search) => "web search",
        Some(ProbeTest::Pdf) => "document input",
        Some(ProbeTest::Models) => "a model list",
        _ => "this",
    }
}

const fn absence(test: Option<ProbeTest>) -> &'static str {
    match test {
        Some(ProbeTest::Vision) => "the model answered but did not read the image",
        Some(ProbeTest::Tools) => "the model answered without calling the tool",
        Some(ProbeTest::Reasoning) => "no reasoning was observed",
        Some(ProbeTest::Cache) => "no cache hit seen",
        Some(ProbeTest::Search) => "the answer has no search event or citation",
        Some(ProbeTest::Pdf) => "the model did not read the document",
        _ => "the model answered but lacks this capability",
    }
}

#[cfg(test)]
mod tests;
