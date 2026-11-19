//! Request code-snippet generation: cURL, JavaScript, Python, Go, Java, C#, Ruby and PHP.
//!
//! Single responsibility: turn one [`Request`] into copyable client code, so a UI shell
//! re-implements none of it. [`request_code_snippet`] renders a request as given (the shared
//! fixture pins it). [`request_code_snippet_impl`], the command path, first resolves inherited
//! auth through the request's folder chain and collection, then applies the one credential rule
//! set of `redact`: template mode keeps `{{variables}}` and turns literal credentials into
//! `{{placeholders}}`; values mode (the UI default) expands plain variables to the chosen
//! environment's values and writes secret variables and literal credentials as shell variables
//! (`"$TOKEN"`), so no secret value reaches a snippet in either mode. Everything else follows the
//! executor's wire rules: enabled headers, auth headers (an explicit header of the same name
//! wins), the implicit `Content-Type`, path variables, query overrides, and the rule that a
//! GET/HEAD body is sent only when non-empty. OAuth 2.0 fetches its token at send time, so a
//! snippet sends `Authorization: Bearer` with an access-token placeholder. cURL also carries the
//! request's own transport options (`-L`, `-k`, `--max-time`, `--proxy` without its password,
//! the HTTP version); options that follow the global settings are left to curl's defaults.
//! A provider-target AI request has neither a real URL nor a readable credential, so
//! [`request_code_snippet_impl`] renders the plain HTTP export the core sends instead: the
//! provider's endpoint URL and `$PROVIDER_API_KEY` in the credential header, never a key.
//! This module builds the neutral [`Prepared`] request; `languages` renders it.

mod languages;
mod redact;
#[cfg(test)]
mod tests;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use url::form_urlencoded;

use crate::app::AppState;
use crate::app::execution_service::{ExecutionContext, execution_context};
use crate::app::request_model_service::URI_COMPONENT;
use crate::error::AppError;
use crate::executor::{substitute_path_variables, with_default_scheme};
use crate::llm::ai_run::missing_endpoint;
use crate::llm::wire::endpoint_url;
use crate::platform::portable_file_name;
use crate::workspace::ai_request::{AiTarget, RequestMode, plain_http_headers};
use crate::workspace::models::{HttpVersionPreference, NetworkField, NetworkOptions};
use crate::workspace::{
    ApiKeyLocation, AuthConfig, HttpMethod, KeyValue, ProtocolKind, Request, RequestBody,
    effective_auth,
};
use pingdeck_base::url_encode::encode_path_segment;

/// Where a provider-target snippet puts the key: the shell variable the gateway log uses,
/// never the key itself.
pub const PROVIDER_KEY_VARIABLE: &str = crate::llm::sniff::CURL_KEY_VARIABLE;

/// How a snippet treats variables.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnippetMode {
    /// `{{variables}}` as written; literal credentials as `{{placeholders}}`.
    #[default]
    Template,
    /// Plain variables expanded to the environment's values; secret variables and literal
    /// credentials as shell variables (`$TOKEN`).
    Values,
}

/// The optional third argument of the snippet command. Absent: template mode, no environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnippetOptions {
    pub mode: SnippetMode,
    /// The environment values mode reads; `None`: globals and collection variables only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
}

/// A language the snippet menu offers. Serialized as its lowercase id (`"curl"`, `"csharp"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnippetLanguage {
    Curl,
    Fetch,
    Axios,
    Python,
    Go,
    Java,
    Csharp,
    Ruby,
    Php,
}

impl SnippetLanguage {
    /// Every language, in menu order.
    pub const ALL: [Self; 9] = [
        Self::Curl,
        Self::Fetch,
        Self::Axios,
        Self::Python,
        Self::Go,
        Self::Java,
        Self::Csharp,
        Self::Ruby,
        Self::Php,
    ];

    /// The wire id (identical to the serde name).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Curl => "curl",
            Self::Fetch => "fetch",
            Self::Axios => "axios",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
            Self::Csharp => "csharp",
            Self::Ruby => "ruby",
            Self::Php => "php",
        }
    }

    /// The menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Curl => "cURL",
            Self::Fetch => "JavaScript fetch",
            Self::Axios => "JavaScript axios",
            Self::Python => "Python requests",
            Self::Go => "Go net/http",
            Self::Java => "Java HttpClient",
            Self::Csharp => "C# HttpClient",
            Self::Ruby => "Ruby Net::HTTP",
            Self::Php => "PHP cURL",
        }
    }
}

/// One entry of the snippet language menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetLanguageInfo {
    /// Value to pass back as the `language` of [`request_code_snippet`].
    pub id: String,
    /// Menu label.
    pub label: String,
}

/// The snippet language menu, in display order.
#[must_use]
pub fn snippet_languages() -> Vec<SnippetLanguageInfo> {
    SnippetLanguage::ALL
        .iter()
        .map(|language| SnippetLanguageInfo {
            id: language.id().to_owned(),
            label: language.label().to_owned(),
        })
        .collect()
}

/// Renders `request` as `language` client code.
///
/// `request` must already carry its effective (inherited) auth. WebSocket and gRPC requests have
/// no HTTP client equivalent and fail with [`AppError::Unsupported`].
pub fn request_code_snippet(
    request: &Request,
    language: SnippetLanguage,
) -> Result<String, AppError> {
    if matches!(
        request.protocol,
        ProtocolKind::WebSocket | ProtocolKind::Grpc
    ) {
        return Err(AppError::unsupported(
            "Code snippets are available for HTTP and GraphQL requests only",
        ));
    }
    Ok(languages::render(&prepare(request), language))
}

/// [`request_code_snippet`] with the store behind it: a provider-target AI request renders as the
/// plain HTTP request it stands for — the provider's endpoint URL and
/// [`PROVIDER_KEY_VARIABLE`] in the credential header, never a key. Every other request renders
/// as it is.
///
/// # Errors
/// As [`request_code_snippet`]; [`AppError::NotFound`] when the provider is gone,
/// [`AppError::Unsupported`] when it has no endpoint for the request's protocol, and
/// [`AppError::Validation`] when its endpoint URL cannot be built.
pub fn request_code_snippet_impl(
    state: &AppState,
    request: &Request,
    language: SnippetLanguage,
    options: &SnippetOptions,
) -> Result<String, AppError> {
    if let Some(plain) = provider_export(state, request)? {
        return request_code_snippet(&plain, language);
    }
    if matches!(
        request.protocol,
        ProtocolKind::WebSocket | ProtocolKind::Grpc
    ) {
        return request_code_snippet(request, language);
    }
    // A collection or folder deleted under an open draft leaves nothing to inherit or expand:
    // the snippet still renders, with no inherited auth and variables as written.
    let context = {
        let store = state.store();
        execution_context(&store, request, options.environment_id.as_deref()).ok()
    };
    let mut resolved = request.clone();
    resolved.auth = match &context {
        Some(context) => effective_auth(request, context.collection.as_ref(), &context.folders),
        None if matches!(request.auth, AuthConfig::Inherit) => AuthConfig::None,
        None => request.auth.clone(),
    };
    let scopes = context.as_ref().map(ExecutionContext::scopes);
    let mut redactor = redact::Redactor::new(options.mode, scopes.as_ref());
    redactor.apply(&mut resolved);
    // Values mode: the token OAuth 2.0 fetches at send time is a shell variable too.
    if options.mode == SnippetMode::Values && matches!(resolved.auth, AuthConfig::OAuth2 { .. }) {
        let var = format!("${}", OAUTH2_TOKEN_NAME.to_ascii_uppercase());
        if !redactor.shell_vars.contains(&var) {
            redactor.shell_vars.push(var.clone());
        }
        resolved.auth = AuthConfig::Bearer { token_ref: var };
    }
    let mut prepared = prepare(&resolved);
    prepared.shell_vars = redactor.shell_vars;
    Ok(languages::render(&prepared, language))
}

/// The plain HTTP request a provider-target AI request stands for, or `None` for any other
/// request. The store's provider record supplies what the request itself cannot: base URL,
/// endpoint path and the key's place (the request holds only a non-routable label URL).
fn provider_export(state: &AppState, request: &Request) -> Result<Option<Request>, AppError> {
    let Some(ai) = request
        .ai
        .as_ref()
        .filter(|_| request.mode == RequestMode::AiApi)
    else {
        return Ok(None);
    };
    let AiTarget::Provider {
        provider_id, model, ..
    } = &ai.target
    else {
        return Ok(None);
    };
    let provider = {
        let store = state.store();
        crate::llm::store::get_provider(store.connection(), provider_id)?
    }
    .ok_or_else(|| AppError::not_found(format!("provider {provider_id} not found")))?;
    let endpoint = provider
        .endpoints
        .iter()
        .find(|endpoint| endpoint.protocol == ai.protocol)
        .ok_or_else(|| AppError::unsupported(missing_endpoint(&provider.name, ai.protocol)))?;
    let mut plain = request.clone();
    plain.mode = RequestMode::Http;
    plain.ai = None;
    plain.url = endpoint_url(
        &provider.base_url,
        &endpoint.path_prefix,
        endpoint.url_mode,
        ai.protocol,
        Some(model),
        false,
    )?;
    plain.headers = plain_http_headers(&request.headers, ai.protocol, PROVIDER_KEY_VARIABLE);
    plain.auth = AuthConfig::None;
    // A provider run sends no query or path overrides: the destination is the endpoint itself.
    plain.query_params.clear();
    plain.path_variables.clear();
    Ok(Some(plain))
}

/// The request body as a client sends it.
enum Body {
    None,
    Text(String),
    /// A file sent verbatim (path as typed, possibly containing `{{variables}}`).
    File(String),
    Multipart(Vec<Part>),
}

/// One multipart field.
enum Part {
    Text { key: String, value: String },
    File { key: String, path: String },
}

impl Part {
    /// The filename a file part is announced with.
    fn file_name(path: &str) -> &str {
        portable_file_name(path)
    }
}

/// A request reduced to what every language needs.
struct Prepared {
    /// Upper-case method name.
    method: String,
    url: String,
    /// Wire headers in send order: explicit, auth, then the implicit `Content-Type`. A multipart
    /// request carries no `Content-Type`: each client generates it with its own boundary.
    headers: Vec<(String, String)>,
    body: Body,
    /// `(username, password)` of Digest auth; only cURL can express the challenge exchange.
    digest: Option<(String, String)>,
    /// Shell variables (`$TOKEN`) a values-mode snippet wrote for credentials; cURL keeps them
    /// outside single quotes so the shell expands them.
    shell_vars: Vec<String>,
    /// cURL transport options from the request's own network options: short flags written on
    /// the command line (`-L`, `-k`) and long options on lines of their own.
    curl_flags: Vec<&'static str>,
    curl_options: Vec<String>,
}

/// Name of the OAuth 2.0 access-token placeholder (`{{access_token}}`, `$ACCESS_TOKEN`).
const OAUTH2_TOKEN_NAME: &str = "access_token";

/// The cURL flags and options of `options`, leaving out fields that follow the global settings
/// (curl's own defaults stand in for them). The proxy loses its password.
fn curl_transport(options: &NetworkOptions) -> (Vec<&'static str>, Vec<String>) {
    let own = |field: NetworkField| !options.inherit.contains(&field);
    let mut flags = Vec::new();
    let mut lines = Vec::new();
    if own(NetworkField::FollowRedirects) && options.follow_redirects {
        flags.push("-L");
        if own(NetworkField::MaxRedirects)
            && options.max_redirects != NetworkOptions::default().max_redirects
        {
            lines.push(format!("--max-redirs {}", options.max_redirect_hops()));
        }
    }
    if own(NetworkField::VerifyTls) && !options.verify_tls {
        flags.push("-k");
    }
    if let Some(ms) = options.total_timeout_ms.filter(|ms| *ms > 0) {
        let seconds = if ms % 1000 == 0 {
            (ms / 1000).to_string()
        } else {
            format!("{}.{:03}", ms / 1000, ms % 1000)
                .trim_end_matches('0')
                .to_owned()
        };
        lines.push(format!("--max-time {seconds}"));
    }
    if own(NetworkField::ProxyUrl)
        && let Some(proxy) = options
            .proxy_url
            .as_deref()
            .filter(|proxy| !proxy.trim().is_empty())
    {
        let proxy = pingdeck_base::proxy_url::without_password(proxy.trim());
        lines.push(format!("--proxy '{}'", proxy.replace('\'', "'\\''")));
    }
    if own(NetworkField::HttpVersion) {
        match options.http_version {
            HttpVersionPreference::Auto => {}
            HttpVersionPreference::Http10 => lines.push("--http1.0".to_owned()),
            HttpVersionPreference::Http11 => lines.push("--http1.1".to_owned()),
            HttpVersionPreference::Http2 => lines.push("--http2".to_owned()),
        }
    }
    (flags, lines)
}

impl Prepared {
    /// The effective `Content-Type`, if any.
    fn content_type(&self) -> Option<&str> {
        header_value(&self.headers, "content-type")
    }
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPTIONS",
    }
}

fn prepare(request: &Request) -> Prepared {
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .filter(|header| header.enabled && !header.key.trim().is_empty())
        .map(|header| (header.key.clone(), header.value.clone()))
        .collect();
    let explicit = |name: &str| header_value(&headers, name).is_some();

    let mut auth_headers = Vec::new();
    let mut api_key_query = None;
    let mut digest = None;
    match &request.auth {
        AuthConfig::Bearer { token_ref } if !token_ref.is_empty() && !explicit("authorization") => {
            auth_headers.push(("Authorization".to_owned(), format!("Bearer {token_ref}")));
        }
        AuthConfig::Basic {
            username,
            password_ref,
        } if !username.is_empty() && !password_ref.is_empty() && !explicit("authorization") => {
            auth_headers.push((
                "Authorization".to_owned(),
                format!(
                    "Basic {}",
                    BASE64_STANDARD.encode(format!("{username}:{password_ref}"))
                ),
            ));
        }
        AuthConfig::ApiKey {
            key,
            value_ref,
            location,
        } if !key.is_empty() && !value_ref.is_empty() => match location {
            ApiKeyLocation::Header if !explicit(key) => {
                auth_headers.push((key.clone(), value_ref.clone()));
            }
            ApiKeyLocation::Query => api_key_query = Some((key.clone(), value_ref.clone())),
            ApiKeyLocation::Header => {}
        },
        AuthConfig::AwsSignatureV4 {
            access_key,
            region,
            service,
            session_token_ref,
            ..
        } => {
            // The real signature depends on the send time, so the snippet shows the shape only.
            if !explicit("authorization") {
                auth_headers.push((
                    "Authorization".to_owned(),
                    format!(
                        "AWS4-HMAC-SHA256 Credential={}/<date>/{}/{}/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=<signature>",
                        or_default(access_key, "<access-key>"),
                        or_default(region, "us-east-1"),
                        or_default(service, "s3"),
                    ),
                ));
            }
            if let Some(token) = session_token_ref
                .as_deref()
                .filter(|token| !token.is_empty())
                && !explicit("x-amz-security-token")
            {
                auth_headers.push(("X-Amz-Security-Token".to_owned(), token.to_owned()));
            }
        }
        AuthConfig::Digest {
            username,
            password_ref,
        } => {
            digest = Some((username.clone(), password_ref.clone().unwrap_or_default()));
        }
        // The token is fetched at send time: the snippet names it with a placeholder.
        AuthConfig::OAuth2 { .. } if !explicit("authorization") => {
            auth_headers.push((
                "Authorization".to_owned(),
                format!("Bearer {{{{{OAUTH2_TOKEN_NAME}}}}}"),
            ));
        }
        // Inherit was resolved by the caller.
        _ => {}
    }
    headers.extend(auth_headers);

    let (body, implicit_content_type) = prepare_body(request);
    if matches!(body, Body::Multipart(_)) {
        headers.retain(|(key, _)| !key.eq_ignore_ascii_case("content-type"));
    } else if let Some(content_type) = implicit_content_type
        && header_value(&headers, "content-type").is_none()
    {
        headers.push(("Content-Type".to_owned(), content_type));
    }

    let (curl_flags, curl_options) = curl_transport(&request.network_options);
    Prepared {
        method: method_name(request.method).to_owned(),
        url: request_url(request, api_key_query),
        headers,
        body,
        digest,
        shell_vars: Vec::new(),
        curl_flags,
        curl_options,
    }
}

fn or_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.is_empty() { default } else { value }
}

/// The body plus the `Content-Type` the executor adds when no header sets one.
fn prepare_body(request: &Request) -> (Body, Option<String>) {
    // GET/HEAD carry a body only when the editor holds actual content for one.
    let skip_empty = matches!(request.method, HttpMethod::Get | HttpMethod::Head);
    let text = |content: String, content_type: &str| {
        if skip_empty && content.is_empty() {
            (Body::None, None)
        } else {
            (Body::Text(content), Some(content_type.to_owned()))
        }
    };
    match &request.body {
        RequestBody::None => (Body::None, None),
        RequestBody::Json { content } => text(content.clone(), "application/json"),
        RequestBody::RawText { content, .. } => text(content.clone(), "text/plain; charset=utf-8"),
        RequestBody::FormUrlEncoded { fields } => text(
            fields
                .iter()
                .filter(|field| field.enabled)
                .map(|field| {
                    format!(
                        "{}={}",
                        encode_component(&field.key),
                        encode_component(&field.value)
                    )
                })
                .collect::<Vec<_>>()
                .join("&"),
            "application/x-www-form-urlencoded",
        ),
        RequestBody::Graphql {
            query,
            variables_json,
            operation_name,
        } => (
            Body::Text(graphql_payload(
                query,
                variables_json.as_deref(),
                operation_name.as_deref(),
            )),
            Some("application/json".to_owned()),
        ),
        RequestBody::Multipart { parts } => {
            let parts: Vec<Part> = parts
                .iter()
                .filter(|part| part.enabled && !part.key.trim().is_empty())
                .map(|part| {
                    let key = part.key.trim().to_owned();
                    match part.file_path.as_deref() {
                        Some(path) if !path.trim().is_empty() => Part::File {
                            key,
                            path: path.to_owned(),
                        },
                        _ => Part::Text {
                            key,
                            value: part.value.clone(),
                        },
                    }
                })
                .collect();
            // shortcut: a multipart body without a usable part shows as no body at all (the
            // executor would send an empty multipart); upgrade if that distinction matters.
            if parts.is_empty() {
                (Body::None, None)
            } else {
                (Body::Multipart(parts), None)
            }
        }
        RequestBody::Binary {
            file_path,
            content_type,
        } => {
            if file_path.trim().is_empty() {
                return (Body::None, None);
            }
            let content_type = content_type
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("application/octet-stream");
            (Body::File(file_path.clone()), Some(content_type.to_owned()))
        }
    }
}

/// The GraphQL JSON payload, pretty-printed (`query`, then `variables`, then `operationName`).
///
/// Unlike the executor, invalid variables JSON is embedded as a plain string instead of failing:
/// a snippet must stay available while the variables are being typed (or hold `{{variables}}`).
fn graphql_payload(
    query: &str,
    variables_json: Option<&str>,
    operation_name: Option<&str>,
) -> String {
    let mut fields = vec![format!("  \"query\": {}", languages::json_string(query))];
    if let Some(variables) = variables_json.filter(|value| !value.trim().is_empty()) {
        let value = match serde_json::from_str::<serde_json::Value>(variables) {
            Ok(parsed) => serde_json::to_string_pretty(&parsed)
                .unwrap_or_default()
                .replace('\n', "\n  "),
            Err(_) => languages::json_string(variables),
        };
        fields.push(format!("  \"variables\": {value}"));
    }
    if let Some(name) = operation_name.filter(|value| !value.trim().is_empty()) {
        fields.push(format!(
            "  \"operationName\": {}",
            languages::json_string(name)
        ));
    }
    format!("{{\n{}\n}}", fields.join(",\n"))
}

/// Applies `f` to every stretch of `value` outside `{{...}}` tokens; tokens stay verbatim.
fn map_outside_tokens(value: &str, f: impl Fn(&str) -> String) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("{{") {
        let Some(length) = rest[start + 2..].find("}}") else {
            break;
        };
        let end = start + 2 + length + 2;
        output.push_str(&f(&rest[..start]));
        output.push_str(&rest[start..end]);
        rest = &rest[end..];
    }
    output.push_str(&f(rest));
    output
}

/// `encodeURIComponent` that leaves `{{variables}}` readable.
fn encode_component(value: &str) -> String {
    map_outside_tokens(value, |literal| {
        percent_encoding::utf8_percent_encode(literal, URI_COMPONENT).to_string()
    })
}

/// The URL as sent: path variables, default scheme, query params as overrides, API-key query.
fn request_url(request: &Request, api_key_query: Option<(String, String)>) -> String {
    let encode_values = request.network_options.encode_url_automatically;
    let path_values: Vec<(String, String)> = request
        .path_variables
        .iter()
        .filter(|variable| variable.enabled && !variable.key.trim().is_empty())
        // An empty value keeps the `:id` token in place, as the executor does.
        .filter(|variable| !variable.value.is_empty())
        .map(|variable| {
            let value = if encode_values {
                map_outside_tokens(&variable.value, encode_path_segment)
            } else {
                variable.value.clone()
            };
            (variable.key.trim().to_owned(), value)
        })
        .collect();
    // shortcut: a `{{variable}}` inside the URL is not resolved, so a path variable named like
    // one would replace the inner `{name}` of `{{name}}`; upgrade by skipping `{{` tokens.
    let mut url = substitute_path_variables(&request.url, &path_values);
    // A leading `{{baseUrl}}` may supply the scheme itself, so only literal hosts get `http://`.
    if !url.trim_start().starts_with("{{") {
        url = with_default_scheme(&url);
    }

    let params: Vec<(String, String)> = request
        .query_params
        .iter()
        .filter(|param: &&KeyValue| param.enabled && !param.key.trim().is_empty())
        .map(|param| (param.key.clone(), param.value.clone()))
        .collect();
    url = append_query(&url, &params, true);
    if let Some((key, value)) = api_key_query
        && !query_keys(&url).contains(&key)
    {
        url = append_query(&url, &[(key, value)], false);
    }
    url
}

/// Splits `url` into `(without fragment, fragment including '#')`.
fn split_fragment(url: &str) -> (&str, &str) {
    url.find('#').map_or((url, ""), |index| url.split_at(index))
}

/// The decoded key of a raw `key=value` query pair.
fn query_key(pair: &str) -> String {
    let raw = pair.split('=').next().unwrap_or_default();
    form_urlencoded::parse(raw.as_bytes())
        .next()
        .map(|(key, _)| key.into_owned())
        .unwrap_or_default()
}

/// Decoded keys of the URL's query.
fn query_keys(url: &str) -> Vec<String> {
    let (without_fragment, _) = split_fragment(url);
    without_fragment
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(query_key)
        .collect()
}

/// Appends `pairs` to the query. With `replace`, existing pairs of the same (decoded) key are
/// dropped first, so the Params tab overrides the URL bar instead of duplicating keys.
fn append_query(url: &str, pairs: &[(String, String)], replace: bool) -> String {
    if pairs.is_empty() {
        return url.to_owned();
    }
    let (without_fragment, fragment) = split_fragment(url);
    let (base, query) = without_fragment
        .split_once('?')
        .unwrap_or((without_fragment, ""));
    let kept = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| !replace || !pairs.iter().any(|(key, _)| *key == query_key(pair)))
        .map(str::to_owned);
    let added = pairs
        .iter()
        .map(|(key, value)| format!("{}={}", encode_component(key), encode_component(value)));
    let query = kept.chain(added).collect::<Vec<_>>().join("&");
    format!("{base}?{query}{fragment}")
}
