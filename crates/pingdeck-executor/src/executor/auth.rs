//! Request authentication: the OAuth 2.0 token request, Digest challenge answering, AWS SigV4 signing and credential masking.

use crate::executor::body::BodyKind;
use crate::executor::client::matching_client_certificate;
use crate::executor::probe::{ConnectProbe, ProbedResolver};
use crate::executor::request::{request_url, resolve_value};
use crate::executor::send::send_with_redirects;
use crate::executor::tls::{client_identity, trust_roots};
use crate::executor::types::{
    AuxiliaryNetwork, ExecutionError, ExecutionHeader, ExecutionResult, ExecutorOptions,
    GuardRefusal, ResolveGuard, truncate_message,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use pingdeck_base::crypto;
use pingdeck_base::secrets::REDACTED;
use pingdeck_base::url_encode::encode_path_segment;
use pingdeck_model::{
    ApiKeyLocation, AuthConfig, KeyValue, OAuth2ClientAuth, Request, VariableScopes,
};
use reqwest::header::{CONTENT_TYPE, HeaderName};
use reqwest::redirect::Policy;
use reqwest::{Method, RequestBuilder};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;
use url::form_urlencoded::Serializer;

/// What a token endpoint answered; the token fields are credentials: never print or log them.
pub struct TokenGrant {
    pub(crate) access_token: String,
    pub(crate) token_type: String,
    pub(crate) scope: String,
    /// Lifetime in seconds as the server stated it.
    pub(crate) expires_in: Option<u64>,
    pub(crate) refresh_token: Option<String>,
}

/// The client credentials of a token request and where they go.
#[derive(Debug, Clone, Copy)]
pub struct OAuthClient<'a> {
    pub auth: OAuth2ClientAuth,
    pub id: &'a str,
    /// Empty for a public client: no `client_secret` field is sent.
    pub secret: &'a str,
}

/// Largest token endpoint response read; a grant is a few hundred bytes.
const MAX_TOKEN_RESPONSE_BYTES: usize = 256 * 1024;

/// Posts `form` to the token endpoint and reads the grant (client credentials, password, code
/// exchange and refresh share it). The form carries credentials, so the request never follows a
/// redirect (a 3xx is an error), never uses the ambient proxy and connects only to addresses
/// `guard` permits for the token host (an IP literal is checked up front, a name by the resolver
/// that pins the connection to the validated addresses).
pub async fn token_request(
    network: &AuxiliaryNetwork,
    guard: &ResolveGuard,
    access_token_url: &str,
    form: &[(&str, &str)],
    client: OAuthClient<'_>,
) -> Result<TokenGrant, ExecutionError> {
    let url = Url::parse(access_token_url).map_err(|error| ExecutionError::OAuth2Token {
        message: format!("invalid token URL: {error}"),
    })?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let port = url.port_or_known_default().unwrap_or(0);
    guard
        .check_literal(&host, port)
        .map_err(|refusal| ExecutionError::GuardRefused { host: refusal.host })?;
    let mut builder = reqwest::Client::builder();
    // The roots and the client certificate every client of the request uses: a token endpoint
    // behind mTLS or a private CA is reached like the request itself.
    if !network.ca_bundle_path.is_empty() || !network.tls.certificates.is_empty() {
        let (tls, ca_bundle_path, tls_host) = (
            network.tls.clone(),
            network.ca_bundle_path.clone(),
            host.clone(),
        );
        let (roots, identity) = tokio::task::spawn_blocking(move || {
            let roots = trust_roots(&ca_bundle_path, &tls.certificates)?;
            let identity = matching_client_certificate(&tls.certificates, &tls_host, port)
                .map(|certificate| client_identity(certificate, &tls.passphrases))
                .transpose()?;
            Ok::<_, String>((roots, identity))
        })
        .await
        .map_err(|error| ExecutionError::ClientBuild(format!("client task failed: {error}")))?
        .map_err(ExecutionError::ClientBuild)?;
        for root in roots {
            builder = builder.add_root_certificate(root);
        }
        if let Some(identity) = identity {
            builder = builder.identity(identity);
        }
    }
    builder = builder
        .timeout(network.timeout)
        .redirect(Policy::none())
        .danger_accept_invalid_certs(!network.verify_tls)
        .dns_resolver(Arc::new(ProbedResolver::guarded(
            Arc::new(ConnectProbe::default()),
            guard.clone(),
            port,
        )));
    builder = match network.explicit_proxy() {
        Some(proxy_url) => builder.proxy(
            reqwest::Proxy::all(proxy_url)
                .map_err(|error| ExecutionError::ClientBuild(error.to_string()))?,
        ),
        None => builder.no_proxy(),
    };
    let http = builder
        .build()
        .map_err(|error| ExecutionError::ClientBuild(error.to_string()))?;

    // `Serializer` is not `Send`, so it lives only inside this block, not across the await.
    let (encoded, basic) = {
        let mut serializer = Serializer::new(String::new());
        serializer.extend_pairs(form.iter().copied());
        let basic = match client.auth {
            OAuth2ClientAuth::Body => {
                serializer.append_pair("client_id", client.id);
                if !client.secret.is_empty() {
                    serializer.append_pair("client_secret", client.secret);
                }
                None
            }
            // RFC 6749 §2.3.1: both parts are form-encoded before the Basic encoding.
            OAuth2ClientAuth::Basic => {
                let encode = |value: &str| -> String {
                    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
                };
                let pair = format!("{}:{}", encode(client.id), encode(client.secret));
                Some(format!("Basic {}", BASE64_STANDARD.encode(pair)))
            }
        };
        (serializer.finish(), basic)
    };
    let mut post = http.post(url);
    if let Some(basic) = basic {
        post = post.header(reqwest::header::AUTHORIZATION, basic);
    }

    let mut response = post
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(reqwest::header::ACCEPT, "application/json")
        .body(encoded)
        .send()
        .await
        .map_err(|error| match GuardRefusal::find(&error) {
            Some(refusal) => ExecutionError::GuardRefused {
                host: refusal.host.clone(),
            },
            // The token URL may carry credentials in its query: never echo it.
            None => ExecutionError::OAuth2Token {
                message: error.without_url().to_string(),
            },
        })?;

    let status = response.status();
    if status.is_redirection() {
        return Err(ExecutionError::OAuth2Token {
            message: format!("token endpoint answered {status}: redirects are not followed"),
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ExecutionError::OAuth2Token {
            message: error.without_url().to_string(),
        })?
    {
        if bytes.len() + chunk.len() > MAX_TOKEN_RESPONSE_BYTES {
            return Err(ExecutionError::OAuth2Token {
                message: "token endpoint response is larger than 256 KiB".to_owned(),
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    let body = String::from_utf8_lossy(&bytes);
    if !status.is_success() {
        return Err(ExecutionError::OAuth2Token {
            message: format!(
                "token endpoint returned {status}: {}",
                truncate_message(&body, 500)
            ),
        });
    }

    let payload =
        serde_json::from_str::<Value>(&body).map_err(|error| ExecutionError::OAuth2Token {
            message: format!("token endpoint returned invalid JSON: {error}"),
        })?;
    let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_owned);
    let access_token = text("access_token")
        .filter(|token| !token.is_empty())
        .ok_or_else(|| ExecutionError::OAuth2Token {
            message: "token endpoint response is missing access_token".to_owned(),
        })?;
    let expires_in = payload.get("expires_in").and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    });

    Ok(TokenGrant {
        access_token,
        token_type: text("token_type").unwrap_or_else(|| "Bearer".to_owned()),
        scope: text("scope").unwrap_or_default(),
        expires_in,
        refresh_token: text("refresh_token").filter(|token| !token.is_empty()),
    })
}

/// Appends the API key to the URL query when the auth config places it there.
pub(super) fn append_api_key_query(
    url: &mut Url,
    auth: &AuthConfig,
    scopes: &VariableScopes,
) -> Result<(), ExecutionError> {
    let AuthConfig::ApiKey {
        key,
        value_ref,
        location: ApiKeyLocation::Query,
    } = auth
    else {
        return Ok(());
    };

    let key = resolve_value(key, scopes)?;
    let value = resolve_value(value_ref, scopes)?;
    if key.is_empty() || value.is_empty() {
        return Ok(());
    }
    if url.query_pairs().any(|(existing, _)| existing == key) {
        return Ok(());
    }

    url.query_pairs_mut().append_pair(&key, &value);

    Ok(())
}

/// Resolves saved auth into request headers. An enabled explicit header of the same name wins.
pub fn auth_header_pairs(
    auth: &AuthConfig,
    scopes: &VariableScopes,
    explicit_header_names: &[String],
) -> Result<Vec<(String, String)>, ExecutionError> {
    let has_explicit_header = |name: &str| {
        explicit_header_names
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(name))
    };
    let mut pairs = Vec::new();

    match auth {
        AuthConfig::Inherit | AuthConfig::None => {}
        AuthConfig::OAuth2 { .. } => {
            return Err(ExecutionError::Network {
                message: "OAuth 2.0 token was not resolved before the request".to_owned(),
                attempt_count: 1,
                connect_phase: false,
                cause: None,
            });
        }
        AuthConfig::Bearer { token_ref } => {
            let token = resolve_value(token_ref, scopes)?;
            if !token.is_empty() && !has_explicit_header("authorization") {
                pairs.push(("Authorization".to_owned(), format!("Bearer {token}")));
            }
        }
        AuthConfig::Basic {
            username,
            password_ref,
        } => {
            let username = resolve_value(username, scopes)?;
            let password = resolve_value(password_ref, scopes)?;
            if !username.is_empty() && !password.is_empty() && !has_explicit_header("authorization")
            {
                pairs.push((
                    "Authorization".to_owned(),
                    format!(
                        "Basic {}",
                        BASE64_STANDARD.encode(format!("{username}:{password}"))
                    ),
                ));
            }
        }
        AuthConfig::ApiKey {
            key,
            value_ref,
            location: ApiKeyLocation::Header,
        } => {
            let key = resolve_value(key, scopes)?;
            let value = resolve_value(value_ref, scopes)?;
            if !key.is_empty() && !value.is_empty() && !has_explicit_header(&key) {
                pairs.push((key, value));
            }
        }
        AuthConfig::ApiKey {
            location: ApiKeyLocation::Query,
            ..
        } => {}
        // Digest answers a 401 challenge and SigV4 signs the prepared request; neither
        // produces a header here.
        AuthConfig::Digest { .. } | AuthConfig::AwsSignatureV4 { .. } => {}
    }

    Ok(pairs)
}

/// Whether `key` names the `Authorization` header (case-insensitive).
pub(super) fn is_authorization_header(key: &str) -> bool {
    key.eq_ignore_ascii_case("authorization") || key.eq_ignore_ascii_case("proxy-authorization")
}

/// Captured credentials keep only their scheme (`Basic ••••••`); cookies are fully masked.
pub(super) fn mask_credential_header(value: &str) -> String {
    match value.split_once(' ') {
        Some((scheme, _)) if !scheme.is_empty() && !scheme.contains('=') => {
            format!("{scheme} {REDACTED}")
        }
        _ => REDACTED.to_owned(),
    }
}

pub(super) struct AwsCredentials {
    pub(super) access_key: String,
    pub(super) secret_key: String,
    pub(super) region: String,
    pub(super) service: String,
    pub(super) session_token: Option<String>,
}

/// Signs the prepared request with AWS Signature Version 4.
///
/// Runs after the body is attached and before sending, so the payload hash and
/// the canonical headers describe what actually goes on the wire. Redirects are
/// signed again for each hop rather than reusing the previous hop's signature.
pub(super) fn apply_aws_signature(
    mut builder: RequestBuilder,
    request: &Request,
    scopes: &VariableScopes,
    url: &Url,
    method: &Method,
    payload_hash: &str,
    mut sent_headers: Vec<ExecutionHeader>,
) -> Result<(RequestBuilder, Vec<ExecutionHeader>), ExecutionError> {
    let AuthConfig::AwsSignatureV4 {
        access_key,
        secret_key_ref,
        region,
        service,
        session_token_ref,
    } = &request.auth
    else {
        return Ok((builder, sent_headers));
    };

    let credentials = AwsCredentials {
        access_key: resolve_value(access_key, scopes)?,
        // A secret reference that cannot be resolved must not block the send; the
        // signature is then invalid and the server rejects the request.
        secret_key: resolve_value(secret_key_ref.as_deref().unwrap_or_default(), scopes)
            .unwrap_or_default(),
        region: resolve_value(region, scopes)?,
        service: resolve_value(service, scopes)?,
        session_token: session_token_ref
            .as_deref()
            .map(|token| resolve_value(token, scopes).unwrap_or_default())
            .filter(|token| !token.is_empty()),
    };
    let amz_date = amz_timestamp(SystemTime::now());

    for (key, value) in aws_sigv4_headers(
        &credentials,
        method,
        url,
        &sent_headers,
        payload_hash,
        &amz_date,
    ) {
        let header_name = HeaderName::from_bytes(key.as_bytes()).map_err(|error| {
            ExecutionError::InvalidHeader {
                key: key.clone(),
                message: error.to_string(),
            }
        })?;
        builder = builder.header(header_name, value.clone());
        sent_headers.push(ExecutionHeader { key, value });
    }

    Ok((builder, sent_headers))
}

/// Computes the SigV4 headers (`Authorization`, `x-amz-*`) for the canonical request.
pub(super) fn aws_sigv4_headers(
    credentials: &AwsCredentials,
    method: &Method,
    url: &Url,
    headers: &[ExecutionHeader],
    payload_hash: &str,
    amz_date: &str,
) -> Vec<(String, String)> {
    let mut canonical_headers = BTreeMap::<String, String>::new();
    canonical_headers.insert("host".to_owned(), canonical_host(url));
    canonical_headers.insert("x-amz-date".to_owned(), amz_date.to_owned());
    // Temporary credentials are only accepted when the token is part of the
    // signed headers, not merely attached to the request.
    if let Some(token) = &credentials.session_token {
        canonical_headers.insert(
            "x-amz-security-token".to_owned(),
            normalize_header_value(token),
        );
    }
    for header in headers {
        let name = header.key.trim().to_ascii_lowercase();
        if name.is_empty()
            || matches!(
                name.as_str(),
                "host"
                    | "authorization"
                    | "proxy-authorization"
                    | "content-length"
                    | "x-amz-date"
                    | "x-amz-content-sha256"
                    | "x-amz-security-token"
            )
            || canonical_headers.contains_key(&name)
        {
            continue;
        }
        canonical_headers.insert(name, normalize_header_value(&header.value));
    }

    let signed_headers = canonical_headers
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join(";");
    let canonical_header_block = canonical_headers
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect::<String>();
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.as_str(),
        canonical_uri(url),
        canonical_query(url),
        canonical_header_block,
        signed_headers,
        payload_hash,
    );

    let date = amz_date.get(..8).unwrap_or(amz_date);
    let scope = format!(
        "{date}/{}/{}/aws4_request",
        credentials.region, credentials.service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        crypto::sha256_hex(canonical_request.as_bytes())
    );
    let mut signing_key = crypto::hmac_sha256(
        format!("AWS4{}", credentials.secret_key).as_bytes(),
        date.as_bytes(),
    );
    signing_key = crypto::hmac_sha256(&signing_key, credentials.region.as_bytes());
    signing_key = crypto::hmac_sha256(&signing_key, credentials.service.as_bytes());
    signing_key = crypto::hmac_sha256(&signing_key, b"aws4_request");
    let signature = crypto::hex_lower(&crypto::hmac_sha256(
        &signing_key,
        string_to_sign.as_bytes(),
    ));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key
    );
    let mut pairs = vec![
        ("X-Amz-Date".to_owned(), amz_date.to_owned()),
        ("X-Amz-Content-Sha256".to_owned(), payload_hash.to_owned()),
    ];
    if let Some(token) = &credentials.session_token {
        pairs.push(("X-Amz-Security-Token".to_owned(), token.clone()));
    }
    pairs.push(("Authorization".to_owned(), authorization));
    pairs
}

fn canonical_host(url: &Url) -> String {
    match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_owned(),
    }
}

/// Percent-encodes a URI path the way SigV4 requires: every byte except
/// unreserved characters and `/`, with existing `%XX` escapes kept and
/// uppercased (they are already escapes because the URL parser produced them).
/// The extra double-encoding some non-S3 services tolerate is not applied.
pub(super) fn canonical_uri(url: &Url) -> String {
    let path = match url.path() {
        "" => "/",
        path => path,
    };
    let bytes = path.as_bytes();
    let mut encoded = String::with_capacity(path.len());
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%'
            && index + 2 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit()
        {
            encoded.push('%');
            encoded.push(char::from(bytes[index + 1].to_ascii_uppercase()));
            encoded.push(char::from(bytes[index + 2].to_ascii_uppercase()));
            index += 3;
            continue;
        }
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
        index += 1;
    }

    encoded
}

/// SigV4 canonical query string: sorted, percent-encoded key/value pairs.
pub(super) fn canonical_query(url: &Url) -> String {
    let mut pairs = url
        .query_pairs()
        .map(|(key, value)| (encode_path_segment(&key), encode_path_segment(&value)))
        .collect::<Vec<_>>();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn normalize_header_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Formats `now` as the SigV4 `YYYYMMDDTHHMMSSZ` timestamp.
pub(super) fn amz_timestamp(now: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(now)
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DigestAlgorithm {
    Md5,
    Sha256,
}

impl DigestAlgorithm {
    fn hash(self, data: &[u8]) -> String {
        match self {
            Self::Md5 => crypto::hex_lower(&crypto::md5(data)),
            Self::Sha256 => crypto::sha256_hex(data),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Sha256 => "SHA-256",
        }
    }
}

/// The subset of a `WWW-Authenticate: Digest` challenge PingDeck answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DigestChallenge {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    algorithm: DigestAlgorithm,
    qop: Vec<String>,
}

impl DigestChallenge {
    /// Parses a `WWW-Authenticate: Digest` header value into a challenge.
    pub(super) fn parse(value: &str) -> Option<Self> {
        let sanitized: String = value
            .chars()
            .map(|character| if character.is_ascii() { character } else { '?' })
            .collect();
        let (scheme, parameters) = sanitized.trim().split_once(char::is_whitespace)?;
        if !scheme.eq_ignore_ascii_case("digest") {
            return None;
        }
        let parameters = parse_auth_parameters(parameters);
        let realm = parameters.get("realm")?.clone();
        let nonce = parameters.get("nonce")?.clone();
        let algorithm = match parameters
            .get("algorithm")
            .map(|algorithm| algorithm.trim().to_ascii_lowercase())
        {
            None => DigestAlgorithm::Md5,
            Some(algorithm) if algorithm == "md5" => DigestAlgorithm::Md5,
            Some(algorithm) if algorithm == "sha-256" => DigestAlgorithm::Sha256,
            // `-sess` variants need a different HA1 and are not answered.
            Some(_) => return None,
        };
        let qop = parameters
            .get("qop")
            .map(|qop| {
                qop.split(',')
                    .map(|token| token.trim().to_ascii_lowercase())
                    .filter(|token| !token.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            realm,
            nonce,
            opaque: parameters.get("opaque").cloned(),
            algorithm,
            qop,
        })
    }

    fn uses_auth_qop(&self) -> bool {
        self.qop.iter().any(|token| token == "auth")
    }
}

/// Parses `key=value` pairs with optional quoting, as used by auth challenges.
fn parse_auth_parameters(input: &str) -> HashMap<String, String> {
    let bytes = input.as_bytes();
    let mut parameters = HashMap::new();
    let mut index = 0;

    while index < bytes.len() {
        while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b',') {
            index += 1;
        }
        let key_start = index;
        while index < bytes.len() && bytes[index] != b'=' && bytes[index] != b',' {
            index += 1;
        }
        let key = input[key_start..index].trim().to_ascii_lowercase();
        if index >= bytes.len() || bytes[index] != b'=' {
            continue;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }

        let mut value = String::new();
        if index < bytes.len() && bytes[index] == b'"' {
            index += 1;
            while index < bytes.len() && bytes[index] != b'"' {
                if bytes[index] == b'\\' && index + 1 < bytes.len() {
                    index += 1;
                }
                value.push(char::from(bytes[index]));
                index += 1;
            }
            if index < bytes.len() {
                index += 1;
            }
        } else {
            let value_start = index;
            while index < bytes.len() && bytes[index] != b',' {
                index += 1;
            }
            value = input[value_start..index].trim().to_owned();
        }

        if !key.is_empty() {
            parameters.insert(key, value);
        }
    }

    parameters
}

/// Builds the `Authorization: Digest ...` header value answering `challenge` (RFC 7616).
pub(super) fn digest_authorization(
    challenge: &DigestChallenge,
    username: &str,
    password: &str,
    method: &Method,
    uri: &str,
    cnonce: &str,
    nonce_count: &str,
) -> String {
    let hash = |data: &str| challenge.algorithm.hash(data.as_bytes());
    let ha1 = hash(&format!("{username}:{}:{password}", challenge.realm));
    let ha2 = hash(&format!("{}:{uri}", method.as_str()));
    let (response, qop_part) = if challenge.uses_auth_qop() {
        (
            hash(&format!(
                "{ha1}:{}:{nonce_count}:{cnonce}:auth:{ha2}",
                challenge.nonce
            )),
            format!(", qop=auth, nc={nonce_count}, cnonce=\"{cnonce}\""),
        )
    } else {
        // ponytail: only qop=auth is answered; a server offering only auth-int falls
        // back to the legacy RFC 2069 response, which it will reject.
        (
            hash(&format!("{ha1}:{}:{ha2}", challenge.nonce)),
            String::new(),
        )
    };
    let opaque = challenge
        .opaque
        .as_ref()
        .map(|opaque| format!(", opaque=\"{opaque}\""))
        .unwrap_or_default();

    format!(
        "Digest username=\"{username}\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", algorithm={}, response=\"{response}\"{opaque}{qop_part}",
        challenge.realm,
        challenge.nonce,
        challenge.algorithm.label(),
    )
}

fn request_target(url: &Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    }
}

fn digest_nonce_count() -> String {
    static NONCE_COUNT: AtomicU64 = AtomicU64::new(0);
    format!("{:08x}", NONCE_COUNT.fetch_add(1, Ordering::Relaxed) + 1)
}

fn digest_cnonce() -> String {
    static CNONCE_COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let seed = format!(
        "{nanos}:{}:{}",
        CNONCE_COUNT.fetch_add(1, Ordering::Relaxed),
        std::process::id()
    );
    crypto::hex_lower(&crypto::sha256(seed.as_bytes())[..8])
}

/// Answers a Digest 401 challenge exactly once with an authenticated retry.
pub(super) async fn retry_with_digest_auth(
    request: &Request,
    scopes: &VariableScopes,
    options: &ExecutorOptions,
    method: Method,
    url: Url,
    body_kind: BodyKind,
    result: ExecutionResult,
) -> Result<ExecutionResult, ExecutionError> {
    if result.status_code != 401 {
        return Ok(result);
    }
    let AuthConfig::Digest {
        username,
        password_ref,
    } = &request.auth
    else {
        return Ok(result);
    };
    // A response may carry several challenges (for example `Basic` first and
    // `Digest` second), so every WWW-Authenticate header is inspected.
    let Some(challenge) = result
        .headers
        .iter()
        .filter(|header| header.key.eq_ignore_ascii_case("www-authenticate"))
        .find_map(|header| DigestChallenge::parse(&header.value))
    else {
        return Ok(result);
    };

    let username = resolve_value(username, scopes)?;
    let password = resolve_value(password_ref.as_deref().unwrap_or_default(), scopes)?;
    let authorization = digest_authorization(
        &challenge,
        &username,
        &password,
        &method,
        &request_target(&url),
        &digest_cnonce(),
        &digest_nonce_count(),
    );

    let mut retry = request.clone();
    retry.auth = AuthConfig::None;
    // The authenticated retry replaces any explicit Authorization header instead
    // of sending two competing header lines.
    retry
        .headers
        .retain(|header| !header.key.eq_ignore_ascii_case("authorization"));
    retry.headers.push(KeyValue {
        key: "Authorization".to_owned(),
        value: authorization,
        enabled: true,
        description: None,
    });
    let retry_url = request_url(&retry, scopes)?;

    send_with_redirects(&retry, scopes, options, method, retry_url, body_kind).await
}
