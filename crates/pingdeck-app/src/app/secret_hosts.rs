//! Declared hosts: the only places a secret variable may be sent to.
//!
//! Single responsibility: work out which hosts an environment declares, list them for the
//! Environments header, and refuse a request the agent built when it carries a secret to any
//! other host. Requests the user builds and sends from the Requests module are never restricted.
//!
//! An environment declares a host when it appears in a plain variable value of the environment
//! (`base_url`), in the URL of a saved request of the workspace (unless another environment names
//! the host and this one does not; saved requests the agent touched declare nothing), when it is a
//! mock server of the workspace or the local gateway, or when the user added it by hand. Hosts are
//! lowercase
//! `host[:port]`; a declared bare host accepts every port, a declared `host:port` only that port.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::AppError;
use crate::executor::with_default_scheme;
use crate::workspace::models::MAX_DECLARED_HOSTS;
use crate::workspace::variables::resolve_template_lenient;
use crate::workspace::{
    AuthConfig, Environment, EnvironmentId, Request, RequestBody, VariableScopes, WorkspaceId,
};

use super::unresolved_scan::request_tokens;
use super::{AppState, command_error};

/// Where a declared host comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostSource {
    /// A URL or host in a plain variable value of the environment.
    Variable,
    /// The URL of a saved request of the workspace.
    SavedRequest,
    /// A mock server of the workspace (`127.0.0.1:<port>`, `localhost:<port>`).
    MockServer,
    /// The local gateway (`127.0.0.1:<port>`, `localhost:<port>`).
    Gateway,
    /// A host the user added by hand.
    Explicit,
}

/// One host an environment declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredHostDto {
    /// Lowercase `host` or `host:port`.
    pub host: String,
    /// Why the environment declares it.
    pub source: HostSource,
}

/// Whether `host` is a DNS name or an IPv4 address (letters, digits, `.`, `-`, `_`; no leading
/// or trailing dot or dash).
fn is_plain_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
}

/// Parses `host` or `host:port` (a bracketed IPv6 address included) into its lowercase form.
fn parse_host_port(text: &str) -> Option<String> {
    let text = text.trim().to_ascii_lowercase();
    let (host, port): (&str, Option<&str>) = if text.starts_with('[') {
        let end = text.find(']')?;
        let inner = &text[1..end];
        if inner.is_empty()
            || !inner
                .chars()
                .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'))
        {
            return None;
        }
        let rest = &text[end + 1..];
        let port = if rest.is_empty() {
            None
        } else {
            Some(rest.strip_prefix(':')?)
        };
        (&text[..=end], port)
    } else {
        let (host, port) = match text.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (text.as_str(), None),
        };
        if !is_plain_host(host) {
            return None;
        }
        (host, port)
    };
    match port {
        None => Some(host.to_owned()),
        Some(port) => {
            let port: u16 = port.parse().ok().filter(|port| *port != 0)?;
            Some(format!("{host}:{port}"))
        }
    }
}

/// Validates the hosts a user adds by hand: lowercase `host[:port]`, no duplicates, at most
/// [`MAX_DECLARED_HOSTS`].
pub(crate) fn normalize_declared_hosts(hosts: Vec<String>) -> Result<Vec<String>, AppError> {
    let mut normalized: Vec<String> = Vec::with_capacity(hosts.len());
    for host in hosts {
        let parsed = parse_host_port(&host).ok_or_else(|| {
            AppError::validation(format!("invalid host \"{host}\": use host or host:port"))
        })?;
        if !normalized.contains(&parsed) {
            normalized.push(parsed);
        }
    }
    if normalized.len() > MAX_DECLARED_HOSTS {
        return Err(AppError::validation(format!(
            "an environment declares at most {MAX_DECLARED_HOSTS} hosts"
        )));
    }
    Ok(normalized)
}

/// The lowercase `host[:port]` a URL (or scheme-less `host/path`) targets; the port is kept only
/// when the URL names a non-default one. `None` when the host part holds a `{{variable}}` or the
/// text is not a URL.
pub fn host_of_url(text: &str) -> Option<String> {
    let with_scheme = with_default_scheme(text);
    let authority = with_scheme
        .split_once("://")?
        .1
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.contains("{{") || authority.contains("}}") {
        return None;
    }
    let url = Url::parse(&with_scheme).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// The host a plain variable value names: a URL, or a scheme-less `host[:port][/path]` that
/// looks like a domain name, `localhost` or a dotted IPv4 address (a version such as `1.5` or a
/// word such as `true` is not a host).
fn host_of_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.contains("{{") || value.contains(char::is_whitespace) {
        return None;
    }
    let host = host_of_url(value)?;
    if value.contains("://") {
        return Some(host);
    }
    let bare = host.split(':').next().unwrap_or_default();
    let dotted_quad = bare.split('.').count() == 4
        && bare
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
    let domain_name = bare.chars().any(|c| c.is_ascii_alphabetic()) && bare.contains('.');
    (dotted_quad || domain_name || bare == "localhost").then_some(host)
}

/// The hosts `environment` declares. `requests` are the saved requests of the workspace;
/// `untrusted_request_ids` names the ones the agent created or edited, which declare nothing.
/// `foreign_hosts` are the hosts other environments of the workspace name and this one does not:
/// a saved request declares none of them, so a host that belongs to Staging is not declared for
/// Production just because a request targets it. `mock_ports` are the mock servers' ports and
/// `gateway_port` the local gateway's.
fn collect_declared_hosts(
    environment: Option<&Environment>,
    requests: &[Request],
    untrusted_request_ids: &HashSet<String>,
    foreign_hosts: &[String],
    mock_ports: &[u16],
    gateway_port: u16,
) -> Vec<DeclaredHostDto> {
    let mut hosts: Vec<DeclaredHostDto> = Vec::new();
    let mut add = |host: String, source: HostSource| {
        if !hosts.iter().any(|known| known.host == host) {
            hosts.push(DeclaredHostDto { host, source });
        }
    };
    if let Some(environment) = environment {
        for variable in environment
            .variables
            .iter()
            .filter(|variable| variable.enabled && !variable.is_secret)
        {
            if let Some(host) = variable.value.as_deref().and_then(host_of_value) {
                add(host, HostSource::Variable);
            }
        }
    }
    for request in requests
        .iter()
        .filter(|request| !untrusted_request_ids.contains(request.id.as_str()))
    {
        if let Some(host) =
            host_of_url(&request.url).filter(|host| !host_listed(foreign_hosts, host))
        {
            add(host, HostSource::SavedRequest);
        }
    }
    for &port in mock_ports.iter().filter(|port| **port != 0) {
        add(format!("127.0.0.1:{port}"), HostSource::MockServer);
        add(format!("localhost:{port}"), HostSource::MockServer);
    }
    add(format!("127.0.0.1:{gateway_port}"), HostSource::Gateway);
    add(format!("localhost:{gateway_port}"), HostSource::Gateway);
    if let Some(environment) = environment {
        for host in &environment.declared_hosts {
            match hosts.iter_mut().find(|known| &known.host == host) {
                Some(known) => known.source = HostSource::Explicit,
                None => hosts.push(DeclaredHostDto {
                    host: host.clone(),
                    source: HostSource::Explicit,
                }),
            }
        }
    }
    hosts
}

/// The hosts `environment` (or no environment) declares in `workspace_id`, read from the store,
/// the mock servers and the gateway settings.
///
/// `untrusted_request_ids` are the saved requests the agent created or edited and the user has
/// not sent once from the UI (`requests.agent_touched_ms`, see
/// [`WorkspaceStore::agent_touched_request_ids`](crate::workspace::WorkspaceStore::agent_touched_request_ids));
/// they declare nothing.
pub fn declared_hosts(
    state: &AppState,
    workspace_id: &WorkspaceId,
    environment: Option<&Environment>,
    untrusted_request_ids: &HashSet<String>,
) -> Result<Vec<DeclaredHostDto>, AppError> {
    let (requests, servers, environments) = {
        let store = state.store();
        (
            store
                .list_requests(workspace_id.as_str())
                .map_err(command_error)?,
            store
                .list_mock_servers(workspace_id.as_str())
                .map_err(command_error)?,
            match environment {
                Some(_) => store
                    .list_environments(workspace_id.as_str())
                    .map_err(command_error)?,
                None => Vec::new(),
            },
        )
    };
    let foreign_hosts = environment.map_or_else(Vec::new, |environment| {
        let own = environment_hosts(environment);
        let mut foreign: Vec<String> = Vec::new();
        for other in environments
            .iter()
            .filter(|other| other.id != environment.id)
        {
            for host in environment_hosts(other) {
                if !host_listed(&own, &host) && !foreign.contains(&host) {
                    foreign.push(host);
                }
            }
        }
        foreign
    });
    let mut mock_ports = Vec::new();
    for server in &servers {
        mock_ports.push(server.port);
        if let Some(bound) = state.mocks.bound_addr(&server.id) {
            mock_ports.push(bound.port());
        }
    }
    Ok(collect_declared_hosts(
        environment,
        &requests,
        untrusted_request_ids,
        &foreign_hosts,
        &mock_ports,
        state.settings().ai.gateway.port,
    ))
}

/// Lists the hosts an environment declares, derived ones and the user's explicit
/// additions, for the read-only list in the Environments header.
///
/// Saved requests the agent created or edited and the user has not sent from the UI since
/// declare nothing (their marker is `requests.agent_touched_ms`).
pub fn environment_declared_hosts_impl(
    state: &AppState,
    workspace_id: String,
    environment_id: String,
) -> Result<Vec<DeclaredHostDto>, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let environment_id = EnvironmentId::new(&environment_id).map_err(command_error)?;
    let environment = state
        .store()
        .get_environment(&workspace, &environment_id)
        .map_err(command_error)?
        .ok_or_else(|| {
            AppError::not_found(format!(
                "environment {} not found in workspace {workspace_id}",
                environment_id.as_str()
            ))
        })?;
    let untrusted = state
        .store()
        .agent_touched_request_ids(workspace.as_str())
        .map_err(command_error)?;
    declared_hosts(state, &workspace, Some(&environment), &untrusted)
}

/// Whether `target` (`host[:port]`) is covered by `declared`: the exact `host:port`, or the bare
/// host.
fn is_declared(declared: &HashSet<&str>, target: &str) -> bool {
    declared.contains(target)
        || target.rsplit_once(':').is_some_and(|(host, port)| {
            port.chars().all(|c| c.is_ascii_digit()) && declared.contains(host)
        })
}

/// What `request` carries besides `{{secret}}` tokens that must not leave for an undeclared host:
/// the stored or fetched OAuth 2.0 token its auth attaches, a non-empty credential field of its
/// auth, or a local file its body uploads. `None` when it carries none.
fn carried_credential(request: &Request) -> Option<&'static str> {
    let filled = |fields: &[&str]| fields.iter().any(|field| !field.trim().is_empty());
    let auth = match &request.auth {
        AuthConfig::OAuth2 { .. } => Some("The stored OAuth 2.0 token"),
        AuthConfig::Bearer { token_ref } => filled(&[token_ref]).then_some("The bearer token"),
        AuthConfig::Basic {
            username,
            password_ref,
        } => filled(&[username, password_ref]).then_some("The basic-auth credentials"),
        AuthConfig::ApiKey { value_ref, .. } => filled(&[value_ref]).then_some("The API key"),
        AuthConfig::Digest {
            username,
            password_ref,
        } => filled(&[username, password_ref.as_deref().unwrap_or_default()])
            .then_some("The digest-auth credentials"),
        AuthConfig::AwsSignatureV4 {
            access_key,
            secret_key_ref,
            session_token_ref,
            ..
        } => filled(&[
            access_key,
            secret_key_ref.as_deref().unwrap_or_default(),
            session_token_ref.as_deref().unwrap_or_default(),
        ])
        .then_some("The AWS signing credentials"),
        AuthConfig::Inherit | AuthConfig::None => None,
    };
    auth.or_else(|| match &request.body {
        RequestBody::Binary { file_path, .. } => filled(&[file_path]).then_some("A local file"),
        RequestBody::Multipart { parts } => parts
            .iter()
            .any(|part| {
                part.enabled
                    && part
                        .file_path
                        .as_deref()
                        .is_some_and(|path| !path.trim().is_empty())
            })
            .then_some("A local file"),
        _ => None,
    })
}

/// What `request` carries that may only go to a declared host: the first secret variable it
/// references (`Secret {{name}}`) or, failing that, a stored credential or local file (see
/// [`carried_credential`]). `None` when it carries none.
pub fn carried_secret(request: &Request, scopes: &VariableScopes) -> Option<String> {
    request_tokens(request)
        .into_iter()
        .find(|name| scopes.resolve_trace(name).2)
        .map(|secret| format!("Secret {{{{{secret}}}}}"))
        .or_else(|| carried_credential(request).map(str::to_owned))
}

/// The credential values `request` carries once its `{{variables}}` resolve against `scopes`:
/// every filled credential field of its auth, the values of its enabled credential headers
/// (`Authorization`, `Cookie`, `X-Api-Key`, `X-…-Token`, …) and credential query parameters
/// (`key`, `token`, `access_token`, …), and the password of the URL's userinfo. A header value
/// behind a scheme word (`Bearer abc`) also yields the bare token. `request` is the request as it
/// will be sent (scripts and inherited auth applied); the stored OAuth 2.0 access token is not
/// part of the definition and is reported by the executor's token source instead.
#[must_use]
pub fn sent_credential_values(request: &Request, scopes: &VariableScopes) -> Vec<String> {
    let resolve = |text: &str| resolve_template_lenient(text, scopes).trim().to_owned();
    let mut values: Vec<String> = Vec::new();
    let mut push = |value: String| {
        if !value.is_empty() && !value.contains("{{") && !values.contains(&value) {
            values.push(value);
        }
    };
    match &request.auth {
        AuthConfig::Bearer { token_ref } => push(resolve(token_ref)),
        AuthConfig::Basic { password_ref, .. } => push(resolve(password_ref)),
        AuthConfig::ApiKey { value_ref, .. } => push(resolve(value_ref)),
        AuthConfig::Digest { password_ref, .. } => {
            push(resolve(password_ref.as_deref().unwrap_or_default()));
        }
        AuthConfig::AwsSignatureV4 {
            secret_key_ref,
            session_token_ref,
            ..
        } => {
            push(resolve(secret_key_ref.as_deref().unwrap_or_default()));
            push(resolve(session_token_ref.as_deref().unwrap_or_default()));
        }
        AuthConfig::OAuth2 {
            client_secret_ref,
            password_ref,
            ..
        } => {
            push(resolve(client_secret_ref));
            push(resolve(password_ref));
        }
        AuthConfig::Inherit | AuthConfig::None => {}
    }
    for header in request.headers.iter().filter(|header| header.enabled) {
        let name = header.key.trim().to_ascii_lowercase();
        let credential = matches!(
            name.as_str(),
            "authorization"
                | "proxy-authorization"
                | "cookie"
                | "x-api-key"
                | "api-key"
                | "x-goog-api-key"
                | "x-auth-token"
        ) || crate::secrets::is_secret_header_name(&name);
        if !credential {
            continue;
        }
        let value = resolve(&header.value);
        if let Some((scheme, token)) = value.split_once(char::is_whitespace)
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            push(token.trim().to_owned());
        }
        push(value);
    }
    let credential_param = |name: &str| {
        matches!(
            name.trim().to_ascii_lowercase().as_str(),
            "key"
                | "token"
                | "api_key"
                | "apikey"
                | "access_token"
                | "password"
                | "secret"
                | "client_secret"
                | "signature"
                | "sig"
        )
    };
    for row in request.query_params.iter().filter(|row| row.enabled) {
        if credential_param(&row.key) {
            push(resolve(&row.value));
        }
    }
    let url = resolve(&request.url);
    if let Some(query) = url.split_once('?').map(|(_, query)| query) {
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if credential_param(&name) {
                push(value.into_owned());
            }
        }
    }
    if let Some(password) = url
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default())
        .and_then(|authority| authority.rsplit_once('@'))
        .and_then(|(userinfo, _)| userinfo.split_once(':'))
        .map(|(_, password)| password.to_owned())
    {
        push(password);
    }
    values
}

/// The hosts `environment` itself names (its plain variable values and the user's additions),
/// without the workspace-wide ones (saved requests, mock servers, gateway): what makes a host
/// belong to that environment.
#[must_use]
pub fn environment_hosts(environment: &Environment) -> Vec<String> {
    collect_declared_hosts(Some(environment), &[], &HashSet::new(), &[], &[], 0)
        .into_iter()
        .filter(|entry| matches!(entry.source, HostSource::Variable | HostSource::Explicit))
        .map(|entry| entry.host)
        .collect()
}

/// Whether `target` (`host[:port]`) is one of `hosts`: the exact `host:port`, or the bare host.
#[must_use]
pub fn host_listed<S: AsRef<str>>(hosts: &[S], target: &str) -> bool {
    let bare = match target.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => target,
    };
    hosts
        .iter()
        .any(|host| host.as_ref() == target || host.as_ref() == bare)
}

/// Refuses a request that carries a secret variable, a credential or a local file to a host its
/// environment does not declare.
///
/// `request` is the request as it will be sent (scripts and inherited auth applied) and `scopes`
/// the variables it resolves against. When any `{{token}}` of it (URL, path variables, query,
/// headers, body, auth) resolves to a secret variable, or [`carried_credential`] finds a stored
/// OAuth 2.0 token, a non-empty auth credential or a body file, the target host (the
/// resolved URL's, and for OAuth 2.0 the token endpoint's) must be declared; a host that cannot
/// be determined (a `{{variable}}` still in it) counts as undeclared.
///
/// Returns the declared hosts (`host` or `host:port`), which the request's redirect hops of an
/// agent send must stay within whether or not it carries a secret.
///
/// # Errors
/// [`AppError::Validation`] naming the first secret token, or what else is carried, when a target
/// host is not declared.
pub fn ensure_secret_hosts_declared(
    state: &AppState,
    request: &Request,
    environment: Option<&Environment>,
    scopes: &VariableScopes,
    untrusted_request_ids: &HashSet<String>,
) -> Result<Vec<String>, AppError> {
    let carried = carried_secret(request, scopes);
    let declared = declared_hosts(
        state,
        &request.workspace_id,
        environment,
        untrusted_request_ids,
    )?;
    let declared: HashSet<&str> = declared.iter().map(|entry| entry.host.as_str()).collect();

    if let Some(carried) = carried {
        let mut targets = vec![resolve_template_lenient(&request.url, scopes)];
        if let AuthConfig::OAuth2 {
            access_token_url, ..
        } = &request.auth
        {
            targets.push(resolve_template_lenient(access_token_url, scopes));
        }
        let allowed = targets
            .iter()
            .all(|url| host_of_url(url).is_some_and(|host| is_declared(&declared, &host)));
        if !allowed {
            return Err(AppError::validation(format!(
                "{carried} can only be sent to hosts this environment declares"
            )));
        }
    }
    Ok(declared.iter().map(|host| (*host).to_owned()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::FormPart;

    fn request_with(auth: AuthConfig, body: RequestBody) -> Request {
        let mut request = Request::new("r", "ws", "R", "https://api.example.test").unwrap();
        request.auth = auth;
        request.body = body;
        request
    }

    #[test]
    fn a_request_carries_a_credential_when_its_auth_has_a_filled_field_or_its_body_uploads_a_file()
    {
        let none = RequestBody::None;
        let carried = |auth, body| carried_credential(&request_with(auth, body));

        assert_eq!(carried(AuthConfig::None, RequestBody::None), None);
        assert_eq!(carried(AuthConfig::Inherit, RequestBody::None), None);
        assert_eq!(
            carried(
                AuthConfig::Bearer {
                    token_ref: " ".into()
                },
                RequestBody::None
            ),
            None
        );
        assert!(
            carried(
                AuthConfig::Bearer {
                    token_ref: "abc".into()
                },
                RequestBody::None
            )
            .is_some()
        );
        assert!(
            carried(
                AuthConfig::Basic {
                    username: "me".into(),
                    password_ref: String::new()
                },
                RequestBody::None
            )
            .is_some()
        );
        assert!(
            carried(
                AuthConfig::ApiKey {
                    key: "X-Key".into(),
                    value_ref: "k".into(),
                    location: crate::workspace::models::ApiKeyLocation::Header
                },
                RequestBody::None
            )
            .is_some()
        );
        assert!(
            carried(
                AuthConfig::Digest {
                    username: String::new(),
                    password_ref: Some("pw".into())
                },
                RequestBody::None
            )
            .is_some()
        );
        assert!(
            carried(
                AuthConfig::AwsSignatureV4 {
                    access_key: "AKIA".into(),
                    secret_key_ref: None,
                    region: "eu".into(),
                    service: "s3".into(),
                    session_token_ref: None
                },
                RequestBody::None
            )
            .is_some()
        );
        assert!(
            carried(
                AuthConfig::OAuth2 {
                    grant_type: crate::workspace::OAuth2GrantType::AuthorizationCodePkce,
                    access_token_url: "https://auth.example.test/token".into(),
                    client_id: "id".into(),
                    client_secret_ref: String::new(),
                    username: String::new(),
                    password_ref: String::new(),
                    scope: String::new(),
                    authorization_url: "https://auth.example.test/authorize".into(),
                    redirect_port: None,
                    client_auth: Default::default(),
                    extra_params: Vec::new(),
                },
                none
            )
            .is_some()
        );
        assert!(
            carried(
                AuthConfig::None,
                RequestBody::Binary {
                    file_path: "/etc/passwd".into(),
                    content_type: None
                }
            )
            .is_some()
        );
        let part = |file_path: Option<&str>, enabled| FormPart {
            key: "f".into(),
            value: String::new(),
            file_path: file_path.map(Into::into),
            enabled,
        };
        let multipart = |parts| RequestBody::Multipart { parts };
        assert!(carried(AuthConfig::None, multipart(vec![part(Some("/a"), true)])).is_some());
        assert_eq!(
            carried(
                AuthConfig::None,
                multipart(vec![part(Some("/a"), false), part(None, true)])
            ),
            None
        );
    }
}
