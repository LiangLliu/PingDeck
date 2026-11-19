//! Insomnia export (format 4, and the near-identical 3) as one collection.

use std::collections::HashMap;

use serde_json::Value;

use super::helpers::{RequestDraft, non_empty_or, parse_json, query_pairs, split_url_query};
use super::tree::{TreeBuilder, method_or_get, note_count, rows, text};
use super::{ImportError, ImportedCollection};
use crate::workspace::models::{
    ApiKeyLocation, AuthConfig, EnvironmentVariable, FolderId, FormPart, OAuth2ClientAuth,
    OAuth2GrantType, RawLanguage, RequestBody,
};

/// Imports an Insomnia v4 (or v3) JSON export: the first workspace becomes the collection, its
/// request groups folders, its requests requests and the base environment's values collection
/// variables. `{{ _.name }}` becomes `{{name}}`. Template tags (`{% … %}`), sub-environments,
/// gRPC/WebSocket requests, design documents and tests are reported, not imported.
///
/// # Errors
/// The text is not JSON, or not an Insomnia v3/v4 export.
pub fn import_insomnia(
    payload: &str,
    workspace_id: &str,
) -> Result<ImportedCollection, ImportError> {
    let root = parse_json(payload)?;
    let format = root.get("__export_format").and_then(Value::as_u64);
    if root.get("_type").and_then(Value::as_str) != Some("export") || !matches!(format, Some(3 | 4))
    {
        return Err(ImportError::UnsupportedDocument {
            message: "not an Insomnia v4 export; in Insomnia choose Export › Insomnia v4 (JSON)"
                .to_owned(),
        });
    }
    let resources: Vec<&Value> = root
        .get("resources")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter(|value| value.is_object()).collect())
        .unwrap_or_default();
    fn kind(resource: &Value) -> &str {
        resource.get("_type").and_then(Value::as_str).unwrap_or("")
    }
    let workspace = resources
        .iter()
        .find(|resource| kind(resource) == "workspace");
    let name = non_empty_or(
        &workspace.map(|w| text(w, "name")).unwrap_or_default(),
        "Insomnia import",
    );
    let workspace_ref = workspace.map(|w| text(w, "_id")).unwrap_or_default();

    let mut tree = TreeBuilder::new(&name, workspace_id)?;
    if let Some(workspace) = workspace {
        tree.collection.description = text(workspace, "description");
    }

    // Children by parent id, in Insomnia's order (`metaSortKey`).
    let mut children: HashMap<String, Vec<&Value>> = HashMap::new();
    for resource in &resources {
        children
            .entry(text(resource, "parentId"))
            .or_default()
            .push(resource);
    }
    for list in children.values_mut() {
        list.sort_by(|left, right| {
            let key = |value: &Value| {
                value
                    .get("metaSortKey")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
            };
            key(left).total_cmp(&key(right))
        });
    }

    let mut skipped: HashMap<&str, usize> = HashMap::new();
    let mut sub_environments = 0_usize;
    for resource in &resources {
        match kind(resource) {
            "environment" if text(resource, "parentId") == workspace_ref => {
                base_environment(&mut tree, resource)?;
            }
            "environment" => sub_environments += 1,
            "workspace" | "request" | "request_group" | "cookie_jar" => {}
            other => *skipped.entry(other).or_default() += 1,
        }
    }
    // Requests and groups hang off the workspace; an export without one lists them at the root.
    let top = if workspace.is_some() {
        workspace_ref
    } else {
        String::new()
    };
    let mut walker = Walker {
        tree,
        children: &children,
        tags: 0,
    };
    walker.level(&top, None, 0)?;
    let mut tree = walker.tree;
    if walker.tags > 0 {
        tree.warnings.push(format!(
            "{} value(s) use Insomnia template tags ({{% … %}}), which are kept as written and do not run",
            walker.tags
        ));
    }
    note_count(
        &mut tree.warnings,
        sub_environments,
        "1 sub-environment is not imported; import it as an environment instead",
        "{count} sub-environments are not imported; import them as environments instead",
    );
    let mut skipped: Vec<(&str, usize)> = skipped.into_iter().collect();
    skipped.sort_unstable();
    for (kind, count) in skipped {
        tree.warnings.push(format!(
            "{count} Insomnia {kind} resource(s) are not imported"
        ));
    }
    Ok(tree.finish())
}

fn base_environment(tree: &mut TreeBuilder, environment: &Value) -> Result<(), ImportError> {
    let Some(data) = environment.get("data").and_then(Value::as_object) else {
        return Ok(());
    };
    for (key, value) in data {
        let value = match value {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            Value::Bool(flag) => flag.to_string(),
            Value::Null => String::new(),
            other => {
                tree.warnings.push(format!(
                    "environment value '{key}' is an object; stored as JSON text"
                ));
                other.to_string()
            }
        };
        tree.collection
            .variables
            .push(EnvironmentVariable::plain(key, value)?);
    }
    Ok(())
}

/// `{{ _.name }}` and `{{_.name}}` as `{{name}}`.
fn convert(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("{{") {
        let Some(length) = rest[start + 2..].find("}}") else {
            break;
        };
        let inner = rest[start + 2..start + 2 + length].trim();
        let name = inner.strip_prefix("_.").unwrap_or(inner);
        out.push_str(&rest[..start]);
        out.push_str("{{");
        out.push_str(name.trim());
        out.push_str("}}");
        rest = &rest[start + 2 + length + 2..];
    }
    out.push_str(rest);
    out
}

struct Walker<'a> {
    tree: TreeBuilder,
    children: &'a HashMap<String, Vec<&'a Value>>,
    /// Values that hold a `{% tag %}`.
    tags: usize,
}

impl Walker<'_> {
    fn convert(&mut self, value: &str) -> String {
        if value.contains("{%") {
            self.tags += 1;
        }
        convert(value)
    }

    fn level(
        &mut self,
        parent: &str,
        folder: Option<&FolderId>,
        depth: usize,
    ) -> Result<(), ImportError> {
        // Insomnia ids are unique, but a hand-edited file may loop.
        if depth > 64 {
            self.tree
                .warnings
                .push("request groups nest deeper than 64 levels; the rest is skipped".to_owned());
            return Ok(());
        }
        let children = self.children;
        let Some(list) = children.get(parent) else {
            return Ok(());
        };
        for (position, resource) in list.iter().enumerate() {
            let position = i64::try_from(position).unwrap_or(i64::MAX);
            match resource.get("_type").and_then(Value::as_str) {
                Some("request_group") => {
                    let id = self
                        .tree
                        .folder(&text(resource, "name"), folder, position)?;
                    if resource
                        .get("environment")
                        .and_then(Value::as_object)
                        .is_some_and(|data| !data.is_empty())
                    {
                        self.tree.warnings.push(format!(
                            "folder '{}': its environment values are not imported",
                            text(resource, "name")
                        ));
                    }
                    self.level(&text(resource, "_id"), Some(&id), depth + 1)?;
                }
                Some("request") => self.request(resource, folder, position)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn request(
        &mut self,
        resource: &Value,
        folder: Option<&FolderId>,
        position: i64,
    ) -> Result<(), ImportError> {
        let name = non_empty_or(&text(resource, "name"), "Request");
        let label = format!("request '{name}'");
        let raw_url = self.convert(&text(resource, "url"));
        let (base, query) = split_url_query(&raw_url);
        let mut query_params = query_pairs(query);
        query_params.extend(rows(
            resource.get("parameters"),
            "name",
            "disabled",
            false,
            convert,
        ));
        let headers = rows(resource.get("headers"), "name", "disabled", false, convert);
        let method = method_or_get(&text(resource, "method"), &mut self.tree.warnings, &label);
        let body = self.body(resource.get("body"), &label);
        let auth = self.auth(resource.get("authentication"), &label);
        let draft = RequestDraft {
            name,
            method,
            url: base.to_owned(),
            query_params,
            headers,
            body,
            auth,
            description: text(resource, "description"),
            folder_id: folder.cloned(),
        };
        let pre = text(resource, "preRequestScript");
        let post = text(resource, "afterResponseScript");
        if !pre.trim().is_empty() || !post.trim().is_empty() {
            self.tree.warnings.push(format!(
                "{label}: scripts are imported as written; the insomnia.* API is not available"
            ));
        }
        let request = self.tree.request(draft, position)?;
        request.pre_request_script = pre;
        request.test_script = post;
        Ok(())
    }

    fn body(&mut self, body: Option<&Value>, label: &str) -> RequestBody {
        let Some(body) = body.filter(|body| body.is_object()) else {
            return RequestBody::None;
        };
        let mime = text(body, "mimeType").to_ascii_lowercase();
        let content = self.convert(&text(body, "text"));
        let params = || rows(body.get("params"), "name", "disabled", false, convert);
        match mime.split(';').next().unwrap_or("").trim() {
            "" if content.is_empty() => RequestBody::None,
            "application/json" => RequestBody::Json { content },
            "application/x-www-form-urlencoded" => RequestBody::FormUrlEncoded { fields: params() },
            "multipart/form-data" => {
                let mut dropped = false;
                let parts = body
                    .get("params")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|row| !text(row, "name").trim().is_empty())
                    .map(|row| {
                        let is_file = text(row, "type") == "file";
                        dropped |= is_file && !text(row, "fileName").is_empty();
                        FormPart {
                            key: convert(text(row, "name").trim()),
                            value: if is_file {
                                String::new()
                            } else {
                                convert(&text(row, "value"))
                            },
                            file_path: is_file.then(String::new),
                            enabled: !row
                                .get("disabled")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        }
                    })
                    .collect();
                if dropped {
                    self.tree.warnings.push(format!(
                        "{label}: local file path was not imported; select the file again before sending"
                    ));
                }
                RequestBody::Multipart { parts }
            }
            "application/graphql" => {
                let parsed: Value = serde_json::from_str(&content).unwrap_or(Value::Null);
                RequestBody::Graphql {
                    query: text(&parsed, "query"),
                    variables_json: parsed
                        .get("variables")
                        .filter(|variables| !variables.is_null())
                        .map(Value::to_string),
                    operation_name: Some(text(&parsed, "operationName"))
                        .filter(|name| !name.is_empty()),
                }
            }
            "application/octet-stream" => {
                if !text(body, "fileName").is_empty() {
                    self.tree.warnings.push(format!(
                        "{label}: local file path was not imported; select the file again before sending"
                    ));
                }
                RequestBody::Binary {
                    file_path: String::new(),
                    content_type: None,
                }
            }
            "application/xml" | "text/xml" => RequestBody::RawText {
                content,
                language: Some(RawLanguage::Xml),
            },
            "text/html" => RequestBody::RawText {
                content,
                language: Some(RawLanguage::Html),
            },
            _ => RequestBody::RawText {
                content,
                language: None,
            },
        }
    }

    fn auth(&mut self, auth: Option<&Value>, label: &str) -> AuthConfig {
        let Some(auth) = auth.filter(|auth| auth.as_object().is_some_and(|map| !map.is_empty()))
        else {
            return AuthConfig::Inherit;
        };
        if auth.get("disabled").and_then(Value::as_bool) == Some(true) {
            return AuthConfig::None;
        }
        let field = |key: &str| convert(&text(auth, key));
        match text(auth, "type").as_str() {
            "none" => AuthConfig::None,
            "bearer" => {
                let prefix = text(auth, "prefix");
                if !prefix.trim().is_empty() && !prefix.trim().eq_ignore_ascii_case("bearer") {
                    self.tree.warnings.push(format!(
                        "{label}: bearer prefix '{}' is not imported; Bearer is sent",
                        prefix.trim()
                    ));
                }
                AuthConfig::Bearer {
                    token_ref: field("token"),
                }
            }
            "basic" => AuthConfig::Basic {
                username: field("username"),
                password_ref: field("password"),
            },
            "digest" => AuthConfig::Digest {
                username: field("username"),
                password_ref: Some(field("password")),
            },
            "apikey" => AuthConfig::ApiKey {
                key: field("key"),
                value_ref: field("value"),
                location: if text(auth, "addTo") == "queryParams" {
                    ApiKeyLocation::Query
                } else {
                    ApiKeyLocation::Header
                },
            },
            "iam" => AuthConfig::AwsSignatureV4 {
                access_key: field("accessKeyId"),
                secret_key_ref: Some(field("secretAccessKey")),
                region: field("region"),
                service: field("service"),
                session_token_ref: Some(field("sessionToken")).filter(|token| !token.is_empty()),
            },
            "oauth2" => {
                let grant_type = match text(auth, "grantType").as_str() {
                    "client_credentials" => OAuth2GrantType::ClientCredentials,
                    "password" => OAuth2GrantType::Password,
                    "authorization_code"
                        if auth.get("usePkce").and_then(Value::as_bool) == Some(true) =>
                    {
                        OAuth2GrantType::AuthorizationCodePkce
                    }
                    other => {
                        self.tree.warnings.push(format!(
                            "{label}: OAuth 2.0 grant '{other}' is not supported; auth inherits"
                        ));
                        return AuthConfig::Inherit;
                    }
                };
                AuthConfig::OAuth2 {
                    grant_type,
                    access_token_url: field("accessTokenUrl"),
                    client_id: field("clientId"),
                    client_secret_ref: field("clientSecret"),
                    username: field("username"),
                    password_ref: field("password"),
                    scope: field("scope"),
                    authorization_url: field("authorizationUrl"),
                    redirect_port: None,
                    client_auth: if text(auth, "credentialsInBody") == "true" {
                        OAuth2ClientAuth::Body
                    } else {
                        OAuth2ClientAuth::Basic
                    },
                    extra_params: ["audience", "resource"]
                        .into_iter()
                        .filter_map(|name| {
                            let value = field(name);
                            (!value.is_empty()).then(|| crate::workspace::models::KeyValue {
                                key: name.to_owned(),
                                value,
                                enabled: true,
                                description: None,
                            })
                        })
                        .collect(),
                }
            }
            other => {
                self.tree.warnings.push(format!(
                    "{label}: Insomnia auth '{other}' is not supported; auth inherits"
                ));
                AuthConfig::Inherit
            }
        }
    }
}
