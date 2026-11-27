//! The documents of a sync directory: typed bodies, identity and the collection of local data.
//!
//! A [`Doc`] is one file's worth of one entity. Local data and incoming files both become `Doc`s
//! through [`Doc::from_value`], so a comparison never depends on which side a body came from.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::agent::mcp::McpTransportConfig;
use crate::agent::model::AgentMcpServer;
use crate::agent::skills::SkillInfo;
use crate::app::settings_service::local_only_fields;
use crate::error::AppError;
use crate::llm::store::{self, Alias, KeyStrategy, Provider, ProviderModel};
use crate::settings::{Settings, SettingsSection, remove_path};
use crate::workspace::models::{
    Collection, Environment, EnvironmentVariable, Folder, Request, Workspace,
};
use crate::workspace::{Flow, MockServer, WorkspaceStore};

use super::SyncKind;

/// One skill reference: the name, where it was found and its enabled flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SkillRef {
    pub(crate) name: String,
    pub(crate) origin: String,
    pub(crate) enabled: bool,
}

#[derive(Serialize, Deserialize)]
pub(super) struct WorkspaceBody {
    pub(super) workspace: Workspace,
}

#[derive(Serialize, Deserialize)]
pub(super) struct CollectionBody {
    pub(super) collection: Collection,
    pub(super) folders: Vec<Folder>,
    pub(super) requests: Vec<Request>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct LooseBody {
    pub(super) workspace_id: String,
    pub(super) requests: Vec<Request>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct EnvironmentBody {
    pub(super) environment: Environment,
}

#[derive(Serialize, Deserialize)]
pub(super) struct GlobalsBody {
    pub(super) workspace_id: String,
    pub(super) variables: Vec<EnvironmentVariable>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct MockBody {
    pub(super) mock_server: MockServer,
}

#[derive(Serialize, Deserialize)]
pub(super) struct FlowBody {
    pub(super) flow: Flow,
}

/// A provider key as a sync file carries it: the secret travels (masked unless the export
/// includes secrets) because the file, not the secret store, is the transport.
#[derive(Serialize, Deserialize)]
pub(super) struct SyncKey {
    pub(super) id: String,
    pub(super) provider_id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) secret: String,
    pub(super) enabled: bool,
    #[serde(default)]
    pub(super) position: i64,
    #[serde(default)]
    pub(super) created_at_ms: i64,
    #[serde(default)]
    pub(super) updated_at_ms: i64,
    /// Absent when empty, so files written before key notes existed compare unchanged.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(super) notes: String,
    /// The relay group the key belongs to (format 2); absent when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) group_name: Option<String>,
    /// The protocols the key is limited to (format 2); absent when not limited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) protocols: Option<Vec<crate::llm::wire::Protocol>>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ProviderBody {
    pub(super) provider: Provider,
    pub(super) keys: Vec<SyncKey>,
    pub(super) models: Vec<ProviderModel>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct AliasBody {
    pub(super) alias: Alias,
}

#[derive(Serialize, Deserialize)]
pub(super) struct McpBody {
    pub(super) server: AgentMcpServer,
}

#[derive(Serialize, Deserialize)]
pub(super) struct SkillsBody {
    pub(super) skills: Vec<SkillRef>,
}

/// One entity as a sync document.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Doc {
    pub(crate) kind: SyncKind,
    /// Identity within the kind.
    pub(crate) key: String,
    pub(crate) name: String,
    /// The owning workspace of workspace-scoped kinds.
    pub(crate) workspace: Option<String>,
    /// Last change, when the entity records one in its file.
    pub(crate) updated_at_ms: Option<i64>,
    /// Row time of a local entity whose file carries no `updated_at_ms` (workspaces, loose
    /// requests, settings sections); `None` when the store keeps none, which counts as 0.
    pub(crate) row_ms: Option<i64>,
    /// The relative path of the file an incoming document was read from (`None` for local data).
    pub(crate) file: Option<String>,
    /// Content fingerprint of the file an incoming document was read from (`None` for local data).
    pub(crate) fingerprint: Option<String>,
    /// The file content without the top-level `updated_at_ms`.
    pub(crate) body: Value,
}

fn typed<T: Serialize + DeserializeOwned>(raw: Value) -> Result<(T, Value), String> {
    let parsed: T = serde_json::from_value(raw).map_err(|error| error.to_string())?;
    let body = serde_json::to_value(&parsed).map_err(|error| error.to_string())?;
    Ok((parsed, body))
}

/// Writes `proxy_url` into the `provider` object of a provider document body.
fn set_provider_proxy_url(body: &mut Value, proxy_url: String) {
    if let Some(Value::Object(provider)) = body.get_mut("provider") {
        provider.insert("proxy_url".to_owned(), Value::String(proxy_url));
    }
}

impl Doc {
    /// Validates `raw` as a `kind` document (the top-level `updated_at_ms` is lifted out) and
    /// derives its identity. Settings documents are built by [`Doc::settings`] instead.
    pub(crate) fn from_value(kind: SyncKind, raw: Value) -> Result<Self, String> {
        let updated_at_ms = raw.get("updated_at_ms").and_then(Value::as_i64);
        let doc = |key: &str, name: &str, workspace: Option<&str>, body| Self {
            kind,
            key: key.to_owned(),
            name: name.to_owned(),
            workspace: workspace.map(str::to_owned),
            updated_at_ms,
            row_ms: None,
            file: None,
            fingerprint: None,
            body,
        };
        let blank = |what: &str, value: &str| {
            if value.trim().is_empty() {
                Err(format!("{what} must not be blank"))
            } else {
                Ok(())
            }
        };
        Ok(match kind {
            SyncKind::Workspace => {
                let (parsed, body) = typed::<WorkspaceBody>(raw)?;
                let workspace = parsed.workspace;
                blank("workspace name", &workspace.name)?;
                doc(workspace.id.as_str(), &workspace.name, None, body)
            }
            SyncKind::Collection => {
                let (parsed, body) = typed::<CollectionBody>(raw)?;
                let collection = parsed.collection;
                blank("collection name", &collection.name)?;
                doc(
                    collection.id.as_str(),
                    &collection.name,
                    Some(collection.workspace_id.as_str()),
                    body,
                )
            }
            SyncKind::LooseRequests => {
                let (parsed, body) = typed::<LooseBody>(raw)?;
                doc(
                    &parsed.workspace_id,
                    "requests without a collection",
                    Some(&parsed.workspace_id),
                    body,
                )
            }
            SyncKind::Environment => {
                let (parsed, body) = typed::<EnvironmentBody>(raw)?;
                let environment = parsed.environment;
                blank("environment name", &environment.name)?;
                doc(
                    environment.id.as_str(),
                    &environment.name,
                    Some(environment.workspace_id.as_str()),
                    body,
                )
            }
            SyncKind::Globals => {
                let (parsed, body) = typed::<GlobalsBody>(raw)?;
                doc(
                    &parsed.workspace_id,
                    "globals",
                    Some(&parsed.workspace_id),
                    body,
                )
            }
            SyncKind::Mock => {
                let (parsed, body) = typed::<MockBody>(raw)?;
                let server = parsed.mock_server;
                doc(
                    &server.id,
                    &server.name,
                    Some(server.workspace_id.as_str()),
                    body,
                )
            }
            SyncKind::Flow => {
                let (parsed, body) = typed::<FlowBody>(raw)?;
                let flow = parsed.flow;
                doc(&flow.id, &flow.name, Some(flow.workspace_id.as_str()), body)
            }
            SyncKind::LlmProvider => {
                let proxy_url = raw.pointer("/provider/proxy_url").cloned();
                let (parsed, mut body) = typed::<ProviderBody>(raw)?;
                blank("provider id", &parsed.provider.id)?;
                // Parsing and writing back would read a `{{secret:..}}` placeholder as a masked
                // password: keep the text as the file wrote it.
                if let Some(Value::String(proxy_url)) = proxy_url {
                    set_provider_proxy_url(&mut body, proxy_url);
                }
                doc(&parsed.provider.id, &parsed.provider.name, None, body)
            }
            SyncKind::LlmAlias => {
                let (parsed, body) = typed::<AliasBody>(raw)?;
                blank("alias id", &parsed.alias.id)?;
                doc(&parsed.alias.id, &parsed.alias.name, None, body)
            }
            SyncKind::McpServer => {
                let (parsed, mut body) = typed::<McpBody>(raw)?;
                blank("MCP server name", &parsed.server.name)?;
                // `trusted` is a local decision: never exported, never imported.
                if let Some(Value::Object(server)) = body.get_mut("server") {
                    server.remove("trusted");
                }
                doc(&parsed.server.name, &parsed.server.name, None, body)
            }
            SyncKind::Skills => {
                let (_, body) = typed::<SkillsBody>(raw)?;
                doc("skills", "skills", None, body)
            }
            SyncKind::Settings => return Err("settings sections are built per section".to_owned()),
        })
    }

    /// A document built from local data.
    fn local<T: Serialize>(
        kind: SyncKind,
        body: &T,
        updated_at_ms: Option<i64>,
    ) -> Result<Self, AppError> {
        let raw =
            serde_json::to_value(body).map_err(|error| AppError::internal(error.to_string()))?;
        let mut doc = Self::from_value(kind, raw).map_err(AppError::internal)?;
        doc.updated_at_ms = updated_at_ms;
        Ok(doc)
    }

    /// [`Doc::local`] for an entity whose file records no timestamp: `row_ms` is the store's row
    /// time, used only when comparing with an incoming copy.
    fn local_with_row_time<T: Serialize>(
        kind: SyncKind,
        body: &T,
        row_ms: Option<i64>,
    ) -> Result<Self, AppError> {
        let mut doc = Self::local(kind, body, None)?;
        doc.row_ms = row_ms;
        Ok(doc)
    }

    /// One settings section as a document; `row_ms` is the time of its stored row, if any.
    pub(crate) fn settings(section: SettingsSection, value: Value, row_ms: Option<i64>) -> Self {
        Self {
            kind: SyncKind::Settings,
            key: section.as_str().to_owned(),
            name: section.as_str().to_owned(),
            workspace: None,
            updated_at_ms: None,
            row_ms,
            file: None,
            fingerprint: None,
            body: value,
        }
    }

    /// The time the merge rule compares: the recorded one, else the row time; 0 when unknown.
    pub(crate) fn merge_time_ms(&self) -> i64 {
        self.updated_at_ms.or(self.row_ms).unwrap_or(0)
    }
}

/// Orders children by position, then id, so documents do not depend on row creation order.
fn by_position<T>(items: &mut [T], key: impl Fn(&T) -> (i64, String)) {
    items.sort_by_key(key);
}

/// Everything configurable on this machine as documents, in a deterministic order.
pub(crate) fn collect_local(
    store: &WorkspaceStore,
    settings: &Settings,
    skills: &[SkillInfo],
) -> Result<Vec<Doc>, AppError> {
    let storage = |error: crate::workspace::StorageError| AppError::from(error);
    let connection = store.connection();
    let mut docs = Vec::new();

    for workspace in store.list_workspaces().map_err(storage)? {
        let wid = workspace.id.as_str();
        let stamps = store.sync_workspace_stamps(wid).map_err(storage)?;
        docs.push(Doc::local_with_row_time(
            SyncKind::Workspace,
            &WorkspaceBody {
                workspace: workspace.clone(),
            },
            stamps.workspaces.get(wid).copied(),
        )?);

        let mut requests = store.list_requests(wid).map_err(storage)?;
        // The agent-touched marker is this machine's trust decision: never exported.
        for request in &mut requests {
            request.agent_touched_ms = None;
        }
        for collection in store.list_collections(wid).map_err(storage)? {
            let mut folders = store
                .list_folders(&workspace.id, &collection.id)
                .map_err(storage)?;
            by_position(&mut folders, |folder| {
                (folder.position, folder.id.as_str().to_owned())
            });
            let mut members: Vec<Request> = requests
                .iter()
                .filter(|request| request.collection_id.as_ref() == Some(&collection.id))
                .cloned()
                .collect();
            by_position(&mut members, |request| {
                (request.position, request.id.as_str().to_owned())
            });
            let id = collection.id.as_str();
            let updated = [
                &stamps.collections,
                &stamps.folders,
                &stamps.collection_requests,
            ]
            .into_iter()
            .filter_map(|map| map.get(id).copied())
            .max();
            docs.push(Doc::local(
                SyncKind::Collection,
                &CollectionBody {
                    collection,
                    folders,
                    requests: members,
                },
                updated,
            )?);
        }

        let mut loose: Vec<Request> = requests
            .into_iter()
            .filter(|request| request.collection_id.is_none())
            .collect();
        if !loose.is_empty() {
            by_position(&mut loose, |request| {
                (request.position, request.id.as_str().to_owned())
            });
            docs.push(Doc::local_with_row_time(
                SyncKind::LooseRequests,
                &LooseBody {
                    workspace_id: wid.to_owned(),
                    requests: loose,
                },
                stamps.loose_requests.get(wid).copied(),
            )?);
        }

        for environment in store.list_environments(wid).map_err(storage)? {
            let updated = stamps.environments.get(environment.id.as_str()).copied();
            docs.push(Doc::local(
                SyncKind::Environment,
                &EnvironmentBody { environment },
                updated,
            )?);
        }
        let variables = store.list_global_variables(wid).map_err(storage)?;
        if !variables.is_empty() {
            docs.push(Doc::local(
                SyncKind::Globals,
                &GlobalsBody {
                    workspace_id: wid.to_owned(),
                    variables,
                },
                None,
            )?);
        }
        for server in store.list_mock_servers(wid).map_err(storage)? {
            let updated = Some(server.updated_at_ms);
            docs.push(Doc::local(
                SyncKind::Mock,
                &MockBody {
                    mock_server: server,
                },
                updated,
            )?);
        }
        for flow in store.list_flows(wid).map_err(storage)? {
            let updated = Some(flow.updated_at_ms);
            docs.push(Doc::local(SyncKind::Flow, &FlowBody { flow }, updated)?);
        }
    }

    let mut keys: Vec<SyncKey> = store::list_resolved_keys(connection)?
        .into_iter()
        .map(|resolved| SyncKey {
            id: resolved.key.id,
            provider_id: resolved.key.provider_id,
            name: resolved.key.name,
            secret: resolved.secret,
            enabled: resolved.key.enabled,
            position: resolved.key.position,
            created_at_ms: resolved.key.created_at_ms,
            updated_at_ms: resolved.key.updated_at_ms,
            notes: resolved.key.notes.unwrap_or_default(),
            group_name: resolved.key.group_name.filter(|name| !name.is_empty()),
            protocols: resolved.key.protocols.filter(|list| !list.is_empty()),
        })
        .collect();
    let mut models = store::list_models(connection, None)?;
    for mut provider in store::list_providers(connection)? {
        // Rotation settings at their defaults are left out (an absent field means the default on
        // import), so files written before they existed compare unchanged.
        provider.key_strategy = provider.key_strategy.filter(|s| *s != KeyStrategy::Primary);
        provider.key_cooldown = provider.key_cooldown.filter(|on| !on);
        // The review mark is this machine's: the importing side sets its own (C-S1).
        provider.review_pending_since_ms = None;
        let (own_keys, rest): (Vec<_>, Vec<_>) = keys
            .into_iter()
            .partition(|key| key.provider_id == provider.id);
        keys = rest;
        let (own_models, rest): (Vec<_>, Vec<_>) = models
            .into_iter()
            .partition(|model| model.provider_id == provider.id);
        models = rest;
        let updated = own_keys
            .iter()
            .map(|key| key.updated_at_ms)
            .chain([provider.updated_at_ms])
            .max();
        // `Provider` serializes its proxy URL masked; the document carries the real one so the
        // slot table can mask it on export (and Include secrets can carry the password).
        let proxy_url = provider.proxy_url.clone();
        let mut doc = Doc::local(
            SyncKind::LlmProvider,
            &ProviderBody {
                provider,
                keys: own_keys,
                models: own_models,
            },
            updated,
        )?;
        set_provider_proxy_url(&mut doc.body, proxy_url);
        docs.push(doc);
    }
    for mut server in store.list_agent_mcp_servers().map_err(storage)? {
        unmask_mcp_transport(store, &mut server.transport);
        let updated = Some(server.updated_at_ms);
        docs.push(Doc::local(
            SyncKind::McpServer,
            &McpBody { server },
            updated,
        )?);
    }

    let flags = store.list_agent_skill_flags().map_err(storage)?;
    let mut refs: Vec<SkillRef> = skills
        .iter()
        .map(|skill| SkillRef {
            name: skill.name.clone(),
            origin: skill.origin.clone(),
            enabled: flags.get(&skill.name).copied().unwrap_or(true),
        })
        .collect();
    refs.sort_by(|a, b| a.name.cmp(&b.name));
    docs.push(Doc::local(
        SyncKind::Skills,
        &SkillsBody { skills: refs },
        None,
    )?);

    let section_times = store.sync_section_times().map_err(storage)?;
    for section in SettingsSection::ALL
        .iter()
        .copied()
        .filter(|section| *section != SettingsSection::Sync)
    {
        let mut value = settings.section_value(section);
        // Local-only settings and the hints of local secrets stay on this machine.
        for path in local_only_fields(section) {
            remove_path(&mut value, path);
        }
        if section == SettingsSection::Ai {
            // The search keys travel with the `search` group (as values only under "Include
            // secrets", else as placeholders); they are stored apart from the settings.
            for (field, secret_ref) in [
                ("tavily_key", crate::secrets::SEARCH_TAVILY),
                ("brave_key", crate::secrets::SEARCH_BRAVE),
            ] {
                let key = store
                    .secret_get(secret_ref)
                    .map_err(storage)?
                    .unwrap_or_default();
                if let Some(Value::Object(search)) = value.get_mut("search") {
                    search.insert(field.to_owned(), Value::String(key));
                }
            }
        }
        docs.push(Doc::settings(
            section,
            value,
            section_times.get(section.as_str()).copied(),
        ));
    }

    docs.sort_by(|a, b| (a.kind, &a.key).cmp(&(b.kind, &b.key)));
    Ok(docs)
}

/// The settings section named `name`, if it is one that syncs.
pub(super) fn syncable_section(name: &str) -> Option<SettingsSection> {
    SettingsSection::ALL
        .iter()
        .copied()
        .find(|section| section.as_str() == name && *section != SettingsSection::Sync)
}

/// Section values of the settings documents by name (for `settings.json`).
pub(super) fn settings_map(docs: &[Doc]) -> BTreeMap<String, Value> {
    docs.iter()
        .filter(|doc| doc.kind == SyncKind::Settings)
        .map(|doc| (doc.key.clone(), doc.body.clone()))
        .collect()
}

/// Replaces the `{{secret:mcp.…}}` placeholders the store keeps in an MCP server's transport with
/// the stored values, so the export's own masking (by slot, unless `include_secrets`) sees the real
/// values exactly as it did before secrets left the table. A placeholder without a stored value
/// stays as it is (it exports as the same placeholder and imports empty with a warning).
fn unmask_mcp_transport(
    store: &crate::workspace::WorkspaceStore,
    transport: &mut McpTransportConfig,
) {
    let unmask = |text: &str| -> String {
        const OPEN: &str = "{{secret:";
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find(OPEN) {
            out.push_str(&rest[..start]);
            let after = &rest[start + OPEN.len()..];
            let Some(end) = after.find("}}") else {
                break;
            };
            let key = &after[..end];
            let value = key
                .starts_with("mcp.")
                .then(|| store.secret_get(key).ok().flatten())
                .flatten();
            match value {
                Some(value) => out.push_str(&value),
                None => out.push_str(&rest[start..start + OPEN.len() + end + 2]),
            }
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        out
    };
    match transport {
        McpTransportConfig::Stdio { args, env, .. } => {
            args.iter_mut().for_each(|arg| *arg = unmask(arg));
            env.values_mut().for_each(|value| *value = unmask(value));
        }
        McpTransportConfig::Http { url, headers } => {
            *url = unmask(url);
            headers
                .values_mut()
                .for_each(|value| *value = unmask(value));
        }
    }
}
