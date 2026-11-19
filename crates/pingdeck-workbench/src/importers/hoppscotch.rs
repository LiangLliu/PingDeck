//! Hoppscotch collection export (JSON) as one collection.

use serde_json::Value;

use super::helpers::{RequestDraft, non_empty_or, parse_json, query_pairs, split_url_query};
use super::tree::{TreeBuilder, method_or_get, note_count, rows, text};
use super::{ImportError, ImportedCollection};
use crate::workspace::models::{
    ApiKeyLocation, AuthConfig, FolderId, FormPart, RawLanguage, RequestBody,
};

/// Imports a Hoppscotch collection export: one collection object, or an array of them (one
/// becomes the collection; several become top-level folders of a `Hoppscotch import`
/// collection). `<<name>>` becomes `{{name}}`. Scripts are kept as written (Hoppscotch's `pw` API
/// is not available).
///
/// # Errors
/// The text is not JSON, or not a Hoppscotch collection.
pub fn import_hoppscotch(
    payload: &str,
    workspace_id: &str,
) -> Result<ImportedCollection, ImportError> {
    let root = parse_json(payload)?;
    let collections: Vec<&Value> = match &root {
        Value::Array(list) => list.iter().filter(|value| is_collection(value)).collect(),
        value if is_collection(value) => vec![value],
        _ => Vec::new(),
    };
    if collections.is_empty() {
        return Err(ImportError::UnsupportedDocument {
            message: "not a Hoppscotch collection export (expected folders and requests)"
                .to_owned(),
        });
    }
    let single = collections.len() == 1;
    let name = if single {
        non_empty_or(&text(collections[0], "name"), "Hoppscotch import")
    } else {
        "Hoppscotch import".to_owned()
    };
    let mut walker = Walker {
        tree: TreeBuilder::new(&name, workspace_id)?,
        scripts: 0,
    };
    if single {
        walker.tree.collection.auth = walker.auth(collections[0].get("auth"), "collection");
        walker.collection(collections[0], None, 0)?;
    } else {
        for (position, collection) in collections.into_iter().enumerate() {
            let position = i64::try_from(position).unwrap_or(i64::MAX);
            let folder = walker
                .tree
                .folder(&text(collection, "name"), None, position)?;
            walker.collection(collection, Some(&folder), 0)?;
        }
    }
    let mut tree = walker.tree;
    note_count(
        &mut tree.warnings,
        walker.scripts,
        "1 request has Hoppscotch scripts, kept as written; the pw API is not available",
        "{count} requests have Hoppscotch scripts, kept as written; the pw API is not available",
    );
    Ok(tree.finish())
}

fn is_collection(value: &Value) -> bool {
    value.get("folders").is_some_and(Value::is_array)
        && value.get("requests").is_some_and(Value::is_array)
}

/// `<<name>>` as `{{name}}`.
fn convert(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("<<") {
        let Some(length) = rest[start + 2..].find(">>") else {
            break;
        };
        let name = rest[start + 2..start + 2 + length].trim();
        out.push_str(&rest[..start]);
        if name.is_empty() || name.contains(char::is_whitespace) {
            out.push_str(&rest[start..start + 2 + length + 2]);
        } else {
            out.push_str("{{");
            out.push_str(name);
            out.push_str("}}");
        }
        rest = &rest[start + 2 + length + 2..];
    }
    out.push_str(rest);
    out
}

struct Walker {
    tree: TreeBuilder,
    scripts: usize,
}

impl Walker {
    /// The requests, then the folders, of one collection or folder object under `folder`.
    fn collection(
        &mut self,
        node: &Value,
        folder: Option<&FolderId>,
        depth: usize,
    ) -> Result<(), ImportError> {
        if depth > 64 {
            self.tree
                .warnings
                .push("folders nest deeper than 64 levels; the rest is skipped".to_owned());
            return Ok(());
        }
        let mut position = 0_i64;
        for request in node
            .get("requests")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.request(request, folder, position)?;
            position += 1;
        }
        for child in node
            .get("folders")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = self.tree.folder(&text(child, "name"), folder, position)?;
            position += 1;
            self.collection(child, Some(&id), depth + 1)?;
        }
        Ok(())
    }

    fn request(
        &mut self,
        node: &Value,
        folder: Option<&FolderId>,
        position: i64,
    ) -> Result<(), ImportError> {
        let name = non_empty_or(&text(node, "name"), "Request");
        let label = format!("request '{name}'");
        let endpoint = convert(&text(node, "endpoint"));
        let (base, query) = split_url_query(&endpoint);
        let mut query_params = query_pairs(query);
        query_params.extend(rows(node.get("params"), "key", "active", true, convert));
        let headers = rows(node.get("headers"), "key", "active", true, convert);
        let method = method_or_get(&text(node, "method"), &mut self.tree.warnings, &label);
        let body = self.body(node.get("body"), &label);
        let auth = self.auth(node.get("auth"), &label);
        let draft = RequestDraft {
            name,
            method,
            url: base.to_owned(),
            query_params,
            headers,
            body,
            auth,
            description: String::new(),
            folder_id: folder.cloned(),
        };
        let pre = text(node, "preRequestScript");
        let post = text(node, "testScript");
        if !pre.trim().is_empty() || !post.trim().is_empty() {
            self.scripts += 1;
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
        let content_type = text(body, "contentType").to_ascii_lowercase();
        let content = body.get("body");
        let raw = || {
            content
                .and_then(Value::as_str)
                .map(convert)
                .unwrap_or_default()
        };
        match content_type.split(';').next().unwrap_or("").trim() {
            "" => RequestBody::None,
            "application/json"
            | "application/ld+json"
            | "application/hal+json"
            | "application/vnd.api+json" => RequestBody::Json { content: raw() },
            "application/xml" | "text/xml" => RequestBody::RawText {
                content: raw(),
                language: Some(RawLanguage::Xml),
            },
            "text/html" => RequestBody::RawText {
                content: raw(),
                language: Some(RawLanguage::Html),
            },
            "application/x-www-form-urlencoded" => {
                // Older exports write the fields as `key: value` lines.
                let fields = match content {
                    Some(Value::Array(_)) => rows(content, "key", "active", true, convert),
                    _ => raw()
                        .lines()
                        .filter_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            let key = key.trim();
                            (!key.is_empty()).then(|| crate::workspace::models::KeyValue {
                                key: key.trim_start_matches('#').trim().to_owned(),
                                value: value.trim().to_owned(),
                                enabled: !key.starts_with('#'),
                                description: None,
                            })
                        })
                        .collect(),
                };
                RequestBody::FormUrlEncoded { fields }
            }
            "multipart/form-data" => {
                let mut dropped = false;
                let parts = content
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|row| !text(row, "key").trim().is_empty())
                    .map(|row| {
                        let is_file = row.get("isFile").and_then(Value::as_bool) == Some(true);
                        dropped |= is_file;
                        FormPart {
                            key: convert(text(row, "key").trim()),
                            value: if is_file {
                                String::new()
                            } else {
                                convert(&text(row, "value"))
                            },
                            file_path: is_file.then(String::new),
                            enabled: row.get("active").and_then(Value::as_bool).unwrap_or(true),
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
            _ => RequestBody::RawText {
                content: raw(),
                language: None,
            },
        }
    }

    fn auth(&mut self, auth: Option<&Value>, label: &str) -> AuthConfig {
        let Some(auth) = auth.filter(|auth| auth.is_object()) else {
            return AuthConfig::Inherit;
        };
        if auth.get("authActive").and_then(Value::as_bool) == Some(false) {
            return AuthConfig::None;
        }
        let field = |key: &str| convert(&text(auth, key));
        match text(auth, "authType").as_str() {
            "inherit" | "" => AuthConfig::Inherit,
            "none" => AuthConfig::None,
            "bearer" => AuthConfig::Bearer {
                token_ref: field("token"),
            },
            "basic" => AuthConfig::Basic {
                username: field("username"),
                password_ref: field("password"),
            },
            "api-key" => AuthConfig::ApiKey {
                key: field("key"),
                value_ref: field("value"),
                location: if text(auth, "addTo").to_ascii_lowercase().contains("query") {
                    ApiKeyLocation::Query
                } else {
                    ApiKeyLocation::Header
                },
            },
            other => {
                self.tree.warnings.push(format!(
                    "{label}: Hoppscotch auth '{other}' is not supported; auth inherits"
                ));
                AuthConfig::Inherit
            }
        }
    }
}
