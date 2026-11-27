use std::collections::HashMap;

use serde_json::{Map, Value};

use super::helpers::{
    Ids, RequestDraft, build_request, folder_for, method_label, non_empty_or, parse_json_or_yaml,
    scalar_string,
};
use super::{ImportError, ImportedCollection};
use crate::workspace::items::{
    MockResponse, MockResponseMode, MockRoute, MockRouteType, MockRulesOperator,
};
use crate::workspace::models::{
    ApiKeyLocation, AuthConfig, Collection, EnvironmentVariable, FolderId, FormPart, HttpMethod,
    KeyValue, MAX_REQUEST_EXAMPLES, RequestBody, RequestExample,
};

const MAX_SCHEMA_DEPTH: usize = 8;
const MAX_REF_HOPS: usize = 16;
/// Nodes one generated JSON body example may hold; the rest of a bigger schema is left out.
const MAX_SCHEMA_NODES: usize = 10_000;

/// Imports an OpenAPI 2.0 (`swagger: "2.0"`) or OpenAPI 3.x document.
///
/// Operations become requests, first tags become folders, and the base URL is
/// exposed as the `baseUrl` collection variable. `$ref`s are resolved locally;
/// unsupported constructs are reported as warnings.
pub fn import_openapi(text: &str, workspace_id: &str) -> Result<ImportedCollection, ImportError> {
    let root = parse_json_or_yaml(text)?;
    root.as_object()
        .ok_or_else(|| ImportError::UnsupportedDocument {
            message: "OpenAPI document root must be a JSON object".to_string(),
        })?;
    let version = openapi_version(&root)?;

    let mut warnings = Vec::new();
    let info = root.get("info").and_then(Value::as_object);
    let raw_title = info
        .and_then(|info| info.get("title"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if raw_title.is_empty() {
        warnings
            .push("OpenAPI: info.title is missing; using a default collection name".to_string());
    }
    let name = non_empty_or(raw_title, "OpenAPI import");
    let description = info
        .and_then(|info| info.get("description"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let base_url = openapi_base_url(&root, version, &mut warnings);

    let security_schemes = root
        .get("components")
        .and_then(|components| components.get("securitySchemes"))
        .or_else(|| root.get("securityDefinitions"))
        .and_then(Value::as_object);

    let mut ids = Ids::new(&name);
    let mut collection = Collection::new(ids.next("col"), workspace_id, &name)?;
    collection.description = description;
    collection
        .variables
        .push(EnvironmentVariable::plain("baseUrl", base_url)?);
    let collection_id = collection.id.clone();

    if let Some(security) = root.get("security")
        && let Some(auth) = security_auth(
            &root,
            security_schemes,
            security,
            &mut warnings,
            "collection",
        )
    {
        collection.auth = auth;
    }

    let mut folders = Vec::new();
    let mut folder_ids: HashMap<String, FolderId> = HashMap::new();
    let mut requests = Vec::new();

    let Some(paths) = root.get("paths").and_then(Value::as_object) else {
        warnings.push("OpenAPI: document has no paths object".to_string());
        return Ok(ImportedCollection {
            collection,
            folders,
            requests,
            warnings,
        });
    };

    for (path, path_item) in paths {
        let path_item = resolve_local_ref(&root, path_item);
        let Some(path_item) = path_item.as_object() else {
            warnings.push(format!("OpenAPI: path '{path}' is not an object; skipped"));
            continue;
        };
        let path_parameters = path_item
            .get("parameters")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for (operation_key, method) in OPENAPI_METHODS {
            let Some(operation) = path_item.get(operation_key).and_then(Value::as_object) else {
                continue;
            };
            let label = format!("OpenAPI {} {path}", method_label(method));

            let summary = operation
                .get("summary")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|summary| !summary.is_empty());
            let operation_id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty());
            let operation_name = summary
                .or(operation_id)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} {path}", method_label(method)));
            let operation_description = operation
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            let tag = operation
                .get("tags")
                .and_then(Value::as_array)
                .and_then(|tags| tags.iter().find_map(Value::as_str))
                .map(str::trim)
                .filter(|tag| !tag.is_empty());
            let folder_id = match tag {
                Some(tag) => Some(folder_for(
                    &mut folders,
                    &mut folder_ids,
                    &collection_id,
                    &mut ids,
                    tag,
                )?),
                None => None,
            };

            let operation_parameters = operation
                .get("parameters")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let parameters = merge_parameters(&root, &path_parameters, &operation_parameters);

            let mut query_params = Vec::new();
            let mut headers = Vec::new();
            let mut form_fields = Vec::new();
            let mut form_has_file = false;
            let mut body_parameter: Option<&Map<String, Value>> = None;
            let mut warned_cookie = false;

            for parameter in &parameters {
                let Some(parameter) = parameter.as_object() else {
                    continue;
                };
                let parameter_name = parameter
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                if parameter_name.is_empty() {
                    continue;
                }
                let location = parameter
                    .get("in")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match location.as_str() {
                    "query" => query_params.push(KeyValue {
                        key: parameter_name.to_string(),
                        value: parameter_example_value(&root, parameter),
                        enabled: true, description: None,
                    }),
                    "header" => headers.push(KeyValue {
                        key: parameter_name.to_string(),
                        value: parameter_example_value(&root, parameter),
                        enabled: true, description: None,
                    }),
                    "path" => {}
                    "cookie" => {
                        if !warned_cookie {
                            warnings.push(format!(
                                "{label}: cookie parameters are not imported"
                            ));
                            warned_cookie = true;
                        }
                    }
                    "body" => body_parameter = Some(parameter),
                    "formdata" => {
                        if parameter
                            .get("type")
                            .and_then(Value::as_str)
                            .is_some_and(|kind| kind.eq_ignore_ascii_case("file"))
                        {
                            form_has_file = true;
                        }
                        form_fields.push(KeyValue {
                            key: parameter_name.to_string(),
                            value: parameter_example_value(&root, parameter),
                            enabled: true, description: None,
                        });
                    }
                    other => warnings.push(format!(
                        "{label}: parameter '{parameter_name}' has unsupported location '{other}'; skipped"
                    )),
                }
            }

            let body = if let Some(parameter) = body_parameter {
                let schema = parameter.get("schema").unwrap_or(&Value::Null);
                json_body_from_schema(&root, schema, &label, &mut warnings)
            } else if let Some(request_body) = operation.get("requestBody") {
                body_from_request_body(&root, request_body, &label, &mut warnings)
            } else if form_has_file {
                warnings.push(format!(
                    "{label}: formData file uploads are imported without a file path"
                ));
                RequestBody::Multipart {
                    parts: form_fields
                        .into_iter()
                        .map(|field| FormPart {
                            key: field.key,
                            value: field.value,
                            file_path: None,
                            enabled: field.enabled,
                        })
                        .collect(),
                }
            } else if form_fields.is_empty() {
                RequestBody::None
            } else {
                RequestBody::FormUrlEncoded {
                    fields: form_fields,
                }
            };

            let auth = operation
                .get("security")
                .and_then(|security| {
                    security_auth(&root, security_schemes, security, &mut warnings, &label)
                })
                .unwrap_or(AuthConfig::Inherit);

            let draft = RequestDraft {
                name: operation_name,
                method,
                url: format!("{{{{baseUrl}}}}{path}"),
                query_params,
                headers,
                body,
                auth,
                description: operation_description,
                folder_id,
            };
            let position = requests.len() as i64;
            let mut request =
                build_request(draft, &mut ids, workspace_id, &collection_id, position)?;
            let produces = operation.get("produces").or_else(|| root.get("produces"));
            request.examples =
                documented_examples(&root, operation, version, produces, &label, &mut warnings);
            requests.push(request);
        }
    }

    Ok(ImportedCollection {
        collection,
        folders,
        requests,
        warnings,
    })
}

const OPENAPI_METHODS: [(&str, HttpMethod); 7] = [
    ("get", HttpMethod::Get),
    ("post", HttpMethod::Post),
    ("put", HttpMethod::Put),
    ("patch", HttpMethod::Patch),
    ("delete", HttpMethod::Delete),
    ("head", HttpMethod::Head),
    ("options", HttpMethod::Options),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecVersion {
    V2,
    V3,
}

fn openapi_version(root: &Value) -> Result<SpecVersion, ImportError> {
    if root.get("swagger").and_then(Value::as_str).map(str::trim) == Some("2.0") {
        return Ok(SpecVersion::V2);
    }
    if let Some(version) = root.get("openapi").and_then(Value::as_str).map(str::trim) {
        if version.starts_with("3.") {
            return Ok(SpecVersion::V3);
        }
        return Err(ImportError::UnsupportedDocument {
            message: format!("unsupported OpenAPI version '{version}'"),
        });
    }
    Err(ImportError::UnsupportedDocument {
        message: "document declares neither swagger 2.0 nor openapi 3.x".to_string(),
    })
}

fn openapi_base_url(root: &Value, version: SpecVersion, warnings: &mut Vec<String>) -> String {
    match version {
        SpecVersion::V3 => {
            let url = root
                .get("servers")
                .and_then(Value::as_array)
                .and_then(|servers| servers.first())
                .and_then(|server| server.get("url"))
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if url.is_empty() {
                warnings.push(
                    "OpenAPI: servers[0].url is missing; the baseUrl variable is empty".to_string(),
                );
            } else if url.contains('{') {
                warnings.push(
                    "OpenAPI: server URL variables are kept literally in baseUrl".to_string(),
                );
            }
            url.trim_end_matches('/').to_string()
        }
        SpecVersion::V2 => {
            let host = root
                .get("host")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            let base_path = root
                .get("basePath")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if host.is_empty() {
                warnings.push(
                    "OpenAPI: host is missing; the baseUrl variable uses basePath only".to_string(),
                );
                return base_path.trim_end_matches('/').to_string();
            }
            let scheme = root
                .get("schemes")
                .and_then(Value::as_array)
                .and_then(|schemes| schemes.first())
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|scheme| !scheme.is_empty())
                .unwrap_or("https");
            format!("{scheme}://{host}{}", base_path.trim_end_matches('/'))
        }
    }
}

fn security_auth(
    root: &Value,
    schemes: Option<&Map<String, Value>>,
    security: &Value,
    warnings: &mut Vec<String>,
    label: &str,
) -> Option<AuthConfig> {
    let Some(requirements) = security.as_array() else {
        warnings.push(format!("{label}: security is not an array; ignored"));
        return None;
    };
    let Some(requirement) = requirements.first() else {
        return Some(AuthConfig::None);
    };
    let Some(requirement) = requirement.as_object() else {
        warnings.push(format!(
            "{label}: security requirement is not an object; ignored"
        ));
        return None;
    };
    if requirement.is_empty() {
        return Some(AuthConfig::None);
    }
    if requirement.len() > 1 {
        warnings.push(format!(
            "{label}: multiple security requirements; using the first"
        ));
    }
    let (scheme_name, _scopes) = requirement.iter().next()?;

    let Some(scheme) = schemes.and_then(|schemes| schemes.get(scheme_name)) else {
        warnings.push(format!(
            "{label}: security scheme '{scheme_name}' is not defined; auth inherits"
        ));
        return Some(AuthConfig::Inherit);
    };
    let scheme = resolve_local_ref(root, scheme);

    Some(auth_from_security_scheme(
        scheme,
        scheme_name,
        warnings,
        label,
    ))
}

fn auth_from_security_scheme(
    scheme: &Value,
    name: &str,
    warnings: &mut Vec<String>,
    label: &str,
) -> AuthConfig {
    let Some(scheme) = scheme.as_object() else {
        warnings.push(format!(
            "{label}: security scheme '{name}' is not an object; auth inherits"
        ));
        return AuthConfig::Inherit;
    };
    let kind = scheme
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase();

    match kind.as_str() {
        "basic" => AuthConfig::Basic {
            username: String::new(),
            password_ref: String::new(),
        },
        "http" => {
            let http_scheme = scheme
                .get("scheme")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if http_scheme.eq_ignore_ascii_case("bearer") {
                AuthConfig::Bearer {
                    token_ref: String::new(),
                }
            } else if http_scheme.eq_ignore_ascii_case("basic") {
                AuthConfig::Basic {
                    username: String::new(),
                    password_ref: String::new(),
                }
            } else if http_scheme.eq_ignore_ascii_case("digest") {
                AuthConfig::Digest {
                    username: String::new(),
                    password_ref: None,
                }
            } else {
                warnings.push(format!(
                    "{label}: unsupported HTTP security scheme '{http_scheme}'; auth inherits"
                ));
                AuthConfig::Inherit
            }
        }
        "apikey" => {
            let key = scheme
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .unwrap_or(name);
            let location = if scheme
                .get("in")
                .and_then(Value::as_str)
                .is_some_and(|location| location.eq_ignore_ascii_case("query"))
            {
                ApiKeyLocation::Query
            } else {
                ApiKeyLocation::Header
            };
            AuthConfig::ApiKey {
                key: key.to_string(),
                value_ref: String::new(),
                location,
            }
        }
        "oauth2" | "openidconnect" => {
            warnings.push(format!(
                "{label}: OAuth2 security scheme '{name}' is not imported; auth inherits"
            ));
            AuthConfig::Inherit
        }
        other => {
            warnings.push(format!(
                "{label}: unsupported security scheme type '{other}'; auth inherits"
            ));
            AuthConfig::Inherit
        }
    }
}

fn merge_parameters(root: &Value, path_level: &[Value], operation_level: &[Value]) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for parameter in path_level.iter().chain(operation_level) {
        let parameter = resolve_local_ref(root, parameter);
        let key = parameter_key(parameter);
        if let Some(existing) = merged
            .iter_mut()
            .find(|existing| parameter_key(existing) == key)
        {
            *existing = parameter.clone();
        } else {
            merged.push(parameter.clone());
        }
    }
    merged
}

fn parameter_key(parameter: &Value) -> String {
    let name = parameter
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let location = parameter
        .get("in")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    format!("{location}:{name}")
}

fn parameter_example_value(root: &Value, parameter: &Map<String, Value>) -> String {
    for key in ["example", "default"] {
        if let Some(value) = parameter.get(key).and_then(scalar_string) {
            return value;
        }
    }
    let Some(schema) = parameter
        .get("schema")
        .map(|schema| resolve_local_ref(root, schema))
    else {
        return String::new();
    };
    for key in ["example", "default"] {
        if let Some(value) = schema.get(key).and_then(scalar_string) {
            return value;
        }
    }
    schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .and_then(scalar_string)
        .unwrap_or_default()
}

fn body_from_request_body(
    root: &Value,
    request_body: &Value,
    label: &str,
    warnings: &mut Vec<String>,
) -> RequestBody {
    let request_body = resolve_local_ref(root, request_body);
    let Some(content) = request_body.get("content").and_then(Value::as_object) else {
        warnings.push(format!(
            "{label}: requestBody has no content; body is empty"
        ));
        return RequestBody::None;
    };
    if content.is_empty() {
        return RequestBody::None;
    }
    if let Some((_media, media_type)) = content
        .iter()
        .find(|(media, _)| media.to_ascii_lowercase().contains("json"))
    {
        if let Some(example) = media_type.get("example") {
            let content =
                serde_json::to_string_pretty(example).unwrap_or_else(|_| "{}".to_string());
            return RequestBody::Json { content };
        }
        let schema = media_type.get("schema").unwrap_or(&Value::Null);
        return json_body_from_schema(root, schema, label, warnings);
    }
    let Some((media, media_type)) = content.iter().next() else {
        return RequestBody::None;
    };
    let example = media_type.get("example").cloned().or_else(|| {
        media_type
            .get("examples")
            .and_then(Value::as_object)
            .and_then(|examples| examples.values().next())
            .and_then(|example| example.get("value"))
            .cloned()
    });
    match example {
        Some(Value::String(text)) => RequestBody::RawText {
            content: text,
            language: None,
        },
        Some(other) => RequestBody::RawText {
            language: None,
            content: other.to_string(),
        },
        None => {
            warnings.push(format!(
                "{label}: media type '{media}' has no example; body is empty"
            ));
            RequestBody::None
        }
    }
}

fn json_body_from_schema(
    root: &Value,
    schema: &Value,
    label: &str,
    warnings: &mut Vec<String>,
) -> RequestBody {
    let mut budget = SchemaBudget::default();
    let value = example_from_schema(root, schema, 0, &mut budget).unwrap_or_else(|| {
        if schema.get("$ref").is_some() {
            warnings.push(format!(
                "{label}: unresolved schema reference; body is an empty object"
            ));
        }
        Value::Object(Map::new())
    });
    if budget.exhausted {
        warnings.push(format!(
            "{label}: schema is too large; the example body is cut short"
        ));
    }
    let content = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    RequestBody::Json { content }
}

/// What one body example may still spend, plus the `$ref` targets it already expanded.
///
/// A schema whose properties all `$ref` back to it fans out to N^depth nodes; the budget turns
/// that into a bounded walk and the cache makes each `(target, depth)` pair expand once.
struct SchemaBudget {
    /// Nodes that may still be produced.
    left: usize,
    /// Set once something was left out for lack of budget.
    exhausted: bool,
    /// Expanded `$ref` targets by `(pointer, depth)`; depth decides how much of a target fits.
    refs: HashMap<(String, usize), Option<Value>>,
}

impl Default for SchemaBudget {
    fn default() -> Self {
        Self {
            left: MAX_SCHEMA_NODES,
            exhausted: false,
            refs: HashMap::new(),
        }
    }
}

impl SchemaBudget {
    /// Takes `nodes` from the budget; `false` (and the budget is spent) when it is not enough.
    fn spend(&mut self, nodes: usize) -> bool {
        if nodes > self.left {
            self.left = 0;
            self.exhausted = true;
            return false;
        }
        self.left -= nodes;
        true
    }

    /// Takes the cost of a ready-made `value` (an `example`, a cached expansion).
    fn spend_value(&mut self, value: &Value) -> bool {
        let cost = node_count(value, self.left);
        self.spend(cost)
    }
}

/// Number of JSON nodes in `value`, counting stops once it passes `cap`.
fn node_count(value: &Value, cap: usize) -> usize {
    let mut count = 1;
    match value {
        Value::Array(items) => {
            for item in items {
                if count > cap {
                    break;
                }
                count += node_count(item, cap - count.min(cap));
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                if count > cap {
                    break;
                }
                count += node_count(item, cap - count.min(cap));
            }
        }
        _ => {}
    }
    count
}

fn example_from_schema(
    root: &Value,
    schema: &Value,
    depth: usize,
    budget: &mut SchemaBudget,
) -> Option<Value> {
    if depth > MAX_SCHEMA_DEPTH || !budget.spend(1) {
        return None;
    }
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return expand_schema(root, schema, depth, budget);
    };
    let key = (reference.to_owned(), depth);
    if let Some(hit) = budget.refs.get(&key) {
        let hit = hit.clone();
        return match &hit {
            Some(value) if !budget.spend_value(value) => None,
            _ => hit,
        };
    }
    let value = expand_schema(root, schema, depth, budget);
    budget.refs.insert(key, value.clone());
    value
}

fn expand_schema(
    root: &Value,
    schema: &Value,
    depth: usize,
    budget: &mut SchemaBudget,
) -> Option<Value> {
    let schema = resolve_local_ref(root, schema);
    let object = schema.as_object()?;

    for key in ["example", "default"] {
        if let Some(value) = object.get(key) {
            return budget.spend_value(value).then(|| value.clone());
        }
    }
    if let Some(value) = object
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return budget.spend_value(value).then(|| value.clone());
    }
    if let Some(members) = object.get("allOf").and_then(Value::as_array) {
        let mut merged = Map::new();
        for member in members {
            if let Some(Value::Object(example)) =
                example_from_schema(root, member, depth + 1, budget)
            {
                merged.extend(example);
            }
        }
        return if merged.is_empty() {
            None
        } else {
            Some(Value::Object(merged))
        };
    }
    if let Some(member) = object
        .get("oneOf")
        .or_else(|| object.get("anyOf"))
        .and_then(Value::as_array)
        .and_then(|members| members.first())
    {
        return example_from_schema(root, member, depth + 1, budget);
    }
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        let mut example = Map::new();
        for (name, property) in properties {
            example.insert(
                name.clone(),
                example_from_schema(root, property, depth + 1, budget).unwrap_or(Value::Null),
            );
        }
        return Some(Value::Object(example));
    }

    let schema_type = object.get("type").and_then(Value::as_str).unwrap_or("");
    let value = match schema_type {
        "object" => Value::Object(Map::new()),
        "array" => Value::Array(
            object
                .get("items")
                .and_then(|items| example_from_schema(root, items, depth + 1, budget))
                .into_iter()
                .collect(),
        ),
        "string" => Value::String(string_example(object)),
        "integer" | "number" => Value::from(0),
        "boolean" => Value::Bool(false),
        "null" => Value::Null,
        _ => return None,
    };
    Some(value)
}

fn string_example(schema: &Map<String, Value>) -> String {
    match schema.get("format").and_then(Value::as_str).unwrap_or("") {
        "date-time" => "2024-01-01T00:00:00Z",
        "date" => "2024-01-01",
        "uuid" => "00000000-0000-0000-0000-000000000000",
        "email" => "user@example.com",
        "uri" | "url" => "https://example.com",
        _ => "string",
    }
    .to_string()
}

fn resolve_local_ref<'a>(root: &'a Value, value: &'a Value) -> &'a Value {
    let mut current = value;
    for _ in 0..MAX_REF_HOPS {
        let Some(reference) = current.get("$ref").and_then(Value::as_str) else {
            return current;
        };
        let Some(pointer) = reference.strip_prefix('#') else {
            return current;
        };
        let Some(resolved) = root.pointer(pointer) else {
            return current;
        };
        current = resolved;
    }
    current
}

/// Mock routes generated from an OpenAPI document.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenApiMockRoutes {
    /// `info.title`, or a default name.
    pub name: String,
    /// The path of the document's base URL (`/v1`), empty when it has none.
    pub endpoint_prefix: String,
    pub routes: Vec<MockRoute>,
    pub warnings: Vec<String>,
}

/// One mock route per operation: the path as written (`{id}` is a path parameter), one response
/// per documented status with its example, or a body generated from its schema, and the first
/// `2xx` as the default. Ids are placeholders the caller reissues; the routes neither proxy nor
/// fire callbacks.
///
/// # Errors
/// The document is not JSON or YAML, or not OpenAPI 2.0 or 3.x.
pub fn openapi_mock_routes(text: &str) -> Result<OpenApiMockRoutes, ImportError> {
    let root = parse_json_or_yaml(text)?;
    let version = openapi_version(&root)?;
    let mut warnings = Vec::new();
    let name = non_empty_or(
        root.pointer("/info/title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim(),
        "OpenAPI import",
    );
    let base_url = openapi_base_url(&root, version, &mut Vec::new());
    let endpoint_prefix = base_url
        .split_once("://")
        .map_or(base_url.as_str(), |(_, rest)| {
            rest.find('/').map_or("", |at| &rest[at..])
        })
        .trim_end_matches('/')
        .to_owned();
    let produces = root.get("produces");
    let mut routes = Vec::new();
    let mut paths: Vec<(&String, &Value)> = root
        .get("paths")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .collect();
    paths.sort_by(|a, b| a.0.cmp(b.0));
    for (path, path_item) in paths {
        let Some(path_item) = resolve_local_ref(&root, path_item).as_object() else {
            continue;
        };
        for (operation_key, method) in OPENAPI_METHODS {
            let Some(operation) = path_item.get(operation_key).and_then(Value::as_object) else {
                continue;
            };
            let label = format!("OpenAPI {} {path}", method_label(method));
            let produces = operation.get("produces").or(produces);
            let mut responses: Vec<MockResponse> = operation
                .get("responses")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(code, documented)| {
                    let status = mock_status(code)?;
                    let documented = resolve_local_ref(&root, documented);
                    let (media, body) =
                        mock_body(&root, documented, version, produces, &label, &mut warnings);
                    Some(mock_response(status, documented, media, body))
                })
                .collect();
            if responses.is_empty() {
                responses.push(mock_response(200, &Value::Null, None, String::new()));
            }
            responses.sort_by_key(|response| !(200..300).contains(&response.status_code));
            for (position, response) in responses.iter_mut().enumerate() {
                let position = i64::try_from(position).unwrap_or(i64::MAX);
                response.id = format!("resp_{}_{position}", routes.len());
                response.is_default = position == 0;
                response.position = position;
            }
            let position = i64::try_from(routes.len()).unwrap_or(i64::MAX);
            routes.push(MockRoute {
                id: format!("route_{position}"),
                method: method_label(method).to_owned(),
                path: path.clone(),
                enabled: true,
                description: operation
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_owned(),
                route_type: MockRouteType::Http,
                data_bucket_id: String::new(),
                crud_id_key: "id".to_owned(),
                response_mode: MockResponseMode::FirstMatch,
                responses,
                position,
            });
        }
    }
    Ok(OpenApiMockRoutes {
        name,
        endpoint_prefix,
        routes,
        warnings,
    })
}

/// The responses of `operation` that carry an explicit example, as saved request examples (a
/// body generated from a schema only goes to mock routes): at most [`MAX_REQUEST_EXAMPLES`],
/// bodies cut to the size limit.
fn documented_examples(
    root: &Value,
    operation: &Map<String, Value>,
    version: SpecVersion,
    produces: Option<&Value>,
    label: &str,
    warnings: &mut Vec<String>,
) -> Vec<RequestExample> {
    let mut examples = Vec::new();
    let mut cut = false;
    for (code, documented) in operation
        .get("responses")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let Some(status) = mock_status(code) else {
            continue;
        };
        let documented = resolve_local_ref(root, documented);
        let explicit = match version {
            SpecVersion::V3 => documented
                .get("content")
                .and_then(Value::as_object)
                .is_some_and(|media| {
                    media.values().any(|content| {
                        content.get("example").is_some() || content.get("examples").is_some()
                    })
                }),
            SpecVersion::V2 => documented.get("examples").is_some(),
        };
        if !explicit {
            continue;
        }
        if examples.len() == MAX_REQUEST_EXAMPLES {
            warnings.push(format!(
                "{label}: only the first {MAX_REQUEST_EXAMPLES} response examples are kept on the request"
            ));
            break;
        }
        let (media, body) = mock_body(root, documented, version, produces, label, &mut Vec::new());
        let response = mock_response(status, documented, media, body);
        let (example, was_cut) = RequestExample {
            name: if response.label.is_empty() {
                status.to_string()
            } else {
                response.label
            },
            status,
            headers: response.headers,
            body: response.body,
        }
        .capped();
        cut |= was_cut;
        examples.push(example);
    }
    if cut {
        warnings.push(format!(
            "{label}: a response example body was cut to 64 KiB"
        ));
    }
    examples
}

/// The status a documented response key stands for: `404`, `4XX` as `400`; `default` and
/// anything else is skipped.
fn mock_status(code: &str) -> Option<u16> {
    let code = code.trim().to_ascii_uppercase();
    let status = match code.strip_suffix("XX") {
        Some(class) => class.parse::<u16>().ok()?.checked_mul(100)?,
        None => code.parse::<u16>().ok()?,
    };
    (100..=599).contains(&status).then_some(status)
}

/// The media type and body of a documented response: an example when there is one, else a JSON
/// body generated from the schema; JSON media types are preferred.
fn mock_body(
    root: &Value,
    documented: &Value,
    version: SpecVersion,
    produces: Option<&Value>,
    label: &str,
    warnings: &mut Vec<String>,
) -> (Option<String>, String) {
    let text = |value: &Value| match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    let from_schema = |schema: &Value, warnings: &mut Vec<String>| match json_body_from_schema(
        root, schema, label, warnings,
    ) {
        RequestBody::Json { content } => content,
        _ => String::new(),
    };
    let pick = |media: &Map<String, Value>| {
        media
            .iter()
            .find(|(name, _)| name.to_ascii_lowercase().contains("json"))
            .or_else(|| media.iter().next())
            .map(|(name, value)| (name.clone(), value.clone()))
    };
    match version {
        SpecVersion::V3 => {
            let Some((media, content)) = documented
                .get("content")
                .and_then(Value::as_object)
                .and_then(pick)
            else {
                return (None, String::new());
            };
            let example = content.get("example").cloned().or_else(|| {
                content
                    .get("examples")
                    .and_then(Value::as_object)
                    .and_then(|examples| examples.values().next())
                    .map(|example| resolve_local_ref(root, example))
                    .and_then(|example| example.get("value"))
                    .cloned()
            });
            let body = match (example, content.get("schema")) {
                (Some(example), _) => text(&example),
                (None, Some(schema)) => from_schema(schema, warnings),
                (None, None) => String::new(),
            };
            (Some(media), body)
        }
        SpecVersion::V2 => {
            if let Some((media, example)) = documented
                .get("examples")
                .and_then(Value::as_object)
                .and_then(pick)
            {
                return (Some(media), text(&example));
            }
            let Some(schema) = documented.get("schema") else {
                return (None, String::new());
            };
            let media = produces
                .and_then(Value::as_array)
                .and_then(|list| list.iter().find_map(Value::as_str))
                .unwrap_or("application/json");
            (Some(media.to_owned()), from_schema(schema, warnings))
        }
    }
}

fn mock_response(
    status_code: u16,
    documented: &Value,
    media: Option<String>,
    body: String,
) -> MockResponse {
    MockResponse {
        id: String::new(),
        label: documented
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty() && text.len() <= 80)
            .unwrap_or("")
            .to_owned(),
        status_code,
        headers: media
            .map(|media| KeyValue {
                key: "Content-Type".to_owned(),
                value: media,
                enabled: true,
                description: None,
            })
            .into_iter()
            .collect(),
        body,
        body_source: None,
        fault: None,
        latency_ms: 0,
        is_default: false,
        rules_operator: MockRulesOperator::And,
        rules: Vec::new(),
        callback_ids: Vec::new(),
        position: 0,
        stream: None,
    }
}
