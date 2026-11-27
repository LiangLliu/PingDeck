use std::collections::HashMap;

use serde_json::Value;

use super::helpers::{
    Ids, RequestDraft, build_request, folder_for, http_method, json_body_like, parse_json,
    query_pairs, request_name, scalar_string, split_url_query, take_authorization_auth, url_host,
};
use super::{ImportError, ImportedCollection};
use crate::workspace::models::{
    AuthConfig, Collection, FolderId, HttpMethod, KeyValue, RequestBody,
};

/// Imports a HAR archive; every entry becomes a request grouped by host.
pub fn import_har(text: &str, workspace_id: &str) -> Result<ImportedCollection, ImportError> {
    let value = parse_json(text)?;
    let log = value.get("log").and_then(Value::as_object).ok_or_else(|| {
        ImportError::UnsupportedDocument {
            message: "HAR document has no 'log' object".to_string(),
        }
    })?;
    let entries = log
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| ImportError::UnsupportedDocument {
            message: "HAR log.entries is missing or not an array".to_string(),
        })?;

    let mut warnings = Vec::new();
    let mut prepared = Vec::new();

    for (index, entry) in entries.iter().enumerate() {
        let label = format!("HAR entry {}", index + 1);
        let Some(request) = entry.get("request").and_then(Value::as_object) else {
            warnings.push(format!("{label}: missing request; skipped"));
            continue;
        };
        let raw_url = request
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty());
        let Some(raw_url) = raw_url else {
            warnings.push(format!("{label}: missing request.url; skipped"));
            continue;
        };

        let method = match request
            .get("method")
            .and_then(Value::as_str)
            .map(|method| method.trim().to_ascii_uppercase())
        {
            Some(name) => match http_method(&name) {
                Some(method) => method,
                None => {
                    warnings.push(format!("{label}: unsupported method '{name}'; using GET"));
                    HttpMethod::Get
                }
            },
            None => HttpMethod::Get,
        };

        let mut headers = har_headers(request.get("headers"));
        let auth = take_authorization_auth(&mut headers).unwrap_or(AuthConfig::None);
        let query = {
            let declared = har_query(request.get("queryString"));
            if declared.is_empty() {
                query_pairs(split_url_query(raw_url).1)
            } else {
                declared
            }
        };
        let body = har_body(request.get("postData"), &label, &mut warnings);
        let url = split_url_query(raw_url).0.to_string();
        let host = url_host(&url);

        prepared.push(HarPrepared {
            name: request_name(method, &url),
            method,
            url,
            query_params: query,
            headers,
            body,
            auth,
            host,
        });
    }

    let mut shared_host: Option<&str> = None;
    let mut multiple_hosts = false;
    for entry in &prepared {
        match (shared_host, entry.host.as_deref()) {
            (None, Some(host)) => shared_host = Some(host),
            (Some(known), Some(host)) if known != host => multiple_hosts = true,
            _ => {}
        }
    }
    let name = if multiple_hosts {
        "HAR import".to_string()
    } else {
        shared_host.unwrap_or("HAR import").to_string()
    };

    let mut ids = Ids::new(&name);
    let collection = Collection::new(ids.next("col"), workspace_id, &name)?;
    let collection_id = collection.id.clone();
    let mut folders = Vec::new();
    let mut folder_ids: HashMap<String, FolderId> = HashMap::new();
    let mut requests = Vec::new();

    for (position, entry) in prepared.into_iter().enumerate() {
        let folder_id = match entry.host.as_deref() {
            Some(host) => Some(folder_for(
                &mut folders,
                &mut folder_ids,
                &collection_id,
                &mut ids,
                host,
            )?),
            None => None,
        };
        let draft = RequestDraft {
            name: entry.name,
            method: entry.method,
            url: entry.url,
            query_params: entry.query_params,
            headers: entry.headers,
            body: entry.body,
            auth: entry.auth,
            description: String::new(),
            folder_id,
        };
        requests.push(build_request(
            draft,
            &mut ids,
            workspace_id,
            &collection_id,
            position as i64,
        )?);
    }

    Ok(ImportedCollection {
        collection,
        folders,
        requests,
        warnings,
    })
}

#[derive(Debug)]
struct HarPrepared {
    name: String,
    method: HttpMethod,
    url: String,
    query_params: Vec<KeyValue>,
    headers: Vec<KeyValue>,
    body: RequestBody,
    auth: AuthConfig,
    host: Option<String>,
}

fn har_headers(value: Option<&Value>) -> Vec<KeyValue> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let name = object.get("name").and_then(scalar_string)?;
            // HTTP/2 pseudo-headers and the headers the client computes itself are not replayed.
            let trimmed = name.trim();
            if trimmed.is_empty()
                || trimmed.starts_with(':')
                || trimmed.eq_ignore_ascii_case("host")
                || trimmed.eq_ignore_ascii_case("content-length")
            {
                return None;
            }
            Some(KeyValue {
                key: name,
                value: object
                    .get("value")
                    .and_then(scalar_string)
                    .unwrap_or_default(),
                enabled: true,
                description: None,
            })
        })
        .collect()
}

fn har_query(value: Option<&Value>) -> Vec<KeyValue> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let key = object.get("name").and_then(scalar_string)?;
            if key.trim().is_empty() {
                return None;
            }
            Some(KeyValue {
                key,
                value: object
                    .get("value")
                    .and_then(scalar_string)
                    .unwrap_or_default(),
                enabled: true,
                description: None,
            })
        })
        .collect()
}

fn har_body(post_data: Option<&Value>, label: &str, warnings: &mut Vec<String>) -> RequestBody {
    let Some(object) = post_data.and_then(Value::as_object) else {
        return RequestBody::None;
    };
    let mime_type = object
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let text = object
        .get("text")
        .and_then(scalar_string)
        .filter(|text| !text.is_empty());

    if let Some(text) = text {
        if mime_type.contains("json") && json_body_like(&text) {
            return RequestBody::Json { content: text };
        }
        return RequestBody::RawText {
            content: text,
            language: None,
        };
    }
    if object
        .get("params")
        .and_then(Value::as_array)
        .is_some_and(|params| !params.is_empty())
    {
        warnings.push(format!(
            "{label}: postData params without text are not imported"
        ));
    }
    RequestBody::None
}
