//! The OAuth 2.0 token of a send. The token the user fetched with the token card wins; when it
//! expired and a refresh token is stored, it is refreshed and stored again; otherwise a
//! client-credentials or password token is fetched and cached in memory. Per configuration one
//! fetch or refresh runs at a time (single flight), so a collection run that starts many sends at
//! once makes one token request. After a 401 answer the send is retried once with a new token
//! when the configuration has a way to one (a stored refresh token, or a grant that fetches);
//! the refused token is marked expired only once a different token replaced it, so an expected
//! 401 never strands a card token that cannot be renewed. Every token a send attaches is
//! reported to the token store (`StoredOAuthTokens::sent`).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use spanbay_base::crypto;
use spanbay_model::{AuthConfig, OAuth2ClientAuth, OAuth2GrantType, Request, VariableScopes};

use crate::executor::auth::{OAuthClient, token_request};
use crate::executor::request::resolve_value;
use crate::executor::send::await_or_cancel;
use crate::executor::types::{AuxiliaryNetwork, ExecutionError, ExecutorOptions};

/// One OAuth 2.0 configuration with every `{{variable}}` resolved.
#[derive(Clone)]
struct OAuthTarget {
    grant: OAuth2GrantType,
    token_url: String,
    client_id: String,
    client_secret: String,
    client_auth: OAuth2ClientAuth,
    username: String,
    password: String,
    scope: String,
    extra: Vec<(String, String)>,
    /// Where the token card keeps this configuration's token.
    stored_key: String,
}

impl OAuthTarget {
    /// The resolved configuration of `request`'s auth; `None` when it is not OAuth 2.0.
    fn of(request: &Request, scopes: &VariableScopes) -> Result<Option<Self>, ExecutionError> {
        let AuthConfig::OAuth2 {
            grant_type,
            access_token_url,
            client_id,
            client_secret_ref,
            username,
            password_ref,
            scope,
            client_auth,
            extra_params,
            ..
        } = &request.auth
        else {
            return Ok(None);
        };
        let token_url = resolve_value(access_token_url, scopes)?;
        let client_id = resolve_value(client_id, scopes)?;
        let username = resolve_value(username, scopes).unwrap_or_default();
        let scope = resolve_value(scope, scopes).unwrap_or_default();
        let extra =
            crate::oauth::resolve_extra_params(extra_params, |text| resolve_value(text, scopes))?;
        let stored_key = crate::oauth::token_key(
            request.workspace_id.as_str(),
            *grant_type,
            &token_url,
            &client_id,
            &scope,
            &username,
            &extra,
        );
        Ok(Some(Self {
            grant: *grant_type,
            token_url,
            client_id,
            client_secret: resolve_value(client_secret_ref, scopes).unwrap_or_default(),
            client_auth: *client_auth,
            username,
            password: resolve_value(password_ref, scopes).unwrap_or_default(),
            scope,
            extra,
            stored_key,
        }))
    }

    /// Key of the in-memory token cache and of the single flight; it covers the secret, so a
    /// changed secret never reuses a token, and it is hashed so the secret is not kept as text.
    fn cache_key(&self) -> String {
        let mut material = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            self.token_url,
            self.client_id,
            self.scope,
            grant_name(self.grant),
            self.username,
            self.client_secret,
        );
        for (key, value) in &self.extra {
            material.push_str(&format!("\n{key}={value}"));
        }
        crypto::sha256_hex(material.as_bytes())
    }

    fn client(&self) -> OAuthClient<'_> {
        OAuthClient {
            auth: self.client_auth,
            id: &self.client_id,
            secret: &self.client_secret,
        }
    }

    /// The token to send: stored, cached, refreshed or fetched (see the module docs). `refused`
    /// is a token the server just answered 401 to: it is never handed out again, so a stored or
    /// cached copy of it is skipped and a new token is refreshed or fetched instead.
    async fn token(
        &self,
        options: &ExecutorOptions,
        refused: Option<&str>,
    ) -> Result<String, ExecutionError> {
        let cache_key = self.cache_key();
        if let Some(token) = self.ready_token(options, &cache_key, refused).await {
            return Ok(token);
        }
        let flight = Flight::join(&cache_key);
        let _turn = await_or_cancel(options, flight.lock.lock()).await?;
        // Another send of the same configuration may have finished while this one waited.
        if let Some(token) = self.ready_token(options, &cache_key, refused).await {
            return Ok(token);
        }
        let network = token_network(options);
        // Without an agent guard the token URL may still be loopback or private (the user's own
        // IdP), but link-local and metadata addresses are never reachable.
        let guard = options.resolve_guard.clone().unwrap_or_default();

        let stored = options.oauth_tokens.clone();
        let key = self.stored_key.clone();
        let refresh = tokio::task::spawn_blocking(move || stored.refresh_token(&key))
            .await
            .ok()
            .flatten();
        if let Some(refresh) = refresh {
            let mut form = vec![
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
            ];
            form.extend(self.extra_form());
            let refreshed = await_or_cancel(
                options,
                token_request(&network, &guard, &self.token_url, &form, self.client()),
            )
            .await?;
            match refreshed {
                Ok(grant) => {
                    let token = grant.access_token.clone();
                    let stored = options.oauth_tokens.clone();
                    let key = self.stored_key.clone();
                    let _ =
                        tokio::task::spawn_blocking(move || stored.store_grant(&key, &grant)).await;
                    return Ok(token);
                }
                // The interactive grant has no other way to a token.
                Err(error) if self.grant == OAuth2GrantType::AuthorizationCodePkce => {
                    return Err(ExecutionError::OAuth2Token {
                        message: format!(
                            "the stored token expired and refreshing it failed ({error}); get a new \
                             one with the token card in the Auth tab"
                        ),
                    });
                }
                // A grant that can fetch a fresh token falls through to that.
                Err(_) => {}
            }
        }
        if self.grant == OAuth2GrantType::AuthorizationCodePkce {
            return Err(ExecutionError::OAuth2Token {
                message: "no valid OAuth 2.0 token: get one with the token card in the Auth tab"
                    .to_owned(),
            });
        }

        let mut form = vec![("grant_type", grant_name(self.grant))];
        if !self.scope.is_empty() {
            form.push(("scope", self.scope.as_str()));
        }
        if self.grant == OAuth2GrantType::Password {
            form.push(("username", self.username.as_str()));
            form.push(("password", self.password.as_str()));
        }
        form.extend(self.extra_form());
        let grant = await_or_cancel(
            options,
            token_request(&network, &guard, &self.token_url, &form, self.client()),
        )
        .await??;
        let lifetime =
            Duration::from_secs(grant.expires_in.unwrap_or(300).saturating_sub(30).max(1));
        if let Ok(mut cache) = token_cache().lock() {
            cache.insert(
                cache_key,
                (grant.access_token.clone(), Instant::now() + lifetime),
            );
        }
        Ok(grant.access_token)
    }

    /// A token that needs no request: the card's valid stored token, else an unexpired cached one;
    /// neither when it is the `refused` token.
    async fn ready_token(
        &self,
        options: &ExecutorOptions,
        cache_key: &str,
        refused: Option<&str>,
    ) -> Option<String> {
        let usable = |token: &String| refused != Some(token.as_str());
        let stored = options.oauth_tokens.clone();
        let key = self.stored_key.clone();
        let token = tokio::task::spawn_blocking(move || stored.valid_access_token(&key))
            .await
            .ok()
            .flatten()
            .filter(usable);
        if token.is_some() {
            return token;
        }
        let now = Instant::now();
        token_cache()
            .lock()
            .ok()
            .and_then(|cache| cache.get(cache_key).cloned())
            .filter(|(token, expires_at)| *expires_at > now && usable(token))
            .map(|(token, _)| token)
    }

    /// Whether a token other than the one sent can be had: a grant that fetches one, or a stored
    /// refresh token for the interactive grant.
    async fn can_renew(&self, options: &ExecutorOptions) -> bool {
        if self.grant != OAuth2GrantType::AuthorizationCodePkce {
            return true;
        }
        let stored = options.oauth_tokens.clone();
        let key = self.stored_key.clone();
        tokio::task::spawn_blocking(move || stored.refresh_token(&key).is_some())
            .await
            .unwrap_or(false)
    }

    fn extra_form(&self) -> impl Iterator<Item = (&str, &str)> {
        self.extra
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// Forgets `token` wherever it is kept for this configuration (the server refused it and a
    /// different token replaced it).
    async fn invalidate(&self, options: &ExecutorOptions, token: &str) {
        if let Ok(mut cache) = token_cache().lock() {
            let cache_key = self.cache_key();
            if cache
                .get(&cache_key)
                .is_some_and(|(cached, _)| cached == token)
            {
                cache.remove(&cache_key);
            }
        }
        let stored = options.oauth_tokens.clone();
        let (key, token) = (self.stored_key.clone(), token.to_owned());
        let _ = tokio::task::spawn_blocking(move || stored.invalidate(&key, &token)).await;
    }
}

fn grant_name(grant: OAuth2GrantType) -> &'static str {
    match grant {
        OAuth2GrantType::ClientCredentials => "client_credentials",
        OAuth2GrantType::Password => "password",
        OAuth2GrantType::AuthorizationCodePkce => "authorization_code",
    }
}

/// The token request's transport: the auxiliary settings with the request's certificates, so a
/// token endpoint behind mTLS or a private CA is reached like the request itself.
fn token_network(options: &ExecutorOptions) -> AuxiliaryNetwork {
    let mut network = options.auxiliary.clone();
    if network.tls.certificates.is_empty() {
        network.tls.certificates.clone_from(&options.certificates);
    }
    network
}

/// Fetched client-credentials and password tokens by configuration, with their expiry.
fn token_cache() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    static CACHE: LazyLock<Mutex<HashMap<String, (String, Instant)>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    &CACHE
}

/// The single flight of one configuration: sends that need a new token queue on its lock; the
/// entry leaves the map with its last holder.
struct Flight {
    key: String,
    lock: Arc<tokio::sync::Mutex<()>>,
}

static FLIGHTS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

impl Flight {
    fn join(key: &str) -> Self {
        let lock = FLIGHTS.lock().map_or_else(
            |_| Arc::new(tokio::sync::Mutex::new(())),
            |mut flights| Arc::clone(flights.entry(key.to_owned()).or_default()),
        );
        Self {
            key: key.to_owned(),
            lock,
        }
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        if let Ok(mut flights) = FLIGHTS.lock()
            && flights
                .get(&self.key)
                .is_some_and(|lock| Arc::ptr_eq(lock, &self.lock) && Arc::strong_count(lock) <= 2)
        {
            flights.remove(&self.key);
        }
    }
}

fn with_bearer(request: &Request, token: String) -> Request {
    let mut request = request.clone();
    request.auth = AuthConfig::Bearer { token_ref: token };
    request
}

/// The OAuth 2.0 token a send carries, and the configuration it came from.
pub(super) struct OAuthToken {
    target: OAuthTarget,
    token: String,
}

/// The token for `request` when its auth is OAuth 2.0 (`None` otherwise).
pub(super) async fn oauth2_token(
    request: &Request,
    scopes: &VariableScopes,
    options: &ExecutorOptions,
) -> Result<Option<OAuthToken>, ExecutionError> {
    let Some(target) = OAuthTarget::of(request, scopes)? else {
        return Ok(None);
    };
    let token = target.token(options, None).await?;
    options.oauth_tokens.sent(&token);
    Ok(Some(OAuthToken { target, token }))
}

impl OAuthToken {
    /// `request` carrying this token as its bearer credential.
    pub(super) fn apply(&self, request: &Request) -> Request {
        with_bearer(request, self.token.clone())
    }

    /// After the server refused this token (401): a different token, refreshed or fetched, when
    /// the configuration has a way to one; the refused token is then invalidated wherever it is
    /// kept. `None` when the 401 response stands, and the refused token is left as it was: the
    /// 401 may be about the resource, and a card token without a refresh token cannot come back.
    pub(super) async fn renewed(
        &self,
        options: &ExecutorOptions,
    ) -> Result<Option<OAuthToken>, ExecutionError> {
        if !self.target.can_renew(options).await {
            return Ok(None);
        }
        match self.target.token(options, Some(&self.token)).await {
            Ok(fresh) if fresh != self.token => {
                self.target.invalidate(options, &self.token).await;
                options.oauth_tokens.sent(&fresh);
                Ok(Some(OAuthToken {
                    target: self.target.clone(),
                    token: fresh,
                }))
            }
            Err(error) if error.is_cancelled() => Err(error),
            _ => Ok(None),
        }
    }
}
