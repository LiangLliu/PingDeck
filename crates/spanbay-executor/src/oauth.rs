//! OAuth 2.0 token card: where fetched tokens live and the PKCE loopback redirect.
//!
//! Single responsibility: keys, storage and the browser round trip, with no application state.
//! Tokens live in the [`SecretStore`] under `oauth.<workspace>.<digest>.access` / `.refresh`
//! (credentials, so [`spanbay_base::secrets::redact_secrets`] scrubs them like every stored secret) and
//! `.meta` (type, scope, expiry and the last characters: nothing that authorizes a request). No
//! DTO, log line or history row ever carries a token.

use std::sync::Arc;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::executor::{StoredOAuthTokens, TokenGrant};
use spanbay_base::cancel::{CancelToken, until_cancelled};
use spanbay_base::crypto;
use spanbay_base::error::AppError;
use spanbay_base::secrets::SecretStore;
use spanbay_model::OAuth2GrantType;

/// A token is treated as expired this long before its stated end, so a send does not race it.
const EXPIRY_SKEW_MS: i64 = 30_000;
/// How long the loopback listener waits for the browser (the user may be signing in).
pub const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(300);
/// How long one loopback connection may take to send its request line and get its answer.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Pause after a failed `accept` (file descriptors exhausted), so the loop does not spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);
/// The path the loopback redirect URI uses.
const CALLBACK_PATH: &str = "/callback";

/// The secret-store prefix of one token: the same configuration always maps to the same key, so
/// the card and the send path agree without sharing state. The resolved extra parameters
/// (`audience`, `resource`) are part of it, since another audience is another token; a
/// configuration without any keeps the key it always had.
#[must_use]
pub fn token_key(
    workspace_id: &str,
    grant: OAuth2GrantType,
    token_url: &str,
    client_id: &str,
    scope: &str,
    username: &str,
    extra_params: &[(String, String)],
) -> String {
    let grant = match grant {
        OAuth2GrantType::ClientCredentials => "client_credentials",
        OAuth2GrantType::Password => "password",
        OAuth2GrantType::AuthorizationCodePkce => "authorization_code_pkce",
    };
    let mut material = format!("{grant}\n{token_url}\n{client_id}\n{scope}\n{username}");
    for (key, value) in extra_params {
        material.push_str(&format!("\n{key}={value}"));
    }
    let digest = crypto::sha256_hex(material.as_bytes());
    format!("oauth.{workspace_id}.{}", &digest[..16])
}

/// Field names a grant sets itself; an extra parameter with one of these names is not sent, so
/// it can never replace the grant type, the client credentials or the code.
pub const RESERVED_TOKEN_PARAMS: &[&str] = &[
    "grant_type",
    "client_id",
    "client_secret",
    "username",
    "password",
    "scope",
    "code",
    "code_verifier",
    "code_challenge",
    "code_challenge_method",
    "redirect_uri",
    "refresh_token",
    "response_type",
    "state",
];

/// The enabled extra parameters of an OAuth 2.0 configuration with `resolve` applied to their
/// keys and values; rows with a blank key or a [`RESERVED_TOKEN_PARAMS`] name are dropped.
///
/// # Errors
/// The first error `resolve` returns (an unresolved `{{variable}}`).
pub fn resolve_extra_params<E>(
    rows: &[spanbay_model::KeyValue],
    mut resolve: impl FnMut(&str) -> Result<String, E>,
) -> Result<Vec<(String, String)>, E> {
    let mut params = Vec::new();
    for row in rows.iter().filter(|row| row.enabled) {
        let key = resolve(&row.key)?.trim().to_owned();
        if key.is_empty() || RESERVED_TOKEN_PARAMS.contains(&key.to_ascii_lowercase().as_str()) {
            continue;
        }
        params.push((key, resolve(&row.value)?));
    }
    Ok(params)
}

/// The non-secret facts about a stored token, as saved under `<key>.meta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenMeta {
    /// The `token_type` the server stated (`Bearer` when it did not).
    pub token_type: String,
    /// The last four characters of the access token (`••••` for a short one).
    pub last4: String,
    /// The granted scope, as the server stated it.
    pub scope: String,
    /// Expiry in Unix milliseconds; `None` when the server gave no lifetime.
    pub expires_at_ms: Option<i64>,
}

fn last4(token: &str) -> String {
    let length = token.chars().count();
    if length < 12 {
        return "••••".to_owned();
    }
    token.chars().skip(length - 4).collect()
}

/// Saves `grant` under `key`, replacing a previous token; a grant without a refresh token
/// keeps the stored one (servers may omit it on refresh).
///
/// # Errors
/// [`AppError::Database`] when the secret store cannot be written.
pub fn store_grant(
    secrets: &dyn SecretStore,
    key: &str,
    grant: &TokenGrant,
    now_ms: i64,
) -> Result<TokenMeta, AppError> {
    let meta = TokenMeta {
        token_type: grant.token_type.clone(),
        last4: last4(&grant.access_token),
        scope: grant.scope.clone(),
        expires_at_ms: grant.expires_in.map(|seconds| {
            let millis = i64::try_from(seconds)
                .unwrap_or(i64::MAX / 2_000)
                .saturating_mul(1_000);
            now_ms.saturating_add(millis)
        }),
    };
    secrets.put(&format!("{key}.access"), &grant.access_token)?;
    if let Some(refresh) = &grant.refresh_token {
        secrets.put(&format!("{key}.refresh"), refresh)?;
    }
    let text =
        serde_json::to_string(&meta).map_err(|error| AppError::internal(error.to_string()))?;
    secrets.put(&format!("{key}.meta"), &text)?;
    Ok(meta)
}

/// The facts about the token stored under `key`, `None` when there is none.
///
/// # Errors
/// [`AppError::Database`] when the secret store cannot be read.
pub fn load_meta(secrets: &dyn SecretStore, key: &str) -> Result<Option<TokenMeta>, AppError> {
    let Some(text) = secrets.get(&format!("{key}.meta"))? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&text).ok())
}

/// The stored access token (a credential: hand it only to the executor).
///
/// # Errors
/// [`AppError::Database`] when the secret store cannot be read.
pub fn access_token(secrets: &dyn SecretStore, key: &str) -> Result<Option<String>, AppError> {
    secrets.get(&format!("{key}.access"))
}

/// The stored refresh token (a credential).
///
/// # Errors
/// [`AppError::Database`] when the secret store cannot be read.
pub fn refresh_token(secrets: &dyn SecretStore, key: &str) -> Result<Option<String>, AppError> {
    secrets.get(&format!("{key}.refresh"))
}

/// Whether a refresh token is stored under `key`.
///
/// # Errors
/// [`AppError::Database`] when the secret store cannot be read.
pub fn has_refresh(secrets: &dyn SecretStore, key: &str) -> Result<bool, AppError> {
    Ok(secrets.hint(&format!("{key}.refresh"))?.has_secret)
}

/// Removes every row of the token under `key`.
///
/// # Errors
/// [`AppError::Database`] when the secret store cannot be written.
pub fn clear(secrets: &dyn SecretStore, key: &str) -> Result<(), AppError> {
    for suffix in ["access", "refresh", "meta"] {
        secrets.delete(&format!("{key}.{suffix}"))?;
    }
    Ok(())
}

/// [`StoredOAuthTokens`] over the secret store: the executor's view of the token card.
pub struct SecretTokens(pub Arc<dyn SecretStore>);

impl StoredOAuthTokens for SecretTokens {
    fn valid_access_token(&self, key: &str) -> Option<String> {
        let meta = load_meta(self.0.as_ref(), key).ok().flatten()?;
        let now = spanbay_base::time::now_ms();
        if meta
            .expires_at_ms
            .is_some_and(|expires| expires.saturating_sub(EXPIRY_SKEW_MS) <= now)
        {
            return None;
        }
        access_token(self.0.as_ref(), key).ok().flatten()
    }

    fn refresh_token(&self, key: &str) -> Option<String> {
        refresh_token(self.0.as_ref(), key).ok().flatten()
    }

    fn store_grant(&self, key: &str, grant: &TokenGrant) -> bool {
        store_grant(self.0.as_ref(), key, grant, spanbay_base::time::now_ms()).is_ok()
    }

    fn invalidate(&self, key: &str, token: &str) {
        let secrets = self.0.as_ref();
        let still_stored = access_token(secrets, key)
            .ok()
            .flatten()
            .is_some_and(|stored| crypto::constant_time_eq(stored.as_bytes(), token.as_bytes()));
        let Some(mut meta) = load_meta(secrets, key).ok().flatten() else {
            return;
        };
        if !still_stored {
            return;
        }
        // The card then shows the token as expired; the refresh token is kept for the next send.
        meta.expires_at_ms = Some(spanbay_base::time::now_ms());
        if let Ok(text) = serde_json::to_string(&meta) {
            let _ = secrets.put(&format!("{key}.meta"), &text);
        }
    }
}

/// The per-attempt secrets of the authorization code flow with PKCE (RFC 7636, S256).
pub struct Pkce {
    /// The secret the token request proves possession with.
    pub verifier: String,
    /// `BASE64URL(SHA256(verifier))`, sent with the authorization request.
    pub challenge: String,
    /// Binds the browser's answer to this attempt (CSRF protection).
    pub state: String,
}

fn random_token() -> Result<String, AppError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| AppError::internal(format!("no secure random source: {error}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

impl Pkce {
    /// A fresh verifier, its challenge and a state value.
    ///
    /// # Errors
    /// [`AppError::Internal`] when the system has no secure random source.
    pub fn generate() -> Result<Self, AppError> {
        let verifier = random_token()?;
        let challenge = URL_SAFE_NO_PAD.encode(crypto::sha256(verifier.as_bytes()));
        Ok(Self {
            verifier,
            challenge,
            state: random_token()?,
        })
    }
}

/// A listener on `127.0.0.1` (never another interface) that waits for the browser's redirect.
pub struct LoopbackRedirect {
    listener: TcpListener,
    port: u16,
}

impl LoopbackRedirect {
    /// Binds `127.0.0.1:port`; `None` lets the system pick a free port.
    ///
    /// # Errors
    /// [`AppError::Io`] when the port is taken or cannot be bound.
    pub async fn bind(port: Option<u16>) -> Result<Self, AppError> {
        let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
            .await
            .map_err(|error| {
                AppError::io(match port {
                    Some(port) => {
                        format!("cannot listen on 127.0.0.1:{port} for the redirect: {error}")
                    }
                    None => format!("cannot listen for the redirect: {error}"),
                })
            })?;
        let port = listener
            .local_addr()
            .map_err(|error| AppError::io(error.to_string()))?
            .port();
        Ok(Self { listener, port })
    }

    /// The `redirect_uri` to register and send.
    #[must_use]
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{CALLBACK_PATH}", self.port)
    }

    /// Waits for the redirect that carries `expected_state` and returns its authorization code.
    /// Requests for other paths or with another state are answered and ignored.
    ///
    /// # Errors
    /// [`AppError::Cancelled`] when `cancel` fires, [`AppError::Network`] when `timeout` passes
    /// or the server answered with an `error`.
    pub async fn wait_for_code(
        self,
        expected_state: &str,
        timeout: Duration,
        cancel: Option<&CancelToken>,
    ) -> Result<String, AppError> {
        // Each connection is its own future with a whole-request deadline, so one idle or
        // trickling client cannot delay the real redirect.
        let mut connections = FuturesUnordered::new();
        let wait = async {
            loop {
                tokio::select! {
                    accepted = self.listener.accept() => match accepted {
                        Ok((stream, _)) => connections.push(tokio::time::timeout(
                            READ_TIMEOUT,
                            handle_connection(stream, expected_state),
                        )),
                        // Out of file descriptors and the like: back off instead of spinning.
                        Err(_) => tokio::time::sleep(ACCEPT_BACKOFF).await,
                    },
                    Some(finished) = connections.next(), if !connections.is_empty() => {
                        if let Ok(Some(outcome)) = finished {
                            return outcome;
                        }
                    }
                }
            }
        };
        match until_cancelled(cancel, tokio::time::timeout(timeout, wait)).await {
            None => Err(AppError::cancelled()),
            Some(Err(_)) => Err(AppError::network(
                "the browser did not return to SpanBay in time; get the token again",
            )),
            Some(Ok(outcome)) => outcome,
        }
    }
}

/// Reads one request and answers it; `Some` when it was the redirect that ends the wait. The
/// caller bounds the whole exchange, so a client that trickles bytes cannot hold it open.
async fn handle_connection(
    mut stream: TcpStream,
    expected_state: &str,
) -> Option<Result<String, AppError>> {
    let mut buffer = vec![0_u8; 8 * 1024];
    let mut filled = 0;
    let head = loop {
        let read = stream.read(&mut buffer[filled..]).await.ok()?;
        if read == 0 {
            return None;
        }
        filled += read;
        if buffer[..filled].windows(2).any(|pair| pair == b"\r\n") || filled == buffer.len() {
            break String::from_utf8_lossy(&buffer[..filled]).into_owned();
        }
    };
    let target = head
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_default();
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        respond(&mut stream, "400 Bad Request", "Bad request.").await;
        return None;
    };
    if url.path() != CALLBACK_PATH {
        respond(&mut stream, "404 Not Found", "Not found.").await;
        return None;
    }
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    if !param("state")
        .is_some_and(|state| crypto::constant_time_eq(state.as_bytes(), expected_state.as_bytes()))
    {
        respond(
            &mut stream,
            "400 Bad Request",
            "This sign-in belongs to another attempt.",
        )
        .await;
        return None;
    }
    if let Some(error) = param("error") {
        respond(
            &mut stream,
            "200 OK",
            "Sign-in was refused. You can close this tab.",
        )
        .await;
        let detail = param("error_description").unwrap_or_default();
        return Some(Err(AppError::network(format!(
            "the authorization server answered {error}{}{}",
            if detail.is_empty() { "" } else { ": " },
            detail.chars().take(300).collect::<String>()
        ))));
    }
    match param("code").filter(|code| !code.is_empty()) {
        Some(code) => {
            respond(
                &mut stream,
                "200 OK",
                "Signed in. You can close this tab and return to SpanBay.",
            )
            .await;
            Some(Ok(code))
        }
        None => {
            respond(
                &mut stream,
                "400 Bad Request",
                "The redirect carried no code.",
            )
            .await;
            None
        }
    }
}

async fn respond(stream: &mut TcpStream, status: &str, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>SpanBay</title><p style=\"font:16px system-ui;margin:3em\">{message}</p>"
    );
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // The browser may already have gone away; nothing to do about it.
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests;
