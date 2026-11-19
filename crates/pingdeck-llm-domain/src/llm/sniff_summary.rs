//! What a saved provider keeps of its last scan: the summary behind the header chips, the runner-up
//! family and the redacted evidence rows, plus the field cleaning and note markers the scan writes
//! and the provider record reads back.

use serde::{Deserialize, Serialize};

use crate::llm::wire::GatewayFamily;

/// The second-best family of the scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerUp {
    /// The family.
    pub family: GatewayFamily,
    /// Its score.
    pub score: i32,
}

/// One request of the scan, redacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceItem {
    /// Row id, shared with the [`SniffEvent`]s of the request.
    pub id: u32,
    /// `identify`, `local`, `models`, `routes`, `balance`, `verify`.
    pub phase: String,
    /// `GET` or `POST`.
    pub method: String,
    /// URL without query.
    pub url: String,
    /// HTTP status; `None` when no response came.
    pub status: Option<u16>,
    /// Time to the end of the body.
    pub latency_ms: u64,
    /// First 16 hex digits of the body's SHA-256.
    pub body_sha256_prefix: String,
    /// Human sentence built by core; never a body excerpt or a secret.
    pub note: String,
    /// The report field the row supports: `identity`, `models`, `route:<protocol>`, `key`,
    /// `balance`.
    pub conclusion: String,
    /// `timeout`, `tls`, `dns`, `refused`, `reset`, `blocked`, `other` or `none`.
    pub error_kind: String,
}

/// What is kept of the last scan of a saved provider (`llm_providers.sniff_summary_json`): the
/// identity and confidence behind the header chips and the "Evidence" link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SniffSummary {
    /// The detected family.
    pub family: GatewayFamily,
    /// Brand (`system_name`, `site_name` or the host).
    pub brand_name: Option<String>,
    /// Version from the family's field or header.
    pub version: Option<String>,
    /// Score of the winner.
    pub confidence_score: i32,
    /// Whether a signal came from a response of this host and the winner was accepted.
    pub confirmed: bool,
    /// The second-best family.
    pub runner_up: Option<RunnerUp>,
    /// Requests the scan sent.
    pub requests_used: u32,
    /// Wall time of the scan.
    pub elapsed_ms: u64,
    /// Tokens spent by real requests.
    #[serde(default)]
    pub tokens_spent: u64,
    /// done-hub semantics: the `quota` of `/api/status` is what is left.
    #[serde(default)]
    pub quota_is_remaining: bool,
    /// When the scan finished, Unix ms.
    pub scanned_at_ms: i64,
}

/// Cut of brand and version text.
const MAX_FIELD_CHARS: usize = 64;

/// A model-field text without control characters, trimmed and cut at 64 characters.
#[must_use]
pub fn clean_field(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_FIELD_CHARS)
        .collect()
}

/// Appended to the first evidence note when a proxy resolves the host's name.
pub const PROXY_DNS_NOTE: &str = ", proxy_dns";

/// Lead-in of the gateway's name in an identify note; `providers` reads the name back.
pub const SYSTEM_NAME_NOTE: &str = "system_name ";

#[cfg(test)]
mod tests;
