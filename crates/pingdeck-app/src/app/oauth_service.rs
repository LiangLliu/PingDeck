//! The OAuth 2.0 token card: status, fetch, refresh, clear and "store as variable".
//!
//! Single responsibility: resolve a saved OAuth 2.0 configuration against an environment, call
//! the token endpoint through the executor's token request, and keep the result in the secret
//! store (see [`crate::oauth`]). The token text never leaves this module except into the
//! executor at send time and into the secret variable `oauth_token_store_as_impl` writes.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::cancel::until_cancelled;
use crate::error::AppError;
use crate::executor::{
    AuxiliaryNetwork, CertificatePassphrases, OAuthClient, ResolveGuard, TlsMaterial, TokenGrant,
    token_request,
};
use crate::oauth::{self, LoopbackRedirect, Pkce, TokenMeta};
use crate::workspace::models::TlsCertificate;
use crate::workspace::{
    AuthConfig, OAuth2GrantType, RequestId, VariableScopes, WorkspaceId, resolve_template,
};
use pingdeck_model::OAuth2ClientAuth;

use super::environment_service::{DefineVariableInput, VariableScope, environment_variable_define};
use super::execution_service::{execution_context, execution_context_for};
use super::{AppState, command_error, current_time_ms};

/// Input of every token-card command: the saved `OAuth2` auth and where its `{{variables}}`
/// resolve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthTokenInput {
    /// Workspace of the request.
    pub workspace_id: String,
    /// The active environment, `None` when none is selected.
    pub environment_id: Option<String>,
    /// The collection whose variables the auth's `{{tokens}}` resolve against; used when
    /// `request_id` names no saved request (a draft). `None` resolves without a collection.
    #[serde(default)]
    pub collection_id: Option<String>,
    /// The request the auth belongs to: its collection and folder decide the variable scopes.
    /// A request that is not saved (a draft) falls back to `collection_id`.
    #[serde(default)]
    pub request_id: Option<String>,
    /// The request's auth; anything but `OAuth2` is a validation error.
    pub auth: AuthConfig,
}

/// What the token card shows about a stored token; the token itself never crosses IPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfoDto {
    /// The server's `token_type` (`Bearer` when it did not say).
    pub token_type: String,
    /// The last four characters of the access token (`••••` for a short one).
    pub last4: String,
    /// The granted scope.
    pub scope: String,
    /// Expiry in Unix milliseconds; `None` when the server gave no lifetime.
    pub expires_at_ms: Option<i64>,
    /// Whether a refresh token is stored.
    pub has_refresh: bool,
}

impl TokenInfoDto {
    fn new(meta: TokenMeta, has_refresh: bool) -> Self {
        Self {
            token_type: meta.token_type,
            last4: meta.last4,
            scope: meta.scope,
            expires_at_ms: meta.expires_at_ms,
            has_refresh,
        }
    }
}

/// Opens the authorization URL in the user's browser; blocking.
pub(crate) type BrowserOpener = Arc<dyn Fn(&str) -> Result<(), AppError> + Send + Sync>;

/// The configuration with every `{{variable}}` resolved, and the key its token lives under.
struct Resolved {
    grant: OAuth2GrantType,
    token_url: String,
    authorization_url: String,
    redirect_port: Option<u16>,
    client_id: String,
    client_secret: String,
    client_auth: OAuth2ClientAuth,
    username: String,
    password: String,
    scope: String,
    /// The enabled extra parameters (`audience`, `resource`), resolved.
    extra: Vec<(String, String)>,
    /// The workspace certificates the token request presents and trusts, like the send's.
    certificates: Vec<TlsCertificate>,
    key: String,
}

fn resolve(state: &AppState, input: &OAuthTokenInput) -> Result<Resolved, AppError> {
    let AuthConfig::OAuth2 {
        grant_type,
        access_token_url,
        client_id,
        client_secret_ref,
        username,
        password_ref,
        scope,
        authorization_url,
        redirect_port,
        client_auth,
        extra_params,
    } = &input.auth
    else {
        return Err(AppError::validation("the request's auth is not OAuth 2.0"));
    };
    let workspace = WorkspaceId::new(&input.workspace_id).map_err(command_error)?;
    // The send path resolves the same templates in the request's collection scope, so the token
    // key the card derives matches the one the executor looks up.
    let (scopes, certificates): (VariableScopes, Vec<TlsCertificate>) = {
        let store = state.store();
        let request = match input.request_id.as_deref() {
            Some(id) => store
                .get_request(&workspace, &RequestId::new(id).map_err(command_error)?)
                .map_err(command_error)?,
            None => None,
        };
        let context = match request {
            Some(request) => execution_context(&store, &request, input.environment_id.as_deref())?,
            None => execution_context_for(
                &store,
                workspace.as_str(),
                input.collection_id.as_deref(),
                None,
                input.environment_id.as_deref(),
            )?,
        };
        (context.scopes(), context.certificates.clone())
    };
    let value = |template: &str| {
        resolve_template(template, &scopes).map_err(|error| AppError::validation(error.to_string()))
    };
    let lenient = |template: &str| value(template).unwrap_or_default();

    let token_url = value(access_token_url)?;
    let client_id = value(client_id)?;
    let scope = lenient(scope);
    let username = lenient(username);
    let authorization_url = if *grant_type == OAuth2GrantType::AuthorizationCodePkce {
        value(authorization_url)?
    } else {
        String::new()
    };
    check_http_url("Access token URL", &token_url)?;
    if client_id.trim().is_empty() {
        return Err(AppError::validation("Enter the client ID"));
    }
    match grant_type {
        OAuth2GrantType::Password if username.trim().is_empty() => {
            return Err(AppError::validation("Enter the username"));
        }
        OAuth2GrantType::AuthorizationCodePkce => {
            check_http_url("Authorization URL", &authorization_url)?;
        }
        _ => {}
    }
    let extra = oauth::resolve_extra_params(extra_params, &value)?;
    let key = oauth::token_key(
        workspace.as_str(),
        *grant_type,
        &token_url,
        &client_id,
        &scope,
        &username,
        &extra,
    );
    Ok(Resolved {
        grant: *grant_type,
        token_url,
        authorization_url,
        redirect_port: *redirect_port,
        client_id,
        client_secret: lenient(client_secret_ref),
        client_auth: *client_auth,
        username,
        password: lenient(password_ref),
        scope,
        extra,
        certificates,
        key,
    })
}

impl Resolved {
    fn client(&self) -> OAuthClient<'_> {
        OAuthClient {
            auth: self.client_auth,
            id: &self.client_id,
            secret: &self.client_secret,
        }
    }

    fn extra_form(&self) -> impl Iterator<Item = (&str, &str)> {
        self.extra
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// The token request's transport: Settings › Network plus the workspace certificates.
    fn network(&self, state: &AppState) -> AuxiliaryNetwork {
        AuxiliaryNetwork::from(&state.settings().network).with_tls(TlsMaterial {
            certificates: self.certificates.clone(),
            passphrases: CertificatePassphrases::new(Arc::clone(state.secrets())),
        })
    }
}

fn check_http_url(label: &str, url: &str) -> Result<(), AppError> {
    match url::Url::parse(url.trim()) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") && parsed.has_host() => Ok(()),
        _ => Err(AppError::validation(format!(
            "{label} must be an absolute http or https URL"
        ))),
    }
}

/// Runs blocking secret-store work off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| AppError::internal(format!("token store task failed: {error}")))?
}

/// What is stored for `input`'s configuration: `None` when no token was fetched yet.
pub fn oauth_token_status_impl(
    state: &AppState,
    input: &OAuthTokenInput,
) -> Result<Option<TokenInfoDto>, AppError> {
    let resolved = resolve(state, input)?;
    let secrets = state.secrets();
    let Some(meta) = oauth::load_meta(secrets.as_ref(), &resolved.key)? else {
        return Ok(None);
    };
    let has_refresh = oauth::has_refresh(secrets.as_ref(), &resolved.key)?;
    Ok(Some(TokenInfoDto::new(meta, has_refresh)))
}

/// Fetches a token (client credentials, password, or authorization code with PKCE through the
/// system browser) and stores it. `cancel_token` aborts it through `execution_cancel`.
pub async fn oauth_token_fetch_impl(
    state: &AppState,
    input: OAuthTokenInput,
    cancel_token: Option<String>,
) -> Result<TokenInfoDto, AppError> {
    let opener: BrowserOpener = Arc::new(super::system_open_external_impl);
    fetch_with(
        state,
        input,
        cancel_token,
        opener,
        oauth::AUTHORIZATION_TIMEOUT,
    )
    .await
}

pub(crate) async fn fetch_with(
    state: &AppState,
    input: OAuthTokenInput,
    cancel_token: Option<String>,
    opener: BrowserOpener,
    authorization_timeout: std::time::Duration,
) -> Result<TokenInfoDto, AppError> {
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let cancel = guard.token();
    let resolved = resolve(state, &input)?;
    let network = resolved.network(state);
    // The card is user-initiated: a loopback or private IdP is fine, link-local and metadata
    // addresses are never reachable.
    let resolve_guard = ResolveGuard::default();

    let client = resolved.client();
    let grant = match resolved.grant {
        OAuth2GrantType::ClientCredentials | OAuth2GrantType::Password => {
            let is_password = resolved.grant == OAuth2GrantType::Password;
            let mut form = vec![(
                "grant_type",
                if is_password {
                    "password"
                } else {
                    "client_credentials"
                },
            )];
            if !resolved.scope.is_empty() {
                form.push(("scope", resolved.scope.as_str()));
            }
            if is_password {
                form.push(("username", resolved.username.as_str()));
                form.push(("password", resolved.password.as_str()));
            }
            form.extend(resolved.extra_form());
            let request =
                token_request(&network, &resolve_guard, &resolved.token_url, &form, client);
            cancellable(cancel, request).await?
        }
        OAuth2GrantType::AuthorizationCodePkce => {
            let pkce = Pkce::generate()?;
            let redirect = LoopbackRedirect::bind(resolved.redirect_port).await?;
            let redirect_uri = redirect.redirect_uri();
            let url = authorization_request_url(&resolved, &pkce, &redirect_uri)?;
            blocking(move || opener(&url)).await?;
            let code = redirect
                .wait_for_code(&pkce.state, authorization_timeout, Some(cancel))
                .await?;
            let mut form = vec![
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("code_verifier", pkce.verifier.as_str()),
            ];
            form.extend(resolved.extra_form());
            let request =
                token_request(&network, &resolve_guard, &resolved.token_url, &form, client);
            cancellable(cancel, request).await?
        }
    };
    store(state, &resolved.key, grant).await
}

/// The authorization endpoint URL with the PKCE query (RFC 7636, S256).
fn authorization_request_url(
    resolved: &Resolved,
    pkce: &Pkce,
    redirect_uri: &str,
) -> Result<String, AppError> {
    let mut url = url::Url::parse(resolved.authorization_url.trim())
        .map_err(|_| AppError::validation("Authorization URL must be an absolute URL"))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &resolved.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", &pkce.state)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256");
        if !resolved.scope.is_empty() {
            query.append_pair("scope", &resolved.scope);
        }
        for (key, value) in &resolved.extra {
            query.append_pair(key, value);
        }
    }
    Ok(url.into())
}

/// Awaits a token request, aborting it when `cancel` fires.
async fn cancellable(
    cancel: &crate::cancel::CancelToken,
    request: impl Future<Output = Result<TokenGrant, crate::executor::ExecutionError>>,
) -> Result<TokenGrant, AppError> {
    until_cancelled(Some(cancel), request)
        .await
        .ok_or_else(AppError::cancelled)?
        .map_err(AppError::from)
}

async fn store(state: &AppState, key: &str, grant: TokenGrant) -> Result<TokenInfoDto, AppError> {
    let secrets = Arc::clone(state.secrets());
    let key = key.to_owned();
    blocking(move || {
        let meta = oauth::store_grant(secrets.as_ref(), &key, &grant, current_time_ms())?;
        let has_refresh = oauth::has_refresh(secrets.as_ref(), &key)?;
        Ok(TokenInfoDto::new(meta, has_refresh))
    })
    .await
}

/// Exchanges the stored refresh token for a new access token.
pub async fn oauth_token_refresh_impl(
    state: &AppState,
    input: OAuthTokenInput,
    cancel_token: Option<String>,
) -> Result<TokenInfoDto, AppError> {
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let resolved = resolve(state, &input)?;
    let network = resolved.network(state);
    let secrets = Arc::clone(state.secrets());
    let key = resolved.key.clone();
    let refresh = blocking(move || oauth::refresh_token(secrets.as_ref(), &key))
        .await?
        .ok_or_else(|| AppError::validation("No refresh token is stored: get a new token"))?;

    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh.as_str()),
    ];
    form.extend(resolved.extra_form());
    let grant = cancellable(
        guard.token(),
        token_request(
            &network,
            &ResolveGuard::default(),
            &resolved.token_url,
            &form,
            resolved.client(),
        ),
    )
    .await?;
    store(state, &resolved.key, grant).await
}

/// Forgets the stored token of `input`'s configuration.
pub fn oauth_token_clear_impl(state: &AppState, input: &OAuthTokenInput) -> Result<(), AppError> {
    let resolved = resolve(state, input)?;
    oauth::clear(state.secrets().as_ref(), &resolved.key)
}

/// Writes the stored access token into `variable` of the active environment as a secret
/// (created when missing, its note and id kept); every other variable, secret or not, is saved
/// unchanged. The text never reaches the caller.
pub fn oauth_token_store_as_impl(
    state: &AppState,
    input: &OAuthTokenInput,
    variable: &str,
) -> Result<(), AppError> {
    let resolved = resolve(state, input)?;
    let environment_id = input
        .environment_id
        .as_deref()
        .ok_or_else(|| AppError::validation("Select an environment to store the token in"))?;
    let token = oauth::access_token(state.secrets().as_ref(), &resolved.key)?
        .ok_or_else(|| AppError::not_found("No token is stored: get one first"))?;
    environment_variable_define(
        state,
        DefineVariableInput {
            workspace_id: input.workspace_id.clone(),
            scope: VariableScope::Environment,
            owner_id: Some(environment_id.to_owned()),
            key: variable.to_owned(),
            value: token,
            is_secret: true,
        },
        "oauth",
    )?;
    Ok(())
}
