//! Why a balance request produced no reading: the error classes the keep-last-good policy and the
//! card text switch on. A message never carries a credential: constructors take text the adapter
//! composed or a vendor message that was redacted first.

use serde::{Deserialize, Serialize};

use crate::llm::safe_http::{SafeResponse, SafeTransportError};
pub(super) use pingdeck_llm_domain::llm::balance_state::BalanceErrorKind;

/// A failed balance request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceError {
    /// Class.
    pub kind: BalanceErrorKind,
    /// HTTP status when a response arrived.
    #[serde(default)]
    pub status: Option<u16>,
    /// Human text; never a credential.
    pub message: String,
    /// Endpoint path (and host when it is not the provider's) of the failing request.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// The service's `Retry-After` of a 429, in milliseconds from the failure. Kept in the
    /// stored error record; not part of the wire form.
    #[serde(skip)]
    pub retry_after_ms: Option<u64>,
}

impl BalanceError {
    /// An error without a response or an endpoint.
    #[must_use]
    pub fn new(kind: BalanceErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            message: message.into(),
            endpoint: None,
            retry_after_ms: None,
        }
    }

    /// Adds the endpoint of the failing request.
    #[must_use]
    pub fn at(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// Adds the HTTP status.
    #[must_use]
    const fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// The class of a non-2xx response, with the `Retry-After` of a 429 (delay seconds or an
    /// HTTP date).
    pub(super) fn from_response(response: &SafeResponse, endpoint: &str) -> Self {
        let mut error = Self::from_status(response.status, endpoint);
        if error.kind == BalanceErrorKind::RateLimited {
            let now_ms = chrono::Utc::now().timestamp_millis();
            error.retry_after_ms = response
                .header("retry-after")
                .and_then(|value| retry_after_ms(value, now_ms));
        }
        error
    }

    /// The class of a non-2xx status.
    pub(super) fn from_status(status: u16, endpoint: &str) -> Self {
        let (kind, message) = match status {
            401 | 403 => (BalanceErrorKind::Auth, "The key was rejected".to_owned()),
            429 => (
                BalanceErrorKind::RateLimited,
                "The service is rate limiting balance requests".to_owned(),
            ),
            408 | 500..=599 => (
                BalanceErrorKind::Transient,
                format!("The service answered HTTP {status}"),
            ),
            _ => (
                BalanceErrorKind::NotFound,
                format!("Balance unavailable: HTTP {status}"),
            ),
        };
        Self::new(kind, message).with_status(status).at(endpoint)
    }

    /// The class of a transport failure.
    pub(super) fn from_transport(error: &SafeTransportError, endpoint: &str) -> Self {
        let (kind, message) = match error {
            SafeTransportError::Timeout => (
                BalanceErrorKind::Transient,
                "The balance request timed out".to_owned(),
            ),
            SafeTransportError::Cancelled => (
                BalanceErrorKind::Cancelled,
                "The balance request was cancelled".to_owned(),
            ),
            SafeTransportError::Blocked { reason } => (
                BalanceErrorKind::Blocked,
                format!("The address is not allowed ({reason})"),
            ),
            other => (
                BalanceErrorKind::Network,
                format!("The service could not be reached ({})", other.kind()),
            ),
        };
        Self::new(kind, message).at(endpoint)
    }

    /// An answer that is not the expected shape.
    pub(super) fn schema(message: impl Into<String>, endpoint: &str) -> Self {
        Self::new(BalanceErrorKind::SchemaMismatch, message).at(endpoint)
    }

    /// Whether the cancel token stopped the request.
    pub(super) fn is_cancelled(&self) -> bool {
        self.kind == BalanceErrorKind::Cancelled
    }

    /// A rejected console credential: Auth becomes ConsoleTokenExpired; status and endpoint stay.
    pub(super) fn into_console_rejected(mut self) -> Self {
        if self.kind == BalanceErrorKind::Auth {
            self.kind = BalanceErrorKind::ConsoleTokenExpired;
            self.message = "The console token was rejected".to_owned();
        }
        self
    }
}

/// Longest wait a `Retry-After` may impose on the automatic refresh: one day.
const MAX_RETRY_AFTER_MS: u64 = 24 * 60 * 60 * 1000;

/// A `Retry-After` value (delay seconds, or an HTTP date) as milliseconds from `now_ms`, capped
/// at one day; a date in the past is `0`, anything unparseable is `None`.
fn retry_after_ms(value: &str, now_ms: i64) -> Option<u64> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds.saturating_mul(1000).min(MAX_RETRY_AFTER_MS));
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let wait = at.timestamp_millis().saturating_sub(now_ms).max(0);
    Some(u64::try_from(wait).unwrap_or(0).min(MAX_RETRY_AFTER_MS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_reads_seconds_and_dates_and_is_capped() {
        assert_eq!(retry_after_ms("120", 0), Some(120_000));
        assert_eq!(retry_after_ms("999999999", 0), Some(MAX_RETRY_AFTER_MS));
        // Sun, 06 Nov 1994 08:49:40 GMT is 784111780000 ms.
        assert_eq!(
            retry_after_ms("Sun, 06 Nov 1994 08:49:40 GMT", 784_111_770_000),
            Some(10_000)
        );
        assert_eq!(
            retry_after_ms("Sun, 06 Nov 1994 08:49:40 GMT", 784_111_790_000),
            Some(0)
        );
        assert_eq!(retry_after_ms("soon", 0), None);
    }

    #[test]
    fn statuses_map_to_the_error_classes() {
        let class = |status| BalanceError::from_status(status, "/x").kind;
        assert_eq!(class(401), BalanceErrorKind::Auth);
        assert_eq!(class(403), BalanceErrorKind::Auth);
        assert_eq!(class(404), BalanceErrorKind::NotFound);
        assert_eq!(class(302), BalanceErrorKind::NotFound);
        assert_eq!(class(429), BalanceErrorKind::RateLimited);
        assert_eq!(class(408), BalanceErrorKind::Transient);
        assert_eq!(class(502), BalanceErrorKind::Transient);
    }

    #[test]
    fn transport_failures_map_to_classes_and_keep_the_blocked_reason() {
        let class = |error: SafeTransportError| BalanceError::from_transport(&error, "/x");
        assert_eq!(
            class(SafeTransportError::Timeout).kind,
            BalanceErrorKind::Transient
        );
        assert_eq!(
            class(SafeTransportError::Dns).kind,
            BalanceErrorKind::Network
        );
        assert_eq!(
            class(SafeTransportError::Cancelled).kind,
            BalanceErrorKind::Cancelled
        );
        let blocked = class(SafeTransportError::Blocked {
            reason: "link_local".to_owned(),
        });
        assert_eq!(blocked.kind, BalanceErrorKind::Blocked);
        assert!(blocked.message.contains("link_local"));
    }
}
