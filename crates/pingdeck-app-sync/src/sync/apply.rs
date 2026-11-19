//! Applying an import: per-kind writes through the store and the existing use-cases.
//!
//! Removals are applied only on request, children before parents. Secrets are never deleted: a
//! provider's local keys that the files do not list stay unless `keep_local_keys` is off, an empty
//! secret leaves the stored one, and placeholders were already refilled from local values by
//! [`super::diff::compare`]. Providers go through `pingdeck_app_llm::import_policy`.

use std::collections::{HashMap, HashSet};
use std::fmt::Display;

use serde::de::DeserializeOwned;

use crate::agent::mcp::config_fingerprint;
use crate::app::environment_service::{first_free_environment_name, notify_environments_changed};
use crate::app::misc_logic_service::{first_issue_error, validate_flow};
use crate::app::mock_service::validate_mock_server;
use crate::app::settings_service::{
    SettingsSecret, settings_replace_section_impl, settings_secret_set_impl,
};
use crate::app::tree_service::{DeletedKind, delete_discarding};
use crate::app::{
    AppState, current_time_ms, ensure_workspace_exists, environment_delete_discarding_secrets,
    flow_delete_impl, mock_server_delete_impl, workspace_delete_impl,
};
use crate::llm::events::LlmEvent;
use crate::llm::store;
use crate::workspace::ai_request::{AiTarget, Relink, RequestMode, relink_provider};
use crate::workspace::models::{Environment, EnvironmentVariable, Request, Workspace};
use crate::workspace::storage::MCP_REVIEW_PENDING;
use pingdeck_app_agent::{agent_mcp_server_delete_impl, agent_mcp_server_save_impl};
use pingdeck_app_llm::import_policy::{
    ImportOptions, ImportedKey, ImportedProvider, apply_imported_provider,
};
use pingdeck_app_llm::llm_provider_delete_impl;
use pingdeck_workbench::mock::file_body_paths;

use super::diff::{Item, mock_proxy_target, refused_fields};
use super::docs::{
    AliasBody, CollectionBody, Doc, EnvironmentBody, FlowBody, GlobalsBody, LooseBody, McpBody,
    MockBody, ProviderBody, SkillsBody, WorkspaceBody, syncable_section,
};
use super::{
    EntityId, SyncChange, SyncChoice, SyncImportOptions, SyncImportResult, SyncKind, SyncSkipped,
};

fn text(error: impl Display) -> String {
    error.to_string()
}

fn parse<T: DeserializeOwned>(doc: &Doc) -> Result<T, String> {
    serde_json::from_value(doc.body.clone()).map_err(text)
}

fn check_variables(variables: &[EnvironmentVariable]) -> Result<(), String> {
    variables
        .iter()
        .try_for_each(|variable| EnvironmentVariable::plain(&variable.key, "").map(drop))
        .map_err(text)
}

/// Whether the import writes `item`: the per-entity choice replaces the merge rule.
pub(crate) fn incoming_wins(item: &Item, choices: &HashMap<(SyncKind, &str), SyncChoice>) -> bool {
    let choice = choices
        .get(&(item.entry.kind, item.entry.key.as_str()))
        .copied();
    match item.entry.change {
        SyncChange::Conflict => choice == Some(SyncChoice::Incoming),
        _ => choice != Some(SyncChoice::Local),
    }
}

/// The providers an imported request may link to: local ones plus the ones this import writes.
type ProviderTargets = Result<Vec<(String, String)>, String>;

/// `(id, base_url)` of every local provider and of the provider documents `writes` imports.
fn provider_targets(
    state: &AppState,
    writes: &[Item],
    choices: &HashMap<(SyncKind, &str), SyncChoice>,
) -> ProviderTargets {
    let mut targets: Vec<(String, String)> = store::list_providers(state.store().connection())
        .map_err(|error| text(error.message()))?
        .into_iter()
        .map(|provider| (provider.id, provider.base_url))
        .collect();
    for item in writes
        .iter()
        .filter(|item| item.entry.kind == SyncKind::LlmProvider && item.problem.is_none())
    {
        let Some(incoming) = &item.incoming else {
            continue;
        };
        if incoming_wins(item, choices)
            && let Ok(body) = parse::<ProviderBody>(incoming)
        {
            targets.push((body.provider.id, body.provider.base_url));
        }
    }
    Ok(targets)
}

/// Re-links the provider targets of imported AI requests: the local provider with
/// the same id, else the one with the same base URL. A request matching neither keeps its AI
/// settings and its provider id is cleared, so the editor asks for a provider ("Choose a
/// provider") while the plain HTTP fields stay as they are.
fn relink_requests(requests: &mut [Request], providers: &ProviderTargets) -> Result<(), String> {
    if !requests.iter().any(|request| request.is_provider_target()) {
        return Ok(());
    }
    let providers = providers.as_ref().map_err(ToString::to_string)?;
    let pairs: Vec<(&str, &str)> = providers
        .iter()
        .map(|(id, base_url)| (id.as_str(), base_url.as_str()))
        .collect();
    for request in requests.iter_mut() {
        if request.mode != RequestMode::AiApi {
            continue;
        }
        let Some(ai) = request.ai.as_mut() else {
            continue;
        };
        match relink_provider(&ai.target, &pairs) {
            Relink::Linked(id) => {
                if let AiTarget::Provider { provider_id, .. } = &mut ai.target {
                    *provider_id = id;
                }
            }
            Relink::NeedsChoice => {
                if let AiTarget::Provider { provider_id, .. } = &mut ai.target {
                    provider_id.clear();
                }
            }
            Relink::NotApplicable => {}
        }
    }
    Ok(())
}

/// How an import writes one entity.
#[derive(Clone, Copy)]
struct Puts {
    /// The provider moved to another host or proxy and the user did not keep the keys for it:
    /// its keys end switched off (values kept).
    detach_keys: bool,
    /// Local children the file no longer lists may be deleted.
    apply_removals: bool,
    /// A provider's local keys the file does not list stay.
    keep_local_keys: bool,
    /// The user kept the provider's keys for its new host.
    keep_keys: bool,
}

fn put(
    state: &AppState,
    doc: &Doc,
    local: Option<&Doc>,
    puts: Puts,
    providers: &ProviderTargets,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    match doc.kind {
        SyncKind::Workspace => {
            let body: WorkspaceBody = parse(doc)?;
            let workspace =
                Workspace::new(body.workspace.id.as_str(), &body.workspace.name).map_err(text)?;
            state.store().save_workspace(&workspace).map_err(text)
        }
        SyncKind::Collection => {
            let mut body: CollectionBody = parse(doc)?;
            relink_requests(&mut body.requests, providers)?;
            let store = state.store();
            ensure_workspace_exists(&store, body.collection.workspace_id.as_str()).map_err(text)?;
            check_variables(&body.collection.variables)?;
            store
                .save_imported_collection(&body.collection, &body.folders, &body.requests)
                .map_err(text)?;
            let id = body.collection.id.as_str();
            if let Some(ms) = doc.updated_at_ms {
                store.sync_stamp_collection(id, ms).map_err(text)?;
            }
            // What the file no longer lists goes (requests first, then folders deepest-first)
            // only when removals are applied; otherwise it stays and is reported.
            if let Some(local) = local {
                let before: CollectionBody = parse(local)?;
                let kept_requests: HashSet<&str> =
                    body.requests.iter().map(|r| r.id.as_str()).collect();
                let kept_folders: HashSet<&str> =
                    body.folders.iter().map(|f| f.id.as_str()).collect();
                let workspace = &body.collection.workspace_id;
                let gone_requests: Vec<_> = before
                    .requests
                    .iter()
                    .filter(|r| !kept_requests.contains(r.id.as_str()))
                    .collect();
                let gone_folders: Vec<_> = before
                    .folders
                    .iter()
                    .rev()
                    .filter(|f| !kept_folders.contains(f.id.as_str()))
                    .collect();
                if !puts.apply_removals && !(gone_requests.is_empty() && gone_folders.is_empty()) {
                    notes.push(format!(
                        "collection {}: {} local request(s) and {} folder(s) the file no longer lists were kept; apply removals to delete them",
                        body.collection.name,
                        gone_requests.len(),
                        gone_folders.len()
                    ));
                } else {
                    for request in gone_requests {
                        store.delete_request(workspace, &request.id).map_err(text)?;
                    }
                    for folder in gone_folders {
                        store.delete_folder(workspace, &folder.id).map_err(text)?;
                    }
                }
            }
            Ok(())
        }
        SyncKind::LooseRequests => {
            let mut body: LooseBody = parse(doc)?;
            relink_requests(&mut body.requests, providers)?;
            let store = state.store();
            ensure_workspace_exists(&store, &body.workspace_id).map_err(text)?;
            if body
                .requests
                .iter()
                .any(|r| r.collection_id.is_some() || r.workspace_id.as_str() != body.workspace_id)
            {
                return Err(
                    "loose requests must have no collection and belong to the workspace".to_owned(),
                );
            }
            for request in &body.requests {
                store.save_request(request).map_err(text)?;
            }
            if let Some(local) = local {
                let before: LooseBody = parse(local)?;
                let kept: HashSet<&str> = body.requests.iter().map(|r| r.id.as_str()).collect();
                let workspace =
                    crate::workspace::WorkspaceId::new(&body.workspace_id).map_err(text)?;
                let gone: Vec<_> = before
                    .requests
                    .iter()
                    .filter(|r| !kept.contains(r.id.as_str()))
                    .collect();
                if !puts.apply_removals && !gone.is_empty() {
                    notes.push(format!(
                        "loose requests: {} local request(s) the file no longer lists were kept; apply removals to delete them",
                        gone.len()
                    ));
                } else {
                    for request in gone {
                        store
                            .delete_request(&workspace, &request.id)
                            .map_err(text)?;
                    }
                }
            }
            Ok(())
        }
        SyncKind::Environment => {
            let body: EnvironmentBody = parse(doc)?;
            let environment = Environment::new(
                body.environment.id.as_str(),
                body.environment.workspace_id.as_str(),
                &body.environment.name,
            )
            .map_err(text)?;
            let environment = Environment {
                variables: body.environment.variables,
                confirm_before_send: body.environment.confirm_before_send,
                confirm_safe_methods: body.environment.confirm_safe_methods,
                declared_hosts: body.environment.declared_hosts,
                ..environment
            };
            check_variables(&environment.variables)?;
            let store = state.store();
            ensure_workspace_exists(&store, environment.workspace_id.as_str()).map_err(text)?;
            // A different local environment may already own the name (names are unique per
            // workspace, ignoring case). Ids are what documents refer to, so the incoming
            // environment keeps its id and takes the first free "Name (N)": the import never
            // fails on the clash and never renames or overwrites the local environment.
            let environment = if store
                .environment_name_taken(
                    &environment.workspace_id,
                    &environment.name,
                    &environment.id,
                )
                .map_err(text)?
            {
                let base = environment.name.clone();
                let name = first_free_environment_name(
                    &store,
                    &environment.workspace_id,
                    &environment.id,
                    (2_u32..).map(|n| format!("{base} ({n})")),
                )
                .map_err(|error| text(error.message()))?;
                Environment {
                    name,
                    ..environment
                }
            } else {
                environment
            };
            store.save_environment(&environment).map_err(text)?;
            if let Some(ms) = doc.updated_at_ms {
                store
                    .sync_stamp_environment(environment.id.as_str(), ms)
                    .map_err(text)?;
            }
            drop(store);
            notify_environments_changed(
                state,
                environment.workspace_id.as_str(),
                vec![environment.id.as_str().to_owned()],
                "sync",
            );
            Ok(())
        }
        SyncKind::Globals => {
            let body: GlobalsBody = parse(doc)?;
            check_variables(&body.variables)?;
            let store = state.store();
            ensure_workspace_exists(&store, &body.workspace_id).map_err(text)?;
            store
                .save_global_variables(&body.workspace_id, &body.variables)
                .map_err(text)?;
            drop(store);
            notify_environments_changed(
                state,
                &body.workspace_id,
                vec![body.workspace_id.clone()],
                "sync",
            );
            Ok(())
        }
        SyncKind::Mock => {
            let body: MockBody = parse(doc)?;
            let mut server = body.mock_server;
            validate_mock_server(&server).map_err(|e| text(e.message()))?;
            if let Some(bound) = state.mocks.bound_addr(&server.id)
                && bound.port() != server.port
            {
                return Err(format!(
                    "stop the mock server before changing its port (running on {})",
                    bound.port()
                ));
            }
            // An import must not start outbound traffic by itself (as in the JSON mock import):
            // proxying stays on only where the local copy already forwarded to the same target.
            let known_target = local
                .and_then(|before| parse::<MockBody>(before).ok())
                .is_some_and(|before| {
                    mock_proxy_target(&before.mock_server) == mock_proxy_target(&server)
                });
            if server.proxy_enabled && !known_target {
                server.proxy_enabled = false;
                notes.push(format!(
                    "mock server {}: proxying was switched off on import; review the target URL and enable it manually",
                    server.name
                ));
            }
            // Imported content is not authorization: name every local file body the local copy
            // did not already serve.
            let known_files: Vec<String> = local
                .and_then(|before| parse::<MockBody>(before).ok())
                .map(|before| {
                    file_body_paths(&before.mock_server)
                        .into_iter()
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let new_files: Vec<&str> = file_body_paths(&server)
                .into_iter()
                .filter(|path| !known_files.iter().any(|known| known == path))
                .collect();
            if !new_files.is_empty() {
                notes.push(format!(
                    "mock server {}: responses serve these local files to the mock's clients: {}; check the paths",
                    server.name,
                    new_files.join(", ")
                ));
            }
            if let Some(ms) = doc.updated_at_ms {
                server.updated_at_ms = ms;
            }
            {
                let store = state.store();
                ensure_workspace_exists(&store, server.workspace_id.as_str()).map_err(text)?;
                store.save_mock_server(&server).map_err(text)?;
            }
            if state.mocks.is_running(&server.id) {
                let runtime = crate::app::mock_service::mock_runtime_config(state, &server)
                    .map_err(|e| text(e.message()))?;
                state.mocks.update(runtime);
            }
            Ok(())
        }
        SyncKind::Flow => {
            let body: FlowBody = parse(doc)?;
            let mut flow = body.flow;
            first_issue_error(validate_flow(&flow)).map_err(|e| text(e.message()))?;
            if let Some(ms) = doc.updated_at_ms {
                flow.updated_at_ms = ms;
            }
            let store = state.store();
            ensure_workspace_exists(&store, flow.workspace_id.as_str()).map_err(text)?;
            store.save_flow(&flow).map_err(text)
        }
        SyncKind::LlmProvider => {
            let body: ProviderBody = parse(doc)?;
            let now = current_time_ms();
            let stamped = |ms: i64| if ms > 0 { ms } else { now };
            let provider = store::Provider {
                created_at_ms: stamped(body.provider.created_at_ms),
                updated_at_ms: stamped(body.provider.updated_at_ms),
                ..body.provider.clone()
            };
            let keys: Vec<ImportedKey> = body
                .keys
                .iter()
                .map(|key| ImportedKey {
                    key: store::ProviderKey {
                        id: key.id.clone(),
                        provider_id: key.provider_id.clone(),
                        name: key.name.clone(),
                        enabled: key.enabled,
                        position: key.position,
                        created_at_ms: stamped(key.created_at_ms),
                        updated_at_ms: stamped(key.updated_at_ms),
                        notes: Some(key.notes.clone()),
                        group_name: key.group_name.clone(),
                        protocols: key.protocols.clone(),
                        ..store::ProviderKey::default()
                    },
                    secret: key.secret.clone(),
                })
                .collect();
            let outcome = {
                let store = state.store();
                store.with_transaction(
                    |transaction| {
                        let local = store::get_provider(transaction, &provider.id)
                            .map_err(|e| text(e.message()))?;
                        apply_imported_provider(
                            transaction,
                            &ImportedProvider {
                                provider: &provider,
                                keys: &keys,
                                models: &body.models,
                            },
                            local.as_ref(),
                            ImportOptions {
                                keep_local_keys: puts.keep_local_keys,
                                apply_removals: puts.apply_removals,
                                detach_keys: puts.detach_keys,
                                keys_kept_for_new_host: puts.keep_keys,
                            },
                        )
                        .map_err(|e| text(e.message()))
                    },
                    text,
                )?
            };
            if outcome.new_keys_off > 0 {
                notes.push(format!(
                    "LLM provider {}: {} key(s) new to this machine were imported switched off; switch them on after checking the host",
                    provider.name, outcome.new_keys_off
                ));
            }
            state.llm.touch();
            // Open pages re-read what the file changed: a page left on the old copy would write
            // it back (C-I5 refuses that write; these events keep it from happening).
            let provider_id = outcome.provider_id;
            state.llm.emit(LlmEvent::ProvidersChanged);
            state.llm.emit(LlmEvent::KeysChanged {
                provider_id: provider_id.clone(),
            });
            state.llm.emit(LlmEvent::ModelsChanged { provider_id });
            Ok(())
        }
        SyncKind::LlmAlias => {
            // An export from before model routing: aliases are gone, their chains are not
            // reconstructed from files.
            let body: AliasBody = parse(doc)?;
            notes.push(format!(
                "Old alias {} was not imported: aliases were replaced by model fallbacks.",
                body.alias.name
            ));
            Ok(())
        }
        SyncKind::McpServer => {
            let body: McpBody = parse(doc)?;
            let mut server = body.server;
            let local = state.store().list_agent_mcp_servers().map_err(text)?;
            // Ids are per machine: keep the local id of the same name, never reuse one that
            // belongs to a differently named server. `enabled` and `trusted` are local decisions:
            // an existing server keeps them, a new one starts disabled and untrusted.
            // What the user last reviewed of the server this import changes (none for a new one).
            let mut reviewed = None;
            if let Some(same) = local.iter().find(|s| s.name == server.name) {
                server.id.clone_from(&same.id);
                server.enabled = same.enabled;
                server.trusted = same.trusted;
                let baseline = state.store().mcp_review_baseline(&same.id).map_err(text)?;
                reviewed = Some((config_fingerprint(&same.transport), baseline));
            } else {
                if local.iter().any(|s| s.id == server.id) {
                    server.id.clear();
                }
                server.enabled = false;
                server.trusted = false;
            }
            let saved =
                agent_mcp_server_save_impl(state, server, true).map_err(|e| text(e.message()))?;
            // The save counts as the user's review: an import must not. A server whose command or
            // headers changed (or that is new) stays "Needs review" and is not started until the
            // user saves it; an unchanged one keeps the baseline it had.
            let unchanged = reviewed
                .as_ref()
                .is_some_and(|(before, _)| *before == config_fingerprint(&saved.transport));
            let baseline = match reviewed {
                Some((_, baseline)) if unchanged => baseline,
                _ => Some(MCP_REVIEW_PENDING.to_owned()),
            };
            if let Some(baseline) = baseline {
                state
                    .store()
                    .set_mcp_review_baseline(&saved.id, &baseline)
                    .map_err(text)?;
            }
            if let Some(ms) = doc.updated_at_ms {
                state
                    .store()
                    .sync_stamp_mcp_server(&saved.id, ms)
                    .map_err(text)?;
            }
            Ok(())
        }
        SyncKind::Skills => {
            let body: SkillsBody = parse(doc)?;
            let known: HashSet<String> = match local {
                Some(local) => parse::<SkillsBody>(local)?
                    .skills
                    .into_iter()
                    .map(|s| s.name)
                    .collect(),
                None => HashSet::new(),
            };
            let store = state.store();
            for skill in body
                .skills
                .iter()
                .filter(|skill| known.contains(&skill.name))
            {
                store
                    .set_agent_skill_enabled(&skill.name, skill.enabled)
                    .map_err(text)?;
            }
            Ok(())
        }
        SyncKind::Settings => {
            let section = syncable_section(&doc.key)
                .ok_or_else(|| format!("unknown settings section {}", doc.key))?;
            // The replace path keeps the local-only fields (LAN binding, token requirement,
            // folders, rules, key hints); the search keys are not settings, a value in the
            // file goes to the secret store.
            settings_replace_section_impl(
                state,
                section,
                &super::secrets::without_search_keys(&doc.body),
            )
            .map_err(|e| text(e.message()))?;
            let (tavily, brave) = super::secrets::search_keys(&doc.body);
            for (value, secret) in [
                (tavily, SettingsSecret::SearchTavily),
                (brave, SettingsSecret::SearchBrave),
            ] {
                if !value.is_empty() {
                    settings_secret_set_impl(state, secret, &value)
                        .map_err(|e| text(e.message()))?;
                }
            }
            Ok(())
        }
    }
}

fn remove(state: &AppState, local: &Doc) -> Result<(), String> {
    let workspace = local.workspace.clone().unwrap_or_default();
    let done = |result: Result<(), crate::error::AppError>| result.map_err(|e| text(e.message()));
    match local.kind {
        SyncKind::Workspace => done(workspace_delete_impl(state, local.key.clone())),
        SyncKind::Collection => done(delete_discarding(
            state,
            &workspace,
            DeletedKind::Collection,
            &local.key,
        )),
        SyncKind::LooseRequests => {
            let body: LooseBody = parse(local)?;
            body.requests.iter().try_for_each(|r| {
                done(delete_discarding(
                    state,
                    &workspace,
                    DeletedKind::Request,
                    r.id.as_str(),
                ))
            })
        }
        SyncKind::Environment => {
            done(environment_delete_discarding_secrets(
                state, &workspace, &local.key,
            ))?;
            notify_environments_changed(state, &workspace, vec![local.key.clone()], "sync");
            Ok(())
        }
        SyncKind::Globals => {
            state
                .store()
                .save_global_variables(&workspace, &[])
                .map_err(text)?;
            notify_environments_changed(state, &workspace, vec![workspace.clone()], "sync");
            Ok(())
        }
        SyncKind::Mock => done(mock_server_delete_impl(state, workspace, local.key.clone())),
        SyncKind::Flow => done(flow_delete_impl(state, workspace, local.key.clone())),
        SyncKind::LlmProvider => done(llm_provider_delete_impl(state, &local.key)),
        // No local alias exists any more.
        SyncKind::LlmAlias => Ok(()),
        SyncKind::McpServer => {
            let body: McpBody = parse(local)?;
            done(agent_mcp_server_delete_impl(state, &body.server.id).map(drop))
        }
        SyncKind::Skills | SyncKind::Settings => Ok(()),
    }
}

/// Applies `items` (from [`super::diff::compare`]) under `options`. Also returns the entities
/// that could not be applied (a problem, a failed write or removal); a refused local-only
/// settings field is reported but does not count.
pub(crate) fn apply(
    state: &AppState,
    items: Vec<Item>,
    options: &SyncImportOptions,
    warnings: Vec<String>,
) -> (SyncImportResult, HashSet<EntityId>) {
    let choices: HashMap<(SyncKind, &str), SyncChoice> = options
        .overrides
        .iter()
        .map(|choice| ((choice.kind, choice.key.as_str()), choice.choice))
        .collect();
    let mut result = SyncImportResult {
        added: 0,
        updated: 0,
        removed: 0,
        kept_local: 0,
        skipped_with_reason: Vec::new(),
        warnings,
    };
    let mut failed: HashSet<EntityId> = HashSet::new();
    let skip = |result: &mut SyncImportResult, item: &Item, reason: String| {
        result.skipped_with_reason.push(SyncSkipped {
            kind: item.entry.kind,
            key: item.entry.key.clone(),
            reason,
        });
    };
    let id = |item: &Item| (item.entry.kind, item.entry.key.clone());

    let (removals, writes): (Vec<Item>, Vec<Item>) = items
        .into_iter()
        .partition(|item| item.entry.change == SyncChange::Removed);
    // Providers written later in this loop must be linkable now.
    let providers = provider_targets(state, &writes, &choices);
    for item in &writes {
        if let Some(problem) = &item.problem {
            skip(&mut result, item, problem.clone());
            failed.insert(id(item));
            continue;
        }
        if !incoming_wins(item, &choices) {
            result.kept_local += 1;
            continue;
        }
        let Some(incoming) = &item.incoming else {
            continue;
        };
        if item.entry.kind == SyncKind::Settings
            && let Some(section) = syncable_section(&incoming.key)
        {
            for name in refused_fields(&state.settings(), section, &incoming.body) {
                skip(
                    &mut result,
                    item,
                    format!("{name} is never imported; the local value was kept"),
                );
            }
        }
        let keep_keys = options.keep_key_providers.contains(&item.entry.key);
        let detach_keys = item.entry.host_change.is_some() && !keep_keys;
        let put_result = put(
            state,
            incoming,
            item.local.as_ref(),
            Puts {
                detach_keys,
                apply_removals: options.apply_removals,
                keep_local_keys: options.keep_local_keys,
                keep_keys,
            },
            &providers,
            &mut result.warnings,
        );
        match put_result {
            Ok(()) if item.entry.change == SyncChange::Added => result.added += 1,
            Ok(()) => result.updated += 1,
            Err(reason) => {
                skip(&mut result, item, reason);
                failed.insert(id(item));
            }
        }
    }
    if options.apply_removals {
        // Children before parents: the reverse of the apply order.
        for item in removals.iter().rev() {
            if choices.get(&(item.entry.kind, item.entry.key.as_str())) == Some(&SyncChoice::Local)
            {
                result.kept_local += 1;
                continue;
            }
            let Some(local) = &item.local else { continue };
            match remove(state, local) {
                Ok(()) => result.removed += 1,
                Err(reason) => {
                    skip(&mut result, item, reason);
                    failed.insert(id(item));
                }
            }
        }
    }
    (result, failed)
}
