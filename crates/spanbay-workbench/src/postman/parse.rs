use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::workspace::models::{
    ApiKeyLocation, AuthConfig, FormPart, HttpMethod, KeyValue, OAuth2ClientAuth, OAuth2GrantType,
    RawLanguage, RequestBody,
};

pub(super) fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

pub(super) fn parse_url(
    value: Option<&Value>,
    warnings: &mut Vec<String>,
    label: &str,
) -> Option<(String, Vec<KeyValue>, Vec<KeyValue>)> {
    match value {
        Some(Value::String(raw)) => {
            let (base, query_string) = split_raw_url(raw);
            Some((base, key_values_from_query_string(query_string), Vec::new()))
        }
        Some(Value::Object(url)) => parse_url_object(url),
        Some(_) => {
            warnings.push(format!("{label}: url has an unsupported shape; ignored"));
            None
        }
        None => None,
    }
}

pub(super) fn split_raw_url(raw: &str) -> (String, &str) {
    let (without_fragment, fragment) = match raw.split_once('#') {
        Some((head, fragment)) => (head, Some(fragment)),
        None => (raw, None),
    };
    let (base, query) = without_fragment
        .split_once('?')
        .unwrap_or((without_fragment, ""));
    let mut base = base.to_string();
    if !base.trim().is_empty()
        && let Some(fragment) = fragment
    {
        base.push('#');
        base.push_str(fragment);
    }
    (base, query)
}

pub(super) fn key_values_from_query_string(query: &str) -> Vec<KeyValue> {
    url::form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| KeyValue {
            key: key.into_owned(),
            value: value.into_owned(),
            enabled: true,
            description: None,
        })
        .filter(|pair| !pair.key.trim().is_empty())
        .collect()
}

fn parse_url_object(url: &Map<String, Value>) -> Option<(String, Vec<KeyValue>, Vec<KeyValue>)> {
    let query = key_values_from_array(url.get("query"));
    let path_variables = key_values_from_array(url.get("variable"));

    if let Some(raw) = url
        .get("raw")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
    {
        let (base, query_string) = split_raw_url(raw);
        let base = base.trim();
        if !base.is_empty() {
            let query_params = if query.is_empty() {
                key_values_from_query_string(query_string)
            } else {
                query
            };
            return Some((base.to_string(), query_params, path_variables));
        }
    }

    build_url_from_parts(url).map(|base| (base, query, path_variables))
}

fn build_url_from_parts(url: &Map<String, Value>) -> Option<String> {
    let protocol = url
        .get("protocol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|protocol| !protocol.is_empty())
        .unwrap_or("https");
    let host = join_url_parts(url.get("host"), ".");
    let path = join_url_parts(url.get("path"), "/");
    let port = url
        .get("port")
        .and_then(scalar_string)
        .map(|port| port.trim().to_string())
        .filter(|port| !port.is_empty());

    let mut built = if host.is_empty() {
        String::new()
    } else if host.contains("://") {
        host
    } else {
        format!("{protocol}://{host}")
    };

    if let Some(port) = port
        && !built.is_empty()
    {
        built.push(':');
        built.push_str(&port);
    }

    if !path.is_empty() {
        if !built.is_empty() && !path.starts_with('/') && !built.ends_with('/') {
            built.push('/');
        }
        built.push_str(&path);
    }

    let built = built.trim().to_string();
    if built.is_empty() { None } else { Some(built) }
}

fn join_url_parts(value: Option<&Value>, separator: &str) -> String {
    match value {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(url_part)
            .collect::<Vec<_>>()
            .join(separator),
        _ => String::new(),
    }
}

fn url_part(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Object(object) => object
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Reads a Postman `description`: a plain string or a `{ "content": "...", "type": "..." }` object.
fn postman_description(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(object) => object
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

pub(super) fn key_values_from_array(value: Option<&Value>) -> Vec<KeyValue> {
    let Some(Value::Array(entries)) = value else {
        return Vec::new();
    };

    entries
        .iter()
        .filter_map(|entry| {
            let object = entry.as_object()?;
            let key = object
                .get("key")
                .and_then(scalar_string)
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty())?;
            let value = object
                .get("value")
                .and_then(scalar_string)
                .unwrap_or_default();
            let enabled = !object
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let description = object
                .get("description")
                .and_then(postman_description)
                .filter(|text| !text.is_empty());
            Some(KeyValue {
                key,
                value,
                enabled,
                description,
            })
        })
        .collect()
}

/// The raw body language Postman names `language` (`text`, `javascript`, `html`, `xml`); JSON
/// is its own body kind and anything else is plain text.
fn raw_language(language: &str) -> Option<RawLanguage> {
    match language.trim().to_ascii_lowercase().as_str() {
        "javascript" => Some(RawLanguage::Javascript),
        "html" => Some(RawLanguage::Html),
        "xml" => Some(RawLanguage::Xml),
        _ => None,
    }
}

/// Postman's `options.raw.language` name of a raw body language.
pub(super) fn raw_language_name(language: RawLanguage) -> &'static str {
    match language {
        RawLanguage::Text => "text",
        RawLanguage::Javascript => "javascript",
        RawLanguage::Html => "html",
        RawLanguage::Xml => "xml",
    }
}

pub(super) fn parse_method(
    value: Option<&Value>,
    warnings: &mut Vec<String>,
    label: &str,
) -> HttpMethod {
    let Some(method) = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|method| !method.is_empty())
    else {
        return HttpMethod::Get;
    };

    match method.to_ascii_uppercase().as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        other => {
            warnings.push(format!("{label}: unsupported method '{other}'; using GET"));
            HttpMethod::Get
        }
    }
}

/// Warning emitted once per request whose literal local file path was discarded on import.
pub(super) fn local_path_warning(label: &str) -> String {
    format!("{label}: local file path was not imported; select the file again before sending")
}

/// A path that is exactly one `{{variable}}` carries no filesystem location and may be kept.
pub(super) fn is_variable_template(path: &str) -> bool {
    path.trim()
        .strip_prefix("{{")
        .and_then(|rest| rest.strip_suffix("}}"))
        .is_some_and(|name| !name.is_empty() && !name.contains("{{") && !name.contains("}}"))
}

/// Parses a Postman body; sets `dropped_local_path` when a literal file path was cleared.
pub(super) fn parse_body(
    body: Option<&Value>,
    warnings: &mut Vec<String>,
    label: &str,
    dropped_local_path: &mut bool,
) -> RequestBody {
    let Some(object) = body.and_then(Value::as_object) else {
        return RequestBody::None;
    };
    let Some(mode) = object
        .get("mode")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
    else {
        return RequestBody::None;
    };

    match mode.to_ascii_lowercase().as_str() {
        "raw" => {
            let content = object
                .get("raw")
                .and_then(scalar_string)
                .unwrap_or_default();
            let language = object
                .get("options")
                .and_then(Value::as_object)
                .and_then(|options| options.get("raw"))
                .and_then(Value::as_object)
                .and_then(|raw| raw.get("language"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if language.eq_ignore_ascii_case("json") {
                RequestBody::Json { content }
            } else {
                RequestBody::RawText {
                    content,
                    language: raw_language(language),
                }
            }
        }
        "urlencoded" => RequestBody::FormUrlEncoded {
            fields: key_values_from_array(object.get("urlencoded")),
        },
        "formdata" => {
            let parts = form_parts(object.get("formdata"), dropped_local_path);
            let typed = object
                .get("formdata")
                .and_then(Value::as_array)
                .map_or(0, |entries| {
                    entries
                        .iter()
                        .filter(|entry| {
                            entry
                                .get("contentType")
                                .and_then(Value::as_str)
                                .is_some_and(|content_type| !content_type.trim().is_empty())
                        })
                        .count()
                });
            if typed > 0 {
                warnings.push(format!(
                    "{label}: the Content-Type of {typed} form part(s) is not imported; parts are sent with the default type"
                ));
            }
            RequestBody::Multipart { parts }
        }
        "graphql" => parse_graphql_body(object, warnings, label),
        "file" => {
            let file = object.get("file").and_then(Value::as_object);
            let source = file
                .and_then(|file| file.get("src"))
                .and_then(scalar_string)
                .unwrap_or_default();
            // An empty source is how export represents a file the user must pick again.
            let file_path = if is_variable_template(&source) {
                source
            } else {
                if !source.trim().is_empty() {
                    *dropped_local_path = true;
                }
                String::new()
            };
            RequestBody::Binary {
                file_path,
                content_type: file
                    .and_then(|file| file.get("contentType").or_else(|| file.get("content_type")))
                    .and_then(scalar_string)
                    .filter(|content_type| !content_type.trim().is_empty()),
            }
        }
        other => {
            warnings.push(format!(
                "{label}: unsupported body mode '{other}'; body skipped"
            ));
            RequestBody::None
        }
    }
}

fn parse_graphql_body(
    object: &Map<String, Value>,
    warnings: &mut Vec<String>,
    label: &str,
) -> RequestBody {
    let Some(graphql) = object.get("graphql").and_then(Value::as_object) else {
        warnings.push(format!(
            "{label}: graphql body is missing its graphql object; body skipped"
        ));
        return RequestBody::None;
    };

    let query = graphql
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let variables_json = match graphql.get("variables") {
        Some(Value::String(text)) if !text.trim().is_empty() => Some(text.clone()),
        Some(value @ Value::Object(_)) => serde_json::to_string(value).ok(),
        _ => None,
    };
    let operation_name = graphql
        .get("operationName")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|name| !name.is_empty());

    RequestBody::Graphql {
        query,
        variables_json,
        operation_name,
    }
}

fn form_parts(value: Option<&Value>, dropped_local_path: &mut bool) -> Vec<FormPart> {
    let Some(Value::Array(entries)) = value else {
        return Vec::new();
    };

    let mut parts = Vec::new();
    for entry in entries {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let Some(key) = object
            .get("key")
            .and_then(scalar_string)
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
        else {
            continue;
        };
        let enabled = !object
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let is_file = object
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|part_type| part_type.eq_ignore_ascii_case("file"));

        let (value, file_path) = if is_file {
            let file_path = match form_file_path(object.get("src")) {
                Some(path) if is_variable_template(&path) => path,
                Some(_) => {
                    *dropped_local_path = true;
                    String::new()
                }
                None => String::new(),
            };
            (String::new(), Some(file_path))
        } else {
            let value = object
                .get("value")
                .and_then(scalar_string)
                .unwrap_or_default();
            (value, None)
        };

        parts.push(FormPart {
            key,
            value,
            file_path,
            enabled,
        });
    }

    parts
}

fn form_file_path(source: Option<&Value>) -> Option<String> {
    let path = match source? {
        Value::String(path) => path.clone(),
        Value::Array(paths) => paths.first().and_then(Value::as_str)?.to_string(),
        _ => return None,
    };

    let trimmed = path.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn parse_description(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(object) => object
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        None => String::new(),
    }
}

pub(super) fn parse_events(value: Option<&Value>) -> (String, String) {
    let mut pre_request_script = String::new();
    let mut test_script = String::new();
    let Some(Value::Array(events)) = value else {
        return (pre_request_script, test_script);
    };

    for event in events {
        let Some(object) = event.as_object() else {
            continue;
        };
        let listen = object
            .get("listen")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let exec = match object.get("script").and_then(|script| script.get("exec")) {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(lines)) => lines
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        if exec.trim().is_empty() {
            continue;
        }

        let target = match listen.as_str() {
            "prerequest" => &mut pre_request_script,
            "test" => &mut test_script,
            _ => continue,
        };
        if !target.is_empty() {
            target.push('\n');
        }
        target.push_str(&exec);
    }

    (pre_request_script, test_script)
}

pub(super) fn parse_auth(
    value: Option<&Value>,
    warnings: &mut Vec<String>,
    label: &str,
) -> AuthConfig {
    let Some(object) = value.and_then(Value::as_object) else {
        return AuthConfig::Inherit;
    };
    let auth_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");

    match auth_type.to_ascii_lowercase().as_str() {
        "" => {
            warnings.push(format!("{label}: auth type is missing; auth inherits"));
            AuthConfig::Inherit
        }
        "noauth" => AuthConfig::None,
        "inherit" => AuthConfig::Inherit,
        "bearer" => {
            let values = auth_values(object.get("bearer"));
            AuthConfig::Bearer {
                token_ref: values.get("token").cloned().unwrap_or_default(),
            }
        }
        "basic" => {
            let values = auth_values(object.get("basic"));
            AuthConfig::Basic {
                username: values.get("username").cloned().unwrap_or_default(),
                password_ref: values.get("password").cloned().unwrap_or_default(),
            }
        }
        "apikey" => {
            let values = auth_values(object.get("apikey"));
            let location = if values
                .get("in")
                .is_some_and(|location| location.trim().eq_ignore_ascii_case("query"))
            {
                ApiKeyLocation::Query
            } else {
                ApiKeyLocation::Header
            };
            AuthConfig::ApiKey {
                key: values.get("key").cloned().unwrap_or_default(),
                value_ref: values.get("value").cloned().unwrap_or_default(),
                location,
            }
        }
        "oauth2" => parse_oauth2_auth(object, warnings, label),
        "digest" => {
            let values = auth_values(object.get("digest"));
            AuthConfig::Digest {
                username: values.get("username").cloned().unwrap_or_default(),
                password_ref: Some(values.get("password").cloned().unwrap_or_default()),
            }
        }
        "awsv4" => {
            let values = auth_values(object.get("awsv4"));
            AuthConfig::AwsSignatureV4 {
                access_key: values.get("accessKey").cloned().unwrap_or_default(),
                secret_key_ref: Some(values.get("secretKey").cloned().unwrap_or_default()),
                region: values.get("region").cloned().unwrap_or_default(),
                service: values.get("service").cloned().unwrap_or_default(),
                session_token_ref: values
                    .get("sessionToken")
                    .cloned()
                    .filter(|token| !token.is_empty()),
            }
        }
        other => {
            warnings.push(format!(
                "{label}: unsupported auth type '{other}'; auth inherits"
            ));
            AuthConfig::Inherit
        }
    }
}

fn parse_oauth2_auth(
    object: &Map<String, Value>,
    warnings: &mut Vec<String>,
    label: &str,
) -> AuthConfig {
    let values = auth_values(object.get("oauth2"));
    let access_token_url = values.get("accessTokenUrl").cloned().unwrap_or_default();
    let client_id = values.get("clientId").cloned().unwrap_or_default();
    if access_token_url.trim().is_empty() || client_id.trim().is_empty() {
        warnings.push(format!(
            "{label}: oauth2 auth is missing accessTokenUrl or clientId; auth inherits"
        ));
        return AuthConfig::Inherit;
    }

    let grant_type = match values
        .get("grant_type")
        .map(|grant| grant.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("client_credentials") => OAuth2GrantType::ClientCredentials,
        Some("password") => OAuth2GrantType::Password,
        Some("authorization_code_with_pkce") => OAuth2GrantType::AuthorizationCodePkce,
        Some(other) => {
            warnings.push(format!(
                "{label}: unsupported oauth2 grant type '{other}'; auth inherits"
            ));
            return AuthConfig::Inherit;
        }
        None => {
            warnings.push(format!(
                "{label}: oauth2 auth is missing grant_type; auth inherits"
            ));
            return AuthConfig::Inherit;
        }
    };

    AuthConfig::OAuth2 {
        grant_type,
        access_token_url,
        client_id,
        client_secret_ref: values.get("clientSecret").cloned().unwrap_or_default(),
        username: values.get("username").cloned().unwrap_or_default(),
        password_ref: values.get("password").cloned().unwrap_or_default(),
        scope: values.get("scope").cloned().unwrap_or_default(),
        authorization_url: values.get("authUrl").cloned().unwrap_or_default(),
        redirect_port: values
            .get("redirect_uri")
            .and_then(|uri| url::Url::parse(uri).ok())
            .filter(|uri| matches!(uri.host_str(), Some("127.0.0.1" | "localhost")))
            .and_then(|uri| uri.port()),
        client_auth: match values.get("client_authentication").map(String::as_str) {
            Some("header") => OAuth2ClientAuth::Basic,
            _ => OAuth2ClientAuth::Body,
        },
        extra_params: oauth2_extra_params(object.get("oauth2"), warnings, label),
    }
}

/// Postman's `tokenRequestParams` and `authRequestParams` rows as one list of extra parameters
/// (SpanBay sends each with both requests); the first row of a name wins. A row Postman sends
/// as a header has no SpanBay equivalent and is dropped with a warning.
fn oauth2_extra_params(
    value: Option<&Value>,
    warnings: &mut Vec<String>,
    label: &str,
) -> Vec<KeyValue> {
    let lists: Vec<&Value> = match value {
        Some(Value::Array(entries)) => ["tokenRequestParams", "authRequestParams"]
            .iter()
            .filter_map(|name| {
                entries
                    .iter()
                    .filter_map(Value::as_object)
                    .find(|object| object.get("key").and_then(Value::as_str) == Some(*name))
                    .and_then(|object| object.get("value"))
            })
            .collect(),
        Some(Value::Object(map)) => ["tokenRequestParams", "authRequestParams"]
            .iter()
            .filter_map(|name| map.get(*name))
            .collect(),
        _ => Vec::new(),
    };
    let mut params: Vec<KeyValue> = Vec::new();
    let mut header_rows = false;
    for row in lists
        .into_iter()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_object)
    {
        let Some(key) = row
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
        else {
            continue;
        };
        if row.get("send_as").and_then(Value::as_str) == Some("request_header") {
            header_rows = true;
            continue;
        }
        if params.iter().any(|param| param.key == key) {
            continue;
        }
        params.push(KeyValue {
            key: key.to_owned(),
            value: row.get("value").and_then(scalar_string).unwrap_or_default(),
            enabled: row.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            description: None,
        });
    }
    if header_rows {
        warnings.push(format!(
            "{label}: oauth2 parameters sent as headers are not supported and were dropped"
        ));
    }
    params
}

/// Auth parameters: v2.1 writes `[{ "key", "value" }]`, v2.0 an object of key to value.
fn auth_values(value: Option<&Value>) -> HashMap<String, String> {
    let mut values = HashMap::new();
    if let Some(Value::Object(map)) = value {
        for (key, value) in map {
            if let Some(text) = scalar_string(value) {
                values.insert(key.trim().to_string(), text);
            }
        }
    }
    if let Some(Value::Array(entries)) = value {
        for entry in entries {
            let Some(object) = entry.as_object() else {
                continue;
            };
            let Some(key) = object.get("key").and_then(Value::as_str) else {
                continue;
            };
            let value = object
                .get("value")
                .and_then(scalar_string)
                .unwrap_or_default();
            values.insert(key.trim().to_string(), value);
        }
    }
    values
}
