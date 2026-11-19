//! Bruno collection export (JSON) as one collection.

use serde_json::Value;

use super::helpers::{RequestDraft, non_empty_or, parse_json};
use super::tree::{TreeBuilder, method_or_get, note_count, rows, text};
use super::{ImportError, ImportedCollection};
use crate::workspace::models::{
    ApiKeyLocation, Assertion, AuthConfig, FolderId, FormPart, KeyValue, RawLanguage, RequestBody,
};

/// Imports a Bruno collection exported as JSON (Collection › Share › Bruno Collection): folders,
/// HTTP and GraphQL requests, query and path parameters, headers, bodies, auth (the collection's
/// too) and docs. `res.status eq N` assertions become status checks. Scripts and tests are kept
/// as written (Bruno's `bru`/`res` API is not available); environments, request variables,
/// collection-level headers and other assertions are reported, not imported.
///
/// # Errors
/// The text is not JSON, or not a Bruno collection.
pub fn import_bruno(payload: &str, workspace_id: &str) -> Result<ImportedCollection, ImportError> {
    let root = parse_json(payload)?;
    let items = root.get("items").and_then(Value::as_array);
    if !root.is_object() || items.is_none() || root.get("name").and_then(Value::as_str).is_none() {
        return Err(ImportError::UnsupportedDocument {
            message: "not a Bruno collection export (expected name and items)".to_owned(),
        });
    }
    let name = non_empty_or(&text(&root, "name"), "Bruno import");
    let mut walker = Walker {
        tree: TreeBuilder::new(&name, workspace_id)?,
        scripts: 0,
        assertions_skipped: 0,
        vars: 0,
    };
    if let Some(request) = root.pointer("/root/request") {
        // Collection-level auth applies to every request that inherits.
        walker.tree.collection.auth = walker.auth(request.get("auth"), "collection");
        let headers = rows(request.get("headers"), "name", "enabled", true, convert);
        if !headers.is_empty() {
            walker.tree.warnings.push(format!(
                "{} collection header(s) are not imported; add them to each request",
                headers.len()
            ));
        }
    }
    if let Some(docs) = root.pointer("/root/docs").and_then(Value::as_str) {
        walker.tree.collection.description = docs.to_owned();
    }
    walker.level(items, None, 0)?;
    let mut tree = walker.tree;
    note_count(
        &mut tree.warnings,
        walker.scripts,
        "1 request has Bruno scripts or tests, kept as written; Bruno's bru and res API is not available",
        "{count} requests have Bruno scripts or tests, kept as written; Bruno's bru and res API is not available",
    );
    note_count(
        &mut tree.warnings,
        walker.assertions_skipped,
        "1 Bruno assertion is not imported (only res.status eq is)",
        "{count} Bruno assertions are not imported (only res.status eq is)",
    );
    note_count(
        &mut tree.warnings,
        walker.vars,
        "1 request defines Bruno request variables, which are not imported",
        "{count} requests define Bruno request variables, which are not imported",
    );
    let environments = root
        .get("environments")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    note_count(
        &mut tree.warnings,
        environments,
        "1 Bruno environment is not imported; recreate it under Environments",
        "{count} Bruno environments are not imported; recreate them under Environments",
    );
    Ok(tree.finish())
}

/// Bruno writes variables as `{{name}}`, like PingDeck: values pass through.
fn convert(value: &str) -> String {
    value.to_owned()
}

struct Walker {
    tree: TreeBuilder,
    scripts: usize,
    assertions_skipped: usize,
    vars: usize,
}

impl Walker {
    fn level(
        &mut self,
        items: Option<&Vec<Value>>,
        folder: Option<&FolderId>,
        depth: usize,
    ) -> Result<(), ImportError> {
        if depth > 64 {
            self.tree
                .warnings
                .push("folders nest deeper than 64 levels; the rest is skipped".to_owned());
            return Ok(());
        }
        let mut items: Vec<&Value> = items.into_iter().flatten().collect();
        items.sort_by_key(|item| item.get("seq").and_then(Value::as_i64).unwrap_or(i64::MAX));
        for (position, item) in items.into_iter().enumerate() {
            let position = i64::try_from(position).unwrap_or(i64::MAX);
            match text(item, "type").as_str() {
                "folder" => {
                    let id = self.tree.folder(&text(item, "name"), folder, position)?;
                    self.level(
                        item.get("items").and_then(Value::as_array),
                        Some(&id),
                        depth + 1,
                    )?;
                }
                kind @ ("http-request" | "graphql-request") => {
                    self.request(item, kind == "graphql-request", folder, position)?;
                }
                other => self.tree.warnings.push(format!(
                    "item '{}' of type '{other}' is not imported",
                    text(item, "name")
                )),
            }
        }
        Ok(())
    }

    fn request(
        &mut self,
        item: &Value,
        graphql: bool,
        folder: Option<&FolderId>,
        position: i64,
    ) -> Result<(), ImportError> {
        let name = non_empty_or(&text(item, "name"), "Request");
        let label = format!("request '{name}'");
        let request = item.get("request").unwrap_or(&Value::Null);
        let params: Vec<&Value> = request
            .get("params")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect();
        let pick = |kind: &str| -> Vec<KeyValue> {
            let list: Vec<Value> = params
                .iter()
                .filter(|param| {
                    text(param, "type") == kind
                        || (kind == "query" && text(param, "type").is_empty())
                })
                .map(|param| (*param).clone())
                .collect();
            rows(Some(&Value::Array(list)), "name", "enabled", true, convert)
        };
        let url = text(request, "url");
        // The query rows carry the query; the URL keeps only its base.
        let base = url.split_once('?').map_or(url.as_str(), |(base, _)| base);
        let query_params = pick("query");
        let path_variables = pick("path");
        let headers = rows(request.get("headers"), "name", "enabled", true, convert);
        let method = method_or_get(&text(request, "method"), &mut self.tree.warnings, &label);
        let body = self.body(request.get("body"), graphql, &label);
        let auth = self.auth(request.get("auth"), &label);
        let draft = RequestDraft {
            name,
            method,
            url: base.to_owned(),
            query_params,
            headers,
            body,
            auth,
            description: text(request, "docs"),
            folder_id: folder.cloned(),
        };
        let pre = request
            .pointer("/script/req")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut post = request
            .pointer("/script/res")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let tests = text(request, "tests");
        if !tests.trim().is_empty() {
            if !post.is_empty() {
                post.push('\n');
            }
            post.push_str(&tests);
        }
        if !pre.trim().is_empty() || !post.trim().is_empty() {
            self.scripts += 1;
        }
        let vars = ["/vars/req", "/vars/res"].iter().any(|path| {
            request
                .pointer(path)
                .and_then(Value::as_array)
                .is_some_and(|list| !list.is_empty())
        });
        self.vars += usize::from(vars);
        let assertions = self.assertions(request.get("assertions"));
        let request = self.tree.request(draft, position)?;
        request.path_variables = path_variables;
        request.pre_request_script = pre;
        request.test_script = post;
        request.assertions = assertions;
        if graphql {
            request.protocol = crate::workspace::models::ProtocolKind::Graphql;
        }
        Ok(())
    }

    fn assertions(&mut self, value: Option<&Value>) -> Vec<Assertion> {
        let mut out = Vec::new();
        for row in value.and_then(Value::as_array).into_iter().flatten() {
            if row.get("enabled").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            let status = (text(row, "name").trim() == "res.status")
                .then(|| text(row, "value"))
                .and_then(|value| {
                    value
                        .trim()
                        .strip_prefix("eq ")
                        .and_then(|code| code.trim().parse::<u16>().ok())
                });
            match status {
                Some(value) => out.push(Assertion::StatusEquals { value }),
                None => self.assertions_skipped += 1,
            }
        }
        out
    }

    fn body(&mut self, body: Option<&Value>, graphql: bool, label: &str) -> RequestBody {
        let Some(body) = body.filter(|body| body.is_object()) else {
            return RequestBody::None;
        };
        if graphql || text(body, "mode") == "graphql" {
            let graphql = body.get("graphql").unwrap_or(&Value::Null);
            let variables = text(graphql, "variables");
            return RequestBody::Graphql {
                query: text(graphql, "query"),
                variables_json: Some(variables).filter(|vars| !vars.trim().is_empty()),
                operation_name: None,
            };
        }
        match text(body, "mode").as_str() {
            "json" => RequestBody::Json {
                content: text(body, "json"),
            },
            "text" => RequestBody::RawText {
                content: text(body, "text"),
                language: None,
            },
            "xml" => RequestBody::RawText {
                content: text(body, "xml"),
                language: Some(RawLanguage::Xml),
            },
            "formUrlEncoded" => RequestBody::FormUrlEncoded {
                fields: rows(body.get("formUrlEncoded"), "name", "enabled", true, convert),
            },
            "multipartForm" => {
                let mut dropped = false;
                let parts = body
                    .get("multipartForm")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|row| !text(row, "name").trim().is_empty())
                    .map(|row| {
                        let is_file = text(row, "type") == "file";
                        dropped |= is_file;
                        FormPart {
                            key: text(row, "name").trim().to_owned(),
                            value: if is_file {
                                String::new()
                            } else {
                                text(row, "value")
                            },
                            file_path: is_file.then(String::new),
                            enabled: row.get("enabled").and_then(Value::as_bool).unwrap_or(true),
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
            "none" | "" => RequestBody::None,
            other => {
                self.tree.warnings.push(format!(
                    "{label}: body mode '{other}' is not supported; body skipped"
                ));
                RequestBody::None
            }
        }
    }

    fn auth(&mut self, auth: Option<&Value>, label: &str) -> AuthConfig {
        let Some(auth) = auth.filter(|auth| auth.is_object()) else {
            return AuthConfig::Inherit;
        };
        let leaf = |group: &str, key: &str| {
            auth.get(group)
                .map(|group| text(group, key))
                .unwrap_or_default()
        };
        match text(auth, "mode").as_str() {
            "inherit" | "" => AuthConfig::Inherit,
            "none" => AuthConfig::None,
            "bearer" => AuthConfig::Bearer {
                token_ref: leaf("bearer", "token"),
            },
            "basic" => AuthConfig::Basic {
                username: leaf("basic", "username"),
                password_ref: leaf("basic", "password"),
            },
            "digest" => AuthConfig::Digest {
                username: leaf("digest", "username"),
                password_ref: Some(leaf("digest", "password")),
            },
            "apikey" => AuthConfig::ApiKey {
                key: leaf("apikey", "key"),
                value_ref: leaf("apikey", "value"),
                location: if leaf("apikey", "placement") == "queryparams" {
                    ApiKeyLocation::Query
                } else {
                    ApiKeyLocation::Header
                },
            },
            "awsv4" => AuthConfig::AwsSignatureV4 {
                access_key: leaf("awsv4", "accessKeyId"),
                secret_key_ref: Some(leaf("awsv4", "secretAccessKey")),
                region: leaf("awsv4", "region"),
                service: leaf("awsv4", "service"),
                session_token_ref: Some(leaf("awsv4", "sessionToken"))
                    .filter(|token| !token.is_empty()),
            },
            other => {
                self.tree.warnings.push(format!(
                    "{label}: Bruno auth '{other}' is not supported; auth inherits"
                ));
                AuthConfig::Inherit
            }
        }
    }
}
