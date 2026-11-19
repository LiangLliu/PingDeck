use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{TlsCertificateId, WorkspaceId};

/// Authentication scheme and credential references for a request, folder, or collection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthConfig {
    #[default]
    Inherit,
    None,
    Bearer {
        token_ref: String,
    },
    Basic {
        username: String,
        password_ref: String,
    },
    ApiKey {
        key: String,
        value_ref: String,
        location: ApiKeyLocation,
    },
    OAuth2 {
        grant_type: OAuth2GrantType,
        access_token_url: String,
        client_id: String,
        client_secret_ref: String,
        username: String,
        password_ref: String,
        scope: String,
        /// Authorization endpoint of the `AuthorizationCodePkce` grant (empty for the others).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        authorization_url: String,
        /// Loopback port the `AuthorizationCodePkce` redirect listens on; `None` picks a free one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redirect_port: Option<u16>,
        /// How the token request carries the client id and secret.
        #[serde(default, skip_serializing_if = "OAuth2ClientAuth::is_body")]
        client_auth: OAuth2ClientAuth,
        /// Extra parameters (`audience`, `resource`, …) sent with every token request and on
        /// the authorization URL; enabled rows only, `{{variables}}` resolved. A row named like a
        /// field the grant sets itself (`grant_type`, `client_id`, `code`, …) is not sent.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extra_params: Vec<KeyValue>,
    },
    Digest {
        username: String,
        password_ref: Option<String>,
    },
    AwsSignatureV4 {
        access_key: String,
        secret_key_ref: Option<String>,
        region: String,
        service: String,
        #[serde(default)]
        session_token_ref: Option<String>,
    },
}

/// Where an OAuth 2.0 token request puts the client credentials (RFC 6749 §2.3.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuth2ClientAuth {
    /// `client_id` and `client_secret` as form fields of the body.
    #[default]
    Body,
    /// An `Authorization: Basic` header; the body carries neither.
    Basic,
}

impl OAuth2ClientAuth {
    #[must_use]
    pub fn is_body(&self) -> bool {
        *self == Self::Body
    }
}

/// OAuth 2.0 grant type used to fetch an access token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OAuth2GrantType {
    ClientCredentials,
    Password,
    /// Authorization code with PKCE (S256) and a loopback redirect.
    AuthorizationCodePkce,
}

/// Where an API key is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiKeyLocation {
    Header,
    Query,
}

/// A key/value pair (header, query, or path variable) that can be toggled on or off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
    pub enabled: bool,
    /// Optional note shown in the editor's Description column; carried through Postman and sync files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A multipart form part carrying either a text value or a file path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormPart {
    pub key: String,
    pub value: String,
    pub file_path: Option<String>,
    pub enabled: bool,
}

/// Preferred HTTP version for a request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HttpVersionPreference {
    #[default]
    Auto,
    Http10,
    Http11,
    Http2,
}

/// A TLS protocol version that can be disabled for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TlsVersionPreference {
    Tls12,
    Tls13,
}

/// Highest number of redirect hops a request may follow; larger
/// [`NetworkOptions::max_redirects`] values are clamped to this.
pub const MAX_REDIRECT_HOPS: u32 = 50;

/// Per-request transport settings: timeout, redirects, retries, cookies, proxy, and TLS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkOptions {
    /// Idle timeout: the longest the connect, the wait for the response headers and every gap
    /// between two body chunks may take. A stream that keeps sending (SSE, NDJSON) never hits it.
    pub timeout_ms: u64,
    /// Hard limit on the whole exchange, every redirect hop and the body included; `None` sets
    /// none. When it fires after the response headers arrived, the partial response is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_timeout_ms: Option<u64>,
    pub follow_redirects: bool,
    pub retry_count: u8,
    pub send_cookies: bool,
    #[serde(default = "default_true")]
    pub verify_tls: bool,
    /// The request's own proxy. Serialized masked (`http://user:****@host`): the password lives in
    /// the secret store (see [`crate::proxy_url`]) and only the in-memory copy carries it.
    #[serde(
        default,
        serialize_with = "pingdeck_base::proxy_url::serialize_masked_option"
    )]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub http_version: HttpVersionPreference,
    #[serde(default)]
    pub follow_original_method: bool,
    #[serde(default)]
    pub follow_auth_header: bool,
    #[serde(default)]
    pub remove_referer_on_redirect: bool,
    #[serde(default = "default_true")]
    pub encode_url_automatically: bool,
    #[serde(default = "default_max_redirects")]
    pub max_redirects: u32,
    #[serde(default)]
    pub disabled_tls_versions: Vec<TlsVersionPreference>,
    /// Fields that follow the global network settings when the request is sent, instead of this
    /// request's own value. Empty (and absent from the JSON) means every field is the request's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherit: Vec<NetworkField>,
}

/// A request network option that has a global default in Settings › Requests / Network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkField {
    TimeoutMs,
    RetryCount,
    HttpVersion,
    FollowRedirects,
    MaxRedirects,
    VerifyTls,
    ProxyUrl,
    /// A field name written by a newer version; ignored.
    #[serde(other)]
    Unknown,
}

impl NetworkField {
    /// Every field with a global default, in Settings order.
    pub const ALL: [Self; 7] = [
        Self::TimeoutMs,
        Self::RetryCount,
        Self::HttpVersion,
        Self::FollowRedirects,
        Self::MaxRedirects,
        Self::VerifyTls,
        Self::ProxyUrl,
    ];
}

fn default_true() -> bool {
    true
}

fn default_max_redirects() -> u32 {
    10
}

impl Default for NetworkOptions {
    fn default() -> Self {
        Self {
            timeout_ms: 30_000,
            total_timeout_ms: None,
            follow_redirects: true,
            retry_count: 0,
            send_cookies: true,
            verify_tls: true,
            proxy_url: None,
            http_version: HttpVersionPreference::default(),
            follow_original_method: false,
            follow_auth_header: false,
            remove_referer_on_redirect: false,
            encode_url_automatically: true,
            max_redirects: default_max_redirects(),
            disabled_tls_versions: Vec::new(),
            inherit: Vec::new(),
        }
    }
}

impl NetworkOptions {
    /// Redirect hops this request may follow, clamped to [`MAX_REDIRECT_HOPS`].
    #[must_use]
    pub fn max_redirect_hops(&self) -> u32 {
        if self.follow_redirects {
            self.max_redirects.min(MAX_REDIRECT_HOPS)
        } else {
            0
        }
    }
}

/// Role of a stored TLS certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TlsCertificateKind {
    Ca,
    Client,
}

/// Reason a TLS certificate configuration failed validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TlsCertificateError {
    #[error("certificate path cannot be blank")]
    BlankCertPath,
    #[error("client certificate requires a private key path (or a .p12/.pfx bundle)")]
    MissingKeyPath,
    #[error("client certificate requires a host pattern")]
    MissingHostPattern,
}

/// A CA or client TLS certificate configuration scoped to a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsCertificate {
    pub id: TlsCertificateId,
    pub workspace_id: WorkspaceId,
    pub kind: TlsCertificateKind,
    /// Empty or `*` for a CA certificate; `api.example.com` or
    /// `*.internal.corp:8443` for a client certificate.
    pub host_pattern: String,
    pub cert_path: String,
    #[serde(default)]
    pub key_path: Option<String>,
    #[serde(default)]
    pub passphrase_ref: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl TlsCertificate {
    /// Checks that the certificate path is set and that client certificates have a key and host.
    pub fn validate(&self) -> Result<(), TlsCertificateError> {
        if self.cert_path.trim().is_empty() {
            return Err(TlsCertificateError::BlankCertPath);
        }
        if self.kind == TlsCertificateKind::Client {
            // A PKCS#12 bundle (`.p12`, `.pfx`) carries its own key.
            let lower = self.cert_path.trim().to_ascii_lowercase();
            let bundle = lower.ends_with(".p12") || lower.ends_with(".pfx");
            if !bundle
                && self
                    .key_path
                    .as_deref()
                    .is_none_or(|path| path.trim().is_empty())
            {
                return Err(TlsCertificateError::MissingKeyPath);
            }
            if self.host_pattern.trim().is_empty() {
                return Err(TlsCertificateError::MissingHostPattern);
            }
        }
        Ok(())
    }
}
