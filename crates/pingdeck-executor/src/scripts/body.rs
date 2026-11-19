//! Postman body objects in scripts: `pm.request.body.update(…)` and the `body` of
//! `pm.sendRequest`. A body is a string (raw text) or `{ mode, raw | urlencoded | formdata |
//! graphql }`; each mode is encoded the way Postman sends it, and a mode the sandbox cannot send
//! (files) is an error instead of a silently different body.

use serde_json::{Map, Value, json};

use pingdeck_model::models::{KeyValue, RawLanguage, RequestBody};

/// A `pm.sendRequest` body ready for the blocking client.
pub(super) enum ScriptBody {
    Empty,
    /// Encoded text and the Content-Type it implies when no header sets one.
    Text {
        text: String,
        content_type: Option<&'static str>,
    },
    /// Text fields of a `formdata` body; the client writes the boundary.
    Multipart(Vec<(String, String)>),
}

const FILE_REFUSED: &str =
    "files cannot be attached from a script; send the file from a saved request";

/// The text of a scalar body field: strings as written, numbers and booleans as JSON, `null`
/// as empty.
fn field_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// The body's mode: `mode` as written, or `raw` for an object that only has `raw`.
fn mode_of(object: &Map<String, Value>) -> Option<&str> {
    object
        .get("mode")
        .and_then(Value::as_str)
        .or_else(|| object.contains_key("raw").then_some("raw"))
}

/// `{ key, value, disabled }` rows (an array) or a `{ key: value }` object, enabled rows only.
fn pairs(value: Option<&Value>) -> Result<Vec<(String, String)>, String> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Object(object)) => Ok(object
            .iter()
            .map(|(key, value)| (key.clone(), field_text(Some(value))))
            .collect()),
        Some(Value::Array(rows)) => {
            let mut out = Vec::new();
            for row in rows {
                let Some(row) = row.as_object() else {
                    return Err("each body row must be a { key, value } object".to_owned());
                };
                if row.get("disabled").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                if row.get("type").and_then(Value::as_str) == Some("file")
                    || row.contains_key("src")
                {
                    return Err(FILE_REFUSED.to_owned());
                }
                let key = field_text(row.get("key"));
                if !key.is_empty() {
                    out.push((key, field_text(row.get("value"))));
                }
            }
            Ok(out)
        }
        Some(_) => Err("body rows must be an array of { key, value } or an object".to_owned()),
    }
}

/// The raw text of a `raw` body (a non-string is sent as its JSON) and whether
/// `options.raw.language` says JSON.
fn raw_text(object: &Map<String, Value>) -> (String, Option<String>) {
    let text = match object.get("raw") {
        Some(Value::String(text)) => text.clone(),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    };
    let language = object
        .get("options")
        .and_then(|options| options.get("raw"))
        .and_then(|raw| raw.get("language"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    (text, language)
}

/// `{ query, variables }` of a `graphql` body; `variables` may be an object or its JSON text.
fn graphql_parts(object: &Map<String, Value>) -> Result<(String, Option<Value>), String> {
    let graphql = object
        .get("graphql")
        .and_then(Value::as_object)
        .ok_or_else(|| "a graphql body needs { graphql: { query, variables } }".to_owned())?;
    let query = field_text(graphql.get("query"));
    let variables = match graphql.get("variables") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.trim().is_empty() => None,
        Some(Value::String(text)) => Some(
            serde_json::from_str(text)
                .map_err(|error| format!("graphql variables are not valid JSON: {error}"))?,
        ),
        Some(other) => Some(other.clone()),
    };
    Ok((query, variables))
}

/// The `body` of a `pm.sendRequest` request object, encoded by its mode.
pub(super) fn send_request_body(body: Option<&Value>) -> Result<ScriptBody, String> {
    let body = match body {
        None | Some(Value::Null) => return Ok(ScriptBody::Empty),
        Some(Value::String(text)) => {
            return Ok(ScriptBody::Text {
                text: text.clone(),
                content_type: None,
            });
        }
        Some(Value::Object(object)) => object,
        Some(_) => {
            return Err("pm.sendRequest body must be text or a { mode, … } object".to_owned());
        }
    };
    match mode_of(body) {
        Some("raw") => {
            let (text, language) = raw_text(body);
            Ok(ScriptBody::Text {
                text,
                content_type: (language.as_deref() == Some("json")).then_some("application/json"),
            })
        }
        Some("urlencoded") => {
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for (key, value) in pairs(body.get("urlencoded"))? {
                serializer.append_pair(&key, &value);
            }
            Ok(ScriptBody::Text {
                text: serializer.finish(),
                content_type: Some("application/x-www-form-urlencoded"),
            })
        }
        Some("formdata") => Ok(ScriptBody::Multipart(pairs(body.get("formdata"))?)),
        Some("graphql") => {
            let (query, variables) = graphql_parts(body)?;
            Ok(ScriptBody::Text {
                text: json!({ "query": query, "variables": variables }).to_string(),
                content_type: Some("application/json"),
            })
        }
        Some("none") => Ok(ScriptBody::Empty),
        Some("file") => Err(FILE_REFUSED.to_owned()),
        Some(other) => Err(format!(
            "pm.sendRequest does not know the body mode {other}"
        )),
        None => Err(
            "pm.sendRequest body object needs a mode (raw, urlencoded, formdata or graphql)"
                .to_owned(),
        ),
    }
}

/// The request body after `pm.request.body.update(content)`: text replaces the content of a raw
/// or JSON body (an empty body becomes raw text); `{ mode, … }` sets the body type and content.
pub(super) fn updated_body(current: &RequestBody, content: &Value) -> Result<RequestBody, String> {
    let object = match content {
        Value::String(text) => {
            let content = text.clone();
            return match current {
                RequestBody::Json { .. } => Ok(RequestBody::Json { content }),
                RequestBody::RawText { language, .. } => Ok(RequestBody::RawText {
                    content,
                    language: *language,
                }),
                RequestBody::None => Ok(RequestBody::RawText {
                    content,
                    language: None,
                }),
                _ => Err(
                    "pm.request.body.update(text) replaces raw and JSON bodies only; pass \
                     { mode, … } to change the body type"
                        .to_owned(),
                ),
            };
        }
        Value::Object(object) => object,
        _ => {
            return Err("pm.request.body.update needs text or a { mode, … } object".to_owned());
        }
    };
    match mode_of(object) {
        Some("raw") => {
            let (content, language) = raw_text(object);
            Ok(match language.as_deref() {
                Some("json") => RequestBody::Json { content },
                Some("javascript") => RequestBody::RawText {
                    content,
                    language: Some(RawLanguage::Javascript),
                },
                Some("html") => RequestBody::RawText {
                    content,
                    language: Some(RawLanguage::Html),
                },
                Some("xml") => RequestBody::RawText {
                    content,
                    language: Some(RawLanguage::Xml),
                },
                Some("text") => RequestBody::RawText {
                    content,
                    language: Some(RawLanguage::Text),
                },
                // No language: keep the current body's kind, as text update does.
                _ => match current {
                    RequestBody::Json { .. } => RequestBody::Json { content },
                    RequestBody::RawText { language, .. } => RequestBody::RawText {
                        content,
                        language: *language,
                    },
                    _ => RequestBody::RawText {
                        content,
                        language: None,
                    },
                },
            })
        }
        Some("urlencoded") => Ok(RequestBody::FormUrlEncoded {
            fields: pairs(object.get("urlencoded"))?
                .into_iter()
                .map(|(key, value)| KeyValue {
                    key,
                    value,
                    enabled: true,
                    description: None,
                })
                .collect(),
        }),
        Some("graphql") => {
            let (query, variables) = graphql_parts(object)?;
            Ok(RequestBody::Graphql {
                query,
                variables_json: variables.map(|variables| variables.to_string()),
                operation_name: None,
            })
        }
        Some("none") => Ok(RequestBody::None),
        Some("formdata" | "file") => Err(
            "pm.request.body.update cannot set multipart or file bodies from a script".to_owned(),
        ),
        Some(other) => Err(format!(
            "pm.request.body.update does not know the body mode {other}"
        )),
        None => Err("pm.request.body.update needs text or a { mode, … } object".to_owned()),
    }
}
