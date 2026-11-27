//! The tools' config files: detect, inspect, preview, connect, apply, restore, the ledger list,
//! "Rotate and update N files" and "Update address" (the gateway's address written again with the
//! token the file already holds, see [`llm_client_rewrite_address_impl`]).
//!
//! Single responsibility: everything that puts a gateway client's token into a tool's config.
//! For these four file writers the core mints (or rotates) the token, builds the plan with the
//! real token, writes the files atomically, records the ledger and answers with a masked hint;
//! **no DTO of this module ever holds a token**. A rotation is all or nothing (see
//! `client_rotation`): the files are written before the store swaps the token, so a failure
//! leaves the old token valid and the files as they were; a Connect that creates a client
//! deletes it again when the write fails. Previews and plans carry only masked tokens. Verify
//! (the first-request watcher and the loopback test request) is `client_verify`. The first
//! Connect of this data folder turns `ai.gateway.enabled_on_start` on, so the address the tools
//! were given answers after SpanBay restarts.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::client_rotation::{
    Applier, apps_of, commit, drop_stale, restore_all, rewrite_others, rotated_files, system_apply,
    tidy_kept, with_leftovers,
};
use super::overview::token_hint;
use super::{current_time_ms, with_connection};
use crate::app::AppState;
use crate::client_config::{
    self, AppliedFile, ApplyResult, ClientApp, ClientConfigPlan, ClientConfigRequest, ClientStatus,
    Clock, DetectEnv, DetectedClient, DetectedSnippetClient, InspectContext, Inspection, KeptCopy,
    ModelFacts, NO_REQUEST_AFTER_MS, RestoreEnv, RestoreOutcome, StatusFacts,
};
use crate::error::AppError;
use crate::llm::capabilities::Capability;
use crate::llm::store::{self, ClientInput, GatewayClient, LedgerEntry, ModelGrant};

// ------------------------------------------------------------------ DTOs

/// What a preview or an apply is for: one tool, optionally for an existing client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPlanInput {
    /// The tool to configure.
    pub app: ClientApp,
    /// The existing client whose token the file would hold (its masked hint is shown); `None`
    /// previews a client that does not exist yet (`pd_cc_••••••••`).
    #[serde(default)]
    pub client_id: Option<String>,
    /// Gateway model id (alias or `provider/model`) the tool uses; `None` leaves its model alone.
    #[serde(default)]
    pub alias: Option<String>,
    /// Claude Code only: the alias for background tasks; `None` uses `alias`.
    #[serde(default)]
    pub fast_alias: Option<String>,
}

/// What Connect does when a client of that name exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnExisting {
    /// Rotate its token and write it (the default).
    #[default]
    Rotate,
    /// Create "name 2" (or the next free number).
    SecondClient,
    /// Refuse.
    Fail,
    /// The record of that name is revoked: delete it and create a new client with the name;
    /// an active record is refused.
    ReplaceRevoked,
}

/// The final "Connect" button of the dialog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientConnectInput {
    /// The tool to configure.
    pub app: ClientApp,
    /// Gateway model id the tool uses; the client is limited to it (and to `fast_alias`). `None`:
    /// the tool's model stays and the client may use every alias.
    #[serde(default)]
    pub alias: Option<String>,
    /// Claude Code only: the alias for background tasks.
    #[serde(default)]
    pub fast_alias: Option<String>,
    /// The client's name; the tool's name when absent or blank.
    #[serde(default)]
    pub client_name: Option<String>,
    /// What to do when a client of that name exists.
    #[serde(default)]
    pub on_existing: OnExisting,
    /// The plan the user reviewed ([`llm_client_preview_impl`]); the write is refused when a
    /// file changed since.
    #[serde(default)]
    pub reviewed: Option<ClientConfigPlan>,
    /// Join the model(s) the file will name into the client's allow-list; absent
    /// means yes.
    #[serde(default)]
    pub extend_allowlist: Option<bool>,
    /// Serve bare model names the gateway does not know with the client's default model; absent
    /// keeps the client's setting (off for a new client).
    #[serde(default)]
    pub unknown_model_to_default: Option<bool>,
}

/// Rewrites an existing client's token into the files of the tools it was connected to earlier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotatedFiles {
    /// The files that now hold the new token (every file of the tools connected earlier that
    /// still held the old one: a rotation that cannot update all of them fails and changes
    /// nothing).
    pub updated: Vec<AppliedFile>,
    /// Files the ledger named that no longer held the old token; their rows were dropped.
    pub skipped: Vec<SkippedFile>,
}

/// A file the ledger named that a rotation skipped because it no longer holds the old token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedFile {
    /// The file.
    pub path: String,
    /// Why (gone, holds another value, no longer managed).
    pub reason: String,
}

/// The outcome of Connect. Holds no token: only the client's masked hint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientConnectResult {
    /// The client the token belongs to.
    pub client: GatewayClient,
    /// `pd_cc_••••9d2f`.
    pub token_hint: String,
    /// An existing client's token was rotated (a new client otherwise).
    pub rotated: bool,
    /// The files written, their backups and hashes, and what to tell the user next.
    pub applied: ApplyResult,
    /// For a rotated client: the files of the tools connected before, re-tokened too.
    pub other_files: RotatedFiles,
}

/// The outcome of "Rotate and update N files". Holds no token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RotateUpdateResult {
    /// The client with its new token's hint fields.
    pub client: GatewayClient,
    /// `pd_cc_••••9d2f`.
    pub token_hint: String,
    /// Which files hold the new token and which kept the dead one.
    pub files: RotatedFiles,
}

/// One tool's drift check with the client it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientInspectDto {
    /// What was found in the tool's config (address, alias, drift); never the token.
    pub inspection: Inspection,
    /// configured / drifted / not configured / waiting for first request.
    pub status: ClientStatus,
    /// The active client whose token the file holds (matched by hash).
    pub client_id: Option<String>,
    /// That client's name.
    pub client_name: Option<String>,
    /// When SpanBay last wrote the tool's config for that client (Unix ms).
    pub written_at_ms: Option<i64>,
}

/// One restored file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoredFile {
    /// The file.
    pub path: String,
    /// Restored, deleted, or needs the user's confirmation (with the diff).
    pub outcome: RestoreOutcome,
}

/// The outcome of "Restore previous config" for one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreResult {
    /// The tool.
    pub app: ClientApp,
    /// One entry per file of the tool's latest write; empty when SpanBay never wrote the tool.
    pub files: Vec<RestoredFile>,
}

// ------------------------------------------------------------------ helpers

/// The loopback address the writers write: the bound port while running, else the configured one.
pub(super) fn gateway_url(state: &AppState) -> String {
    let status = state.llm.gateway.status(&state.llm);
    let port = if status.running {
        status.port
    } else {
        state.llm.settings().ai.gateway.port
    };
    format!("http://127.0.0.1:{port}")
}

/// Where backups go: the data folder; an in-memory state (tests) puts them under the home.
pub(super) fn backup_root(state: &AppState, home: &Path) -> std::path::PathBuf {
    state
        .data_dirs()
        .map_or_else(|| home.join(".spanbay"), |dirs| dirs.root.clone())
}

fn blank_to_none(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// A token-shaped stand-in that masks exactly like the client's real token (`pd_cc_••••9d2f`);
/// a legacy client without a last four masks as a pending token.
fn masked_standin(client: &GatewayClient) -> String {
    if client.token_last4.is_empty() {
        return client_config::pending_token(&store::token_tag(&client.name));
    }
    format!(
        "{}{}{}",
        client.token_prefix,
        "x".repeat(28),
        client.token_last4
    )
}

fn active_hashes(state: &AppState) -> Result<Vec<store::ClientHashEntry>, AppError> {
    with_connection(state, store::client_hashes_snapshot)
}

// ------------------------------------------------------------------ detect, inspect

/// Which tools look installed on this machine: the four file-writer tools and the
/// snippet-only clients (Cursor, Zed, Cherry Studio).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientDetection {
    /// The tools whose config SpanBay can write.
    pub clients: Vec<DetectedClient>,
    /// The tools configured by hand from snippets (the generic environment is not an app).
    pub snippet_clients: Vec<DetectedSnippetClient>,
}

/// Which supported tools are installed on this machine.
///
/// # Errors
/// [`AppError::Internal`] when the home directory is unknown.
pub fn llm_client_detect_impl() -> Result<ClientDetection, AppError> {
    Ok(llm_client_detect_in(&DetectEnv::system(
        &client_config::home_dir()?,
    )))
}

/// [`llm_client_detect_impl`] for explicit detection roots.
#[must_use]
pub(crate) fn llm_client_detect_in(env: &DetectEnv) -> ClientDetection {
    ClientDetection {
        clients: client_config::detect_all(env),
        snippet_clients: client_config::detect_snippet_clients(env),
    }
}

/// Reads a tool's config back (local files only, never the network) and says whether it still
/// points at this gateway with a token and alias that are valid: configured, drifted,
/// not configured, or waiting for the first request. The token found is compared by hash with
/// the active clients and never leaves the core.
///
/// # Errors
/// [`AppError::Validation`] for a config file that cannot be parsed, [`AppError::Io`] for one
/// that cannot be read.
pub fn llm_client_inspect_impl(
    state: &AppState,
    app: ClientApp,
) -> Result<ClientInspectDto, AppError> {
    llm_client_inspect_in(state, app, &DetectEnv::system(&client_config::home_dir()?))
}

/// [`llm_client_inspect_impl`] for explicit detection roots.
pub(crate) fn llm_client_inspect_in(
    state: &AppState,
    app: ClientApp,
    env: &DetectEnv,
) -> Result<ClientInspectDto, AppError> {
    let entries = active_hashes(state)?;
    let snapshot = state.llm.snapshot_blocking()?;
    let matched: std::cell::RefCell<Option<GatewayClient>> = std::cell::RefCell::new(None);
    let token_is_active = |hash: &str| {
        entries
            .iter()
            .find(|entry| entry.client.is_active() && entry.token_sha256 == hash)
            .is_some_and(|entry| {
                *matched.borrow_mut() = Some(entry.client.clone());
                true
            })
    };
    // A legacy alias name still routes, but shows as drift so the file gets written again.
    let alias_exists = |name: &str| name.contains('/') && snapshot.resolve_ref(name).is_some();
    let url = gateway_url(state);
    let inspection = client_config::inspect(
        env,
        app,
        &InspectContext {
            gateway_url: &url,
            token_is_active: &token_is_active,
            alias_exists: &alias_exists,
        },
    )?;
    let client = matched.into_inner();
    super::routing::prune_legacy_names(state, &env.home)?;
    let mut facts = StatusFacts {
        model_allowed: true,
        ..StatusFacts::default()
    };
    let mut written_at_ms = None;
    if let Some(client) = &client {
        let progress = client_progress(state, client, app)?;
        written_at_ms = progress.written_at;
        facts.first_status = progress.first_status;
        facts.verified_at_ms = progress.verified_at;
        facts.waited_out = progress.first_status.is_none()
            && progress.written_at.is_some_and(|written| {
                current_time_ms().saturating_sub(written) >= NO_REQUEST_AFTER_MS
            });
        // Dry run: can this client use every model the file names?
        facts.model_allowed = client_config::config_models(env, app)?
            .iter()
            .all(|model| snapshot.allowed_by(model, &|name: &str| client.allows(name)));
        // Only Gemini CLI's `.env` can be shadowed by a project folder's; the others skip the
        // read of the tools' project lists.
        let projects = if app == ClientApp::GeminiCli {
            client_config::recent_project_dirs(&env.home)
        } else {
            Vec::new()
        };
        facts.override_source = client_config::override_source(
            &env.home,
            app,
            &|name| std::env::var(name).ok(),
            &projects,
        );
    }
    Ok(ClientInspectDto {
        status: client_config::client_status(&inspection.drift, &facts),
        client_id: client.as_ref().map(|client| client.id.clone()),
        client_name: client.map(|client| client.name),
        written_at_ms,
        inspection,
    })
}

/// What [`client_progress`] found.
struct Progress {
    written_at: Option<i64>,
    first_status: Option<u16>,
    verified_at: Option<i64>,
}

/// What came of the tool's config since SpanBay last wrote it: the status of the first answered
/// request of `client` (the in-memory watcher; across restarts a `last_used` at or after the
/// latest ledger write of the tool counts as a success) and when a request last passed with the
/// client's token, when that is after the write (a file written again must be verified
/// again).
fn client_progress(
    state: &AppState,
    client: &GatewayClient,
    app: ClientApp,
) -> Result<Progress, AppError> {
    let (written_at, verified_at) = with_connection(state, |connection| {
        Ok((
            store::latest_applied(connection, &client.id)?
                .into_iter()
                .filter(|entry| entry.app == app)
                .map(|entry| entry.applied_at_ms)
                .max(),
            store::client_verified_at(connection, &client.id)?,
        ))
    })?;
    let used_since =
        matches!((written_at, client.last_used_ms), (Some(written), Some(used)) if used >= written);
    let first_status = state
        .llm
        .services
        .client_watch
        .first_requests
        .seen(&client.id)
        .map(|seen| seen.status)
        .or(used_since.then_some(200));
    let verified = verified_at.filter(|at| written_at.is_none_or(|written| *at >= written));
    Ok(Progress {
        written_at,
        first_status,
        verified_at: verified,
    })
}

// ------------------------------------------------------------------ preview

/// What writing the tool's config would change, per file, without writing anything:
/// the diff with the token masked (`pd_cc_••••••••` for a client that does not exist yet, the
/// client's hint form otherwise) and each file's `base_sha256`, which the write later checks.
///
/// # Errors
/// [`AppError::Validation`] for unusable existing config, [`AppError::NotFound`] for an unknown
/// `client_id`.
pub fn llm_client_preview_impl(
    state: &AppState,
    input: ClientPlanInput,
) -> Result<ClientConfigPlan, AppError> {
    llm_client_preview_in(state, input, &client_config::home_dir()?)
}

/// [`llm_client_preview_impl`] for an explicit home directory.
pub(crate) fn llm_client_preview_in(
    state: &AppState,
    input: ClientPlanInput,
    home: &Path,
) -> Result<ClientConfigPlan, AppError> {
    let (name, token) = match &input.client_id {
        Some(id) => {
            let client = with_connection(state, |connection| store::get_client(connection, id))?;
            (client.name.clone(), masked_standin(&client))
        }
        None => {
            let name = input.app.display_name().to_owned();
            let token = client_config::pending_token(&store::token_tag(&name));
            (name, token)
        }
    };
    let alias = blank_to_none(input.alias);
    let facts = match &alias {
        Some(alias) => resolve_model(state, Some(alias.clone()))
            .ok()
            .flatten()
            .map(|model| model.facts)
            .unwrap_or_default(),
        None => ModelFacts::default(),
    };
    client_config::preview(
        home,
        &ClientConfigRequest {
            app: input.app,
            client_name: name,
            gateway_url: gateway_url(state),
            token,
            alias,
            fast_alias: blank_to_none(input.fast_alias),
            facts,
        },
    )
}

// ------------------------------------------------------------------ connect, apply

/// Who the token belongs to.
enum Subject {
    /// A client created for this operation (deleted again when the write fails); `true` replaces
    /// a revoked record of the same name.
    New(ClientInput, bool),
    /// An existing client whose token is rotated.
    Existing(String),
}

/// A model a tool's config names, resolved.
struct ResolvedModel {
    /// Name form (`<provider name>/<model>`), what the file holds.
    name: String,
    /// Id form, what the client's allowlist holds.
    id: String,
    /// What the gateway knows about it, for the tools' capability hints.
    facts: ModelFacts,
}

/// The model a tool's config gets: `requested` (a model reference in name or id form, or a
/// legacy alias name) resolved, with what the gateway knows about it.
///
/// # Errors
/// [`AppError::Validation`] when it names no model of this gateway.
fn resolve_model(
    state: &AppState,
    requested: Option<String>,
) -> Result<Option<ResolvedModel>, AppError> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    let snapshot = state.llm.snapshot_blocking()?;
    let model = snapshot.resolve_ref(&requested).ok_or_else(|| {
        AppError::validation(format!(
            "`{requested}` is not a provider/model of this gateway"
        ))
    })?;
    let stored = snapshot
        .models
        .iter()
        .find(|stored| stored.provider_id == model.provider_id && stored.model_id == model.model);
    let facts = stored.map_or_else(ModelFacts::default, |stored| ModelFacts {
        context_window: stored.context,
        max_output_tokens: stored.max_output,
        reasoning: stored.capabilities.known(Capability::Reasoning),
        tool_call: stored.capabilities.known(Capability::Tools),
        attachment: stored.capabilities.known(Capability::Vision),
    });
    Ok(Some(ResolvedModel {
        name: snapshot.display_ref(&model.provider_id, &model.model),
        id: model.id_form(),
        facts,
    }))
}

/// The first of `name`, `name 2`, `name 3`, … that no client has (case-insensitively).
fn second_client_name(clients: &[GatewayClient], name: &str) -> String {
    let taken = |candidate: &str| {
        clients
            .iter()
            .any(|client| client.name.eq_ignore_ascii_case(candidate))
    };
    (2..)
        .map(|number| format!("{name} {number}"))
        .find(|candidate| !taken(candidate))
        .unwrap_or_else(|| name.to_owned())
}

/// Deletes the client created for a Connect that did not complete (revoke first: the store only
/// deletes revoked clients).
fn discard_client(state: &AppState, client: &GatewayClient) {
    let _ = with_connection(state, |connection| {
        store::revoke_client(connection, &client.id, current_time_ms())?;
        store::delete_client(connection, &client.id)
    });
    state
        .llm
        .services
        .client_watch
        .first_requests
        .disarm(&client.id);
    super::clients::clients_changed(state);
}

/// Connect for a client created in this operation: writes the files with its token and records
/// the ledger; when either fails the files are restored and the client is deleted again.
// The dialog's inputs plus the writer; a struct would only rename them.
#[allow(clippy::too_many_arguments)]
fn connect_new(
    state: &AppState,
    home: &Path,
    input: &ClientInput,
    replace_revoked: bool,
    grant: &ModelGrant,
    mut request: ClientConfigRequest,
    reviewed: Option<&ClientConfigPlan>,
    apply: Applier<'_>,
) -> Result<ClientConnectResult, AppError> {
    let created = with_connection(state, |connection| {
        let created = if replace_revoked {
            store::create_client_replacing_revoked(connection, input, current_time_ms())?
        } else {
            store::create_client(connection, input, current_time_ms())?
        };
        let client = store::grant_model(connection, &created.client.id, grant)?;
        Ok(store::ClientWithToken { client, ..created })
    })?;
    state.llm.touch();
    request.token = created.token;
    request.client_name.clone_from(&created.client.name);
    let applied = match apply(
        home,
        &backup_root(state, home),
        &request,
        reviewed,
        Clock::system(),
    ) {
        Ok(applied) => applied,
        Err(error) => {
            discard_client(state, &created.client);
            return Err(error);
        }
    };
    if !applied.files.is_empty()
        && let Err(error) = with_connection(state, |connection| {
            store::record_applied(
                connection,
                &created.client.id,
                request.app,
                &applied.files,
                current_time_ms(),
            )
            .map(|_| ())
        })
    {
        let left = restore_all(state, home, &vec![(request.app, applied.files.clone())]);
        discard_client(state, &created.client);
        return Err(with_leftovers(error, &left));
    }
    tidy_kept(state, home, &[request.app]);
    state
        .llm
        .services
        .client_watch
        .first_requests
        .arm(&created.client.id);
    super::clients::clients_changed(state);
    Ok(ClientConnectResult {
        token_hint: token_hint(&created.client),
        client: created.client,
        rotated: false,
        applied,
        other_files: RotatedFiles {
            updated: Vec::new(),
            skipped: Vec::new(),
        },
    })
}

/// Connect for an existing client: the token is minted but not stored; the files of the tools
/// connected earlier and this tool's files are written with it; only then the store swaps the
/// hash and records the ledger. Any failure restores the files and leaves the old token valid
/// (see `client_rotation`).
fn connect_existing(
    state: &AppState,
    home: &Path,
    client_id: &str,
    grant: &ModelGrant,
    mut request: ClientConfigRequest,
    reviewed: Option<&ClientConfigPlan>,
    apply: Applier<'_>,
) -> Result<ClientConnectResult, AppError> {
    let entry = active_hashes(state)?
        .into_iter()
        .find(|entry| entry.client.id == client_id)
        .ok_or_else(|| AppError::not_found(format!("gateway client {client_id} not found")))?;
    if !entry.client.is_active() {
        return Err(AppError::validation("A revoked client cannot be rotated"));
    }
    let (client, old) = (entry.client, entry.token_sha256);
    let minted = store::mint_token(&client.name)?;
    request.token.clone_from(&minted.token);
    request.client_name.clone_from(&client.name);
    let mut done = rewrite_others(
        state,
        home,
        client_id,
        &old,
        &minted.token,
        Some(request.app),
    )?;
    let other_files = rotated_files(&done);
    let applied = match apply(
        home,
        &backup_root(state, home),
        &request,
        reviewed,
        Clock::system(),
    ) {
        Ok(applied) => applied,
        Err(error) => {
            let left = restore_all(state, home, &done.written);
            return Err(with_leftovers(error, &left));
        }
    };
    if !applied.files.is_empty() {
        done.written.push((request.app, applied.files.clone()));
    }
    let client = commit(state, home, client_id, &old, &minted, &done.written, grant)?;
    drop_stale(state, &done.stale_ids);
    tidy_kept(state, home, &apps_of(&done.written));
    state
        .llm
        .services
        .client_watch
        .first_requests
        .arm(client_id);
    super::clients::clients_changed(state);
    Ok(ClientConnectResult {
        token_hint: token_hint(&client),
        client,
        rotated: true,
        applied,
        other_files,
    })
}

/// The allow-list choices of the dialog.
#[derive(Clone, Copy)]
struct GrantChoices {
    extend_allowlist: Option<bool>,
    unknown_model_to_default: Option<bool>,
}

// One argument per input of the dialog plus the writer; a struct would only rename them.
#[allow(clippy::too_many_arguments)]
fn connect_core(
    state: &AppState,
    home: &Path,
    subject: Subject,
    app: ClientApp,
    alias: Option<String>,
    fast_alias: Option<String>,
    choices: GrantChoices,
    reviewed: Option<&ClientConfigPlan>,
    apply: Applier<'_>,
) -> Result<ClientConnectResult, AppError> {
    let alias = resolve_model(state, blank_to_none(alias))?;
    let fast_alias = resolve_model(state, blank_to_none(fast_alias))?;
    let ids: Vec<String> = [&alias, &fast_alias]
        .into_iter()
        .flatten()
        .map(|model| model.id.clone())
        .collect();
    let grant = ModelGrant {
        default_model: alias.as_ref().map(|model| model.id.clone()),
        extend: if choices.extend_allowlist.unwrap_or(true) {
            ids
        } else {
            Vec::new()
        },
        // Only an explicit choice maps unknown names: the written config names the models the
        // tool uses, and a mapped name would hide a typo or a retired model.
        unknown_model_to_default: choices.unknown_model_to_default,
    };
    let facts = alias.as_ref().map(|model| model.facts).unwrap_or_default();
    let alias = alias.map(|model| model.name);
    let fast_alias = fast_alias.map(|model| model.name);
    let gateway_url = gateway_url(state);
    // Nothing is created or written before the files are known to be writable JSON/TOML: an
    // unusable config fails here, with no client to clean up.
    let (name, preflight_token) = match &subject {
        Subject::New(input, _) => (
            input.name.clone(),
            client_config::pending_token(&store::token_tag(&input.name)),
        ),
        Subject::Existing(id) => {
            let client = with_connection(state, |connection| store::get_client(connection, id))?;
            (client.name.clone(), masked_standin(&client))
        }
    };
    let request = ClientConfigRequest {
        app,
        client_name: name,
        gateway_url,
        token: preflight_token,
        alias,
        fast_alias,
        facts,
    };
    client_config::preview(home, &request)?;
    let result = match subject {
        Subject::New(input, replace) => connect_new(
            state, home, &input, replace, &grant, request, reviewed, apply,
        ),
        Subject::Existing(id) => {
            connect_existing(state, home, &id, &grant, request, reviewed, apply)
        }
    }?;
    // The file no longer names a legacy alias it may have held.
    super::routing::prune_legacy_names(state, home)?;
    Ok(result)
}

/// Connects a tool: creates the client (or rotates an existing one of that name, or
/// creates "name 2"), builds the plan with the real token inside the core, writes the files
/// atomically with backups in the data folder, records the ledger and starts the first-request
/// watcher. The token never leaves the core. A write that fails deletes the client created here.
///
/// Rotating a client also rewrites its token in the files of the tools it was connected to
/// before (those would hold a dead token otherwise); `other_files` says what happened to them.
///
/// # Errors
/// [`AppError::Validation`] for unusable existing config, an unknown alias, a duplicate name
/// with [`OnExisting::Fail`] or a revoked client to rotate; [`AppError::Conflict`] when a file
/// changed since the reviewed plan; [`AppError::Io`] on a write failure.
pub fn llm_client_connect_impl(
    state: &AppState,
    input: ClientConnectInput,
) -> Result<ClientConnectResult, AppError> {
    llm_client_connect_in(state, input, &client_config::home_dir()?)
}

/// [`llm_client_connect_impl`] for an explicit home directory.
pub(crate) fn llm_client_connect_in(
    state: &AppState,
    input: ClientConnectInput,
    home: &Path,
) -> Result<ClientConnectResult, AppError> {
    llm_client_connect_with(state, input, home, &system_apply)
}

/// [`llm_client_connect_in`] with the writer of the config files replaced (fault injection).
pub(crate) fn llm_client_connect_with(
    state: &AppState,
    input: ClientConnectInput,
    home: &Path,
    apply: Applier<'_>,
) -> Result<ClientConnectResult, AppError> {
    let wanted = blank_to_none(input.client_name.clone())
        .unwrap_or_else(|| input.app.display_name().to_owned());
    let clients = with_connection(state, store::list_clients)?;
    let first_connect = with_connection(state, |connection| {
        for client in &clients {
            if !store::latest_applied(connection, &client.id)?.is_empty() {
                return Ok(false);
            }
        }
        Ok(true)
    })?;
    let existing = clients
        .iter()
        .find(|client| client.name.eq_ignore_ascii_case(&wanted));
    let mut models: Vec<String> = Vec::new();
    for requested in [input.alias.clone(), input.fast_alias.clone()] {
        if let Some(model) = resolve_model(state, blank_to_none(requested))?
            && !models.contains(&model.id)
        {
            models.push(model.id);
        }
    }
    let new_client = |name: String, replace: bool| {
        Subject::New(
            ClientInput {
                name,
                allow_all: models.is_empty(),
                allowed_aliases: Vec::new(),
                allowed_models: models.clone(),
            },
            replace,
        )
    };
    let subject = match (existing, input.on_existing) {
        (None, _) => new_client(wanted, false),
        (Some(client), OnExisting::Rotate) => Subject::Existing(client.id.clone()),
        (Some(_), OnExisting::SecondClient) => {
            new_client(second_client_name(&clients, &wanted), false)
        }
        (Some(client), OnExisting::ReplaceRevoked) if !client.is_active() => {
            new_client(wanted, true)
        }
        (Some(_), OnExisting::Fail | OnExisting::ReplaceRevoked) => {
            return Err(AppError::validation(format!(
                "A client named \"{wanted}\" already exists"
            )));
        }
    };
    let result = connect_core(
        state,
        home,
        subject,
        input.app,
        input.alias,
        input.fast_alias,
        GrantChoices {
            extend_allowlist: input.extend_allowlist,
            unknown_model_to_default: input.unknown_model_to_default,
        },
        input.reviewed.as_ref(),
        apply,
    )?;
    if first_connect && !state.settings().ai.gateway.enabled_on_start {
        start_on_launch(state);
    }
    Ok(result)
}

/// Turns `ai.gateway.enabled_on_start` on after the first Connect: a tool written with the
/// gateway's address must find it after a restart. A failed write is logged, never the Connect's
/// failure (the files are written; the Access page still offers the switch).
fn start_on_launch(state: &AppState) {
    let on = serde_json::json!({"gateway": {"enabled_on_start": true}});
    if let Err(error) =
        crate::app::settings_update_impl(state, crate::settings::SettingsSection::Ai, &on)
    {
        crate::logging::log_line(
            crate::logging::LogLevel::Warn,
            "llm.clients",
            &format!("could not turn on start on launch: {}", error.message()),
        );
    }
}

/// "Update address": writes the gateway's current address into `app`'s config with the token the
/// file already holds, so a port change does not rotate the client's token. The model the file
/// names stays. A new ledger row records the write (restore and rotation follow it).
///
/// # Errors
/// [`AppError::Validation`] when the file holds no token or one of no active client (Connect
/// again), or for unusable config; [`AppError::Io`] on a write failure.
pub fn llm_client_rewrite_address_impl(
    state: &AppState,
    app: ClientApp,
) -> Result<ApplyResult, AppError> {
    llm_client_rewrite_address_in(state, app, &client_config::home_dir()?)
}

/// [`llm_client_rewrite_address_impl`] for an explicit home directory.
pub(crate) fn llm_client_rewrite_address_in(
    state: &AppState,
    app: ClientApp,
    home: &Path,
) -> Result<ApplyResult, AppError> {
    let token = client_config::read_written_token(home, app)?.ok_or_else(|| {
        AppError::validation(format!(
            "{} holds no gateway token: connect it first",
            app.display_name()
        ))
    })?;
    let digest = crate::crypto::sha256_hex(token.as_bytes());
    let client = active_hashes(state)?
        .into_iter()
        .find(|entry| entry.client.is_active() && entry.token_sha256 == digest)
        .map(|entry| entry.client)
        .ok_or_else(|| {
            AppError::validation(format!(
                "The token in {}'s config belongs to no active client: connect it again",
                app.display_name()
            ))
        })?;
    let request = ClientConfigRequest {
        app,
        client_name: client.name.clone(),
        gateway_url: gateway_url(state),
        token,
        alias: None,
        fast_alias: None,
        facts: ModelFacts::default(),
    };
    let applied = client_config::apply(
        home,
        &backup_root(state, home),
        &request,
        None,
        Clock::system(),
    )?;
    if !applied.files.is_empty() {
        with_connection(state, |connection| {
            store::record_applied(
                connection,
                &client.id,
                app,
                &applied.files,
                current_time_ms(),
            )
            .map(|_| ())
        })?;
        tidy_kept(state, home, &[app]);
    }
    super::clients::clients_changed(state);
    Ok(applied)
}

/// Input of [`llm_client_apply_impl`]: write the tool's config for an existing client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientApplyInput {
    /// The existing client.
    pub client_id: String,
    /// The tool to configure.
    pub app: ClientApp,
    /// Gateway model id the tool uses.
    #[serde(default)]
    pub alias: Option<String>,
    /// Claude Code only: the alias for background tasks.
    #[serde(default)]
    pub fast_alias: Option<String>,
    /// The plan the user reviewed; the write is refused when a file changed since.
    pub reviewed: ClientConfigPlan,
    /// As [`ClientConnectInput::extend_allowlist`].
    #[serde(default)]
    pub extend_allowlist: Option<bool>,
}

/// Writes the tool's config for an existing client from the plan the user reviewed:
/// the client's token is rotated (only its hash is stored, so the file needs a new one) and
/// written, as with [`OnExisting::Rotate`]. Files of other tools holding the client's old token
/// are re-tokened too.
///
/// # Errors
/// As [`llm_client_connect_impl`]; [`AppError::NotFound`] for an unknown client.
pub fn llm_client_apply_impl(
    state: &AppState,
    input: ClientApplyInput,
) -> Result<ClientConnectResult, AppError> {
    llm_client_apply_in(state, input, &client_config::home_dir()?)
}

/// [`llm_client_apply_impl`] for an explicit home directory.
pub(crate) fn llm_client_apply_in(
    state: &AppState,
    input: ClientApplyInput,
    home: &Path,
) -> Result<ClientConnectResult, AppError> {
    connect_core(
        state,
        home,
        Subject::Existing(input.client_id),
        input.app,
        input.alias,
        input.fast_alias,
        GrantChoices {
            extend_allowlist: input.extend_allowlist,
            unknown_model_to_default: None,
        },
        Some(&input.reviewed),
        &system_apply,
    )
}

// ------------------------------------------------------------------ rotate and update

/// "Rotate and update N files": mints a new token, writes it into
/// every file the ledger names (atomically per tool, backups in the data folder) and only then
/// swaps the stored hash and records the ledger, in one transaction. The token is not returned.
///
/// All or nothing: when any file cannot be updated (the user edited its token, it cannot be
/// parsed or written) or the commit fails, every file already written is restored and the old
/// token stays valid. A client whose files cannot all follow needs "Rotate and copy token".
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id, [`AppError::Validation`] for a revoked client or a
/// client without written files, [`AppError::Conflict`] naming the files that could not follow.
pub fn llm_client_rotate_and_update_impl(
    state: &AppState,
    client_id: &str,
) -> Result<RotateUpdateResult, AppError> {
    llm_client_rotate_and_update_in(state, client_id, &client_config::home_dir()?)
}

/// [`llm_client_rotate_and_update_impl`] for an explicit home directory.
pub(crate) fn llm_client_rotate_and_update_in(
    state: &AppState,
    client_id: &str,
    home: &Path,
) -> Result<RotateUpdateResult, AppError> {
    super::client_rotation::rotate_and_update(state, client_id, home)
}

// ------------------------------------------------------------------ restore, ledger

/// The active clients' token hashes, for the restore outcomes' revoked-token warnings.
fn active_token_hashes(state: &AppState) -> Result<Vec<String>, AppError> {
    Ok(active_hashes(state)?
        .into_iter()
        .filter(|entry| entry.client.is_active())
        .map(|entry| entry.token_sha256)
        .collect())
}

/// Whether any file of a restore still waits for the user's confirmation (nothing was changed).
fn needs_confirmation(outcomes: &[(String, RestoreOutcome)]) -> bool {
    outcomes
        .iter()
        .any(|(_, outcome)| matches!(outcome, RestoreOutcome::NeedsConfirmation { .. }))
}

/// "Restore previous config" for one tool: puts the backups of its latest write back (or
/// deletes a file that write created), all files of the tool together. When a file changed after
/// SpanBay wrote it, nothing is changed and every file comes back as
/// [`RestoreOutcome::NeedsConfirmation`] with a diff; `force` restores them anyway. Each outcome
/// says when the content that comes back holds a revoked token. The restored write leaves the
/// ledger, so the next call goes one write further back.
///
/// # Errors
/// [`AppError::Validation`] for a refused file, [`AppError::Io`] when a backup is missing or a
/// file cannot be written (nothing is restored then).
pub fn llm_client_restore_impl(
    state: &AppState,
    app: ClientApp,
    force: bool,
) -> Result<RestoreResult, AppError> {
    llm_client_restore_in(state, app, force, &client_config::home_dir()?)
}

/// [`llm_client_restore_impl`] for an explicit home directory.
pub(crate) fn llm_client_restore_in(
    state: &AppState,
    app: ClientApp,
    force: bool,
    home: &Path,
) -> Result<RestoreResult, AppError> {
    let entries = with_connection(state, |connection| {
        store::latest_applied_for_app(connection, app)
    })?;
    if entries.is_empty() {
        return Ok(RestoreResult {
            app,
            files: Vec::new(),
        });
    }
    let active = active_token_hashes(state)?;
    let is_active = |hash: &str| active.iter().any(|known| known == hash);
    let root = backup_root(state, home);
    let files: Vec<AppliedFile> = entries.iter().map(|entry| entry.file.clone()).collect();
    let outcomes = client_config::restore(
        &files,
        &RestoreEnv {
            app,
            backup_root: &root,
            clock: Clock::system(),
            token_is_active: &is_active,
        },
        force,
    )?;
    if !needs_confirmation(&outcomes) {
        let ids: Vec<String> = entries.into_iter().map(|entry| entry.id).collect();
        with_connection(state, |connection| {
            store::delete_applied_ids(connection, &ids)
        })?;
        tidy_kept(state, home, &[app]);
        super::clients::clients_changed(state);
    }
    Ok(RestoreResult {
        app,
        files: outcomes
            .into_iter()
            .map(|(path, outcome)| RestoredFile { path, outcome })
            .collect(),
    })
}

/// "Restore original config" (also "Disconnect") for one tool: every file goes back to what it
/// was before SpanBay took the tool over (a file SpanBay created is deleted), whatever it holds
/// now. Without `force` nothing is changed: every file that would change comes back as
/// [`RestoreOutcome::NeedsConfirmation`] with its diff and whether it would be deleted, for the
/// user to review; with `force` the current content goes to a safety backup first and the tool's
/// ledger rows go (its files no longer hold a SpanBay token). The original is kept, so this can
/// be done again.
///
/// # Errors
/// [`AppError::Io`] when the original or a file cannot be read or written.
pub fn llm_client_restore_original_impl(
    state: &AppState,
    app: ClientApp,
    force: bool,
) -> Result<RestoreResult, AppError> {
    llm_client_restore_original_in(state, app, force, &client_config::home_dir()?)
}

/// [`llm_client_restore_original_impl`] for an explicit home directory.
pub(crate) fn llm_client_restore_original_in(
    state: &AppState,
    app: ClientApp,
    force: bool,
    home: &Path,
) -> Result<RestoreResult, AppError> {
    let active = active_token_hashes(state)?;
    let is_active = |hash: &str| active.iter().any(|known| known == hash);
    let root = backup_root(state, home);
    let restored = client_config::restore_original(
        home,
        &RestoreEnv {
            app,
            backup_root: &root,
            clock: Clock::system(),
            token_is_active: &is_active,
        },
        force,
    )?;
    if force {
        with_connection(state, |connection| {
            store::delete_applied_for_app(connection, app).map(|_| ())
        })?;
        tidy_kept(state, home, &[app]);
        super::clients::clients_changed(state);
    }
    Ok(RestoreResult {
        app,
        files: restored
            .into_iter()
            .map(|(path, outcome)| RestoredFile { path, outcome })
            .collect(),
    })
}

/// One file SpanBay wrote for a tool, as its latest write left it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedFile {
    /// The file.
    pub path: String,
    /// The client whose token that write put in (it may be revoked since, or deleted).
    pub client_id: String,
    /// That client's name; `None` when the client no longer exists.
    pub client_name: Option<String>,
    /// The client is revoked: the file holds a dead token.
    pub client_revoked: bool,
    /// When SpanBay wrote it (Unix ms).
    pub written_at_ms: i64,
}

/// What SpanBay changed and keeps for one tool: "the files SpanBay changed" with what can be
/// undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedTool {
    /// The tool.
    pub app: ClientApp,
    /// The files of the tool's latest write still in the ledger ("Restore previous config"
    /// undoes it); empty when the tool is disconnected.
    pub files: Vec<ManagedFile>,
    /// An original is kept: "Restore original config" / "Disconnect" is possible.
    pub has_original: bool,
    /// The copies kept in the data folder (originals and backups hold the user's own config,
    /// credentials included).
    pub kept: Vec<KeptCopy>,
}

/// Every tool SpanBay wrote or keeps copies for, in display order.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_client_managed_list_impl(state: &AppState) -> Result<Vec<ManagedTool>, AppError> {
    llm_client_managed_list_in(state, &client_config::home_dir()?)
}

/// [`llm_client_managed_list_impl`] for an explicit home directory.
pub(crate) fn llm_client_managed_list_in(
    state: &AppState,
    home: &Path,
) -> Result<Vec<ManagedTool>, AppError> {
    let clients = with_connection(state, store::list_clients)?;
    let root = backup_root(state, home);
    let clock = Clock::system();
    let mut tools = Vec::new();
    for app in ClientApp::ALL {
        let entries = with_connection(state, |connection| {
            store::latest_applied_for_app(connection, app)
        })?;
        let kept = client_config::kept_copies(&root, app, clock);
        if entries.is_empty() && kept.is_empty() {
            continue;
        }
        let files = entries
            .into_iter()
            .map(|entry| {
                let client = clients.iter().find(|client| client.id == entry.client_id);
                ManagedFile {
                    path: entry.file.path,
                    client_name: client.map(|client| client.name.clone()),
                    client_revoked: client.is_none_or(|client| !client.is_active()),
                    client_id: entry.client_id,
                    written_at_ms: entry.applied_at_ms,
                }
            })
            .collect();
        tools.push(ManagedTool {
            app,
            files,
            has_original: kept.iter().any(|copy| {
                matches!(
                    copy.kind,
                    client_config::KeptKind::Original | client_config::KeptKind::AbsentMarker
                )
            }),
            kept,
        });
    }
    Ok(tools)
}

/// Deletes the copies SpanBay keeps for `app` (originals, safety backups and the backups no
/// ledger row needs); the ledger's older writes go first, so only the backups of each client's
/// latest write stay ("Restore previous config" of the current state). "Restore original config"
/// is not possible afterwards. Returns how many copies were deleted.
///
/// # Errors
/// [`AppError::Database`] on failure, [`AppError::Io`] when a copy cannot be deleted.
pub fn llm_client_kept_purge_impl(state: &AppState, app: ClientApp) -> Result<usize, AppError> {
    llm_client_kept_purge_in(state, app, &client_config::home_dir()?)
}

/// [`llm_client_kept_purge_impl`] for an explicit home directory.
pub(crate) fn llm_client_kept_purge_in(
    state: &AppState,
    app: ClientApp,
    home: &Path,
) -> Result<usize, AppError> {
    let referenced = with_connection(state, |connection| {
        store::prune_history(connection, app, 1)?;
        store::referenced_backups(connection, app)
    })?;
    let keep = |path: &str| referenced.iter().any(|known| known == path);
    client_config::purge_kept(&backup_root(state, home), app, &keep, Clock::system())
}

/// The files that hold a client's current token: the latest write per tool. The rotate
/// dialog lists them ("Rotate and update 2 files").
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_client_applied_list_impl(
    state: &AppState,
    client_id: &str,
) -> Result<Vec<LedgerEntry>, AppError> {
    with_connection(state, |connection| {
        store::latest_applied(connection, client_id)
    })
}
