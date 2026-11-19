//! What a scan takes in: the typed input, the caller's context, and why a scan can fail.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::super::{auth::AuthMemory, diff::StoredProviderView};
use crate::llm::wire::Protocol;
use pingdeck_base::error::AppError;
use pingdeck_model::catalog_entry::CatalogEntry;

/// What the user typed in the Add-provider wizard.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SniffInput {
    /// The typed base URL (any form `normalize_base_url` accepts).
    pub base_url: String,
    /// The key to test; sent only to the typed origin.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Console token (management PAT, panel session) for admin-side probes.
    #[serde(default)]
    pub management_token: Option<String>,
    /// Numeric user id for the `New-API-User`/`Veloera-User` header.
    #[serde(default)]
    pub api_user_id: Option<String>,
    /// Allow the opt-in billable "Verify with real requests" step.
    #[serde(default)]
    pub allow_billable_probe: bool,
    /// The preset the user picked; narrows the scan.
    #[serde(default)]
    pub preset_id: Option<String>,
    /// The preset region the user picked.
    #[serde(default)]
    pub region_id: Option<String>,
    /// The saved provider being re-sniffed.
    #[serde(default)]
    pub provider_id: Option<String>,
    /// Restrict the route phase to one protocol (Advanced).
    #[serde(default)]
    pub protocol_override: Option<Protocol>,
    /// Model list path or same-origin URL (Advanced).
    #[serde(default)]
    pub models_url: Option<String>,
    /// Up to 8 extra request headers (Advanced); values are write-only.
    #[serde(default)]
    pub extra_headers: Vec<(String, String)>,
    /// `User-Agent` override (Advanced).
    #[serde(default)]
    pub user_agent: Option<String>,
    /// The new provider's own proxy (Advanced): a URL or `direct`; absent follows Settings
    /// › Network. A saved provider's re-scan uses the stored value instead.
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// "Try more address variants": budget 20 and the extra mount candidates.
    #[serde(default)]
    pub extended: bool,
    /// The user confirmed scanning a public-looking name that resolves privately.
    #[serde(default)]
    pub confirm_private: bool,
    /// The user chose "Send key anyway" after a look-alike host or foreign key shape warning.
    #[serde(default)]
    pub confirm_key_send: bool,
}

impl SniffInput {
    /// An input with only a URL (everything else off).
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
            management_token: None,
            api_user_id: None,
            allow_billable_probe: false,
            preset_id: None,
            region_id: None,
            provider_id: None,
            protocol_override: None,
            models_url: None,
            extra_headers: Vec::new(),
            user_agent: None,
            proxy_url: None,
            extended: false,
            confirm_private: false,
            confirm_key_send: false,
        }
    }

    /// The typed key, trimmed; `None` when absent or blank.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        self.api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
    }

    /// Every secret string of the input, for redaction of anything built from response bodies.
    pub(in crate::llm::sniff) fn secrets(&self) -> Vec<String> {
        let mut secrets: Vec<String> = [&self.api_key, &self.management_token]
            .into_iter()
            .flatten()
            .map(|secret| secret.trim().to_owned())
            .collect();
        secrets.extend(self.extra_headers.iter().map(|(_, value)| value.clone()));
        secrets
    }
}

/// `Debug` never prints the key, the console token or a header value, and drops any query of
/// the typed URL (a key pasted as `?key=`).
impl fmt::Debug for SniffInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SniffInput")
            .field(
                "base_url",
                &pingdeck_base::secrets::redact_secrets(
                    self.base_url.split(['?', '#']).next().unwrap_or_default(),
                    &[],
                ),
            )
            .field("has_api_key", &self.key().is_some())
            .field("has_management_token", &self.management_token.is_some())
            .field("has_api_user_id", &self.api_user_id.is_some())
            .field("allow_billable_probe", &self.allow_billable_probe)
            .field("preset_id", &self.preset_id)
            .field("region_id", &self.region_id)
            .field("provider_id", &self.provider_id)
            .field("protocol_override", &self.protocol_override)
            .field("models_url", &self.models_url.as_ref().map(|_| "set"))
            .field(
                "extra_header_names",
                &self
                    .extra_headers
                    .iter()
                    .map(|(n, _)| n)
                    .collect::<Vec<_>>(),
            )
            .field("user_agent", &self.user_agent)
            .field("extended", &self.extended)
            .field("confirm_private", &self.confirm_private)
            .field("confirm_key_send", &self.confirm_key_send)
            .finish()
    }
}

/// Everything around the input that the caller resolves (clock, version, memory, catalogue).
pub struct SniffContext<'a> {
    /// Now, Unix milliseconds (balance report stamp).
    pub now_ms: i64,
    /// App version for the `User-Agent` header.
    pub app_version: &'a str,
    /// Per-origin auth styles that worked earlier in this app session.
    pub auth_memory: &'a AuthMemory,
    /// Whether the transport goes through a proxy (the sniffer then reports `proxy_dns`).
    pub uses_proxy: bool,
    /// Catalogue lookup by model id; must be cheap and non-blocking (an in-memory
    /// snapshot), it runs on the scan's task.
    pub catalog: &'a (dyn Fn(&str) -> Option<CatalogEntry> + Send + Sync),
    /// The saved provider of a re-sniff; the report then carries the diff.
    pub stored: Option<&'a StoredProviderView>,
}

/// Why the scan did not produce a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SniffError {
    /// The typed address cannot be scanned; `reason` is a stable code
    /// (`empty`, `too_long`, `unparsable`, `scheme`, `no_host`, `port`, `bad_header`,
    /// `too_many_headers`, `models_url_origin`).
    InvalidInput {
        /// Stable machine-readable code.
        reason: String,
    },
    /// The host is link-local or a metadata address; nothing was sent.
    BlockedAddress {
        /// The refused host.
        host: String,
    },
    /// A public-looking name resolves to a private address; confirm to scan it.
    PrivateResolution {
        /// The typed host.
        host: String,
    },
    /// No request got an answer; `error_kind` uses the evidence vocabulary.
    Transport {
        /// `dns`, `refused`, `tls`, `timeout`, `reset` or `other`.
        error_kind: String,
        /// The typed host.
        host: String,
    },
    /// The cancel token fired.
    Cancelled,
}

impl fmt::Display for SniffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { .. } => {
                f.write_str("Enter an http or https address, like https://api.example.com")
            }
            Self::BlockedAddress { host } => {
                write!(
                    f,
                    "{host} is a link-local or metadata address and is never scanned"
                )
            }
            Self::PrivateResolution { host } => {
                write!(f, "{host} resolves to a private address")
            }
            Self::Transport { error_kind, host } => {
                write!(f, "{host} did not answer ({error_kind})")
            }
            Self::Cancelled => f.write_str(pingdeck_base::cancel::CANCELLED_MESSAGE),
        }
    }
}

impl std::error::Error for SniffError {}

impl From<SniffError> for AppError {
    fn from(error: SniffError) -> Self {
        match error {
            SniffError::InvalidInput { .. } | SniffError::BlockedAddress { .. } => {
                Self::validation(error.to_string())
            }
            SniffError::PrivateResolution { .. } => Self::confirmation_required(error.to_string()),
            SniffError::Transport { .. } => Self::network(error.to_string()),
            SniffError::Cancelled => Self::cancelled(),
        }
    }
}
