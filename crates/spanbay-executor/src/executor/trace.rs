//! Call-chain trace construction: resolved variables, auth summary and transport details.

use crate::executor::client::{disabled_tls_version_key, matching_client_certificate};
use crate::executor::types::ExecutorOptions;
use reqwest::Version as HttpVersion;
use spanbay_base::proxy_url::ProxyRoute;
use spanbay_model::models::{
    AuthTraceSummary, RedirectHopTrace, ResolvedVariableTrace, TlsCertificateKind,
    TransportTraceSummary,
};
use spanbay_model::{
    ApiKeyLocation, AuthConfig, KeyValue, OAuth2GrantType, Request, RequestBody, VariableScopes,
};
use url::Url;

/// Replacement placed over a secret variable value in the call-chain trace.
const MASKED_VARIABLE: &str = "••••••••";

/// `HTTP/1.1`, `HTTP/2.0`, ... for the call-chain transport summary.
pub(super) fn version_label(version: HttpVersion) -> &'static str {
    match version {
        HttpVersion::HTTP_09 => "HTTP/0.9",
        HttpVersion::HTTP_10 => "HTTP/1.0",
        HttpVersion::HTTP_2 => "HTTP/2.0",
        HttpVersion::HTTP_3 => "HTTP/3.0",
        _ => "HTTP/1.1",
    }
}

/// Every `{{token}}` the outgoing request mentions, with where its value came
/// from and whether the value is secret.
pub(super) fn collect_variable_traces(
    request: &Request,
    scopes: &VariableScopes,
) -> Vec<ResolvedVariableTrace> {
    let mut names: Vec<String> = Vec::new();
    let mut collect = |text: &str| {
        for name in template_tokens(text) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    };

    collect(&request.url);
    for row in request
        .path_variables
        .iter()
        .chain(request.query_params.iter())
        .chain(request.headers.iter())
    {
        collect(&row.key);
        collect(&row.value);
    }
    for text in body_templates(&request.body) {
        collect(text);
    }
    for text in auth_templates(&request.auth) {
        collect(text);
    }

    names
        .into_iter()
        .map(|name| {
            let (value, source, secret) = scopes.resolve_trace(&name);
            ResolvedVariableTrace {
                resolved_value: if secret {
                    MASKED_VARIABLE.to_owned()
                } else {
                    value.unwrap_or_default()
                },
                source,
                secret,
                name,
            }
        })
        .collect()
}

/// Splits `text` into the names of its `{{name}}` tokens.
pub fn template_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cursor = 0;

    while let Some(offset) = text[cursor..].find("{{") {
        let start = cursor + offset + 2;
        let Some(end_offset) = text[start..].find("}}") else {
            break;
        };
        let end = start + end_offset;
        let token = text[start..end].trim();
        if !token.is_empty() {
            tokens.push(token.to_owned());
        }
        cursor = end + 2;
    }

    tokens
}

/// The text parts of a request body that may hold `{{tokens}}`.
pub fn body_templates(body: &RequestBody) -> Vec<&str> {
    match body {
        RequestBody::None => Vec::new(),
        RequestBody::RawText { content, .. } | RequestBody::Json { content } => vec![content],
        RequestBody::FormUrlEncoded { fields } => fields
            .iter()
            .flat_map(|field| [field.key.as_str(), field.value.as_str()])
            .collect(),
        RequestBody::Multipart { parts } => parts
            .iter()
            .flat_map(|part| {
                [
                    part.key.as_str(),
                    part.value.as_str(),
                    part.file_path.as_deref().unwrap_or(""),
                ]
            })
            .collect(),
        RequestBody::Graphql {
            query,
            variables_json,
            operation_name,
        } => vec![
            query.as_str(),
            variables_json.as_deref().unwrap_or(""),
            operation_name.as_deref().unwrap_or(""),
        ],
        RequestBody::Binary {
            file_path,
            content_type,
        } => vec![file_path.as_str(), content_type.as_deref().unwrap_or("")],
    }
}

/// The credential fields of an auth config that may hold `{{tokens}}`.
pub fn auth_templates(auth: &AuthConfig) -> Vec<&str> {
    match auth {
        AuthConfig::Inherit | AuthConfig::None => Vec::new(),
        AuthConfig::Bearer { token_ref } => vec![token_ref.as_str()],
        AuthConfig::Basic {
            username,
            password_ref,
        } => vec![username.as_str(), password_ref.as_str()],
        AuthConfig::ApiKey { key, value_ref, .. } => vec![key.as_str(), value_ref.as_str()],
        AuthConfig::OAuth2 {
            access_token_url,
            client_id,
            client_secret_ref,
            username,
            password_ref,
            scope,
            authorization_url,
            ..
        } => vec![
            access_token_url.as_str(),
            client_id.as_str(),
            client_secret_ref.as_str(),
            username.as_str(),
            password_ref.as_str(),
            scope.as_str(),
            authorization_url.as_str(),
        ],
        AuthConfig::Digest {
            username,
            password_ref,
        } => vec![username.as_str(), password_ref.as_deref().unwrap_or("")],
        AuthConfig::AwsSignatureV4 {
            access_key,
            secret_key_ref,
            region,
            service,
            session_token_ref,
        } => vec![
            access_key.as_str(),
            secret_key_ref.as_deref().unwrap_or(""),
            region.as_str(),
            service.as_str(),
            session_token_ref.as_deref().unwrap_or(""),
        ],
    }
}

/// Describes the credential the executor received, without ever exposing it.
pub(super) fn auth_trace(auth: &AuthConfig) -> AuthTraceSummary {
    let mut details: Vec<KeyValue> = Vec::new();
    let (auth_type, applied_to) = match auth {
        AuthConfig::Inherit | AuthConfig::None => ("none", "None".to_owned()),
        AuthConfig::Bearer { .. } => ("bearer", "Header: Authorization (Bearer)".to_owned()),
        AuthConfig::Basic { username, .. } => {
            details.push(KeyValue {
                key: "username".to_owned(),
                value: username.clone(),
                enabled: true,
                description: None,
            });
            ("basic", "Header: Authorization (Basic)".to_owned())
        }
        AuthConfig::ApiKey {
            key,
            location: ApiKeyLocation::Header,
            ..
        } => {
            details.push(KeyValue {
                key: "header".to_owned(),
                value: key.clone(),
                enabled: true,
                description: None,
            });
            ("api_key", format!("Header: {key}"))
        }
        AuthConfig::ApiKey {
            key,
            location: ApiKeyLocation::Query,
            ..
        } => {
            details.push(KeyValue {
                key: "query".to_owned(),
                value: key.clone(),
                enabled: true,
                description: None,
            });
            ("api_key", format!("Query: {key}"))
        }
        AuthConfig::OAuth2 { grant_type, .. } => {
            details.push(KeyValue {
                key: "grant_type".to_owned(),
                value: match grant_type {
                    OAuth2GrantType::ClientCredentials => "client_credentials",
                    OAuth2GrantType::Password => "password",
                    OAuth2GrantType::AuthorizationCodePkce => "authorization_code_pkce",
                }
                .to_owned(),
                enabled: true,
                description: None,
            });
            ("oauth2", "Header: Authorization (OAuth 2.0)".to_owned())
        }
        AuthConfig::Digest { username, .. } => {
            details.push(KeyValue {
                key: "username".to_owned(),
                value: username.clone(),
                enabled: true,
                description: None,
            });
            (
                "digest",
                "Digest Challenge-Response (MD5/SHA-256)".to_owned(),
            )
        }
        AuthConfig::AwsSignatureV4 {
            region, service, ..
        } => {
            details.push(KeyValue {
                key: "service".to_owned(),
                value: service.clone(),
                enabled: true,
                description: None,
            });
            details.push(KeyValue {
                key: "region".to_owned(),
                value: region.clone(),
                enabled: true,
                description: None,
            });
            (
                "aws_sigv4",
                format!("AWS SigV4 Canonical Request (service={service}, region={region})"),
            )
        }
    };

    AuthTraceSummary {
        auth_type: auth_type.to_owned(),
        source: "request".to_owned(),
        applied_to,
        details,
    }
}

/// Replaces the password of a proxy URL with `****`, keeping scheme, user, host and port.
/// A scheme-less `user:pass@host:port` is read the way `reqwest` reads it (as `http://`).
/// A value that cannot be parsed is hidden entirely rather than risk leaking a credential.
pub(super) fn mask_proxy_credentials(proxy_url: &str) -> String {
    let has_scheme = proxy_url.contains("://");
    let candidate = if has_scheme {
        proxy_url.to_owned()
    } else {
        format!("http://{proxy_url}")
    };
    let Ok(mut parsed) = Url::parse(&candidate) else {
        return "****".to_owned();
    };
    if parsed.password().is_none() {
        return proxy_url.to_owned();
    }
    if parsed.set_password(Some("****")).is_err() {
        return "****".to_owned();
    }
    let masked = parsed.to_string();
    if has_scheme {
        masked
    } else {
        masked.strip_prefix("http://").unwrap_or(&masked).to_owned()
    }
}

/// Summarises the transport used for the request: protocol, proxy, remote address and certificates.
pub(super) fn transport_trace(
    request: &Request,
    options: &ExecutorOptions,
    url: &Url,
    protocol: &str,
    remote_addr: Option<String>,
    redirect_hops: Vec<RedirectHopTrace>,
) -> TransportTraceSummary {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let port = url.port_or_known_default().unwrap_or(0);
    let route = crate::executor::client::proxy_route(request, &options.auxiliary, &host);
    // Bypassed: the request follows Settings, which would proxy this host, but it is local or on
    // `no_proxy`.
    let proxy_bypassed = route == ProxyRoute::Direct
        && request
            .network_options
            .inherit
            .contains(&spanbay_model::models::NetworkField::ProxyUrl)
        && !options
            .auxiliary
            .proxy_url
            .trim()
            .eq_ignore_ascii_case(spanbay_base::proxy_url::DIRECT);
    let proxy_url = match &route {
        ProxyRoute::Proxy(url) => Some(mask_proxy_credentials(url)),
        _ => None,
    };

    TransportTraceSummary {
        protocol: protocol.to_owned(),
        remote_addr,
        proxy_bypassed,
        proxy_url,
        custom_ca_count: options
            .certificates
            .iter()
            .filter(|certificate| certificate.enabled && certificate.kind == TlsCertificateKind::Ca)
            .count(),
        client_cert_host: matching_client_certificate(&options.certificates, &host, port)
            .map(|certificate| certificate.host_pattern.clone()),
        tls_disabled_versions: request
            .network_options
            .disabled_tls_versions
            .iter()
            .map(disabled_tls_version_key)
            .map(str::to_owned)
            .collect(),
        redirect_hops,
    }
}
