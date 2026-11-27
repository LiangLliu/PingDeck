//! The stored form of the user's own balance query and the checks it passes before it is saved:
//! the URL, the JSONPaths, the unit, the extra headers and the host rule. The stored form holds
//! header names and secret references only; resolved header values are `#[serde(skip)]`, never
//! printed by `Debug` and never part of a report. Also the key's auth style and the unit names the
//! query and the declarative adapters share.

use serde::{Deserialize, Serialize};
use url::Url;

use spanbay_base::secrets::redact_secrets;
use spanbay_response::response_viewer::{JsonPathFilterOutcomeDto, filter_response_jsonpath};

use super::{BalanceScope, BalanceUnit};

/// Longest unit label of a custom unit.
const MAX_UNIT_LABEL_CHARS: usize = 24;
/// Headers that belong to the transport, never to a balance query.
const FORBIDDEN_HEADERS: [&str; 8] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "cookie",
    "set-cookie",
    "proxy-authorization",
    "accept",
];

/// A header of a custom query: the value lives in the secret store under `secret_ref`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomHeaderRef {
    /// Header name.
    pub name: String,
    /// Secret store reference of the value.
    pub secret_ref: String,
}

/// Resolved header values (secrets). `Debug` prints the names only.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct ResolvedHeaders(
    /// `(name, value)` pairs.
    pub Vec<(String, String)>,
);

impl std::fmt::Debug for ResolvedHeaders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(name, _)| name))
            .finish()
    }
}

/// A saved custom balance query (`llm_providers.custom_balance_json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomBalanceQuery {
    /// `http` or `https` URL; a credential in the URL is refused.
    pub url: String,
    /// Extra headers (names and secret references).
    #[serde(default)]
    pub headers: Vec<CustomHeaderRef>,
    /// How the provider key is injected.
    pub auth: BalanceAuth,
    /// JSONPath of the remaining amount.
    pub remaining_path: String,
    /// JSONPath of the spent amount.
    #[serde(default)]
    pub used_path: Option<String>,
    /// JSONPath of the granted total.
    #[serde(default)]
    pub total_path: Option<String>,
    /// Unit of the numbers.
    pub unit: UnitName,
    /// Whether the number is about the key or the account. Required: a query without a scope
    /// cannot be saved ([`Self::validate`]).
    #[serde(default)]
    pub scope: Option<BalanceScope>,
    /// Label of a custom unit.
    #[serde(default)]
    pub unit_label: Option<String>,
    /// Quota divisor (quota unit only).
    #[serde(default)]
    pub divisor: Option<f64>,
    /// The user confirmed that the host differs from the provider's.
    #[serde(default)]
    pub cross_host: bool,
    /// Header values resolved from the secret store by the caller; never serialised.
    #[serde(skip)]
    pub resolved_headers: ResolvedHeaders,
}

fn path_is_valid(expression: &str) -> bool {
    !matches!(
        filter_response_jsonpath("{}", expression, 2),
        JsonPathFilterOutcomeDto::Error { .. }
    )
}

fn header_name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

impl CustomBalanceQuery {
    /// Checks the query as the form saves it.
    ///
    /// # Errors
    /// The first problem, as text for the form.
    pub fn validate(&self) -> Result<(), String> {
        if self.scope.is_none() {
            return Err("Choose whether the number is for the key or the account".to_owned());
        }
        self.validate_url()?;
        self.validate_paths()?;
        self.validate_unit()?;
        self.validate_headers()
    }

    /// The URL parses as `http(s)` with a host and carries no credential.
    fn validate_url(&self) -> Result<(), String> {
        let url = Url::parse(&self.url).map_err(|_| "The URL is not valid".to_owned())?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("The URL must be http or https".to_owned());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("The URL must not contain a user name or password".to_owned());
        }
        // The path of an endpoint (`/api/biz/account/query-customer-account-report`) may be long;
        // only the query and the fragment are checked for credentials.
        let tail = format!(
            "?{}#{}",
            url.query().unwrap_or_default(),
            url.fragment().unwrap_or_default()
        );
        if redact_secrets(&tail, &[]) != tail {
            return Err("The URL looks like it contains a credential; use a header".to_owned());
        }
        Ok(())
    }

    /// Every path is a valid JSONPath, checked in the order remaining, used, total.
    fn validate_paths(&self) -> Result<(), String> {
        for (name, path) in [
            ("remaining_path", Some(&self.remaining_path)),
            ("used_path", self.used_path.as_ref()),
            ("total_path", self.total_path.as_ref()),
        ] {
            if let Some(path) = path
                && !path_is_valid(path)
            {
                return Err(format!("{name} is not a valid JSONPath"));
            }
        }
        Ok(())
    }

    /// A custom unit has a label of the right length; a divisor is positive and needs quota.
    fn validate_unit(&self) -> Result<(), String> {
        if self.unit == UnitName::Custom
            && !self.unit_label.as_deref().is_some_and(|label| {
                let length = label.trim().chars().count();
                (1..=MAX_UNIT_LABEL_CHARS).contains(&length)
            })
        {
            return Err(format!(
                "A custom unit needs a label of 1 to {MAX_UNIT_LABEL_CHARS} characters"
            ));
        }
        if let Some(divisor) = self.divisor
            && (!divisor.is_finite() || divisor <= 0.0 || self.unit != UnitName::Quota)
        {
            return Err("A divisor must be positive and needs the quota unit".to_owned());
        }
        Ok(())
    }

    /// Every header name is a valid, allowed token named once and not the auth header.
    fn validate_headers(&self) -> Result<(), String> {
        let auth_header = self.auth.header_name();
        let mut seen: Vec<String> = Vec::new();
        for header in &self.headers {
            let lower = header.name.to_ascii_lowercase();
            if !header_name_is_valid(&header.name)
                || FORBIDDEN_HEADERS.contains(&lower.as_str())
                || header.secret_ref.trim().is_empty()
            {
                return Err(format!("Header {} is not allowed", header.name));
            }
            if seen.contains(&lower) || auth_header == Some(lower.as_str()) {
                return Err(format!("Header {} is set twice", header.name));
            }
            seen.push(lower);
        }
        Ok(())
    }

    /// Whether the query leaves the provider's stored endpoint origins (the form must then ask
    /// the user and record `cross_host`). Scheme and port count: another port is another origin.
    #[must_use]
    pub fn is_cross_host(&self, endpoint_origins: &[String]) -> bool {
        let Some(query) = http_origin(&self.url) else {
            return true;
        };
        !endpoint_origins
            .iter()
            .filter_map(|origin| http_origin(origin))
            .any(|origin| origin == query)
    }
}

/// How the key travels in a custom query or a declarative entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalanceAuth {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `x-api-key: <key>`.
    XApiKey,
    /// `Authorization: <key>` with no scheme (Zhipu).
    Raw,
    /// No credential.
    None,
}

impl BalanceAuth {
    /// The credential header for `key`; `None` for [`BalanceAuth::None`].
    pub fn header(self, key: &str) -> Option<(String, String)> {
        match self {
            Self::Bearer => Some(("Authorization".to_owned(), format!("Bearer {key}"))),
            Self::XApiKey => Some(("x-api-key".to_owned(), key.to_owned())),
            Self::Raw => Some(("Authorization".to_owned(), key.to_owned())),
            Self::None => None,
        }
    }

    /// The lower-case name of the credential header, `None` for [`BalanceAuth::None`].
    pub(crate) const fn header_name(self) -> Option<&'static str> {
        match self {
            Self::Bearer | Self::Raw => Some("authorization"),
            Self::XApiKey => Some("x-api-key"),
            Self::None => None,
        }
    }
}

/// A unit name as the fixture and the custom query spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitName {
    /// US dollars.
    #[serde(rename = "USD")]
    Usd,
    /// Chinese yuan.
    #[serde(rename = "CNY")]
    Cny,
    /// Gateway quota units.
    #[serde(rename = "quota")]
    Quota,
    /// A user-labelled unit (custom queries only).
    #[serde(rename = "custom")]
    Custom,
}

impl From<UnitName> for BalanceUnit {
    fn from(name: UnitName) -> Self {
        match name {
            UnitName::Usd => Self::Usd,
            UnitName::Cny => Self::Cny,
            UnitName::Quota => Self::Quota,
            UnitName::Custom => Self::Custom,
        }
    }
}

/// `scheme://host[:port]` of an `http(s)` URL, default ports dropped, host lower-cased; `None`
/// when `input` is not an `http(s)` URL with a host.
#[must_use]
pub fn http_origin(input: &str) -> Option<String> {
    let url = Url::parse(input.trim())
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some());
    url.map(|url| url.origin().ascii_serialization())
}
