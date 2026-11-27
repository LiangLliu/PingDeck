use std::{
    collections::BTreeMap,
    io::Read,
    sync::mpsc,
    time::{Duration, Instant},
};

use serde_json::{Map, Value, json};

use super::ScriptLimits;
use super::runtime::{HostState, cap_text};
use crate::executor::ExecutionResult;
use spanbay_base::cancel::{CANCELLED_MESSAGE, CancelToken};
use spanbay_base::crypto;
use spanbay_model::script_result::{ConsoleEntry, ConsoleLevel, ScriptTestResult};
use spanbay_model::{
    VariableScopes,
    ai_request::AiScriptResult,
    models::{EnvironmentVariable, HttpMethod, KeyValue, Request, RequestBody},
    resolve_template_lenient,
};

pub(super) fn handle_operation(
    host: &mut HostState,
    operation: &str,
    payload: &Value,
) -> Result<String, String> {
    match operation {
        "info" => Ok(serde_json::to_string(&json!({
            "requestName": host.info.request_name,
            "requestId": host.info.request_id,
            "iteration": host.info.iteration,
            "iterationCount": host.info.iteration_count,
            "eventName": host.info.event_name,
        }))
        .map_err(|error| error.to_string())?),
        "log" => {
            let level = match payload.get("level").and_then(Value::as_str) {
                Some("info") => ConsoleLevel::Info,
                Some("warn") => ConsoleLevel::Warn,
                Some("error") => ConsoleLevel::Error,
                Some("debug") => ConsoleLevel::Debug,
                _ => ConsoleLevel::Log,
            };
            let message = payload.get("message").and_then(Value::as_str);
            push_console(host, level, message.unwrap_or_default());
            Ok("null".to_owned())
        }
        "bytes.convert" => {
            let text = |key: &str| payload.get(key).and_then(Value::as_str).unwrap_or_default();
            let bytes = decode_bytes(text("from"), text("value"))?;
            encode_bytes(text("to"), &bytes)
        }
        "crypto.digest" => {
            let data = payload_bytes(payload.get("data"))?;
            match payload.get("algorithm").and_then(Value::as_str) {
                Some("md5") => Ok(crypto::hex_lower(&crypto::md5(&data))),
                Some("sha1") => Ok(crypto::hex_lower(&crypto::sha1(&data))),
                Some("sha256") => Ok(crypto::hex_lower(&crypto::sha256(&data))),
                Some("sha512") => Ok(crypto::hex_lower(&crypto::sha512(&data))),
                other => Err(format!("unsupported digest {other:?}")),
            }
        }
        "crypto.hmac" => {
            let data = payload_bytes(payload.get("data"))?;
            let key = payload_bytes(payload.get("key"))?;
            match payload.get("algorithm").and_then(Value::as_str) {
                Some("sha1") => Ok(crypto::hex_lower(&crypto::hmac_sha1(&key, &data))),
                Some("sha256") => Ok(crypto::hex_lower(&crypto::hmac_sha256(&key, &data))),
                Some("sha512") => Ok(crypto::hex_lower(&crypto::hmac_sha512(&key, &data))),
                other => Err(format!("unsupported HMAC {other:?}")),
            }
        }
        // `pm.response.to.have.jsonSchema`: the same validator as the `JsonSchemaValid`
        // assertion; "" when the value matches, else the first violation.
        "schema.validate" => {
            let schema = payload.get("schema").unwrap_or(&Value::Null);
            if !schema.is_object() && !schema.is_boolean() {
                return serde_json::to_string("the JSON schema must be an object")
                    .map_err(|error| error.to_string());
            }
            let value = payload.get("value").unwrap_or(&Value::Null);
            let outcome = crate::json_schema::validate_document(schema, value)
                .err()
                .unwrap_or_default();
            serde_json::to_string(&outcome).map_err(|error| error.to_string())
        }
        "test.record" => {
            // Dropping results would hide failures, so the entry that reaches the cap is a failed
            // marker test.
            let max_tests = host.limits.max_test_results;
            let max_message_bytes = host.limits.max_message_bytes;
            if host.tests.len() > max_tests {
                return Ok("null".to_owned());
            }
            let result = if host.tests.len() == max_tests {
                ScriptTestResult {
                    name: "Too many tests".to_owned(),
                    passed: false,
                    skipped: false,
                    error: Some(format!(
                        "more than {max_tests} pm.test results; the rest were dropped"
                    )),
                    source: host.current_source.clone(),
                }
            } else {
                ScriptTestResult {
                    name: cap_text(
                        payload
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("Unnamed test"),
                        max_message_bytes,
                    ),
                    passed: payload
                        .get("passed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    skipped: payload
                        .get("skipped")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    error: payload
                        .get("error")
                        .and_then(Value::as_str)
                        .map(|error| cap_text(error, max_message_bytes)),
                    source: host.current_source.clone(),
                }
            };
            host.tests.push(result);
            Ok("null".to_owned())
        }
        "next" => {
            host.next_request = Some(
                payload
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            );
            Ok("null".to_owned())
        }
        "request.get" => Ok(serde_json::to_string(&request_payload(&host.request))
            .map_err(|error| error.to_string())?),
        "request.url.get" => {
            let scopes = host_scopes(host);
            let base = resolve_template_lenient(&host.request.url, &scopes);
            let mut url = url::Url::parse(&base).ok();
            if let Some(parsed) = url.as_mut() {
                let mut resolved = Vec::new();
                for param in host
                    .request
                    .query_params
                    .iter()
                    .filter(|param| param.enabled)
                {
                    let key = resolve_template_lenient(&param.key, &scopes);
                    let value = resolve_template_lenient(&param.value, &scopes);
                    resolved.push((key, value));
                }
                if !resolved.is_empty() {
                    let override_keys = resolved
                        .iter()
                        .map(|(key, _)| key.clone())
                        .collect::<std::collections::HashSet<_>>();
                    let existing = parsed
                        .query_pairs()
                        .filter(|(key, _)| !override_keys.contains(key.as_ref()))
                        .map(|(key, value)| (key.into_owned(), value.into_owned()))
                        .collect::<Vec<_>>();
                    parsed.set_query(None);
                    let mut query = parsed.query_pairs_mut();
                    for (key, value) in existing {
                        query.append_pair(&key, &value);
                    }
                    for (key, value) in resolved {
                        query.append_pair(&key, &value);
                    }
                }
            }
            let url = url.map_or(base, |url| url.to_string());
            Ok(serde_json::to_string(&json!({ "url": url })).map_err(|error| error.to_string())?)
        }
        "request.method.set" => {
            let method = payload
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("GET")
                .to_ascii_uppercase();
            host.request.method = match method.as_str() {
                "GET" => HttpMethod::Get,
                "POST" => HttpMethod::Post,
                "PUT" => HttpMethod::Put,
                "PATCH" => HttpMethod::Patch,
                "DELETE" => HttpMethod::Delete,
                "HEAD" => HttpMethod::Head,
                "OPTIONS" => HttpMethod::Options,
                other => return Err(format!("unsupported method {other}")),
            };
            Ok("null".to_owned())
        }
        "request.header.get" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let value = host
                .request
                .headers
                .iter()
                .find(|header| header.enabled && header.key.eq_ignore_ascii_case(name))
                .map(|header| header.value.clone());
            Ok(serde_json::to_string(&json!({ "value": value }))
                .map_err(|error| error.to_string())?)
        }
        "request.headers.list" => {
            let mut object = Map::new();
            for header in host.request.headers.iter().filter(|header| header.enabled) {
                object.insert(header.key.clone(), Value::String(header.value.clone()));
            }
            Ok(serde_json::to_string(&Value::Object(object)).map_err(|error| error.to_string())?)
        }
        "request.header.add" => {
            let (key, value) = header_pair(payload)?;
            host.request.headers.push(KeyValue {
                key,
                value,
                enabled: true,
                description: None,
            });
            Ok("null".to_owned())
        }
        "request.header.upsert" => {
            let (key, value) = header_pair(payload)?;
            if let Some(header) = host
                .request
                .headers
                .iter_mut()
                .find(|header| header.key.eq_ignore_ascii_case(&key))
            {
                header.value = value;
                header.enabled = true;
            } else {
                host.request.headers.push(KeyValue {
                    key,
                    value,
                    enabled: true,
                    description: None,
                });
            }
            Ok("null".to_owned())
        }
        "request.header.remove" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            host.request
                .headers
                .retain(|header| !header.key.eq_ignore_ascii_case(name));
            Ok("null".to_owned())
        }
        "request.query.remove" => {
            let key = payload
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Ok(mut url) = url::Url::parse(&host.request.url) {
                let pairs = url
                    .query_pairs()
                    .filter(|(existing, _)| existing != key)
                    .map(|(existing, value)| (existing.into_owned(), value.into_owned()))
                    .collect::<Vec<_>>();
                url.set_query(None);
                if !pairs.is_empty() {
                    let mut query = url.query_pairs_mut();
                    for (existing, value) in pairs {
                        query.append_pair(&existing, &value);
                    }
                }
                host.request.url = url.to_string();
            }
            host.request.query_params.retain(|param| param.key != key);
            Ok("null".to_owned())
        }
        "request.query.add" => {
            if let Some(params) = payload.get("params").and_then(Value::as_array) {
                for param in params {
                    let key = param.get("key").and_then(Value::as_str).unwrap_or_default();
                    let value = param
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !key.is_empty() {
                        host.request.query_params.push(KeyValue {
                            key: key.to_owned(),
                            value: value.to_owned(),
                            enabled: true,
                            description: None,
                        });
                    }
                }
            }
            Ok("null".to_owned())
        }
        "request.body.get" => Ok(serde_json::to_string(&json!({
            "raw": body_raw(&host.request.body),
        }))
        .map_err(|error| error.to_string())?),
        "request.body.update" => {
            let content = payload.get("content").unwrap_or(&Value::Null);
            host.request.body = super::body::updated_body(&host.request.body, content)?;
            Ok("null".to_owned())
        }
        "cookies.request" => to_json(&super::cookies::request_cookies(host)),
        "cookies.list" => to_json(&super::cookies::jar_list(host, payload)?),
        "cookies.get" => to_json(&super::cookies::jar_get(host, payload)?),
        "cookies.set" => to_json(&super::cookies::jar_set(host, payload)?),
        "cookies.remove" => to_json(&super::cookies::jar_remove(host, payload)?),
        "response.body" => Ok(host
            .response
            .as_ref()
            .map(|response| response.body_text.clone())
            .unwrap_or_default()),
        "response.get" => match host.response.as_ref() {
            Some(response) => Ok(serde_json::to_string(&response_payload(
                response,
                host.ai.as_ref(),
            ))
            .map_err(|error| error.to_string())?),
            None => Ok(json!({ "available": false }).to_string()),
        },
        "var.get" => {
            let scope = payload
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("variables");
            let scope = effective_scope(host, scope);
            let key = payload
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            let value = variable_get(host, scope, key);
            Ok(serde_json::to_string(&json!({
                "found": value.is_some(),
                "value": value,
            }))
            .map_err(|error| error.to_string())?)
        }
        "var.set" => {
            let scope = payload
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("variables");
            let scope = effective_scope(host, scope);
            let key = payload
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            let value = payload
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            variable_set(host, scope, key, value)?;
            Ok("null".to_owned())
        }
        "var.unset" => {
            let scope = payload
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("variables");
            let scope = effective_scope(host, scope);
            let key = payload
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            variable_unset(host, scope, key);
            Ok("null".to_owned())
        }
        "var.clear" => {
            let scope = payload
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("variables");
            variable_clear(host, scope);
            Ok("null".to_owned())
        }
        "var.list" => {
            let scope = payload
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("variables");
            let values = variable_list(host, effective_scope(host, scope));
            Ok(serde_json::to_string(&values).map_err(|error| error.to_string())?)
        }
        "replaceIn" => {
            let text = payload
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let scopes = host_scopes(host);
            Ok(resolve_template_lenient(text, &scopes))
        }
        "http.send" => {
            let method = payload
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("GET")
                .to_ascii_uppercase();
            let url = payload
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let shown = format!(
                "pm.sendRequest {method} {}",
                spanbay_base::secrets::mask_secrets(url, &host_scopes(host).secret_values())
            );
            // The same production guard as the main send: a guarded method needs the
            // caller's confirmation, and a refusal is answered like a refused main send.
            if let Some(environment) = host.environment.as_ref().filter(|environment| {
                !host.confirmed && environment.requires_confirmation(guarded_method(&method))
            }) {
                let message = format!(
                    "{} is guarded: confirm before sending a {method} request",
                    environment.name
                );
                host.confirmation_required = Some(message.clone());
                push_console(
                    host,
                    ConsoleLevel::Warn,
                    &format!("{shown} → refused: {message}"),
                );
                return Err(message);
            }
            let max_requests = host.limits.max_send_requests;
            if host.send_requests >= max_requests {
                return Err(format!(
                    "pm.sendRequest limit reached ({max_requests} requests per script)"
                ));
            }
            host.send_requests += 1;
            let outcome = script_http_request(
                payload,
                host.cancel.as_ref(),
                &host.limits,
                &mut host.http_clients,
            );
            // Every script request is a console line too, so the Trace and console show it.
            match &outcome {
                Ok(raw) => {
                    let answer = serde_json::from_str::<Value>(raw).unwrap_or(Value::Null);
                    let line = format!(
                        "{shown} → {} {} ({} ms)",
                        answer
                            .get("code")
                            .and_then(Value::as_u64)
                            .unwrap_or_default(),
                        answer
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        answer
                            .get("elapsed_ms")
                            .and_then(Value::as_u64)
                            .unwrap_or_default(),
                    );
                    push_console(host, ConsoleLevel::Info, &line);
                }
                Err(error) => {
                    push_console(
                        host,
                        ConsoleLevel::Error,
                        &format!("{shown} → failed: {error}"),
                    );
                }
            }
            outcome
        }
        other => Err(format!("unsupported script operation {other}")),
    }
}

fn to_json(value: &Value) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}

/// Whether a variable a script writes holds a credential, so it is stored as a secret and never
/// leaves through an export, a Postman export or folder sync: its name is a credential name
/// (`token`, `access_token`, `client_secret`, `x-api-key`, `session_id`, `jwt`, the query keys
/// redaction knows) or its value has a well-known token shape (a JWT, a provider key, `Bearer …`).
fn holds_credential(key: &str, value: &str) -> bool {
    use spanbay_base::secrets::{REDACT_QUERY_KEYS, is_secret_header_name, is_token_shaped};
    let lower = key.trim().to_ascii_lowercase();
    let compact: String = lower.chars().filter(char::is_ascii_alphanumeric).collect();
    REDACT_QUERY_KEYS.contains(&lower.as_str())
        || is_secret_header_name(&lower)
        || ["jwt", "sid", "session", "bearer", "auth"].contains(&compact.as_str())
        || ["jwt", "sessionid", "sessiontoken", "bearer", "authcode"]
            .iter()
            .any(|ending| compact.ends_with(ending))
        || is_token_shaped(value)
}

/// Appends a console line under the entry cap; the entry that reaches the cap is a marker and
/// later ones are dropped.
fn push_console(host: &mut HostState, level: ConsoleLevel, message: &str) {
    let max_entries = host.limits.max_console_entries;
    if host.console.len() > max_entries {
        return;
    }
    let (level, message) = if host.console.len() == max_entries {
        (
            ConsoleLevel::Warn,
            format!("console output truncated after {max_entries} entries"),
        )
    } else {
        (level, cap_text(message, host.limits.max_message_bytes))
    };
    host.console.push(ConsoleEntry {
        level,
        message,
        source: host.current_source.clone(),
    });
}

/// The guard's view of a script request method; anything unknown counts as unsafe.
fn guarded_method(method: &str) -> HttpMethod {
    match method {
        "GET" => HttpMethod::Get,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        _ => HttpMethod::Post,
    }
}

/// `{"text": …}` as UTF-8 bytes or `{"hex": …}` decoded.
fn payload_bytes(value: Option<&Value>) -> Result<Vec<u8>, String> {
    match value {
        Some(Value::Object(object)) => match (object.get("text"), object.get("hex")) {
            (Some(Value::String(text)), _) => Ok(text.as_bytes().to_vec()),
            (_, Some(Value::String(hex))) => decode_bytes("hex", hex),
            _ => Err("expected text or hex bytes".to_owned()),
        },
        _ => Err("expected text or hex bytes".to_owned()),
    }
}

/// Bytes of `value` read as `from`: `hex`, `base64` (whitespace and padding forgiven, as
/// `atob`), `utf8` or `latin1` (every character must be U+0000..=U+00FF, as `btoa`).
fn decode_bytes(from: &str, value: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    match from {
        "hex" => {
            let value = value.trim();
            if !value.len().is_multiple_of(2) {
                return Err("hex string has an odd length".to_owned());
            }
            (0..value.len())
                .step_by(2)
                .map(|index| {
                    value
                        .get(index..index + 2)
                        .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                        .ok_or_else(|| "invalid hex string".to_owned())
                })
                .collect()
        }
        "base64" => {
            let compact: String = value
                .chars()
                .filter(|character| !character.is_ascii_whitespace())
                .collect();
            base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(compact.trim_end_matches('='))
                .map_err(|_| "the string to be decoded is not correctly encoded".to_owned())
        }
        "utf8" => Ok(value.as_bytes().to_vec()),
        "latin1" => value
            .chars()
            .map(|character| {
                u8::try_from(u32::from(character)).map_err(|_| {
                    "the string contains characters outside of the Latin1 range".to_owned()
                })
            })
            .collect(),
        other => Err(format!("unsupported encoding {other}")),
    }
}

fn encode_bytes(to: &str, bytes: &[u8]) -> Result<String, String> {
    use base64::Engine as _;
    match to {
        "hex" => Ok(crypto::hex_lower(bytes)),
        "base64" => Ok(base64::engine::general_purpose::STANDARD.encode(bytes)),
        "utf8" => Ok(String::from_utf8_lossy(bytes).into_owned()),
        "latin1" => Ok(bytes.iter().map(|&byte| char::from(byte)).collect()),
        other => Err(format!("unsupported encoding {other}")),
    }
}

fn header_pair(payload: &Value) -> Result<(String, String), String> {
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| "header key is required".to_owned())?;
    let value = payload
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok((key.to_owned(), value.to_owned()))
}

fn request_payload(request: &Request) -> Value {
    json!({
        "method": format!("{:?}", request.method).to_uppercase(),
        "url": request.url,
        "headers": request.headers.iter().map(|header| json!({
            "key": header.key,
            "value": header.value,
            "enabled": header.enabled,
        })).collect::<Vec<_>>(),
        "body": body_raw(&request.body),
        "auth": request.auth,
    })
}

fn body_raw(body: &RequestBody) -> String {
    match body {
        RequestBody::None => String::new(),
        RequestBody::RawText { content, .. } | RequestBody::Json { content } => content.clone(),
        RequestBody::FormUrlEncoded { fields } => {
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for field in fields.iter().filter(|field| field.enabled) {
                serializer.append_pair(&field.key, &field.value);
            }
            serializer.finish()
        }
        RequestBody::Multipart { .. } | RequestBody::Binary { .. } => String::new(),
        RequestBody::Graphql {
            query,
            variables_json,
            operation_name,
        } => serde_json::to_string(&json!({
            "query": query,
            "variables": variables_json,
            "operationName": operation_name,
        }))
        .unwrap_or_default(),
    }
}

fn response_payload(response: &ExecutionResult, ai: Option<&AiScriptResult>) -> Value {
    let mut payload = response_fields(response);
    if let (Some(ai), Value::Object(fields)) = (ai, &mut payload) {
        fields.insert("ai".to_owned(), json!(ai));
    }
    payload
}

fn response_fields(response: &ExecutionResult) -> Value {
    json!({
        "available": true,
        "code": response.status_code,
        "status": response.status_text,
        "elapsed_ms": response.elapsed_ms,
        "size": response.body_size_bytes,
        "url": response.url,
        "headers": response.headers.iter().map(|header| json!({
            "key": header.key,
            "value": header.value,
        })).collect::<Vec<_>>(),
        "cookies": response.cookies.iter().map(|cookie| json!({
            "name": cookie.name,
            "value": cookie.value,
        })).collect::<Vec<_>>(),
    })
}

pub(super) fn host_scopes(host: &HostState) -> VariableScopes {
    let data = host
        .iteration_data
        .iter()
        .filter_map(|(key, value)| EnvironmentVariable::plain(key, value.clone()).ok())
        .collect::<Vec<_>>();
    let local = host
        .local
        .iter()
        .filter_map(|(key, value)| EnvironmentVariable::plain(key, value.clone()).ok())
        .collect::<Vec<_>>();
    let environment = host
        .environment
        .as_ref()
        .map(|environment| environment.variables.clone())
        .unwrap_or_default();

    VariableScopes::new()
        .with_globals(host.globals.clone())
        .with_collection_vars(host.collection_variables.clone())
        .with_environment_vars(environment)
        .with_data_vars(data)
        .with_local_vars(local)
}

/// `pm.environment` with no active environment falls back to the local variable scope (the one
/// `pm.variables.set` writes), so `set`/`get`/`has`/`unset`/`toObject` stay consistent and nothing
/// is persisted. `clear` stays a no-op there: it must not wipe unrelated local variables.
fn effective_scope<'a>(host: &HostState, scope: &'a str) -> &'a str {
    if scope == "environment" && host.environment.is_none() {
        "local"
    } else {
        scope
    }
}

fn variable_get(host: &HostState, scope: &str, key: &str) -> Option<String> {
    match scope {
        "variables" => host_scopes(host).resolve_plain(key),
        "local" => host.local.get(key).cloned(),
        "data" => host.iteration_data.get(key).cloned(),
        _ => scope_variables(host, scope).and_then(|variables| {
            variables
                .iter()
                .find(|variable| variable.enabled && variable.key == key)
                .and_then(|variable| variable.value.clone())
        }),
    }
}

fn scope_variables<'a>(host: &'a HostState, scope: &str) -> Option<&'a Vec<EnvironmentVariable>> {
    match scope {
        "environment" => host
            .environment
            .as_ref()
            .map(|environment| &environment.variables),
        "collection" => Some(&host.collection_variables),
        "globals" => Some(&host.globals),
        _ => None,
    }
}

fn scope_variables_mut<'a>(
    host: &'a mut HostState,
    scope: &str,
) -> Option<&'a mut Vec<EnvironmentVariable>> {
    match scope {
        "environment" => host
            .environment
            .as_mut()
            .map(|environment| &mut environment.variables),
        "collection" => Some(&mut host.collection_variables),
        "globals" => Some(&mut host.globals),
        _ => None,
    }
}

fn variable_set(
    host: &mut HostState,
    scope: &str,
    key: String,
    value: String,
) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("variable key cannot be empty".to_owned());
    }
    match scope {
        "variables" | "local" => {
            host.local.insert(key, value);
        }
        "data" => {
            host.iteration_data.insert(key, value);
        }
        _ => {
            let Some(variables) = scope_variables_mut(host, scope) else {
                return Err(format!("unsupported variable scope {scope}"));
            };
            let credential = holds_credential(&key, &value);
            let mut stays_disabled = false;
            if let Some(variable) = variables.iter_mut().find(|variable| variable.key == key) {
                // A secret stays secret. A plain variable becomes secret when a credential is
                // written into one that never held a value (an imported `token: ""`), or a
                // token-shaped value replaces one that was not (a `<your token>` placeholder);
                // a plain variable that already held a token stays as the user chose.
                let old = variable.value.as_deref().unwrap_or_default();
                let replaces_placeholder = old.is_empty()
                    || (spanbay_base::secrets::is_token_shaped(&value)
                        && !spanbay_base::secrets::is_token_shaped(old));
                if credential && replaces_placeholder {
                    variable.is_secret = true;
                }
                variable.value = Some(value);
                // A variable the user switched off stays off: the value is kept for when it is
                // switched on again.
                stays_disabled = !variable.enabled;
            } else {
                let variable = if credential {
                    EnvironmentVariable::masked(&key, value)
                } else {
                    EnvironmentVariable::plain(&key, value)
                };
                variables.push(variable.map_err(|error| error.to_string())?);
            }
            if stays_disabled {
                push_console(
                    host,
                    ConsoleLevel::Warn,
                    &format!(
                        "{key} is disabled in the {scope} scope: the value was saved and the \
                         variable stays disabled"
                    ),
                );
            }
            if let Some(mutations) = host.mutations.scope_mut(scope) {
                mutations.keys.insert(key);
            }
        }
    }
    Ok(())
}

fn variable_unset(host: &mut HostState, scope: &str, key: &str) {
    match scope {
        "variables" | "local" => {
            host.local.remove(key);
        }
        "data" => {
            host.iteration_data.remove(key);
        }
        _ => {
            if let Some(variables) = scope_variables_mut(host, scope) {
                variables.retain(|variable| variable.key != key);
                if let Some(mutations) = host.mutations.scope_mut(scope) {
                    mutations.keys.insert(key.to_owned());
                }
            }
        }
    }
}

fn variable_clear(host: &mut HostState, scope: &str) {
    match scope {
        "variables" => host.local.clear(),
        "data" => host.iteration_data.clear(),
        _ => {
            if let Some(variables) = scope_variables_mut(host, scope) {
                variables.clear();
                if let Some(mutations) = host.mutations.scope_mut(scope) {
                    mutations.cleared = true;
                }
            }
        }
    }
}

fn variable_list(host: &HostState, scope: &str) -> BTreeMap<String, String> {
    match scope {
        "variables" => host_scopes(host).merged_plain_variables(),
        "local" => host.local.clone(),
        "data" => host.iteration_data.clone(),
        _ => scope_variables(host, scope)
            .map(|variables| {
                variables
                    .iter()
                    .filter(|variable| variable.enabled)
                    .filter_map(|variable| {
                        variable
                            .value
                            .clone()
                            .map(|value| (variable.key.clone(), value))
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default(),
    }
}

fn ensure_not_cancelled(cancel: Option<&CancelToken>) -> Result<(), String> {
    if cancel.is_some_and(CancelToken::is_cancelled) {
        Err(CANCELLED_MESSAGE.to_owned())
    } else {
        Ok(())
    }
}

/// The `pm.sendRequest` clients of one run, by the id of the client certificate they present
/// (`""` for none).
pub(super) type ScriptClients = Vec<(String, reqwest::blocking::Client)>;

pub(super) fn script_http_request(
    payload: &Value,
    cancel: Option<&CancelToken>,
    limits: &ScriptLimits,
    http_clients: &mut ScriptClients,
) -> Result<String, String> {
    ensure_not_cancelled(cancel)?;
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "pm.sendRequest requires a URL".to_owned())?;
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_ascii_uppercase();
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|error| format!("invalid method: {error}"))?;
    let parsed = url::Url::parse(url).ok();
    if let Some(parsed) = parsed.as_ref() {
        crate::executor::ensure_script_target_allowed(parsed)?;
    }
    let body = super::body::send_request_body(payload.get("body"))?;

    let max_response_bytes = limits.max_response_bytes;
    // The client certificate the target host matches, like the main send's.
    let certificate = parsed.as_ref().and_then(|parsed| {
        crate::executor::matching_client_certificate(
            &limits.network.tls.certificates,
            &parsed.host_str().unwrap_or_default().to_ascii_lowercase(),
            parsed.port_or_known_default().unwrap_or(0),
        )
    });
    let client_key = certificate.map_or_else(String::new, |certificate| {
        certificate.id.as_str().to_owned()
    });
    let client = match http_clients.iter().position(|(key, _)| *key == client_key) {
        Some(index) => http_clients[index].1.clone(),
        None => {
            let client = crate::executor::blocking_http_client(&limits.network, certificate)?;
            http_clients.push((client_key, client.clone()));
            client
        }
    };
    let mut builder = client.request(method, url);
    let mut has_content_type = false;
    if let Some(headers) = payload.get("headers").and_then(Value::as_array) {
        for header in headers {
            let Some(key) = header.get("key").and_then(Value::as_str) else {
                continue;
            };
            let value = header
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default();
            has_content_type |= key.trim().eq_ignore_ascii_case("content-type");
            builder = builder.header(key, value);
        }
    }
    builder = match body {
        super::body::ScriptBody::Empty => builder,
        super::body::ScriptBody::Text { text, content_type } => {
            if let Some(content_type) = content_type.filter(|_| !has_content_type) {
                builder = builder.header(reqwest::header::CONTENT_TYPE, content_type);
            }
            builder.body(text)
        }
        super::body::ScriptBody::Multipart(fields) => {
            let form = fields.into_iter().fold(
                reqwest::blocking::multipart::Form::new(),
                |form, (key, value)| form.text(key, value),
            );
            builder.multipart(form)
        }
    };

    let started = Instant::now();
    // `reqwest::blocking` cannot be interrupted, so the whole exchange (send and body read) runs on
    // its own thread, bounded by the client timeout, while this thread polls the cancel token. The
    // thread polls it too between body reads, so a cancelled script stops downloading.
    let (sender, receiver) = mpsc::channel();
    let thread_cancel = cancel.cloned();
    std::thread::Builder::new()
        .spawn(move || {
            let _ = sender.send(fetch(
                builder,
                thread_cancel.as_ref(),
                started,
                max_response_bytes,
            ));
        })
        .map_err(|error| error.to_string())?;
    loop {
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => ensure_not_cancelled(cancel)?,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("script request thread stopped unexpectedly".to_owned());
            }
        }
    }
}

/// Sends `builder` and reads the capped response body; returns the JSON handed to the script.
/// Errors never carry the request URL (it may hold credentials).
fn fetch(
    builder: reqwest::blocking::RequestBuilder,
    cancel: Option<&CancelToken>,
    started: Instant,
    max_response_bytes: usize,
) -> Result<String, String> {
    let response = builder
        .send()
        .map_err(|error| error.without_url().to_string())?;
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(key, value)| {
            json!({
                "key": key.as_str(),
                "value": value.to_str().unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let cookies = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| std::str::from_utf8(value.as_bytes()).ok())
        .map(crate::executor::parse_set_cookie_header)
        .filter(|cookie| !cookie.name.is_empty())
        .map(|cookie| {
            json!({
                "name": cookie.name,
                "value": cookie.value,
                "domain": cookie.domain,
                "path": cookie.path,
                "secure": cookie.secure,
                "httpOnly": cookie.http_only,
                "expires": cookie.expires,
                "maxAge": cookie.max_age,
                "sameSite": cookie.same_site,
            })
        })
        .collect::<Vec<_>>();
    let mut body_reader = response.take(max_response_bytes as u64 + 1);
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        ensure_not_cancelled(cancel)?;
        match body_reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => bytes.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            // The io error wraps a reqwest error whose text includes the URL: report the kind only.
            Err(error) => {
                return Err(format!(
                    "failed to read the response body: {:?}",
                    error.kind()
                ));
            }
        }
    }
    let truncated = bytes.len() > max_response_bytes;
    bytes.truncate(max_response_bytes);
    let body = String::from_utf8_lossy(&bytes).into_owned();

    serde_json::to_string(&json!({
        "code": status.as_u16(),
        "status": status.canonical_reason().unwrap_or_default(),
        "headers": headers,
        "cookies": cookies,
        "body": body,
        "size": bytes.len(),
        "elapsed_ms": started.elapsed().as_millis(),
        "truncated": truncated,
    }))
    .map_err(|error| error.to_string())
}
