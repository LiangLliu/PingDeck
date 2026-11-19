//! The `get_request` tool: one saved request as the model may read it.
//!
//! Single responsibility: find a saved request of the chat's workspace by id or by its
//! `Collection/Folder/Name` path and return its definition: method, URL template, query, headers,
//! auth, assertions, place and the agent marker. A credential header, query parameter or auth
//! field keeps only its name unless its value is a `{{variable}}` reference; known secret values
//! are masked by the redaction step. The body follows the JSON; past
//! [`GET_REQUEST_BODY_BYTES`] it stays behind a `read_output` handle.

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::common::{Tree, bound_scope, error_output, json_output, tool_spec, workspace_secrets};
use super::draft::{method_name, request_by_path};
use super::redact::{is_credential_row, redact_for_model, row_value_shown, template_only};
use super::schema::parse_args;
use crate::agent::limits::GET_REQUEST_BODY_BYTES;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolSpec,
};
use crate::app::{AppState, command_error};
use crate::error::AppError;
use crate::llm::hub::HubPart;
use crate::workspace::models::{AuthConfig, Request};
use crate::workspace::{KeyValue, RequestBody, RequestId};

/// Name of the tool.
pub const GET_REQUEST: &str = "get_request";

/// Cap of the definition before the body (the body has its own).
const DEFINITION_CAP: usize = 16 * 1024;

/// `get_request {request_id | request_path}`.
pub struct GetRequestTool {
    spec: ToolSpec,
}

impl GetRequestTool {
    /// The tool with its static spec: class `PingDeckRead`, read-only, parallel-safe.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: tool_spec(
                GET_REQUEST,
                "Read one saved request of this chat's PingDeck workspace: method, URL template, \
                 query, headers, auth, body, assertions, place and whether the agent changed it. \
                 Credential values show only their name unless they are a {{variable}}; a long \
                 body continues behind a read_output handle.",
                ToolClass::PingDeckRead,
                false,
                json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "request_id": {"type": "string", "minLength": 1, "description": "Id of the saved request (from list_requests)"},
                        "request_path": {"type": "string", "minLength": 1, "description": "Collection/Folder/Request name"}
                    }
                }),
            ),
        }
    }
}

impl Default for GetRequestTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct GetArgs {
    request_id: Option<String>,
    request_path: Option<String>,
}

fn check(args: &GetArgs) -> Result<(), AppError> {
    match (&args.request_id, &args.request_path) {
        (Some(_), Some(_)) => Err(AppError::validation(
            "Give request_id or request_path, not both.",
        )),
        (None, None) => Err(AppError::validation(
            "Give request_id or request_path (use list_requests to find one).",
        )),
        _ => Ok(()),
    }
}

/// Rows as `{name, value, enabled}`; a credential row's literal value becomes
/// `value_hidden: true`.
fn rows(rows: &[KeyValue], header: bool) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            let mut out = json!({"name": row.key});
            if is_credential_row(&row.key, header) && !template_only(&row.value) {
                out["value_hidden"] = json!(true);
            } else {
                out["value"] = json!(row.value);
            }
            if !row.enabled {
                out["enabled"] = json!(false);
            }
            out
        })
        .collect()
}

/// `{name, value}` of an auth field that may hold a secret: shown only as a reference.
fn secret_field(name: &str, value: Option<&str>) -> Value {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => json!({"name": name, "value": ""}),
        Some(value) if template_only(value) => json!({"name": name, "value": value}),
        Some(_) => json!({"name": name, "value_hidden": true}),
    }
}

/// The auth of a request: its type, plain fields as written, secret fields as references.
fn auth(config: &AuthConfig) -> Value {
    let plain = |name: &str, value: &str| json!({"name": name, "value": value});
    let (kind, fields) = match config {
        AuthConfig::Inherit => ("inherit", Vec::new()),
        AuthConfig::None => ("none", Vec::new()),
        AuthConfig::Bearer { token_ref } => {
            ("bearer", vec![secret_field("token", Some(token_ref))])
        }
        AuthConfig::Basic {
            username,
            password_ref,
        } => (
            "basic",
            vec![
                plain("username", username),
                secret_field("password", Some(password_ref)),
            ],
        ),
        AuthConfig::ApiKey { key, value_ref, .. } => (
            "api_key",
            vec![plain("key", key), secret_field("value", Some(value_ref))],
        ),
        AuthConfig::OAuth2 {
            access_token_url,
            client_id,
            client_secret_ref,
            username,
            scope,
            ..
        } => (
            "oauth2",
            vec![
                plain("access_token_url", access_token_url),
                plain("client_id", client_id),
                secret_field("client_secret", Some(client_secret_ref)),
                plain("username", username),
                plain("scope", scope),
            ],
        ),
        AuthConfig::Digest {
            username,
            password_ref,
        } => (
            "digest",
            vec![
                plain("username", username),
                secret_field("password", password_ref.as_deref()),
            ],
        ),
        AuthConfig::AwsSignatureV4 {
            access_key,
            secret_key_ref,
            region,
            service,
            ..
        } => (
            "aws_sigv4",
            vec![
                secret_field("access_key", Some(access_key)),
                secret_field("secret_key", secret_key_ref.as_deref()),
                plain("region", region),
                plain("service", service),
            ],
        ),
    };
    json!({"type": kind, "fields": fields})
}

/// A form field's value: a credential parameter shows only a `{{variable}}` reference.
fn shown_value(name: &str, value: &str) -> String {
    row_value_shown(name, value, false)
}

/// `(type, text)` of a body as the model reads it; `None` for no body.
fn body(body: &RequestBody) -> Option<(&'static str, String)> {
    let form = |fields: &[KeyValue]| {
        fields
            .iter()
            .map(|field| format!("{}={}", field.key, shown_value(&field.key, &field.value)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match body {
        RequestBody::None => None,
        RequestBody::Json { content } => Some(("json", content.clone())),
        RequestBody::RawText { content, .. } => Some(("text", content.clone())),
        RequestBody::FormUrlEncoded { fields } => Some(("form", form(fields))),
        RequestBody::Multipart { parts } => Some((
            "multipart",
            parts
                .iter()
                .map(|part| match &part.file_path {
                    Some(path) => format!("{}=@{path}", part.key),
                    None => format!("{}={}", part.key, shown_value(&part.key, &part.value)),
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        RequestBody::Graphql {
            query,
            variables_json,
            ..
        } => Some((
            "graphql",
            match variables_json {
                Some(variables) => format!("{query}\n\nvariables: {variables}"),
                None => query.clone(),
            },
        )),
        RequestBody::Binary { file_path, .. } => Some(("binary", format!("file: {file_path}"))),
    }
}

/// The request named by `args` in the chat's workspace.
fn find(state: &AppState, session_id: &str, args: &GetArgs) -> Result<ToolOutput, AppError> {
    check(args)?;
    let scope = bound_scope(state, session_id)?;
    let workspace_id = scope.workspace.id.clone();
    let store = state.store();
    let tree = Tree::load(&store, &workspace_id)?;
    let secrets = workspace_secrets(&store, workspace_id.as_str())?;
    let request: Request = match (&args.request_id, &args.request_path) {
        (Some(id), _) => {
            let id = RequestId::new(id.trim()).map_err(command_error)?;
            store
                .get_request(&workspace_id, &id)
                .map_err(command_error)?
                .ok_or_else(|| {
                    AppError::not_found(format!(
                        "No saved request {} in workspace \"{}\". Use list_requests to find it.",
                        id.as_str(),
                        scope.workspace.name
                    ))
                })?
        }
        (None, Some(path)) => request_by_path(
            &tree,
            store
                .list_requests(workspace_id.as_str())
                .map_err(command_error)?,
            path,
        )?,
        (None, None) => return Err(AppError::validation("Give request_id or request_path.")),
    };
    let touched = store
        .agent_touched_request_ids(workspace_id.as_str())
        .map_err(command_error)?
        .contains(request.id.as_str());
    drop(store);
    let collection = request.collection_id.as_ref().and_then(|id| {
        tree.collections()
            .iter()
            .find(|collection| &collection.id == id)
            .map(|collection| collection.name.clone())
    });
    let shown_body = body(&request.body);
    let value = json!({
        "workspace": scope.tag(),
        "request_id": request.id.as_str(),
        "name": request.name,
        "collection": collection,
        "folder": tree.location(&request),
        "path": tree.full_path(&request),
        "method": method_name(request.method),
        "url": request.url,
        "query": rows(&request.query_params, false),
        "path_variables": rows(&request.path_variables, false),
        "headers": rows(&request.headers, true),
        "auth": auth(&request.auth),
        "body": shown_body.as_ref().map_or_else(
            || json!({"type": "none"}),
            |(kind, text)| json!({"type": kind, "bytes": text.len(), "follows": true}),
        ),
        "assertions": serde_json::to_value(&request.assertions).unwrap_or_default(),
        "scripts": {
            "pre_request": !request.pre_request_script.trim().is_empty(),
            "test": !request.test_script.trim().is_empty(),
        },
        "description": request.description,
        "agent_touched": touched,
        "view": {
            "kind": "saved_request",
            "request_id": request.id.as_str(),
            "workspace_id": workspace_id.as_str(),
        },
    });
    let output = json_output(&value, &secrets, DEFINITION_CAP);
    let Some((kind, text)) = shown_body else {
        return Ok(output);
    };
    let head = format!(
        "{}\n\nBody ({kind}, {} bytes):\n",
        output.text_content(),
        text.len()
    );
    let body = redact_for_model(&text, &secrets, usize::MAX);
    let cut = (body.len() > GET_REQUEST_BODY_BYTES).then(|| head.len() + GET_REQUEST_BODY_BYTES);
    let mut output = ToolOutput {
        parts: vec![HubPart::text(format!("{head}{body}"))],
        ..output
    };
    if let Some(cut) = cut {
        output = output.capped(cut);
    }
    Ok(output)
}

impl AgentTool for GetRequestTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args: GetArgs = parse_args(&self.spec.input_schema, arguments)?;
        check(&args)?;
        bound_scope(crate::agent_tools::app_state(ctx), ctx.session_id)?;
        Ok(CallInspection {
            read_only: true,
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args: GetArgs = match parse_args(&self.spec.input_schema, &arguments) {
                Ok(args) => args,
                Err(error) => return error_output(&error, &[]),
            };
            find(crate::agent_tools::app_state(ctx), ctx.session_id, &args)
                .unwrap_or_else(|error| error_output(&error, &[]))
        })
    }
}
