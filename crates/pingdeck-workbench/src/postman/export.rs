use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use pingdeck_model::credentials::{
    CredentialPosition, UrlPart, credential_positions, credential_value_mut,
};

use super::fidelity::{
    ExportNotes, export_examples, protocol_profile_behavior, test_script_with_assertions,
};
use super::parse::{is_variable_template, key_values_from_query_string, raw_language_name};
use super::{POSTMAN_SCHEMA_URL, PostmanError, PostmanExport, PostmanExportContext};
use crate::workspace::ai_request::plain_http_for_export;
use crate::workspace::models::{
    ApiKeyLocation, AuthConfig, Collection, EnvironmentVariable, Folder, FormPart, HttpMethod,
    KeyValue, RawLanguage, Request, RequestBody,
};

/// AI requests and literal credentials seen while emitting items.
struct AiExport<'a> {
    urls: &'a HashMap<String, String>,
    attachments: usize,
    unresolved: Vec<String>,
    /// Literal credentials are written only when asked for.
    keep_credentials: bool,
    stripped: usize,
    /// What Postman cannot hold, counted for the warnings.
    notes: ExportNotes,
}

impl AiExport<'_> {
    /// A credential value as exported: blank and `{{variable}}` values stay, a literal is left
    /// empty unless the export keeps literal credentials.
    fn credential(&mut self, value: &str) -> String {
        if self.keep_credentials || value.trim().is_empty() || value.contains("{{") {
            return value.to_owned();
        }
        self.stripped += 1;
        String::new()
    }
}

/// Serializes a collection with its folders, requests and variables as Postman v2.1 JSON.
/// AI API requests are written as plain HTTP requests.
pub fn export_collection(
    collection: &Collection,
    folders: &[Folder],
    requests: &[Request],
    variables: &[EnvironmentVariable],
    context: &PostmanExportContext,
) -> Result<PostmanExport, PostmanError> {
    let folder_ids: HashSet<&str> = folders
        .iter()
        .filter(|folder| folder.collection_id == collection.id)
        .map(|folder| folder.id.as_str())
        .collect();

    let mut folders_by_parent: HashMap<Option<&str>, Vec<&Folder>> = HashMap::new();
    for folder in folders
        .iter()
        .filter(|folder| folder.collection_id == collection.id)
    {
        let parent = folder
            .parent_folder_id
            .as_ref()
            .map(|parent| parent.as_str())
            .filter(|parent| folder_ids.contains(parent) && *parent != folder.id.as_str());
        folders_by_parent.entry(parent).or_default().push(folder);
    }

    let mut requests_by_folder: HashMap<Option<&str>, Vec<&Request>> = HashMap::new();
    for request in requests.iter().filter(|request| {
        request.collection_id.as_ref() == Some(&collection.id)
            || (request.collection_id.is_none()
                && request
                    .folder_id
                    .as_ref()
                    .is_some_and(|id| folder_ids.contains(id.as_str())))
    }) {
        let parent = request
            .folder_id
            .as_ref()
            .map(|folder_id| folder_id.as_str())
            .filter(|folder_id| folder_ids.contains(folder_id));
        requests_by_folder.entry(parent).or_default().push(request);
    }

    let mut ancestry = Vec::new();
    let mut ai = AiExport {
        urls: &context.ai_urls,
        attachments: 0,
        unresolved: Vec::new(),
        keep_credentials: context.include_literal_credentials,
        stripped: 0,
        notes: ExportNotes::default(),
    };
    let items = emit_level(
        None,
        &folders_by_parent,
        &requests_by_folder,
        &mut ancestry,
        &mut ai,
    );

    let exported_variables = merged_variables(&collection.variables, variables);
    let mut info = Map::new();
    info.insert("name".to_string(), Value::String(collection.name.clone()));
    info.insert(
        "schema".to_string(),
        Value::String(POSTMAN_SCHEMA_URL.to_string()),
    );
    insert_text(&mut info, "description", &collection.description);

    let mut envelope = Map::new();
    envelope.insert("info".to_string(), Value::Object(info));
    if collection.auth != AuthConfig::Inherit {
        envelope.insert("auth".to_string(), export_auth(&collection.auth, &mut ai));
    }
    if let Some(events) = export_events(&collection.pre_request_script, &collection.test_script) {
        envelope.insert("event".to_string(), events);
    }
    envelope.insert("item".to_string(), Value::Array(items));
    envelope.insert(
        "variable".to_string(),
        Value::Array(
            exported_variables
                .into_iter()
                .map(export_variable)
                .collect(),
        ),
    );
    let envelope = Value::Object(envelope);

    let json =
        serde_json::to_string_pretty(&envelope).map_err(|error| PostmanError::Serialization {
            message: error.to_string(),
        })?;
    let mut warnings = Vec::new();
    for name in ai.unresolved {
        warnings.push(format!(
            "The provider of request \"{name}\" could not be resolved; its URL is a placeholder"
        ));
    }
    match ai.stripped {
        0 => {}
        1 => warnings.push(
            "1 literal credential was left empty; include literal credentials to export it"
                .to_owned(),
        ),
        count => warnings.push(format!(
            "{count} literal credentials were left empty; include literal credentials to export them"
        )),
    }
    match ai.attachments {
        0 => {}
        1 => warnings.push("1 attachment is not exported".to_owned()),
        count => warnings.push(format!("{count} attachments are not exported")),
    }
    warnings.extend(ai.notes.warnings());
    Ok(PostmanExport { json, warnings })
}

fn emit_level(
    parent: Option<&str>,
    folders_by_parent: &HashMap<Option<&str>, Vec<&Folder>>,
    requests_by_folder: &HashMap<Option<&str>, Vec<&Request>>,
    ancestry: &mut Vec<String>,
    ai: &mut AiExport<'_>,
) -> Vec<Value> {
    let mut entries: Vec<(i64, &str, &str, Value)> = Vec::new();

    if let Some(folders) = folders_by_parent.get(&parent) {
        for folder in folders {
            if ancestry
                .iter()
                .any(|ancestor| ancestor == folder.id.as_str())
            {
                continue;
            }
            ancestry.push(folder.id.as_str().to_string());
            let children = emit_level(
                Some(folder.id.as_str()),
                folders_by_parent,
                requests_by_folder,
                ancestry,
                ai,
            );
            ancestry.pop();

            let mut payload = Map::new();
            payload.insert("name".to_string(), Value::String(folder.name.clone()));
            payload.insert("item".to_string(), Value::Array(children));
            insert_text(&mut payload, "description", &folder.description);
            if folder.auth != AuthConfig::Inherit {
                payload.insert("auth".to_string(), export_auth(&folder.auth, ai));
            }
            if let Some(events) = export_events(&folder.pre_request_script, &folder.test_script) {
                payload.insert("event".to_string(), events);
            }
            entries.push((
                folder.position,
                folder.name.as_str(),
                folder.id.as_str(),
                Value::Object(payload),
            ));
        }
    }

    if let Some(requests) = requests_by_folder.get(&parent) {
        for request in requests {
            entries.push((
                request.position,
                request.name.as_str(),
                request.id.as_str(),
                export_request_item(request, ai),
            ));
        }
    }

    entries.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(right.1))
            .then_with(|| left.2.cmp(right.2))
    });
    entries.into_iter().map(|(_, _, _, value)| value).collect()
}

fn export_request_item(request: &Request, ai: &mut AiExport<'_>) -> Value {
    let plain;
    let request = match plain_http_for_export(
        request,
        ai.urls.get(request.id.as_str()).map(String::as_str),
    ) {
        Some(export) => {
            ai.attachments += export.attachments;
            if export.unresolved_provider {
                ai.unresolved.push(request.name.clone());
            }
            plain = export.request;
            &plain
        }
        None => request,
    };
    let mut redacted = request.clone();
    strip_credentials(&mut redacted, ai);
    let request = &redacted;
    let mut payload = Map::new();
    payload.insert(
        "method".to_string(),
        Value::String(method_name(request.method).to_string()),
    );
    let headers = request.headers.iter().map(export_key_value).collect();
    payload.insert("header".to_string(), Value::Array(headers));
    payload.insert("url".to_string(), export_url(request));
    if let Some(body) = export_body(&request.body) {
        payload.insert("body".to_string(), body);
    }
    if request.auth != AuthConfig::Inherit {
        payload.insert("auth".to_string(), export_auth(&request.auth, ai));
    }

    let mut item = Map::new();
    item.insert("name".to_string(), Value::String(request.name.clone()));
    item.insert("request".to_string(), Value::Object(payload));
    insert_text(&mut item, "description", &request.description);
    let test_script =
        test_script_with_assertions(&request.test_script, &request.assertions, &mut ai.notes);
    if let Some(events) = export_events(&request.pre_request_script, &test_script) {
        item.insert("event".to_string(), events);
    }
    if let Some(behavior) = protocol_profile_behavior(&request.network_options, &mut ai.notes) {
        item.insert("protocolProfileBehavior".to_string(), behavior);
    }
    if !request.examples.is_empty() {
        item.insert(
            "response".to_string(),
            Value::Array(export_examples(&request.examples)),
        );
    }
    Value::Object(item)
}

/// Passes every credential position of `request` (the rule set copied code and folder sync
/// share) through [`AiExport::credential`]: a literal is left empty unless the export keeps
/// literal credentials. Emptied URL userinfo goes with its `@`.
fn strip_credentials(request: &mut Request, ai: &mut AiExport<'_>) {
    let mut url_parts = Vec::new();
    for position in credential_positions(request) {
        if let CredentialPosition::Url(found) = position {
            url_parts.push(found);
        } else if let Some((_, value)) = credential_value_mut(request, &position) {
            *value = ai.credential(value);
        }
    }
    let mut url = request.url.clone();
    for found in url_parts.iter().rev() {
        let Some(text) = request.url.get(found.range.clone()) else {
            continue;
        };
        let kept = ai.credential(text);
        if kept == text {
            continue;
        }
        match found.part {
            UrlPart::Userinfo => url.replace_range(found.range.start..=found.range.end, ""),
            UrlPart::Query(_) => url.replace_range(found.range.clone(), &kept),
        }
    }
    request.url = url;
}

fn insert_text(object: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.is_empty() {
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn export_events(pre_request_script: &str, test_script: &str) -> Option<Value> {
    let mut events = Vec::new();
    if !pre_request_script.is_empty() {
        events.push(export_event("prerequest", pre_request_script));
    }
    if !test_script.is_empty() {
        events.push(export_event("test", test_script));
    }
    if events.is_empty() {
        None
    } else {
        Some(Value::Array(events))
    }
}

fn export_event(listen: &str, script: &str) -> Value {
    json!({
        "listen": listen,
        "script": {
            "type": "text/javascript",
            "exec": script.split('\n').collect::<Vec<_>>(),
        },
    })
}

fn export_url(request: &Request) -> Value {
    let (without_fragment, fragment) = request.url.split_once('#').unwrap_or((&request.url, ""));
    let (base, original_query) = without_fragment
        .split_once('?')
        .unwrap_or((without_fragment, ""));
    // Match execution: enabled rows override URL keys, while other URL pairs remain.
    let override_keys: HashSet<&str> = request
        .query_params
        .iter()
        .filter(|param| param.enabled)
        .map(|param| param.key.as_str())
        .collect();
    let mut query_parts = Vec::new();
    let mut query = Vec::new();
    for part in original_query.split('&').filter(|part| !part.is_empty()) {
        let pair = url::form_urlencoded::parse(part.as_bytes()).next();
        if pair
            .as_ref()
            .is_some_and(|(key, _)| override_keys.contains(key.as_ref()))
        {
            continue;
        }
        query_parts.push(part.to_string());
        query.extend(
            key_values_from_query_string(part)
                .iter()
                .map(export_key_value),
        );
    }
    for param in &request.query_params {
        query.push(export_key_value(param));
        if param.enabled {
            query_parts.push(format!(
                "{}={}",
                encode_query_component(&param.key),
                encode_query_component(&param.value)
            ));
        }
    }
    let mut raw = base.to_string();
    if !query_parts.is_empty() {
        raw.push('?');
        raw.push_str(&query_parts.join("&"));
    } else if without_fragment.ends_with('?') {
        raw.push('?');
    }
    if request.url.contains('#') {
        raw.push('#');
        raw.push_str(fragment);
    }

    json!({
        "raw": raw,
        "query": query,
        "variable": request.path_variables.iter().map(export_key_value).collect::<Vec<_>>(),
    })
}

fn encode_query_component(mut value: &str) -> String {
    let mut encoded = String::new();
    while let Some(start) = value.find("{{") {
        let Some(end) = value[start + 2..].find("}}") else {
            break;
        };
        encoded.extend(url::form_urlencoded::byte_serialize(
            &value.as_bytes()[..start],
        ));
        let end = start + 2 + end + 2;
        encoded.push_str(&value[start..end]);
        value = &value[end..];
    }
    encoded.extend(url::form_urlencoded::byte_serialize(value.as_bytes()));
    encoded
}

fn export_key_value(pair: &KeyValue) -> Value {
    let mut object = Map::new();
    object.insert("key".to_string(), Value::String(pair.key.clone()));
    object.insert("value".to_string(), Value::String(pair.value.clone()));
    if !pair.enabled {
        object.insert("disabled".to_string(), Value::Bool(true));
    }
    if let Some(description) = pair.description.as_deref().filter(|text| !text.is_empty()) {
        object.insert(
            "description".to_string(),
            Value::String(description.to_string()),
        );
    }
    Value::Object(object)
}

fn export_body(body: &RequestBody) -> Option<Value> {
    match body {
        RequestBody::None => None,
        RequestBody::Json { content } => Some(json!({
            "mode": "raw",
            "raw": content,
            "options": { "raw": { "language": "json" } },
        })),
        RequestBody::RawText { content, language } => Some(match language {
            None | Some(RawLanguage::Text) => json!({
                "mode": "raw",
                "raw": content,
            }),
            Some(language) => json!({
                "mode": "raw",
                "raw": content,
                "options": { "raw": { "language": raw_language_name(*language) } },
            }),
        }),
        RequestBody::FormUrlEncoded { fields } => Some(json!({
            "mode": "urlencoded",
            "urlencoded": fields.iter().map(export_key_value).collect::<Vec<_>>(),
        })),
        RequestBody::Multipart { parts } => Some(json!({
            "mode": "formdata",
            "formdata": parts.iter().map(export_form_part).collect::<Vec<_>>(),
        })),
        RequestBody::Graphql {
            query,
            variables_json,
            operation_name,
        } => {
            let mut graphql = Map::new();
            graphql.insert("query".to_string(), Value::String(query.clone()));
            if let Some(variables) = variables_json {
                graphql.insert("variables".to_string(), Value::String(variables.clone()));
            }
            if let Some(name) = operation_name {
                graphql.insert("operationName".to_string(), Value::String(name.clone()));
            }
            Some(json!({
                "mode": "graphql",
                "graphql": Value::Object(graphql),
            }))
        }
        RequestBody::Binary {
            file_path,
            content_type,
        } => Some(json!({
            "mode": "file",
            "file": {
                "src": exported_file_src(file_path),
                "contentType": content_type,
            },
        })),
    }
}

/// Local file paths never leave the machine: only a pure `{{variable}}` template is exported.
fn exported_file_src(path: &str) -> &str {
    if is_variable_template(path) { path } else { "" }
}

fn export_form_part(part: &FormPart) -> Value {
    let mut object = Map::new();
    object.insert("key".to_string(), Value::String(part.key.clone()));
    match &part.file_path {
        Some(path) => {
            object.insert("type".to_string(), Value::String("file".to_string()));
            object.insert(
                "src".to_string(),
                Value::String(exported_file_src(path).to_string()),
            );
        }
        None => {
            object.insert("type".to_string(), Value::String("text".to_string()));
            object.insert("value".to_string(), Value::String(part.value.clone()));
        }
    }
    if !part.enabled {
        object.insert("disabled".to_string(), Value::Bool(true));
    }
    Value::Object(object)
}

fn export_auth(auth: &AuthConfig, ai: &mut AiExport<'_>) -> Value {
    let mut secret = |value: &str| ai.credential(value);
    let entry = |key: &str, value: &str| {
        json!({
            "key": key,
            "value": value,
            "type": "string",
        })
    };

    match auth {
        AuthConfig::Inherit => json!({ "type": "inherit" }),
        AuthConfig::None => json!({ "type": "noauth" }),
        AuthConfig::OAuth2 {
            grant_type,
            access_token_url,
            client_id,
            client_secret_ref,
            username,
            password_ref,
            scope,
            authorization_url,
            redirect_port,
            client_auth,
            extra_params,
        } => {
            let mut fields = vec![
                entry(
                    "grant_type",
                    match grant_type {
                        crate::workspace::models::OAuth2GrantType::ClientCredentials => {
                            "client_credentials"
                        }
                        crate::workspace::models::OAuth2GrantType::Password => "password",
                        crate::workspace::models::OAuth2GrantType::AuthorizationCodePkce => {
                            "authorization_code_with_pkce"
                        }
                    },
                ),
                entry("accessTokenUrl", access_token_url),
                entry("clientId", client_id),
                entry("clientSecret", &secret(client_secret_ref)),
                entry("username", username),
                entry("password", &secret(password_ref)),
                entry("scope", scope),
                entry(
                    "client_authentication",
                    if client_auth.is_body() {
                        "body"
                    } else {
                        "header"
                    },
                ),
            ];
            if !authorization_url.is_empty() {
                fields.push(entry("authUrl", authorization_url));
            }
            if let Some(port) = redirect_port {
                fields.push(entry(
                    "redirect_uri",
                    &format!("http://127.0.0.1:{port}/callback"),
                ));
            }
            // The extra parameters go with the token request, and with the authorization URL
            // of the interactive grant, as PingDeck sends them.
            if !extra_params.is_empty() {
                let rows = |send_as: &str| {
                    extra_params
                        .iter()
                        .map(|row| {
                            json!({
                                "key": row.key,
                                "value": row.value,
                                "enabled": row.enabled,
                                "send_as": send_as,
                            })
                        })
                        .collect::<Vec<_>>()
                };
                fields.push(json!({
                    "key": "tokenRequestParams",
                    "value": rows("request_body"),
                    "type": "any",
                }));
                let interactive =
                    *grant_type == crate::workspace::models::OAuth2GrantType::AuthorizationCodePkce;
                if interactive {
                    fields.push(json!({
                        "key": "authRequestParams",
                        "value": rows("request_url"),
                        "type": "any",
                    }));
                }
            }
            json!({ "type": "oauth2", "oauth2": fields })
        }
        AuthConfig::Digest {
            username,
            password_ref,
        } => json!({
            "type": "digest",
            "digest": [
                entry("username", username),
                entry("password", &secret(password_ref.as_deref().unwrap_or_default())),
            ],
        }),
        AuthConfig::AwsSignatureV4 {
            access_key,
            secret_key_ref,
            region,
            service,
            session_token_ref,
        } => json!({
            "type": "awsv4",
            "awsv4": [
                entry("accessKey", access_key),
                entry("secretKey", &secret(secret_key_ref.as_deref().unwrap_or_default())),
                entry("region", region),
                entry("service", service),
                entry(
                    "sessionToken",
                    &secret(session_token_ref.as_deref().unwrap_or_default()),
                ),
            ],
        }),
        AuthConfig::Bearer { token_ref } => json!({
            "type": "bearer",
            "bearer": [entry("token", &secret(token_ref))],
        }),
        AuthConfig::Basic {
            username,
            password_ref,
        } => json!({
            "type": "basic",
            "basic": [entry("username", username), entry("password", &secret(password_ref))],
        }),
        AuthConfig::ApiKey {
            key,
            value_ref,
            location,
        } => json!({
            "type": "apikey",
            "apikey": [
                entry("key", key),
                entry("value", &secret(value_ref)),
                entry(
                    "in",
                    match location {
                        ApiKeyLocation::Header => "header",
                        ApiKeyLocation::Query => "query",
                    },
                ),
            ],
        }),
    }
}

/// The exported variables: the collection's, then the chosen environment's laid over them (an
/// enabled environment row replaces the collection row of the same key, as at send time, where
/// the environment wins) and its other enabled rows appended.
fn merged_variables<'a>(
    collection: &'a [EnvironmentVariable],
    environment: &'a [EnvironmentVariable],
) -> Vec<&'a EnvironmentVariable> {
    let enabled: Vec<&EnvironmentVariable> = environment
        .iter()
        .filter(|variable| variable.enabled)
        .collect();
    let mut merged: Vec<&EnvironmentVariable> = collection
        .iter()
        .map(|variable| {
            enabled
                .iter()
                .find(|overlay| overlay.key == variable.key)
                .copied()
                .unwrap_or(variable)
        })
        .collect();
    for variable in enabled {
        if !collection.iter().any(|own| own.key == variable.key) {
            merged.push(variable);
        }
    }
    merged
}

fn export_variable(variable: &EnvironmentVariable) -> Value {
    let mut object = Map::new();
    object.insert("key".to_string(), Value::String(variable.key.clone()));
    // Secret values never leave the app in a shareable export.
    let value = if variable.is_secret {
        String::new()
    } else {
        variable.value.clone().unwrap_or_default()
    };
    object.insert("value".to_string(), Value::String(value));
    if variable.is_secret {
        object.insert("type".to_string(), Value::String("secret".to_string()));
    }
    if !variable.enabled {
        object.insert("disabled".to_string(), Value::Bool(true));
    }
    Value::Object(object)
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
