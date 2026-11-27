//! What is stored for one key's balance: the last good report, the last failure with its error
//! class, and the stale rule read at display time.

use serde::{Deserialize, Serialize};

use super::BalanceReport;

/// The class of a balance failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalanceErrorKind {
    /// 401 or 403: the key (or console token) was rejected.
    Auth,
    /// The endpoint is not there (404, 3xx that was not followed, other client errors).
    NotFound,
    /// The answer is not what the adapter expects, or the envelope reports a failure.
    SchemaMismatch,
    /// 429.
    RateLimited,
    /// A timeout, 408 or 5xx: worth retrying later.
    Transient,
    /// DNS, connection or TLS failure.
    Network,
    /// The SSRF guard or the host rule refused the request; nothing was sent.
    Blocked,
    /// The cancel token fired.
    Cancelled,
    /// The adapter needs a console token that is not configured; nothing was sent.
    NeedsConsoleToken,
    /// The console token was rejected or its session ended.
    ConsoleTokenExpired,
}

impl BalanceErrorKind {
    /// The wire name of the class (the `snake_case` serde form).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::NotFound => "not_found",
            Self::SchemaMismatch => "schema_mismatch",
            Self::RateLimited => "rate_limited",
            Self::Transient => "transient",
            Self::Network => "network",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::NeedsConsoleToken => "needs_console_token",
            Self::ConsoleTokenExpired => "console_token_expired",
        }
    }
}

/// Milliseconds per minute.
const MS_PER_MINUTE: i64 = 60_000;
/// A value older than this is stale.
pub(super) const STALE_AFTER_MS: i64 = 60 * MS_PER_MINUTE;

/// A failed refresh as it is stored beside the last good value (`balance_error_json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceErrorRecord {
    /// When the refresh failed, Unix milliseconds.
    pub at_ms: i64,
    /// Class.
    pub kind: BalanceErrorKind,
    /// HTTP status when a response arrived.
    #[serde(default)]
    pub status: Option<u16>,
    /// Human text; never a credential.
    pub message: String,
    /// The automatic refresh does not ask again before this instant (the service's
    /// `Retry-After` on a 429), Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<i64>,
}

impl BalanceErrorKind {
    /// Whether the failure may clear by itself soon (a timeout, a 5xx, a network failure or a
    /// rate limit): the automatic refresh asks again early. Every other class needs the user
    /// (a new key, a console token, a fixed query), so it is asked again rarely.
    #[must_use]
    pub const fn is_transient(self) -> bool {
        matches!(self, Self::Transient | Self::Network | Self::RateLimited)
    }
}

/// What is stored for one key: the last good report, the last failure and the stale marker.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StoredBalance {
    /// The newest successful report. No error clears it.
    #[serde(default)]
    pub last_good: Option<BalanceReport>,
    /// The failure of the newest refresh; cleared by the next success.
    #[serde(default)]
    pub error: Option<BalanceErrorRecord>,
    /// Older than 1 hour, or the last refresh failed, as of the last merge.
    #[serde(default)]
    pub stale: bool,
}

impl StoredBalance {
    /// Whether the value is stale at `now_ms`: the last refresh failed or the report is older
    /// than one hour. Use this at read time; `stale` is the snapshot of the last merge.
    #[must_use]
    pub fn is_stale_at(&self, now_ms: i64) -> bool {
        self.error.is_some()
            || self
                .last_good
                .as_ref()
                .is_some_and(|report| now_ms.saturating_sub(report.checked_at_ms) > STALE_AFTER_MS)
    }

    /// This value with `stale` set by [`Self::is_stale_at`] at `now_ms`: the snapshot a merge
    /// stores.
    #[must_use]
    pub fn settled(mut self, now_ms: i64) -> Self {
        self.stale = self.is_stale_at(now_ms);
        self
    }
}
