//! Request preparation: URL, path and query variable resolution, method mapping and the request builder.

use crate::executor::auth::{append_api_key_query, auth_header_pairs, is_authorization_header};
use crate::executor::client::http_version;
use crate::executor::trace::{auth_templates, body_templates};
use crate::executor::types::{ExecutionError, ExecutionHeader};
use pingdeck_base::url_encode::encode_path_segment;
use pingdeck_model::{
    Environment, HttpMethod, KeyValue, Request, VariableResolutionError, VariableScopes,
    resolve_template,
};
use reqwest::header::{HeaderMap, HeaderName, LOCATION};
use reqwest::{Method, RequestBuilder};
use url::Url;

/// Request editors (and every API client) accept a bare `host[:port][/path]`.
/// `Url` reads `localhost:3001/x` as the *scheme* `localhost`, which then dies
/// in the HTTP client with an opaque builder error, so default to `http://`
/// unless the value already carries a `scheme://` prefix.
pub fn with_default_scheme(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return value.to_owned();
    }
    if let Some(rest) = value.strip_prefix("//") {
        return format!("http://{rest}");
    }
    if let Some((scheme, rest)) = value.split_once(':')
        && rest.starts_with("//")
        && !scheme.is_empty()
        && scheme.starts_with(|first: char| first.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    {
        return value.to_owned();
    }
    format!("http://{value}")
}

/// Resolves the request URL: default scheme, path variables and query parameters.
pub fn request_url(request: &Request, scopes: &VariableScopes) -> Result<Url, ExecutionError> {
    let encode_values = request.network_options.encode_url_automatically;
    let (before_fragment, fragment) = request.url.split_once('#').unwrap_or((&request.url, ""));
    let mut resolved_url = if let Some((base, query)) = before_fragment.split_once('?') {
        let mut resolved = resolve_value(base, scopes)?;
        resolved.push('?');
        // Encode interpolated values, while retaining the URL editor's literal encoding.
        let mut rest = query;
        while let Some(start) = rest.find("{{") {
            let Some(end) = rest[start + 2..].find("}}") else {
                break;
            };
            let end = start + 2 + end + 2;
            resolved.push_str(&rest[..start]);
            let value = resolve_value(&rest[start..end], scopes)?;
            if encode_values {
                resolved.extend(url::form_urlencoded::byte_serialize(value.as_bytes()));
            } else {
                resolved.push_str(&value);
            }
            rest = &rest[end..];
        }
        resolved.push_str(rest);
        resolved
    } else {
        resolve_value(before_fragment, scopes)?
    };
    if request.url.contains('#') {
        resolved_url.push('#');
        resolved_url.push_str(&resolve_value(fragment, scopes)?);
    }
    resolved_url = apply_path_variables(
        &resolved_url,
        &request.path_variables,
        scopes,
        encode_values,
    )?;
    let mut url = Url::parse(&with_default_scheme(&resolved_url))
        .map_err(|error| ExecutionError::InvalidUrl(error.to_string()))?;

    append_query_params(&mut url, &request.query_params, scopes, encode_values)?;
    append_api_key_query(&mut url, &request.auth, scopes)?;

    Ok(url)
}

/// Absolute redirect target for a redirect status with an http(s) `Location`.
pub(super) fn redirect_target(headers: &HeaderMap, url: &Url, status_code: u16) -> Option<Url> {
    if !matches!(status_code, 301 | 302 | 303 | 307 | 308) {
        return None;
    }

    let location = headers.get(LOCATION)?.to_str().ok()?;
    let target = url.join(location.trim()).ok()?;

    matches!(target.scheme(), "http" | "https").then_some(target)
}

/// Scheme, lowercase host and effective port identifying an origin.
pub(super) fn request_origin(url: &Url) -> (String, String, u16) {
    (
        url.scheme().to_owned(),
        url.host_str().unwrap_or_default().to_ascii_lowercase(),
        url.port_or_known_default().unwrap_or(0),
    )
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct RedirectHeaderPolicy {
    pub(super) strip_authorization: bool,
    pub(super) strip_cookies: bool,
    pub(super) strip_referer: bool,
}

/// Builds the `reqwest` request for one hop: headers, auth, cookies and the redirect header policy.
pub(super) fn build_request_builder(
    client: &reqwest::Client,
    request: &Request,
    scopes: &VariableScopes,
    url: &Url,
    method: Method,
    policy: RedirectHeaderPolicy,
) -> Result<(RequestBuilder, Option<String>, bool, Vec<ExecutionHeader>), ExecutionError> {
    let mut builder = client.request(method, url.clone());
    if let Some(version) = http_version(request.network_options.http_version) {
        builder = builder.version(version);
    }
    let mut content_type_header = None;
    let mut has_explicit_cookie_header = false;
    let mut explicit_header_names = Vec::new();
    let mut sent_headers = Vec::new();

    for header in request.headers.iter().filter(|header| header.enabled) {
        let key = resolve_value(&header.key, scopes)?;
        let value = resolve_value(&header.value, scopes)?;
        if key.eq_ignore_ascii_case("content-type") {
            content_type_header = Some(value.clone());
        }
        if policy.strip_authorization && is_authorization_header(&key) {
            continue;
        }
        if policy.strip_cookies && key.eq_ignore_ascii_case("cookie") {
            continue;
        }
        if policy.strip_referer && key.eq_ignore_ascii_case("referer") {
            continue;
        }
        if key.eq_ignore_ascii_case("cookie") {
            has_explicit_cookie_header = true;
        }
        explicit_header_names.push(key.clone());
        let header_name = HeaderName::from_bytes(key.as_bytes()).map_err(|error| {
            ExecutionError::InvalidHeader {
                key: key.clone(),
                message: error.to_string(),
            }
        })?;
        builder = builder.header(header_name, value.clone());
        sent_headers.push(ExecutionHeader { key, value });
    }

    // ponytail: auth is rebuilt per hop; cross-origin redirects suppress it unless
    // `follow_auth_header` keeps it (the explicit Authorization header is stripped by
    // the same flag).
    if !policy.strip_authorization {
        for (key, value) in auth_header_pairs(&request.auth, scopes, &explicit_header_names)? {
            let header_name = HeaderName::from_bytes(key.as_bytes()).map_err(|error| {
                ExecutionError::InvalidHeader {
                    key: key.clone(),
                    message: error.to_string(),
                }
            })?;
            builder = builder.header(header_name, value.clone());
            sent_headers.push(ExecutionHeader { key, value });
        }
    }

    Ok((
        builder,
        content_type_header,
        has_explicit_cookie_header,
        sent_headers,
    ))
}

/// Maps the workspace HTTP method to the `reqwest` method.
pub(super) fn request_method(method: HttpMethod) -> Method {
    match method {
        HttpMethod::Get => Method::GET,
        HttpMethod::Post => Method::POST,
        HttpMethod::Put => Method::PUT,
        HttpMethod::Patch => Method::PATCH,
        HttpMethod::Delete => Method::DELETE,
        HttpMethod::Head => Method::HEAD,
        HttpMethod::Options => Method::OPTIONS,
    }
}

fn apply_path_variables(
    url: &str,
    variables: &[KeyValue],
    scopes: &VariableScopes,
    encode_values: bool,
) -> Result<String, ExecutionError> {
    let enabled = variables
        .iter()
        .filter(|variable| variable.enabled && !variable.key.trim().is_empty())
        .filter_map(|variable| match resolve_value(&variable.value, scopes) {
            // An auto-extracted `:id` / `{id}` row starts with an empty value;
            // leave the token in place until the user fills one in, instead of
            // collapsing `/users/:id` to `/users/`.
            Ok(value) if value.is_empty() => None,
            Ok(value) => {
                let value = if encode_values {
                    encode_path_segment(&value)
                } else {
                    value
                };
                Some(Ok((variable.key.trim().to_owned(), value)))
            }
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, ExecutionError>>()?;
    Ok(substitute_path_variables(url, &enabled))
}

/// Replaces `:name` / `{name}` path tokens of `url` with the matching resolved `(name, value)` pair.
pub fn substitute_path_variables(url: &str, enabled: &[(String, String)]) -> String {
    if enabled.is_empty() {
        return url.to_owned();
    }

    let mut output = String::with_capacity(url.len());
    let mut cursor = 0;

    for (index, character) in url.char_indices() {
        if character == '{' && !url[index + 1..].starts_with('{') {
            if let Some(end) = url[index + 1..].find('}') {
                let key = &url[index + 1..index + 1 + end];
                if let Some((_, value)) = enabled.iter().find(|(name, _)| name == key) {
                    output.push_str(&url[cursor..index]);
                    output.push_str(value);
                    cursor = index + 1 + end + 1;
                }
            }
        } else if character == ':' && (index == 0 || url[..index].ends_with('/')) {
            let rest = &url[index + 1..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let key = &rest[..end];
            if let Some((_, value)) = enabled.iter().find(|(name, _)| name == key) {
                output.push_str(&url[cursor..index]);
                output.push_str(value);
                cursor = index + 1 + end;
            }
        }
    }

    output.push_str(&url[cursor..]);
    output
}

/// Applies the enabled Params rows to `url`, replacing URL-bar pairs with the same key. With
/// `encode` the rows are form-encoded; without it they and the URL bar's query go out
/// as typed, only characters a URL cannot carry at all (such as spaces) being escaped.
fn append_query_params(
    url: &mut Url,
    query_params: &[KeyValue],
    scopes: &VariableScopes,
    encode: bool,
) -> Result<(), ExecutionError> {
    let resolved_pairs = query_params
        .iter()
        .filter(|param| param.enabled)
        .map(|param| {
            Ok((
                resolve_value(&param.key, scopes)?,
                resolve_value(&param.value, scopes)?,
            ))
        })
        .collect::<Result<Vec<_>, ExecutionError>>()?;

    if !resolved_pairs.is_empty() {
        // The URL bar and the Params tab are both editable. Treat enabled
        // Params rows as overrides instead of appending duplicate keys.
        let override_keys = resolved_pairs
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<std::collections::HashSet<_>>();
        if !encode {
            let mut query = url
                .query()
                .unwrap_or_default()
                .split('&')
                .filter(|pair| {
                    let key = pair.split_once('=').map_or(*pair, |(key, _)| key);
                    !pair.is_empty() && !override_keys.contains(key)
                })
                .map(str::to_owned)
                .collect::<Vec<_>>();
            query.extend(
                resolved_pairs
                    .into_iter()
                    .map(|(key, value)| format!("{key}={value}")),
            );
            url.set_query(Some(&query.join("&")));
            return Ok(());
        }
        let existing = url
            .query_pairs()
            .filter(|(key, _)| !override_keys.contains(key.as_ref()))
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();

        url.set_query(None);
        if !existing.is_empty() || !resolved_pairs.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in existing {
                pairs.append_pair(&key, &value);
            }
            for (key, value) in resolved_pairs {
                pairs.append_pair(&key, &value);
            }
        }
    }

    Ok(())
}

/// Resolves `{{variables}}` in `input`, mapping resolution failures to execution errors.
pub(super) fn resolve_value(
    input: &str,
    scopes: &VariableScopes,
) -> Result<String, ExecutionError> {
    resolve_template(input, scopes).map_err(|error| match error {
        VariableResolutionError::UnresolvedVariables { keys } => {
            ExecutionError::UnresolvedVariable { keys }
        }
    })
}

/// `found` (the names that stopped the send) followed by every other name of `request` that
/// `scopes` cannot resolve, so one failure lists them all. Runs on the failure path only.
pub(super) fn all_unresolved_keys(
    request: &Request,
    scopes: &VariableScopes,
    mut found: Vec<String>,
) -> Vec<String> {
    let rows = request
        .path_variables
        .iter()
        .chain(&request.query_params)
        .chain(&request.headers)
        .filter(|row| row.enabled)
        .flat_map(|row| [row.key.as_str(), row.value.as_str()]);
    let texts = std::iter::once(request.url.as_str())
        .chain(rows)
        .chain(body_templates(&request.body))
        .chain(auth_templates(&request.auth));
    for text in texts {
        if let Err(VariableResolutionError::UnresolvedVariables { keys }) =
            resolve_template(text, scopes)
        {
            for key in keys {
                if !found.contains(&key) {
                    found.push(key);
                }
            }
        }
    }
    found
}

/// Variable scopes seeded with the environment's variables, if any.
pub(super) fn variable_scopes(environment: Option<&Environment>) -> VariableScopes {
    let mut scopes = VariableScopes::new();

    if let Some(environment) = environment {
        scopes = scopes.with_environment_vars(environment.variables.clone());
    }

    scopes
}
