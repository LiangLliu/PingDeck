//! The `send_request` tool: send a saved or ad-hoc HTTP request from the chat.
//!
//! Single responsibility: resolve the target (saved id, `request_path` or draft; the environment
//! by name, in the chat's PingDeck workspace), refuse what the host rules forbid (link-local
//! always, private and loopback hosts only when PingDeck already knows them, secrets and
//! credentials, inherited auth included, only to declared hosts), ask for approval with the
//! resolved target in plain words, and run the one execution pipeline with history source
//! `agent`. The model gets status, timing, masked headers, script results and a body of at most
//! 16 KB; secret variable values and the credential values the send actually carried are masked by
//! value. In a guarded environment it gets only the body's shape and, for 4xx and 5xx, a
//! short error text; the raw body reaches it only through a history chip the user
//! attaches. A target outside the environment's declared hosts, or one that belongs to another
//! guarded environment, always asks first; the approved target is compared again when the call
//! runs.
//!
//! Cookies: a send never reads or writes the user's workspace cookie jar. Each run has its own
//! jar in memory: cookies a response of the run sets are sent by its later sends and are gone
//! with the run.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::untrusted::sanitize_untrusted;
use pingdeck_response::response_viewer::json_path_value;

use super::common::{
    RunLimit, Scope, Tree, bound_scope, error_output, find_environment, json_output, timed_out,
    tool_spec, workspace_secrets,
};
use super::draft::{Draft, draft_request, method_name, pair_schema, request_by_path};
use super::redact::{NamedSecret, header_value_for_model, redact_for_model};
use super::schema::parse_args;
use crate::agent::limits::SENDS_PER_RUN;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolApprovalPreview, ToolClass, ToolContext, ToolOutput, ToolSpec,
};
use crate::app::execution_service::{
    ExecutionContext, ExecutionMeta, Persistence, SentCredentials, execute_request_pipeline_traced,
    execution_context_for, response_cookie_entries, runs_pre_request_scripts,
};
use crate::app::secret_hosts::{
    DeclaredHostDto, carried_secret, declared_hosts, ensure_secret_hosts_declared,
    environment_hosts, host_listed, host_of_url,
};
use crate::app::unresolved_scan::request_tokens;
use crate::app::{AppState, command_error};
use crate::error::AppError;
use crate::executor::ExecutionResult;
use crate::net_guard::{self, HostClass};
use crate::workspace::models::Request;
use crate::workspace::variables::resolve_template_lenient;
use crate::workspace::{
    CookieJarEntry, Environment, HistoryOrigin, HistorySource, HttpMethod, RequestId,
    VariableResolutionSource, VariableScopes, effective_auth,
};

/// Name of the tool.
pub const SEND_REQUEST: &str = "send_request";

/// Longest a send runs, whatever the request's own timeout says.
const SEND_TIMEOUT_SECS: u64 = 120;
/// Most response headers the model sees.
const MAX_HEADERS: usize = 30;
/// Most body text the model sees.
const BODY_CAP: usize = 16 * 1024;
/// Cap of the whole result text.
const RESULT_CAP: usize = 24 * 1024;
/// Hosts the approval card lists for a secret, before it says "and more".
const CARD_HOSTS: usize = 8;
/// Error text of a 4xx or 5xx response the model sees when it only gets the shape.
const ERROR_TEXT_CAP: usize = 2 * 1024;
/// Deepest nesting a shape spells out; below it a value is named by its type only.
const SHAPE_DEPTH: usize = 8;
/// Runs whose cookie jars and approved targets are kept before the oldest are dropped.
const RUNS_KEPT: usize = 256;
/// Cookies one run's jar holds; the oldest go first.
const RUN_JAR_COOKIES: usize = 300;
/// The placeholder name a credential value the send carried is masked with.
const SENT_CREDENTIAL: &str = "sent credential";

/// `send_request {request_id | request_path | draft, environment?, save_to_history?}`.
pub struct SendRequestTool {
    spec: ToolSpec,
    limit: RunLimit,
    /// Each run's own cookie jar, by run id.
    jars: Mutex<HashMap<String, Vec<CookieJarEntry>>>,
    /// The target each inspected call was shown with on its card, by [`approval_key`].
    approved: Mutex<HashMap<String, String>>,
}

/// Locks `mutex`, recovering from poisoning.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SendRequestTool {
    /// The tool with its static spec: class `PingDeckWrite`, never read-only (it spends money or
    /// changes a server), a barrier for the other calls of its turn.
    #[must_use]
    pub fn new() -> Self {
        let pair = pair_schema();
        Self {
            spec: tool_spec(
                SEND_REQUEST,
                "Send a saved or ad-hoc HTTP request through PingDeck and return the status, timing, \
                 headers, script results and up to 16 KB of the body. Give exactly one of \
                 request_id, request_path (`Collection/Folder/Request name`) or draft. \
                 Environments are chosen by name; secret variables ({{name}}) are resolved inside \
                 PingDeck and never shown. A guarded environment returns only the body's shape.",
                ToolClass::PingDeckWrite,
                false,
                json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "request_id": {"type": "string", "minLength": 1, "description": "Id of a saved request"},
                        "request_path": {"type": "string", "minLength": 1, "description": "Collection/Folder/Request name of a saved request"},
                        "draft": {
                            "description": "An unsaved request to send instead of a saved one",
                            "type": "object", "additionalProperties": false, "required": ["url"],
                            "properties": {
                                "method": {"type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]},
                                "url": {"type": "string", "minLength": 1},
                                "headers": {"type": "array", "items": pair},
                                "query": {"type": "array", "items": pair},
                                "body": {
                                    "type": "object", "additionalProperties": false, "required": ["type"],
                                    "properties": {
                                        "type": {"type": "string", "enum": ["json", "text", "form"]},
                                        "content": {"description": "json and text: the body text; form: a list of {name, value}"}
                                    }
                                }
                            }
                        },
                        "environment": {"type": "string", "description": "Environment name, for example Staging; the chat's active one by default"},
                        "save_to_history": {"type": "boolean", "description": "Record a GET, HEAD or OPTIONS send in History (default true); every other method is always recorded"},
                        "body": {"type": "string", "enum": ["auto", "shape", "none"], "description": "auto: the text, or only field names and types in a guarded environment; shape: field names and types; none: no body"},
                        "path": {"type": "string", "minLength": 1, "description": "JSONPath of the part of a JSON body to return, for example $.items[0]"}
                    }
                }),
            ),
            limit: RunLimit::new(SENDS_PER_RUN),
            jars: Mutex::new(HashMap::new()),
            approved: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for SendRequestTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct SendArgs {
    request_id: Option<String>,
    request_path: Option<String>,
    draft: Option<Draft>,
    environment: Option<String>,
    save_to_history: Option<bool>,
    body: Option<String>,
    path: Option<String>,
}

/// How much of the response body the model sees.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyView {
    /// The (redacted) text.
    Text,
    /// Field names and types; a 4xx or 5xx keeps a short error text.
    Shape,
    /// Nothing but the size.
    None,
}

impl BodyView {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Shape => "shape",
            Self::None => "none",
        }
    }
}

/// Everything a send needs, read once so approval and execution describe the same call.
struct Prepared {
    scope: Scope,
    environment: Option<Environment>,
    request: Request,
    /// `request` with its inherited auth expanded: what the host rules and the card judge.
    effective: Request,
    context: ExecutionContext,
    /// A draft, or a saved request the agent touched: private hosts must be known to PingDeck.
    strict: bool,
    untrusted: HashSet<String>,
    secrets: Vec<NamedSecret>,
    save_to_history: bool,
    view: BodyView,
    path: Option<String>,
}

fn prepare(state: &AppState, session_id: &str, args: &SendArgs) -> Result<Prepared, AppError> {
    let chosen = [
        args.request_id.is_some(),
        args.request_path.is_some(),
        args.draft.is_some(),
    ]
    .into_iter()
    .filter(|given| *given)
    .count();
    if chosen != 1 {
        return Err(AppError::validation(
            "Give exactly one of request_id, request_path or draft.",
        ));
    }
    if let Some(path) = &args.path {
        // Only the syntax: a path that matches nothing is a result, not an error.
        json_path_value(&Value::Null, path)
            .map_err(|error| AppError::validation(format!("path is not valid: {error}")))?;
    }
    let scope = bound_scope(state, session_id)?;
    let workspace_id = scope.workspace.id.clone();
    let store = state.store();
    let environment = find_environment(
        &store,
        &workspace_id,
        args.environment.as_deref(),
        scope.environment_id.as_deref(),
    )?;
    let request = if let Some(draft) = &args.draft {
        draft_request(workspace_id.as_str(), draft)?
    } else if let Some(id) = &args.request_id {
        let id = RequestId::new(id.trim()).map_err(command_error)?;
        store
            .get_request(&workspace_id, &id)
            .map_err(command_error)?
            .ok_or_else(|| {
                AppError::not_found(format!(
                    "No saved request {} in workspace \"{}\".",
                    id.as_str(),
                    scope.workspace.name
                ))
            })?
    } else {
        let tree = Tree::load(&store, &workspace_id)?;
        let requests = store
            .list_requests(workspace_id.as_str())
            .map_err(command_error)?;
        request_by_path(
            &tree,
            requests,
            args.request_path.as_deref().unwrap_or_default(),
        )?
    };
    if request.is_provider_target() {
        // The provider's address and key exist only inside the AI run service.
        return Err(AppError::validation(format!(
            "\"{}\" is an AI request: it has to run in the Requests module. After the user runs it there, read the result with get_trace.",
            request.name
        )));
    }
    let context = execution_context_for(
        &store,
        workspace_id.as_str(),
        request.collection_id.as_ref().map(|id| id.as_str()),
        request.folder_id.as_ref().map(|id| id.as_str()),
        environment
            .as_ref()
            .map(|environment| environment.id.as_str()),
    )?;
    let untrusted = store
        .agent_touched_request_ids(workspace_id.as_str())
        .map_err(command_error)?;
    let secrets = workspace_secrets(&store, workspace_id.as_str())?;
    let strict = args.draft.is_some() || untrusted.contains(request.id.as_str());
    let mut effective = request.clone();
    effective.auth = effective_auth(&request, context.collection.as_ref(), &context.folders);
    // A send that can change the server is always on record; only a safe method may skip it.
    let save_to_history = args.save_to_history.unwrap_or(true) || !is_safe(request.method);
    let guarded = environment
        .as_ref()
        .is_some_and(|environment| environment.confirm_before_send);
    let view = match args.body.as_deref() {
        Some("shape") => BodyView::Shape,
        Some("none") => BodyView::None,
        _ if guarded => BodyView::Shape,
        _ => BodyView::Text,
    };
    Ok(Prepared {
        scope,
        environment,
        request,
        effective,
        context,
        strict,
        untrusted,
        secrets,
        save_to_history,
        view,
        path: args.path.clone(),
    })
}

/// Whether `method` cannot change the server (GET, HEAD, OPTIONS).
fn is_safe(method: HttpMethod) -> bool {
    matches!(
        method,
        HttpMethod::Get | HttpMethod::Head | HttpMethod::Options
    )
}

/// The secret variables the request references (inherited auth included).
fn secret_names(prepared: &Prepared, scopes: &VariableScopes) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in request_tokens(&prepared.effective) {
        if scopes.resolve_trace(&name).2 && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

fn declared(state: &AppState, prepared: &Prepared) -> Result<Vec<DeclaredHostDto>, AppError> {
    declared_hosts(
        state,
        &prepared.scope.workspace.id,
        prepared.environment.as_ref(),
        &prepared.untrusted,
    )
}

/// A secret goes only to a host the environment declares; refused in every mode.
fn check_secret_hosts(
    state: &AppState,
    prepared: &Prepared,
    scopes: &VariableScopes,
) -> Result<(), AppError> {
    match ensure_secret_hosts_declared(
        state,
        &prepared.effective,
        prepared.environment.as_ref(),
        scopes,
        &prepared.untrusted,
    ) {
        Ok(_) => Ok(()),
        Err(error @ AppError::Validation { .. }) => {
            let hosts: Vec<String> = declared(state, prepared)?
                .into_iter()
                .map(|entry| entry.host)
                .collect();
            // A stored OAuth token, a filled auth credential or a body file carries its own name;
            // only a secret variable needs the `{{name}}` wrapper.
            match secret_names(prepared, scopes).into_iter().next() {
                Some(name) => Err(AppError::validation(format!(
                    "Blocked: variable {{{{{name}}}}} may only be sent to {}.",
                    hosts.join(", ")
                ))),
                None => Err(AppError::validation(format!(
                    "Blocked: {}: {}.",
                    error.message(),
                    hosts.join(", ")
                ))),
            }
        }
        Err(other) => Err(other),
    }
}

/// The forced Ask of a send, in every mode (Full access included), with its code: a guarded
/// environment (`production`), a target host that belongs to another guarded environment and
/// not to this one (`guarded_host`), or a target host the environment does not declare
/// (`undeclared_host`). `host` is the resolved target.
fn forced_ask(
    state: &AppState,
    prepared: &Prepared,
    host: &str,
) -> Result<Option<(&'static str, String)>, AppError> {
    if let Some(environment) = prepared
        .environment
        .as_ref()
        .filter(|environment| environment.confirm_before_send)
    {
        return Ok(Some((
            "production",
            format!(
                "Asks because: environment \"{}\" is guarded (Production); every send needs your \
                 approval",
                environment.name
            ),
        )));
    }
    if let Some(guarded) = guarded_owner(state, prepared, host)? {
        return Ok(Some((
            "guarded_host",
            format!("Asks because: {host} belongs to the guarded environment \"{guarded}\""),
        )));
    }
    if !is_declared(&declared(state, prepared)?, host) {
        return Ok(Some((
            "undeclared_host",
            format!("Asks because: {host} is not a host this chat's environment declares"),
        )));
    }
    Ok(None)
}

/// The name of another guarded environment that declares `host`, when the chosen environment
/// does not declare it itself.
fn guarded_owner(
    state: &AppState,
    prepared: &Prepared,
    host: &str,
) -> Result<Option<String>, AppError> {
    let own = prepared
        .environment
        .as_ref()
        .map(environment_hosts)
        .unwrap_or_default();
    if host_listed(&own, host) {
        return Ok(None);
    }
    let environments = state
        .store()
        .list_environments(prepared.scope.workspace.id.as_str())
        .map_err(command_error)?;
    Ok(environments
        .into_iter()
        .find(|environment| {
            environment.confirm_before_send && host_listed(&environment_hosts(environment), host)
        })
        .map(|environment| environment.name))
}

/// `host` without its `:port`.
fn bare_host(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((bare, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => bare,
        _ => host,
    }
}

fn is_declared(hosts: &[DeclaredHostDto], target: &str) -> bool {
    hosts
        .iter()
        .any(|entry| entry.host == target || entry.host == bare_host(target))
}

fn private_blocked(host: &str) -> AppError {
    AppError::validation(format!(
        "Blocked: {host} is a private address PingDeck does not know. Ask the user to save a \
         request for it first."
    ))
}

fn link_local_blocked(host: &str) -> AppError {
    AppError::validation(format!(
        "Blocked: {host} is a link-local or cloud-metadata address, which PingDeck never contacts."
    ))
}

/// The host rules that need no DNS: the host an IP literal or `localhost` names.
/// Returns the target host, and whether DNS still has to classify it.
fn check_literal_host(
    state: &AppState,
    prepared: &Prepared,
    scopes: &VariableScopes,
) -> Result<(String, bool), AppError> {
    let resolved = resolve_template_lenient(&prepared.request.url, scopes);
    let host = host_of_url(&resolved).ok_or_else(|| {
        AppError::validation(format!(
            "Blocked: the target host of {resolved} cannot be determined (an undefined variable?)."
        ))
    })?;
    match net_guard::classify_host_literal(bare_host(&host)) {
        Some(HostClass::LinkLocalOrMetadata) => Err(link_local_blocked(&host)),
        Some(HostClass::Public) => Ok((host, false)),
        Some(_) => {
            if prepared.strict && !is_declared(&declared(state, prepared)?, &host) {
                return Err(private_blocked(&host));
            }
            Ok((host, false))
        }
        None => Ok((host, true)),
    }
}

/// The same rules for a host name: resolve it and classify what it points at.
async fn check_resolved_host(
    state: &AppState,
    prepared: &Prepared,
    host: &str,
) -> Result<(), AppError> {
    let bare = bare_host(host);
    let port = host
        .strip_prefix(bare)
        .and_then(|rest| rest.strip_prefix(':'))
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or(443);
    match net_guard::resolve_and_validate(bare, port, true).await {
        Ok((HostClass::Public, _)) => Ok(()),
        Ok(_) => {
            if prepared.strict && !is_declared(&declared(state, prepared)?, host) {
                Err(private_blocked(host))
            } else {
                Ok(())
            }
        }
        Err(AppError::Validation { .. }) => Err(link_local_blocked(host)),
        Err(other) => Err(other),
    }
}

static TOKEN: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\{\{([^{}]+)\}\}").ok());

/// `template` with variables filled in for the approval card; a secret shows as
/// `{{secret:name}}`, an undefined variable stays as written, and so does a generated built-in
/// (`{{$timestamp}}`, `{{$guid}}`): it takes a new value at every resolution, so the card and the
/// approved-target check name the token rather than a value the send will not use.
fn card_text(template: &str, scopes: &VariableScopes) -> String {
    let Some(token) = TOKEN.as_ref() else {
        return template.to_owned();
    };
    token
        .replace_all(template, |found: &regex::Captures<'_>| {
            let name = found[1].trim();
            match scopes.resolve_trace(name) {
                (_, _, true) => format!("{{{{secret:{name}}}}}"),
                (Some(_), VariableResolutionSource::BuiltIn, false) if name != "$lcurly" => {
                    found[0].to_owned()
                }
                (Some(value), _, false) => value,
                (None, _, false) => found[0].to_owned(),
            }
        })
        .into_owned()
}

/// `METHOD url?query` of the request with secrets as `{{secret:name}}`.
fn card_target(request: &Request, scopes: &VariableScopes) -> String {
    let mut url = card_text(&request.url, scopes);
    let mut separator = if url.contains('?') { '&' } else { '?' };
    for row in request.query_params.iter().filter(|row| row.enabled) {
        url.push(separator);
        separator = '&';
        url.push_str(&card_text(&row.key, scopes));
        url.push('=');
        url.push_str(&card_text(&row.value, scopes));
    }
    url
}

/// The target a card shows and a call is held to: method, resolved URL and query, environment.
fn approved_target(prepared: &Prepared, scopes: &VariableScopes) -> String {
    format!(
        "Send {} {} in {} (workspace {})",
        method_name(prepared.request.method),
        card_target(&prepared.request, scopes),
        prepared
            .environment
            .as_ref()
            .map_or("no environment", |environment| environment.name.as_str()),
        prepared.scope.workspace.name,
    )
}

/// What the approval card says: the target, the secrets and credentials it carries (inherited
/// auth included) with the hosts they may reach, and what else the send does that the target
/// does not show.
fn plain_words(
    state: &AppState,
    prepared: &Prepared,
    scopes: &VariableScopes,
) -> Result<String, AppError> {
    let mut text = approved_target(prepared, scopes);
    let secrets = secret_names(prepared, scopes);
    // A stored OAuth token, a filled auth field or a body file, possibly inherited from the
    // collection or a folder: named like the host rules name it.
    let credential = if secrets.is_empty() {
        carried_secret(&prepared.effective, scopes)
    } else {
        None
    };
    if !secrets.is_empty() || credential.is_some() {
        let hosts: Vec<String> = declared(state, prepared)?
            .into_iter()
            .map(|entry| entry.host)
            .collect();
        let shown = hosts.iter().take(CARD_HOSTS).cloned().collect::<Vec<_>>();
        let what = match credential {
            Some(credential) => {
                let inherited =
                    matches!(prepared.request.auth, crate::workspace::AuthConfig::Inherit)
                        && !matches!(
                            prepared.effective.auth,
                            crate::workspace::AuthConfig::Inherit
                        );
                format!(
                    "carries {}{}",
                    credential.to_lowercase(),
                    if inherited { " (inherited)" } else { "" }
                )
            }
            None => format!(
                "uses secrets {}",
                secrets
                    .iter()
                    .map(|name| format!("{{{{secret:{name}}}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        text.push_str(&format!(
            "; {what}, which may reach only: {}{}",
            shown.join(", "),
            if hosts.len() > CARD_HOSTS {
                ", and more"
            } else {
                ""
            }
        ));
    }
    let guarded = prepared
        .environment
        .as_ref()
        .is_some_and(|environment| environment.confirm_before_send);
    if guarded && runs_pre_request_scripts(&prepared.context, &prepared.request) {
        text.push_str("; its pre-request scripts run with this approval and may send requests too");
    }
    if !prepared.save_to_history {
        text.push_str("; not recorded in History");
    }
    if prepared.request.network_options.send_cookies {
        text.push_str("; uses this run's own cookies, not your cookie jar");
    }
    Ok(text)
}

/// Field names and value types of `value`: `{"id": "number", "items": ["object…"]}`.
/// An array shows its first item's shape; nesting below [`SHAPE_DEPTH`] is named by type only.
fn shape_of(value: &Value, depth: usize) -> Value {
    match value {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("boolean"),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
        Value::Array(_) if depth >= SHAPE_DEPTH => json!("array"),
        Value::Object(_) if depth >= SHAPE_DEPTH => json!("object"),
        Value::Array(items) => Value::Array(
            items
                .first()
                .map(|first| shape_of(first, depth + 1))
                .into_iter()
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, field)| (name.clone(), shape_of(field, depth + 1)))
                .collect(),
        ),
    }
}

/// The body part of the result: the text, its shape or only its size, of the whole body or of
/// the JSON subtree at `path`.
fn body_json(prepared: &Prepared, result: &ExecutionResult, secrets: &[NamedSecret]) -> Value {
    let content_type = result
        .headers
        .iter()
        .find(|header| header.key.eq_ignore_ascii_case("content-type"))
        .map(|header| header.value.clone());
    if result.body_base64.is_some() {
        return json!({"binary": true, "content_type": content_type, "size_bytes": result.body_size_bytes});
    }
    let size = result.body_size_bytes;
    let parsed = serde_json::from_str::<Value>(&result.body_text).ok();
    let selected = match (&prepared.path, &parsed) {
        (None, _) => parsed.clone(),
        (Some(path), Some(root)) => match json_path_value(root, path) {
            Ok(found) => Some(found.map_or(Value::Null, std::borrow::Cow::into_owned)),
            Err(error) => {
                return json!({"view": prepared.view.as_str(), "size_bytes": size, "path_error": error});
            }
        },
        (Some(_), None) => {
            return json!({"view": prepared.view.as_str(), "size_bytes": size, "path_error": "the body is not JSON"});
        }
    };
    match prepared.view {
        BodyView::None => json!({"view": "none", "size_bytes": size, "content_type": content_type}),
        BodyView::Shape => {
            let mut body = json!({
                "view": "shape",
                "size_bytes": size,
                "content_type": content_type,
                "shape": selected.as_ref().map(|value| shape_of(value, 0)),
            });
            if result.status_code >= 400 {
                body["error_text"] = json!(redact_for_model(
                    &sanitize_untrusted(&result.body_text),
                    secrets,
                    ERROR_TEXT_CAP
                ));
            }
            body
        }
        BodyView::Text => {
            let text = match (&prepared.path, selected) {
                (Some(_), Some(value)) => value.to_string(),
                _ => result.body_text.clone(),
            };
            json!({
                "text": redact_for_model(&sanitize_untrusted(&text), secrets, BODY_CAP),
                "size_bytes": size,
                "truncated": result.body_truncated || text.len() > BODY_CAP,
            })
        }
    }
}

/// The result the model sees for a finished send.
fn result_json(
    prepared: &Prepared,
    result: &ExecutionResult,
    history_id: Option<&str>,
    secrets: &[NamedSecret],
) -> Value {
    let headers: Vec<Value> = result
        .headers
        .iter()
        .take(MAX_HEADERS)
        .map(|header| {
            json!({"name": header.key, "value": sanitize_untrusted(&header_value_for_model(&header.key, &header.value))})
        })
        .collect();
    // With only the shape, what the checks saw stays out too: no messages, no actual values.
    let values = prepared.view == BodyView::Text;
    let assertions: Vec<Value> = result
        .assertion_results
        .iter()
        .map(|check| {
            if values {
                serde_json::to_value(check).unwrap_or_default()
            } else {
                json!({"label": check.label, "passed": check.passed})
            }
        })
        .collect();
    let script_tests: Vec<Value> = result
        .script_tests
        .iter()
        .map(|test| {
            let mut row =
                json!({"name": test.name, "passed": test.passed, "skipped": test.skipped});
            if values && let Some(error) = &test.error {
                row["error"] = json!(sanitize_untrusted(error));
            }
            row
        })
        .collect();
    let script_error = result.script_error.as_ref().map(|error| {
        if values {
            json!({"source": error.source, "message": sanitize_untrusted(&error.message)})
        } else {
            json!({"source": error.source})
        }
    });
    json!({
        "status": result.status_code,
        "status_text": result.status_text,
        "method": method_name(prepared.request.method),
        "url": result.url,
        "elapsed_ms": u64::try_from(result.elapsed_ms).unwrap_or(u64::MAX),
        "attempts": result.attempt_count,
        "headers": headers,
        "headers_total": result.headers.len(),
        "body_view": prepared.view.as_str(),
        "body": body_json(prepared, result, secrets),
        "assertions": assertions,
        "script_tests": script_tests,
        "script_error": script_error,
        "environment": prepared.environment.as_ref().map(|environment| environment.name.clone()),
        "workspace": prepared.scope.tag(),
        "history_id": history_id,
        "view": {
            "kind": "request",
            "history_id": history_id,
            "workspace_id": prepared.scope.workspace.id.as_str(),
        },
    })
}

async fn send(
    tool: &SendRequestTool,
    ctx: &ToolContext<'_>,
    mut prepared: Prepared,
    carried: &SentCredentials,
    approval_key: &str,
) -> Result<ToolOutput, AppError> {
    let scopes = prepared.context.scopes();
    // The card the user approved named a target; a call that would now go elsewhere (a variable
    // or the saved request changed meanwhile) sends nothing.
    if let Some(approved) = lock(&tool.approved).remove(approval_key)
        && approved != approved_target(&prepared, &scopes)
    {
        return Err(AppError::validation(format!(
            "The request changed after it was approved; nothing was sent. It is now: {}. Call \
             send_request again to ask for approval.",
            approved_target(&prepared, &scopes)
        )));
    }
    check_secret_hosts(crate::agent_tools::app_state(ctx), &prepared, &scopes)?;
    let (host, needs_dns) =
        check_literal_host(crate::agent_tools::app_state(ctx), &prepared, &scopes)?;
    if needs_dns {
        check_resolved_host(crate::agent_tools::app_state(ctx), &prepared, &host).await?;
    }
    // A host of another guarded environment gets the guarded view too.
    if prepared.view == BodyView::Text
        && guarded_owner(crate::agent_tools::app_state(ctx), &prepared, &host)?.is_some()
    {
        prepared.view = BodyView::Shape;
    }
    if !tool.limit.take(ctx.run_id) {
        return Err(AppError::validation(format!(
            "At most {} sends per turn.",
            tool.limit.limit()
        )));
    }
    let origin = HistoryOrigin {
        source: HistorySource::Agent,
        source_name: "Agent chat".to_owned(),
        source_id: ctx.session_id.to_owned(),
        run_id: Some(ctx.run_id.to_owned()),
    };
    // History but never the workspace jar: the run's own jar goes out instead.
    let persistence = if prepared.save_to_history {
        Persistence::HistoryOnly(origin)
    } else {
        Persistence::Unrecorded(origin)
    };
    let mut context = prepared.context.clone();
    context.cookie_jar = lock(&tool.jars)
        .get(ctx.run_id)
        .cloned()
        .unwrap_or_default();
    // A guarded environment always asks (`forced_ask`), so a call that runs there was approved:
    // the approval answers the production guard for the main request and for what its
    // pre-request scripts send, as Send's confirmation does.
    let confirmed = prepared
        .environment
        .as_ref()
        .is_some_and(|environment| environment.confirm_before_send)
        .then_some(true);
    let sent = tokio::time::timeout(
        Duration::from_secs(SEND_TIMEOUT_SECS),
        execute_request_pipeline_traced(
            crate::agent_tools::app_state(ctx),
            prepared.request.clone(),
            context,
            ExecutionMeta::confirmed(confirmed).reporting_credentials(carried.clone()),
            Some(ctx.cancel),
            persistence,
        ),
    )
    .await;
    let (result, history_id) = match sent {
        Ok(sent) => sent?,
        Err(_) => return Ok(timed_out(SEND_TIMEOUT_SECS)),
    };
    if prepared.request.network_options.send_cookies {
        tool.keep_cookies(
            ctx.run_id,
            response_cookie_entries(&prepared.request, &result),
        );
    }
    let secrets = with_sent_credentials(&prepared.secrets, carried);
    let value = result_json(&prepared, &result, history_id.as_deref(), &secrets);
    // The response is the server's text: external data for the model.
    Ok(json_output(&value, &secrets, RESULT_CAP).external())
}

/// The call a card was shown for: its id and its exact arguments.
fn approval_key(call_id: &str, arguments: &Value) -> String {
    format!("{call_id}\n{arguments}")
}

/// `secrets` plus every credential value the send carried, so a server that echoes one back
/// shows the model a placeholder.
fn with_sent_credentials(secrets: &[NamedSecret], carried: &SentCredentials) -> Vec<NamedSecret> {
    let mut all = secrets.to_vec();
    all.extend(carried.values().into_iter().map(|value| NamedSecret {
        name: SENT_CREDENTIAL.to_owned(),
        value,
    }));
    all
}

impl SendRequestTool {
    /// Adds the cookies a response set to the run's jar, each replacing the cookie with the same
    /// name, domain and path.
    fn keep_cookies(&self, run_id: &str, cookies: Vec<CookieJarEntry>) {
        if cookies.is_empty() {
            return;
        }
        let mut jars = lock(&self.jars);
        if jars.len() >= RUNS_KEPT && !jars.contains_key(run_id) {
            jars.clear();
        }
        let jar = jars.entry(run_id.to_owned()).or_default();
        for cookie in cookies {
            jar.retain(|old| {
                !(old.name == cookie.name
                    && old.path == cookie.path
                    && old.domain.eq_ignore_ascii_case(&cookie.domain))
            });
            jar.push(cookie);
        }
        let excess = jar.len().saturating_sub(RUN_JAR_COOKIES);
        jar.drain(..excess);
    }

    /// Remembers the target the card of the call under `key` showed.
    fn remember_target(&self, key: String, target: String) {
        let mut approved = lock(&self.approved);
        if approved.len() >= RUNS_KEPT && !approved.contains_key(&key) {
            approved.clear();
        }
        approved.insert(key, target);
    }
}

impl AgentTool for SendRequestTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args: SendArgs = parse_args(&self.spec.input_schema, arguments)?;
        let prepared = prepare(crate::agent_tools::app_state(ctx), ctx.session_id, &args)?;
        let scopes = prepared.context.scopes();
        // The host rules (link-local, private and loopback hosts, secret hosts) end the call as `blocked`, with no card.
        let host = match check_secret_hosts(crate::agent_tools::app_state(ctx), &prepared, &scopes)
            .and_then(|()| {
                check_literal_host(crate::agent_tools::app_state(ctx), &prepared, &scopes)
            }) {
            Ok((host, _)) => host,
            Err(AppError::Validation { message, .. }) => {
                return Ok(CallInspection {
                    hard_block: Some(message),
                    ..CallInspection::default()
                });
            }
            Err(other) => return Err(other),
        };
        let method = prepared.request.method;
        self.remember_target(
            approval_key(ctx.call_id, arguments),
            approved_target(&prepared, &scopes),
        );
        let forced = forced_ask(crate::agent_tools::app_state(ctx), &prepared, &host)?;
        let (ask_code, always_ask) = match forced {
            Some((code, reason)) => (Some(code), Some(reason)),
            None => (None, None),
        };
        Ok(CallInspection {
            read_only: false,
            destructive: method == HttpMethod::Delete,
            preview: Some(ToolApprovalPreview::Summary {
                text: plain_words(crate::agent_tools::app_state(ctx), &prepared, &scopes)?,
            }),
            ask_code,
            always_ask,
            arg_subject: Some(format!("{} {host}", method_name(method))),
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args: SendArgs = match parse_args(&self.spec.input_schema, &arguments) {
                Ok(args) => args,
                Err(error) => return error_output(&error, &[]),
            };
            let prepared = match prepare(crate::agent_tools::app_state(ctx), ctx.session_id, &args)
            {
                Ok(prepared) => prepared,
                Err(error) => return error_output(&error, &[]),
            };
            let carried = SentCredentials::default();
            let secrets = prepared.secrets.clone();
            let key = approval_key(ctx.call_id, &arguments);
            match send(self, ctx, prepared, &carried, &key).await {
                Ok(output) => output,
                Err(error) => error_output(&error, &with_sent_credentials(&secrets, &carried)),
            }
        })
    }
}
