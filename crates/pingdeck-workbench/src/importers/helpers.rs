use std::collections::HashMap;

use base64::Engine as _;
use serde_json::Value;
use url::Url;

use super::ImportError;
use crate::workspace::models::{
    AuthConfig, CollectionId, Folder, FolderId, HttpMethod, KeyValue, Request, RequestBody,
};
use crate::workspace::validation::WorkspaceError;

const MAX_SLUG_LEN: usize = 32;

pub(super) struct RequestDraft {
    pub(super) name: String,
    pub(super) method: HttpMethod,
    pub(super) url: String,
    pub(super) query_params: Vec<KeyValue>,
    pub(super) headers: Vec<KeyValue>,
    pub(super) body: RequestBody,
    pub(super) auth: AuthConfig,
    pub(super) description: String,
    pub(super) folder_id: Option<FolderId>,
}

pub(super) fn build_request(
    draft: RequestDraft,
    ids: &mut Ids,
    workspace_id: &str,
    collection_id: &CollectionId,
    position: i64,
) -> Result<Request, WorkspaceError> {
    let name = non_empty_or(&draft.name, "Request");
    let url = non_empty_or(&draft.url, "{{baseUrl}}");
    let mut request = Request::new(ids.next("req"), workspace_id, name, url)?;
    request.collection_id = Some(collection_id.clone());
    request.folder_id = draft.folder_id;
    request.method = draft.method;
    request.query_params = draft.query_params;
    request.headers = draft.headers;
    request.body = draft.body;
    request.auth = draft.auth;
    request.description = draft.description;
    request.position = position;
    Ok(request)
}

pub(super) fn folder_for(
    folders: &mut Vec<Folder>,
    index: &mut HashMap<String, FolderId>,
    collection_id: &CollectionId,
    ids: &mut Ids,
    name: &str,
) -> Result<FolderId, WorkspaceError> {
    if let Some(folder_id) = index.get(name) {
        return Ok(folder_id.clone());
    }
    let mut folder = Folder::new(ids.next("folder"), collection_id.as_str(), name)?;
    folder.position = folders.len() as i64;
    let folder_id = folder.id.clone();
    folders.push(folder);
    index.insert(name.to_string(), folder_id.clone());
    Ok(folder_id)
}

pub(super) fn parse_json(text: &str) -> Result<Value, ImportError> {
    serde_json::from_str(text).map_err(|error| ImportError::InvalidJson {
        message: error.to_string(),
    })
}

pub(super) fn parse_json_or_yaml(text: &str) -> Result<Value, ImportError> {
    serde_json::from_str(text).or_else(|json_error| {
        pingdeck_base::yaml::to_json(text).map_err(|yaml_error| ImportError::InvalidJson {
            message: format!("JSON: {json_error}; YAML: {yaml_error}"),
        })
    })
}

pub(super) fn http_method(name: &str) -> Option<HttpMethod> {
    match name.trim().to_ascii_uppercase().as_str() {
        "GET" => Some(HttpMethod::Get),
        "POST" => Some(HttpMethod::Post),
        "PUT" => Some(HttpMethod::Put),
        "PATCH" => Some(HttpMethod::Patch),
        "DELETE" => Some(HttpMethod::Delete),
        "HEAD" => Some(HttpMethod::Head),
        "OPTIONS" => Some(HttpMethod::Options),
        _ => None,
    }
}

pub(super) fn method_label(method: HttpMethod) -> &'static str {
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

pub(super) fn request_name(method: HttpMethod, base_url: &str) -> String {
    let path = url_path(base_url);
    let label = if path.is_empty() {
        url_host(base_url).unwrap_or_default()
    } else {
        path
    };
    let label = non_empty_or(&label, "request");
    format!("{} {label}", method_label(method))
}

pub(super) fn collection_name_from_url(raw_url: &str) -> String {
    let host = url_host(raw_url).unwrap_or_default();
    let path = url_path(raw_url);
    match (host.is_empty(), path.is_empty()) {
        (false, false) => format!("{host}{path}"),
        (false, true) => host,
        (true, false) => path,
        (true, true) => "cURL import".to_string(),
    }
}

pub(super) fn url_host(raw_url: &str) -> Option<String> {
    if let Ok(parsed) = Url::parse(raw_url)
        && let Some(host) = parsed.host_str()
    {
        return Some(host.to_string());
    }
    let without_scheme = raw_url.split_once("://").map_or(raw_url, |(_, rest)| rest);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

pub(super) fn url_path(raw_url: &str) -> String {
    if let Ok(parsed) = Url::parse(raw_url) {
        let path = parsed.path().trim_end_matches('/');
        return if path.is_empty() {
            String::new()
        } else {
            path.to_string()
        };
    }
    let without_scheme = raw_url.split_once("://").map_or(raw_url, |(_, rest)| rest);
    let mut segments = without_scheme.splitn(2, '/');
    segments.next();
    let path = segments.next().unwrap_or("");
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    if path.is_empty() {
        String::new()
    } else {
        format!("/{path}")
    }
}

pub(super) fn split_url_query(raw_url: &str) -> (&str, &str) {
    let without_fragment = raw_url.split_once('#').map_or(raw_url, |(head, _)| head);
    without_fragment
        .split_once('?')
        .unwrap_or((without_fragment, ""))
}

pub(super) fn query_pairs(query: &str) -> Vec<KeyValue> {
    url::form_urlencoded::parse(query.as_bytes())
        .filter(|(key, _)| !key.trim().is_empty())
        .map(|(key, value)| KeyValue {
            key: key.into_owned(),
            value: value.into_owned(),
            enabled: true,
            description: None,
        })
        .collect()
}

pub(super) fn header_value<'a>(headers: &'a [KeyValue], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|header| header.enabled && header.key.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

pub(super) fn set_header(headers: &mut Vec<KeyValue>, key: &str, value: &str) {
    if let Some(existing) = headers
        .iter_mut()
        .find(|header| header.key.eq_ignore_ascii_case(key))
    {
        existing.value = value.to_string();
        existing.enabled = true;
    } else {
        headers.push(KeyValue {
            key: key.to_string(),
            value: value.to_string(),
            enabled: true,
            description: None,
        });
    }
}

pub(super) fn take_authorization_auth(headers: &mut Vec<KeyValue>) -> Option<AuthConfig> {
    let index = headers
        .iter()
        .position(|header| header.key.eq_ignore_ascii_case("authorization"))?;
    let auth = auth_from_authorization(&headers[index].value)?;
    headers.remove(index);
    Some(auth)
}

pub(super) fn auth_from_authorization(value: &str) -> Option<AuthConfig> {
    let value = value.trim();
    if let Some(token) = strip_auth_scheme(value, "Bearer") {
        return Some(AuthConfig::Bearer {
            token_ref: token.to_string(),
        });
    }
    if let Some(credentials) = strip_auth_scheme(value, "Basic") {
        return Some(basic_auth_from_credentials(credentials));
    }
    None
}

fn strip_auth_scheme<'a>(value: &'a str, scheme: &str) -> Option<&'a str> {
    let (head, tail) = value.split_once(char::is_whitespace)?;
    if head.eq_ignore_ascii_case(scheme) {
        Some(tail.trim())
    } else {
        None
    }
}

pub(super) fn basic_auth_from_credentials(credentials: &str) -> AuthConfig {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(credentials)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    match decoded.and_then(|text| {
        text.split_once(':')
            .map(|(username, password)| (username.to_string(), password.to_string()))
    }) {
        Some((username, password)) => AuthConfig::Basic {
            username,
            password_ref: password,
        },
        None => AuthConfig::Basic {
            username: credentials.trim().to_string(),
            password_ref: String::new(),
        },
    }
}

pub(super) fn json_body_like(text: &str) -> bool {
    let trimmed = text.trim();
    serde_json::from_str::<Value>(trimmed).is_ok()
        || trimmed.starts_with('{')
        || trimmed.starts_with('[')
}

pub(super) fn body_from_text(text: &str, content_type: Option<&str>) -> RequestBody {
    let is_json = content_type.is_some_and(|content_type| {
        content_type
            .to_ascii_lowercase()
            .contains("application/json")
    });
    if is_json && json_body_like(text) {
        RequestBody::Json {
            content: text.to_string(),
        }
    } else {
        RequestBody::RawText {
            language: None,
            content: text.to_string(),
        }
    }
}

pub(super) fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

pub(super) fn non_empty_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

pub(super) struct Ids {
    slug: String,
    next_index: u32,
}

impl Ids {
    pub(super) fn new(collection_name: &str) -> Self {
        Self {
            slug: slugify(collection_name),
            next_index: 1,
        }
    }

    pub(super) fn next(&mut self, prefix: &str) -> String {
        let index = self.next_index;
        self.next_index += 1;
        format!("{prefix}_{}_{}", self.slug, index)
    }
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator && !slug.is_empty() {
            slug.push('_');
            last_was_separator = true;
        }
        if slug.len() >= MAX_SLUG_LEN {
            break;
        }
    }
    let slug = slug.trim_end_matches('_');
    if slug.is_empty() {
        "import".to_string()
    } else {
        slug.to_string()
    }
}
