//! What a balance check is asked about and what it answers: [`BalanceTarget`] (origin, family,
//! credentials, harvested facts, request budget), [`BalanceOutcome`] and the origin helpers every
//! adapter shares.

use url::Url;

use crate::llm::wire::GatewayFamily;

use super::BalanceReport;
use super::custom::CustomBalanceQuery;
use super::error::{BalanceError, BalanceErrorKind};
use super::fetch::BalanceAuth;
pub(super) use pingdeck_llm_domain::llm::balance_state::http_origin;

/// One balance check.
#[derive(Clone)]
pub struct BalanceTarget<'a> {
    /// `scheme://host[:port]` of the provider, no path. Every adapter path is relative to it
    /// (never to the OpenAI mount).
    pub origin: &'a str,
    /// Origins of the provider's stored endpoints. The key is sent to one of these only (the
    /// custom query of a confirmed cross-host is the one exception), never to a host named in a
    /// response body. `origin` must be one of them.
    pub endpoint_origins: &'a [String],
    /// The detected gateway family.
    pub family: GatewayFamily,
    /// The preset the provider was created from.
    pub preset_id: Option<&'a str>,
    /// The model key, plaintext, caller-resolved; never logged.
    pub api_key: &'a str,
    /// Console access token or JWT.
    pub management_token: Option<&'a str>,
    /// Numeric console user id.
    pub api_user_id: Option<&'a str>,
    /// `quota_per_unit` harvested by the sniffer or stored on the provider.
    pub quota_per_unit: Option<f64>,
    /// `display_in_currency` harvested by the sniffer or stored on the provider.
    pub display_in_currency: Option<bool>,
    /// Whether loopback / private targets are allowed.
    pub allow_private: bool,
    /// The saved custom query; it wins over every adapter.
    pub custom: Option<&'a CustomBalanceQuery>,
    /// Most HTTP requests the check may issue.
    pub request_budget: u32,
}

/// Credentials never appear in `Debug`.
impl std::fmt::Debug for BalanceTarget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BalanceTarget")
            .field("origin", &self.origin)
            .field("endpoint_origins", &self.endpoint_origins)
            .field("family", &self.family)
            .field("preset_id", &self.preset_id)
            .field("has_key", &!self.api_key.is_empty())
            .field("has_management_token", &self.management_token.is_some())
            .field("quota_per_unit", &self.quota_per_unit)
            .field("display_in_currency", &self.display_in_currency)
            .field("allow_private", &self.allow_private)
            .field("has_custom", &self.custom.is_some())
            .field("request_budget", &self.request_budget)
            .finish()
    }
}

impl BalanceTarget<'_> {
    /// The normalised origins the key may be sent to; entries that are not `http(s)` URLs are
    /// dropped.
    pub(super) fn allowed_origins(&self) -> Vec<String> {
        let mut origins: Vec<String> = Vec::new();
        for origin in self.endpoint_origins {
            if let Ok(origin) = normalise_origin(origin)
                && !origins.contains(&origin)
            {
                origins.push(origin);
            }
        }
        origins
    }
}

/// The answer of [`super::query_balance`].
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "one outcome exists per check; the contract fixes Report { report, requests_used }"
)]
pub enum BalanceOutcome {
    /// A reading.
    Report {
        /// The reading.
        report: BalanceReport,
        /// HTTP requests issued.
        requests_used: u32,
    },
    /// No adapter and no custom query: "No balance API". Nothing was sent.
    NoAdapter,
    /// The check failed.
    Failed {
        /// Why.
        error: BalanceError,
        /// HTTP requests issued.
        requests_used: u32,
    },
}

/// `scheme://host[:port]` of an `http(s)` URL, default ports dropped, host lower-cased.
///
/// # Errors
/// [`BalanceErrorKind::Blocked`] when `input` is not an `http(s)` URL with a host.
pub(super) fn normalise_origin(input: &str) -> Result<String, BalanceError> {
    http_origin(input).ok_or_else(|| {
        BalanceError::new(
            BalanceErrorKind::Blocked,
            "The address is not an http(s) URL",
        )
    })
}

/// The lower-case host of a URL.
pub(super) fn host_of(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

/// The key to send for `auth`; `auth = none` needs none.
///
/// # Errors
/// [`BalanceErrorKind::Auth`] when a key is needed and the target has none.
pub(super) fn require_key<'a>(
    target: &BalanceTarget<'a>,
    auth: BalanceAuth,
) -> Result<&'a str, BalanceError> {
    if auth == BalanceAuth::None {
        return Ok("");
    }
    if target.api_key.is_empty() {
        return Err(BalanceError::new(
            BalanceErrorKind::Auth,
            "There is no key to check the balance with",
        ));
    }
    Ok(target.api_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> BalanceTarget<'static> {
        BalanceTarget {
            origin: "https://gw.example",
            endpoint_origins: &[],
            family: GatewayFamily::NewApi,
            preset_id: None,
            api_key: "sk-secret-secret-secret-1234",
            management_token: None,
            api_user_id: None,
            quota_per_unit: None,
            display_in_currency: None,
            allow_private: false,
            custom: None,
            request_budget: 4,
        }
    }

    #[test]
    fn only_stored_endpoint_origins_are_allowed_after_normalising() {
        let stored = vec![
            "HTTPS://Gw.Example:443/v1".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "garbage".to_owned(),
        ];
        let with_stored = BalanceTarget {
            endpoint_origins: &stored,
            ..target()
        };
        assert_eq!(
            with_stored.allowed_origins(),
            ["https://gw.example", "http://127.0.0.1:8080"]
        );
        assert!(
            BalanceTarget {
                endpoint_origins: &[],
                ..target()
            }
            .allowed_origins()
            .is_empty()
        );
    }

    #[test]
    fn origins_are_normalised_and_non_http_is_refused() {
        assert_eq!(
            normalise_origin("HTTPS://Relay.Acme.dev:443/v1?x=1")
                .ok()
                .as_deref(),
            Some("https://relay.acme.dev")
        );
        assert_eq!(
            normalise_origin("http://127.0.0.1:8080").ok().as_deref(),
            Some("http://127.0.0.1:8080")
        );
        for bad in ["ftp://x.test", "not a url", "file:///etc/passwd", ""] {
            assert_eq!(
                normalise_origin(bad).err().map(|e| e.kind),
                Some(BalanceErrorKind::Blocked)
            );
        }
    }

    #[test]
    fn debug_never_prints_credentials() {
        let mut target = target();
        target.management_token = Some("mgmt-token-value-9999");
        let text = format!("{target:?}");
        assert!(!text.contains("sk-secret"));
        assert!(!text.contains("mgmt-token"));
    }
}
