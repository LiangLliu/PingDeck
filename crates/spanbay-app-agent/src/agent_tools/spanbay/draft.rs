//! Request building from the model's arguments: drafts, saved requests by path, bodies.
//!
//! Single responsibility: turn the `{name, value}` rows, `body {type, content}` and `draft`
//! objects of `send_request` and `create_request` into domain [`Request`] values, find a saved
//! request by its `Collection/Folder/Name` path, and enforce the 256 KB body limit. Nothing is
//! sent or stored here.

use serde::Deserialize;
use serde_json::{Value, json};

use super::common::Tree;
use crate::app::command_error;
use crate::error::AppError;
use crate::workspace::models::Request;
use crate::workspace::variables::uuid_v4;
use crate::workspace::{HttpMethod, KeyValue, RequestBody};

/// Largest request body a tool call may carry (PRD §6).
pub(super) const MAX_BODY: usize = 256 * 1024;

/// The schema of one `{name, value}` row (header or query parameter).
pub(super) fn pair_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false,
        "required": ["name", "value"],
        "properties": {"name": {"type": "string", "minLength": 1}, "value": {"type": "string"}}
    })
}

/// One header or query parameter as the model writes it.
#[derive(Deserialize)]
pub(super) struct Pair {
    pub(super) name: String,
    pub(super) value: String,
}

/// A request body as the model writes it.
#[derive(Deserialize)]
pub(super) struct DraftBody {
    pub(super) r#type: String,
    pub(super) content: Option<Value>,
}

/// An ad-hoc request.
#[derive(Deserialize)]
pub(super) struct Draft {
    pub(super) method: Option<String>,
    pub(super) url: String,
    #[serde(default)]
    pub(super) headers: Vec<Pair>,
    #[serde(default)]
    pub(super) query: Vec<Pair>,
    pub(super) body: Option<DraftBody>,
}

/// The method named `name`, in any case.
pub(super) fn method_of(name: &str) -> Result<HttpMethod, AppError> {
    Ok(match name.to_ascii_uppercase().as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        _ => return Err(AppError::validation(format!("unknown method {name}"))),
    })
}

/// Upper-case name of `method`.
pub(super) fn method_name(method: HttpMethod) -> &'static str {
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

/// The rows as enabled key/value pairs.
pub(super) fn pairs(rows: &[Pair]) -> Vec<KeyValue> {
    rows.iter()
        .map(|row| KeyValue {
            key: row.name.clone(),
            value: row.value.clone(),
            enabled: true,
            description: None,
        })
        .collect()
}

/// Bytes a body carries (the text, or the keys and values of a form).
pub(super) fn body_size(body: &RequestBody) -> usize {
    match body {
        RequestBody::Json { content } | RequestBody::RawText { content, .. } => content.len(),
        RequestBody::FormUrlEncoded { fields } => fields
            .iter()
            .map(|field| field.key.len() + field.value.len())
            .sum(),
        _ => 0,
    }
}

const FORM_SHAPE: &str = "A form body's `content` is a list of {name, value} strings.";

/// The body of a call: `json` and `text` carry text (any JSON value is written compactly), `form`
/// a list of `{name, value}` or an object of strings.
pub(super) fn draft_body(body: &DraftBody) -> Result<RequestBody, AppError> {
    let content = body.content.clone().unwrap_or(Value::Null);
    let text = |value: &Value| match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let built = match body.r#type.as_str() {
        "json" => RequestBody::Json {
            content: text(&content),
        },
        "text" => RequestBody::RawText {
            language: None,
            content: text(&content),
        },
        _ => {
            let fields = match &content {
                Value::Array(rows) => rows
                    .iter()
                    .map(|row| {
                        let name = row.get("name").and_then(Value::as_str);
                        let value = row.get("value").and_then(Value::as_str);
                        name.zip(value).map(|(name, value)| KeyValue {
                            key: name.to_owned(),
                            value: value.to_owned(),
                            enabled: true,
                            description: None,
                        })
                    })
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| AppError::validation(FORM_SHAPE))?,
                Value::Object(members) => members
                    .iter()
                    .map(|(name, value)| KeyValue {
                        key: name.clone(),
                        value: text(value),
                        enabled: true,
                        description: None,
                    })
                    .collect(),
                _ => return Err(AppError::validation(FORM_SHAPE)),
            };
            RequestBody::FormUrlEncoded { fields }
        }
    };
    if body_size(&built) > MAX_BODY {
        return Err(AppError::validation(
            "The body is larger than the 256 KB limit.",
        ));
    }
    Ok(built)
}

/// The request a `draft` describes, with a name made from its method and URL.
pub(super) fn draft_request(workspace_id: &str, draft: &Draft) -> Result<Request, AppError> {
    let method = draft
        .method
        .as_deref()
        .map_or(Ok(HttpMethod::Get), method_of)?;
    let shown: String = draft
        .url
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .chars()
        .take(80)
        .collect();
    let mut request = Request::new(
        format!("req_agent_{}", uuid_v4()),
        workspace_id,
        format!("{} {shown}", method_name(method)),
        draft.url.trim(),
    )
    .map_err(command_error)?;
    request.method = method;
    request.headers = pairs(&draft.headers);
    request.query_params = pairs(&draft.query);
    if let Some(body) = &draft.body {
        request.body = draft_body(body)?;
    }
    Ok(request)
}

/// The saved request at `path` (`Collection/Folder/Request name`), exact before case-insensitive.
pub(super) fn request_by_path(
    tree: &Tree,
    requests: Vec<Request>,
    path: &str,
) -> Result<Request, AppError> {
    let path = path.trim();
    let mut exact = Vec::new();
    let mut loose = Vec::new();
    for request in requests {
        let full = tree.full_path(&request);
        if full == path {
            exact.push(request);
        } else if full.eq_ignore_ascii_case(path) {
            loose.push(request);
        }
    }
    let mut found = if exact.is_empty() { loose } else { exact };
    match found.len() {
        0 => Err(AppError::not_found(format!(
            "No saved request at \"{path}\". Use list_requests to find it."
        ))),
        1 => Ok(found.remove(0)),
        _ => Err(AppError::validation(format!(
            "\"{path}\" names {} saved requests; use request_id.",
            found.len()
        ))),
    }
}
