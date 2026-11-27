//! The `create_request` tool: save a request in a collection, or update one, without sending it.
//!
//! Single responsibility: validate like the UI (name, URL, placement, body size), refuse secret
//! literals (use `{{name}}`), create missing folders, save through the workspace store and stamp
//! the request as agent-touched, so it declares no host for secrets until the user sends it from
//! the UI. The folders, the request, its marker and its row in the change journal are written in
//! one transaction, so the Changes tab lists the write and can undo it
//! (`agent_service::request_changes`). The approval card shows a before/after diff. Environment
//! values cannot be set through this tool.

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::common::{
    Scope, Tree, bound_scope, error_output, json_output, tool_spec, workspace_secrets,
};
use super::draft::{
    DraftBody, MAX_BODY, Pair, body_size, draft_body, method_name, method_of, pair_schema, pairs,
};
use super::redact::{NamedSecret, is_credential_param, row_value_shown, template_only};
use super::schema::parse_args;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolApprovalPreview, ToolClass, ToolContext, ToolEvent, ToolOutput,
    ToolSpec,
};
use crate::agent_service::request_changes::{RequestChangeKey, journal_request_write, put_before};
use crate::app::{AppState, command_error, current_time_ms};
use crate::error::AppError;
use crate::secrets::redact_secrets;
use crate::workspace::models::{Collection, Folder, Request};
use crate::workspace::validation::{validate_name, validate_url};
use crate::workspace::variables::uuid_v4;
use crate::workspace::{FolderId, KeyValue, RequestBody, RequestId};

/// Name of the tool.
pub const CREATE_REQUEST: &str = "create_request";

/// Shortest known secret value refused as a literal; shorter ones are common words.
const MIN_LITERAL_SECRET: usize = 8;
/// Cap of one result.
const RESULT_CAP: usize = 4 * 1024;
/// Longest body shown in the approval diff.
const DIFF_BODY_CHARS: usize = 2_000;
/// The refusal of a secret literal.
const PUT_SECRETS_IN_VARIABLES: &str =
    "Put secrets in an environment variable and reference it as {{name}}.";

/// `create_request {collection, folder?, name, method?, url, headers?, query?, body?, description?
/// | request_id, ...}`; an update also takes `headers_add`, `headers_remove`, `query_add` and
/// `query_remove`: only the named fields and rows change.
pub struct CreateRequestTool {
    spec: ToolSpec,
}

impl CreateRequestTool {
    /// The tool with its static spec: class `SpanBayWrite`, a barrier. Updating overwrites a
    /// request, so a call with `request_id` is classified destructive in [`AgentTool::inspect`].
    #[must_use]
    pub fn new() -> Self {
        let pair = pair_schema();
        Self {
            spec: tool_spec(
                CREATE_REQUEST,
                "Save an HTTP request in a collection (folder path created if missing) without \
                 sending it, or update one with request_id: only the fields you give change, and \
                 the request stays in its folder unless you give folder or another collection. \
                 headers and query replace the whole list; headers_add/headers_remove and \
                 query_add/query_remove change single rows. Put secrets in an environment \
                 variable and reference them as {{name}}; literal secrets are refused.",
                ToolClass::SpanBayWrite,
                false,
                json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "collection": {"type": "string", "minLength": 1, "description": "Collection name"},
                        "folder": {"type": "string", "description": "Folder path inside the collection, for example Invoices/Refunds"},
                        "name": {"type": "string", "minLength": 1, "description": "Request name shown in the tree"},
                        "method": {"type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"], "description": "HTTP method"},
                        "url": {"type": "string", "minLength": 1, "description": "URL template, for example {{base_url}}/refunds"},
                        "headers": {"type": "array", "items": pair, "description": "Replaces every header: the full list as {name, value}"},
                        "headers_add": {"type": "array", "items": pair, "description": "Headers to set: a header of the same name is replaced, others are kept"},
                        "headers_remove": {"type": "array", "items": {"type": "string", "minLength": 1}, "description": "Names of headers to delete"},
                        "query": {"type": "array", "items": pair, "description": "Replaces every query parameter: the full list as {name, value}"},
                        "query_add": {"type": "array", "items": pair, "description": "Query parameters to set: one of the same name is replaced, others are kept"},
                        "query_remove": {"type": "array", "items": {"type": "string", "minLength": 1}, "description": "Names of query parameters to delete"},
                        "body": {
                            "description": "Request body",
                            "type": "object", "additionalProperties": false, "required": ["type"],
                            "properties": {
                                "type": {"type": "string", "enum": ["json", "text", "form", "none"]},
                                "content": {"description": "json and text: the body text; form: a list of {name, value}"}
                            }
                        },
                        "description": {"type": "string", "description": "Notes saved with the request"},
                        "request_id": {"type": "string", "minLength": 1, "description": "Update this saved request instead of creating one"}
                    }
                }),
            ),
        }
    }
}

impl Default for CreateRequestTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct CreateArgs {
    collection: Option<String>,
    folder: Option<String>,
    name: Option<String>,
    method: Option<String>,
    url: Option<String>,
    headers: Option<Vec<Pair>>,
    #[serde(default)]
    headers_add: Vec<Pair>,
    #[serde(default)]
    headers_remove: Vec<String>,
    query: Option<Vec<Pair>>,
    #[serde(default)]
    query_add: Vec<Pair>,
    #[serde(default)]
    query_remove: Vec<String>,
    body: Option<DraftBody>,
    description: Option<String>,
    request_id: Option<String>,
}

/// What a call would save, computed without writing anything.
struct Plan {
    scope: Scope,
    /// The stored request when updating.
    existing: Option<Request>,
    /// The request as it would be saved (its folder is the deepest folder that already exists).
    request: Request,
    collection: Collection,
    /// Folder names to create below the deepest existing one.
    missing_folders: Vec<String>,
    /// Header and query names an update deletes (a whole-list replacement included).
    removed: Removed,
    secrets: Vec<NamedSecret>,
    tree: Tree,
}

/// Whether one word of `text` has the shape of a credential (`sk-…`, `ghp_…`, a JWT, …).
fn has_secret_shape(text: &str) -> bool {
    text.split(|c: char| {
        !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '='))
    })
    .any(|word| word.len() >= 16 && redact_secrets(word, &[]) != word)
}

fn refuse() -> AppError {
    AppError::validation(PUT_SECRETS_IN_VARIABLES)
}

/// Refuses a request that carries a secret literal (PRD §6 limits).
fn refuse_secret_literals(request: &Request, secrets: &[NamedSecret]) -> Result<(), AppError> {
    for header in request.headers.iter().filter(|header| header.enabled) {
        if super::redact::is_credential_header(&header.key) && !template_only(&header.value) {
            return Err(refuse());
        }
        if has_secret_shape(&header.value) {
            return Err(refuse());
        }
    }
    let url_query = request.url.split_once('?').map_or("", |(_, query)| query);
    let url_rows = url_query.split('&').filter_map(|pair| pair.split_once('='));
    let rows = request
        .query_params
        .iter()
        .filter(|row| row.enabled)
        .map(|row| (row.key.as_str(), row.value.as_str()))
        .chain(url_rows);
    for (name, value) in rows {
        if (is_credential_param(name) && !template_only(value)) || has_secret_shape(value) {
            return Err(refuse());
        }
    }
    let userinfo = request
        .url
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default())
        .and_then(|authority| authority.rsplit_once('@'))
        .map(|(userinfo, _)| userinfo);
    if let Some((_, password)) = userinfo.and_then(|userinfo| userinfo.split_once(':'))
        && !template_only(password)
    {
        return Err(refuse());
    }
    let body = match &request.body {
        RequestBody::Json { content } | RequestBody::RawText { content, .. } => content.clone(),
        RequestBody::FormUrlEncoded { fields } => fields
            .iter()
            .map(|field| field.value.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    if has_secret_shape(&body) {
        return Err(refuse());
    }
    let everything = format!(
        "{}\n{}\n{}\n{body}\n{}",
        request.url,
        request
            .headers
            .iter()
            .map(|header| header.value.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        request
            .query_params
            .iter()
            .map(|row| row.value.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        request.description,
    );
    if secrets
        .iter()
        .filter(|secret| secret.value.chars().count() >= MIN_LITERAL_SECRET)
        .any(|secret| everything.contains(&secret.value))
    {
        return Err(refuse());
    }
    Ok(())
}

/// Where `folder` (a `/` path) leads in `collection`: the deepest existing folder and the names
/// that still have to be created below it.
fn place(tree: &Tree, collection: &Collection, folder: &str) -> (Option<FolderId>, Vec<String>) {
    let mut parent: Option<FolderId> = None;
    let mut names = folder
        .split('/')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    let mut missing = Vec::new();
    for name in names.by_ref() {
        let found = tree
            .children(
                collection.id.as_str(),
                parent.as_ref().map(FolderId::as_str),
            )
            .into_iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(&name))
            .map(|candidate| candidate.id.clone());
        match found {
            Some(id) => parent = Some(id),
            None => {
                missing.push(name);
                break;
            }
        }
    }
    missing.extend(names);
    (parent, missing)
}

/// Header and query names an update deletes, listed in the result and the approval diff.
#[derive(Default)]
struct Removed {
    headers: Vec<String>,
    query: Vec<String>,
}

impl Removed {
    fn is_empty(&self) -> bool {
        self.headers.is_empty() && self.query.is_empty()
    }
}

/// Whether two row names are the same row: headers ignore case, query parameters do not.
fn same_name(a: &str, b: &str, header: bool) -> bool {
    if header {
        a.trim().eq_ignore_ascii_case(b.trim())
    } else {
        a.trim() == b.trim()
    }
}

/// `rows` with the `remove` names deleted, then each `add` row set: it takes the place of the
/// first row of its name (others of that name go), or comes last.
///
/// # Errors
/// [`AppError::Validation`] for a name to remove that no row has.
fn patch_rows(
    rows: &mut Vec<KeyValue>,
    add: &[Pair],
    remove: &[String],
    header: bool,
) -> Result<(), AppError> {
    let what = if header { "header" } else { "query parameter" };
    for name in remove {
        if !rows.iter().any(|row| same_name(&row.key, name, header)) {
            return Err(AppError::validation(format!(
                "No {what} \"{}\" to remove.",
                name.trim()
            )));
        }
        rows.retain(|row| !same_name(&row.key, name, header));
    }
    for pair in add {
        let first = rows
            .iter()
            .position(|row| same_name(&row.key, &pair.name, header));
        let description = first.and_then(|index| rows[index].description.clone());
        rows.retain(|row| !same_name(&row.key, &pair.name, header));
        let row = KeyValue {
            key: pair.name.trim().to_owned(),
            value: pair.value.clone(),
            enabled: true,
            description,
        };
        match first {
            Some(index) => rows.insert(index.min(rows.len()), row),
            None => rows.push(row),
        }
    }
    Ok(())
}

/// Names of `before` that no row of `after` has any more (each once, in order).
fn removed_names(before: &[KeyValue], after: &[KeyValue], header: bool) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for row in before {
        let gone = !after
            .iter()
            .any(|kept| same_name(&kept.key, &row.key, header));
        if gone && !names.iter().any(|name| same_name(name, &row.key, header)) {
            names.push(row.key.clone());
        }
    }
    names
}

/// A whole-list replacement and single-row changes of the same list do not mix.
fn check_patch(args: &CreateArgs) -> Result<(), AppError> {
    if args.headers.is_some() && !(args.headers_add.is_empty() && args.headers_remove.is_empty()) {
        return Err(AppError::validation(
            "Give `headers` (the whole list) or `headers_add`/`headers_remove`, not both.",
        ));
    }
    if args.query.is_some() && !(args.query_add.is_empty() && args.query_remove.is_empty()) {
        return Err(AppError::validation(
            "Give `query` (the whole list) or `query_add`/`query_remove`, not both.",
        ));
    }
    Ok(())
}

fn plan(state: &AppState, session_id: &str, args: &CreateArgs) -> Result<Plan, AppError> {
    check_patch(args)?;
    let scope = bound_scope(state, session_id)?;
    let workspace_id = scope.workspace.id.clone();
    let store = state.store();
    let tree = Tree::load(&store, &workspace_id)?;
    let secrets = workspace_secrets(&store, workspace_id.as_str())?;
    let existing = match &args.request_id {
        Some(id) => {
            let id = RequestId::new(id.trim()).map_err(command_error)?;
            Some(
                store
                    .get_request(&workspace_id, &id)
                    .map_err(command_error)?
                    .ok_or_else(|| {
                        AppError::not_found(format!(
                            "No saved request {} in workspace \"{}\".",
                            id.as_str(),
                            scope.workspace.name
                        ))
                    })?,
            )
        }
        None => None,
    };
    let collections = tree.collections();
    let collection = match (&args.collection, &existing) {
        (Some(name), _) => collections
            .iter()
            .find(|collection| collection.name.eq_ignore_ascii_case(name.trim()))
            .cloned()
            .ok_or_else(|| {
                AppError::not_found(format!(
                    "No collection \"{}\". Available: {}.",
                    name.trim(),
                    collections
                        .iter()
                        .map(|collection| collection.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?,
        (None, Some(existing)) => existing
            .collection_id
            .as_ref()
            .and_then(|id| collections.iter().find(|collection| &collection.id == id))
            .cloned()
            .ok_or_else(|| AppError::validation("Give `collection`: the request is not in one."))?,
        (None, None) => {
            return Err(AppError::validation(
                "Missing required argument \"collection\".",
            ));
        }
    };
    let mut request = match &existing {
        Some(existing) => existing.clone(),
        None => {
            let name = args
                .name
                .as_deref()
                .ok_or_else(|| AppError::validation("Missing required argument \"name\"."))?;
            let url = args
                .url
                .as_deref()
                .ok_or_else(|| AppError::validation("Missing required argument \"url\"."))?;
            Request::new(
                format!("req_{}", uuid_v4()),
                workspace_id.as_str(),
                name,
                url,
            )
            .map_err(command_error)?
        }
    };
    if let Some(name) = &args.name {
        request.name = validate_name("request", name).map_err(command_error)?;
    }
    if let Some(url) = &args.url {
        request.url = validate_url(url).map_err(command_error)?;
    }
    if let Some(method) = &args.method {
        request.method = method_of(method)?;
    }
    if let Some(headers) = &args.headers {
        request.headers = pairs(headers);
    }
    patch_rows(
        &mut request.headers,
        &args.headers_add,
        &args.headers_remove,
        true,
    )?;
    if let Some(query) = &args.query {
        request.query_params = pairs(query);
    }
    patch_rows(
        &mut request.query_params,
        &args.query_add,
        &args.query_remove,
        false,
    )?;
    let removed = existing
        .as_ref()
        .map(|existing| Removed {
            headers: removed_names(&existing.headers, &request.headers, true),
            query: removed_names(&existing.query_params, &request.query_params, false),
        })
        .unwrap_or_default();
    if let Some(body) = &args.body {
        request.body = if body.r#type == "none" {
            RequestBody::None
        } else {
            draft_body(body)?
        };
    }
    if let Some(description) = &args.description {
        request.description.clone_from(description);
    }
    if body_size(&request.body) > MAX_BODY {
        return Err(AppError::validation(
            "The body is larger than the 256 KB limit.",
        ));
    }
    // Placement: a folder or another collection moves the request; naming the collection it is
    // already in keeps it where it is.
    let mut missing_folders = Vec::new();
    let other_collection = existing
        .as_ref()
        .is_some_and(|existing| existing.collection_id.as_ref() != Some(&collection.id));
    if existing.is_none() || args.folder.is_some() || other_collection {
        let (folder, missing) = place(
            &tree,
            &collection,
            args.folder.as_deref().unwrap_or_default(),
        );
        request.collection_id = Some(collection.id.clone());
        request.folder_id = folder;
        missing_folders = missing;
    }
    request.validate_assertions().map_err(command_error)?;
    request.validate_ai().map_err(command_error)?;
    refuse_secret_literals(&request, &secrets)?;
    drop(store);
    Ok(Plan {
        scope,
        existing,
        request,
        collection,
        missing_folders,
        removed,
        secrets,
        tree,
    })
}

/// `Collection/Folder/…` where the request lands, new folders included.
fn destination(plan: &Plan) -> String {
    let mut path = plan.tree.location(&plan.request);
    if path.is_empty() {
        path.clone_from(&plan.collection.name);
    }
    for name in &plan.missing_folders {
        path.push('/');
        path.push_str(name);
    }
    path
}

/// The whole body as one text (compared whole; [`shown`] cuts it for display).
fn body_text(body: &RequestBody) -> String {
    match body {
        RequestBody::None => "none".to_owned(),
        RequestBody::Json { content } => format!("json {content}"),
        RequestBody::RawText { content, .. } => format!("text {content}"),
        RequestBody::FormUrlEncoded { fields } => format!(
            "form {}",
            fields
                .iter()
                .map(|field| format!(
                    "{}={}",
                    field.key,
                    row_value_shown(&field.key, &field.value, false)
                ))
                .collect::<Vec<_>>()
                .join("&")
        ),
        _ => "(other)".to_owned(),
    }
}

/// `text` as diff lines, cut after [`DIFF_BODY_CHARS`] characters with a note of the rest.
fn shown(text: &str) -> Vec<String> {
    let total = text.chars().count();
    let mut cut: String = text.chars().take(DIFF_BODY_CHARS).collect();
    if total > DIFF_BODY_CHARS {
        cut.push_str(&format!(" … ({} more characters)", total - DIFF_BODY_CHARS));
    }
    cut.lines().map(str::to_owned).collect()
}

/// Rows as `Name: value` lines: a `{{variable}}` reference as written, a credential literal as
/// `<redacted>` (AI-apiwb-4), a disabled row marked.
fn rows_text(rows: &[KeyValue], header: bool) -> Vec<String> {
    rows.iter()
        .map(|row| {
            let value = row_value_shown(&row.key, &row.value, header);
            if row.enabled {
                format!("{}: {value}", row.key)
            } else {
                format!("{}: {value} (disabled)", row.key)
            }
        })
        .collect()
}

/// A one-line field; an empty value is no line.
fn line(value: &str) -> Vec<String> {
    if value.is_empty() {
        Vec::new()
    } else {
        vec![value.to_owned()]
    }
}

/// The fields of a request as `(label, lines)`; rows are compared row by row, the rest whole.
fn fields(request: &Request, location: &str) -> Vec<(&'static str, Vec<String>)> {
    vec![
        ("name", line(&request.name)),
        ("location", line(location)),
        ("method", line(method_name(request.method))),
        ("url", line(&request.url)),
        ("headers", rows_text(&request.headers, true)),
        ("query", rows_text(&request.query_params, false)),
        ("body", vec![body_text(&request.body)]),
        ("description", line(&request.description)),
    ]
}

/// Whether `label` is compared row by row.
fn is_rows(label: &str) -> bool {
    matches!(label, "headers" | "query")
}

/// A field-by-field diff in unified style: `-` before, `+` after; a new request shows only `+`.
/// Header and query hunks list only the rows that go (`-`) and come (`+`), so a row whose name
/// has only a `-` line is deleted (the card's "Will be deleted" part, AI-apiwb-3).
fn diff_text(plan: &Plan) -> String {
    let after_fields = fields(&plan.request, &destination(plan));
    let before_fields = plan
        .existing
        .as_ref()
        .map(|existing| fields(existing, &plan.tree.location(existing)));
    let mut out = String::from("--- before\n+++ after\n");
    for (index, (label, after)) in after_fields.iter().enumerate() {
        let before = before_fields
            .as_ref()
            .and_then(|fields| fields.get(index))
            .map(|(_, lines)| lines.clone())
            .unwrap_or_default();
        let no_body = *label == "body" && after.first().is_some_and(|body| body == "none");
        if before == *after || (before_fields.is_none() && no_body) {
            continue;
        }
        let (gone, came): (Vec<String>, Vec<String>) = if is_rows(label) {
            (
                before
                    .iter()
                    .filter(|row| !after.contains(row))
                    .cloned()
                    .collect(),
                after
                    .iter()
                    .filter(|row| !before.contains(row))
                    .cloned()
                    .collect(),
            )
        } else {
            (before.clone(), after.clone())
        };
        if gone.is_empty() && came.is_empty() {
            continue;
        }
        out.push_str(&format!("@@ {label} @@\n"));
        for (sign, lines) in [('-', &gone), ('+', &came)] {
            for text in lines {
                let shown_lines = if *label == "body" {
                    shown(text)
                } else {
                    vec![text.clone()]
                };
                for line in shown_lines {
                    out.push_str(&format!("{sign}{line}\n"));
                }
            }
        }
    }
    if !plan.missing_folders.is_empty() {
        out.push_str(&format!(
            "@@ new folders @@\n+{}\n",
            plan.missing_folders.join("/")
        ));
    }
    out
}

/// Writes the plan in one transaction: missing folders, the request, its agent marker and its
/// journal row. The before state is the request as stored when the transaction starts.
fn apply(ctx: &ToolContext<'_>, mut plan: Plan) -> Result<ToolOutput, AppError> {
    let state = crate::agent_tools::app_state(ctx);
    let workspace_id = plan.scope.workspace.id.as_str().to_owned();
    let key = RequestChangeKey {
        session_id: ctx.session_id,
        run_id: ctx.run_id,
        call_id: ctx.call_id,
    };
    // The before blob goes to the blob store first; without one the write is not journaled.
    let before_blob = put_before(state, plan.existing.as_ref());
    let summary;
    let journaled;
    {
        let store = state.store();
        let (place, row) = store.with_transaction(
            |_| {
                let before = match &plan.existing {
                    Some(existing) => store
                        .get_request(&plan.scope.workspace.id, &existing.id)
                        .map_err(command_error)?,
                    None => None,
                };
                let mut parent = plan.request.folder_id.clone();
                let mut created = Vec::new();
                for name in &plan.missing_folders {
                    let mut folder = Folder::new(
                        format!("fld_{}", uuid_v4()),
                        plan.collection.id.as_str(),
                        name,
                    )
                    .map_err(command_error)?;
                    if let Some(parent) = &parent {
                        folder = folder.with_parent(parent.as_str()).map_err(command_error)?;
                    }
                    store.save_folder(&folder).map_err(command_error)?;
                    created.push(folder.id.as_str().to_owned());
                    parent = Some(folder.id);
                }
                plan.request.folder_id = parent;
                store.save_request(&plan.request).map_err(command_error)?;
                store
                    .mark_request_agent_touched(
                        &workspace_id,
                        plan.request.id.as_str(),
                        current_time_ms(),
                    )
                    .map_err(command_error)?;
                let tree = Tree::load(&store, &plan.scope.workspace.id)?;
                let place = {
                    let location = tree.location(&plan.request);
                    if location.is_empty() {
                        "the workspace".to_owned()
                    } else {
                        location
                    }
                };
                let row = match &before_blob {
                    Ok(before_sha256) => {
                        let after = store
                            .get_request(&plan.scope.workspace.id, &plan.request.id)
                            .map_err(command_error)?
                            .ok_or_else(|| AppError::internal("the saved request is gone"))?;
                        journal_request_write(
                            state,
                            &store,
                            &key,
                            before.as_ref(),
                            before_sha256.clone(),
                            &after,
                            &created,
                            &tree.full_path(&plan.request),
                        )?
                    }
                    Err(_) => None,
                };
                Ok((place, row))
            },
            |error| AppError::database(error.to_string()),
        )?;
        journaled = row;
        let tree = Tree::load(&store, &plan.scope.workspace.id)?;
        summary = match &plan.existing {
            None => format!(
                "Created {} ({}) in {place}",
                plan.request.name,
                plan.request.id.as_str()
            ),
            Some(existing) => {
                let before = fields(existing, &tree.location(existing));
                let after = fields(&plan.request, &place);
                let changed: Vec<&str> = before
                    .iter()
                    .zip(&after)
                    .filter(|(before, after)| before.1 != after.1)
                    .map(|(_, after)| after.0)
                    .collect();
                format!(
                    "Updated {} ({}): {}",
                    plan.request.name,
                    plan.request.id.as_str(),
                    if changed.is_empty() {
                        "nothing changed".to_owned()
                    } else {
                        changed.join(", ")
                    }
                )
            }
        };
    }
    if let (Some(row), Some(events)) = (&journaled, ctx.events) {
        events(ToolEvent::FileChanged {
            path: row.path.clone(),
            kind: if plan.existing.is_some() {
                crate::agent::tools::FileChangeKind::Edit
            } else {
                crate::agent::tools::FileChangeKind::Create
            },
            added: row.added,
            removed: row.removed,
        });
    }
    state.sync.notify_changed();
    let mut value = json!({
        "message": summary,
        "request_id": plan.request.id.as_str(),
        "workspace": plan.scope.tag(),
        "view": {
            "kind": "saved_request",
            "request_id": plan.request.id.as_str(),
            "workspace_id": workspace_id,
        },
    });
    if !plan.removed.is_empty() {
        value["removed"] = json!({"headers": plan.removed.headers, "query": plan.removed.query});
    }
    if journaled.is_none() {
        value["undo"] = json!("not recorded: this change cannot be undone from the Changes tab");
    }
    Ok(json_output(&value, &plan.secrets, RESULT_CAP))
}

impl AgentTool for CreateRequestTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args: CreateArgs = parse_args(&self.spec.input_schema, arguments)?;
        let plan = plan(crate::agent_tools::app_state(ctx), ctx.session_id, &args)?;
        let place = destination(&plan);
        Ok(CallInspection {
            read_only: false,
            destructive: plan.existing.is_some(),
            preview: Some(ToolApprovalPreview::Diff {
                path: format!(
                    "{place}/{} (workspace {})",
                    plan.request.name, plan.scope.workspace.name
                ),
                unified_diff: diff_text(&plan),
            }),
            always_ask: None,
            arg_subject: Some(format!("{place}/{}", plan.request.name)),
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args: CreateArgs = match parse_args(&self.spec.input_schema, &arguments) {
                Ok(args) => args,
                Err(error) => return error_output(&error, &[]),
            };
            let plan = match plan(crate::agent_tools::app_state(ctx), ctx.session_id, &args) {
                Ok(plan) => plan,
                Err(error) => return error_output(&error, &[]),
            };
            let secrets = plan.secrets.clone();
            apply(ctx, plan).unwrap_or_else(|error| error_output(&error, &secrets))
        })
    }
}
