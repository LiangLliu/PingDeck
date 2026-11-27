//! Comparing incoming documents with local ones: change classes and the last-writer-wins rule.
//!
//! Merge rule: contents are compared with secrets refilled from the local copy and timestamps
//! ignored. When they differ, the copy with the newer `updated_at_ms` wins and the incoming copy
//! wins ties; a strictly newer local copy is a [`SyncChange::Conflict`] and is kept. An incoming
//! entity without a timestamp takes the manifest's export time (see `read_dir`); a local one
//! without a timestamp uses its store row time (workspaces, loose requests, settings) or counts
//! as 0 (globals, aliases), so it is never newer.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::Value;

use crate::app::settings_service::{keep_local_fields, local_only_fields};
use crate::settings::{Settings, SettingsSection, put_path};
use crate::workspace::MockServer;

use super::docs::{Doc, MockBody, syncable_section};
use super::secrets;
use super::{HostChange, SyncChange, SyncDiffEntry, SyncKind};

/// A differing entity with both of its documents.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub(crate) entry: SyncDiffEntry,
    pub(crate) local: Option<Doc>,
    /// The incoming document with its secret placeholders already filled from the local copy.
    pub(crate) incoming: Option<Doc>,
    /// Why this entity cannot be applied (an invalid settings section).
    pub(crate) problem: Option<String>,
}

fn kind_label(kind: SyncKind) -> &'static str {
    match kind {
        SyncKind::Workspace => "workspace",
        SyncKind::Collection => "collection",
        SyncKind::LooseRequests => "loose requests",
        SyncKind::Environment => "environment",
        SyncKind::Globals => "globals",
        SyncKind::Mock => "mock server",
        SyncKind::Flow => "flow",
        SyncKind::LlmProvider => "LLM provider",
        SyncKind::LlmAlias => "LLM alias",
        SyncKind::McpServer => "MCP server",
        SyncKind::Skills => "skills",
        SyncKind::Settings => "settings",
    }
}

/// A settings section as it would read after taking `incoming` (the local-only fields kept,
/// missing fields defaulted, values validated).
fn effective_section(
    local: &Settings,
    section: SettingsSection,
    incoming: &Value,
) -> Result<Value, String> {
    let mut value = incoming.clone();
    keep_local_fields(local, section, &mut value);
    local
        .with_section_value(section, &value)
        .map(|settings| settings.section_value(section))
        .map_err(|error| error.message().to_owned())
}

/// The local-only fields of `section` that `incoming` sets to something else than the local
/// value. Secret hints are left out: they describe each machine's own secrets and always differ.
pub(crate) fn refused_fields(
    local: &Settings,
    section: SettingsSection,
    incoming: &Value,
) -> Vec<&'static str> {
    let mine = local.section_value(section);
    local_only_fields(section)
        .iter()
        .copied()
        .filter(|name| !name.ends_with("_hint"))
        .filter(|name| {
            let pointer = format!("/{}", name.replace('.', "/"));
            incoming.pointer(&pointer).is_some_and(|theirs| {
                match mine.pointer(&pointer) {
                    Some(own) => own != theirs,
                    // A field at its default is not stored: the file's value differs when taking
                    // it would change the section.
                    None => {
                        let mut probe = mine.clone();
                        put_path(&mut probe, name, theirs);
                        local
                            .with_section_value(section, &probe)
                            .map_or(true, |taken| taken.section_value(section) != mine)
                    }
                }
            })
        })
        .collect()
}

fn strip_stamps(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.remove("updated_at_ms");
            fields.remove("created_at_ms");
            fields.values_mut().for_each(strip_stamps);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_stamps),
        _ => {}
    }
}

/// Drops the provider id of every AI provider target in a collection or loose-requests body: the
/// id is machine-local (an import re-links it), while the provider name and base-URL
/// hint still show a changed target.
fn strip_provider_ids(body: &mut Value) {
    let Some(requests) = body.get_mut("requests").and_then(Value::as_array_mut) else {
        return;
    };
    for target in requests
        .iter_mut()
        .filter_map(|request| request.pointer_mut("/ai/target"))
        .filter(|target| target.get("kind").and_then(Value::as_str) == Some("provider"))
    {
        if let Value::Object(target) = target {
            target.remove("provider_id");
        }
    }
}

/// Provider fields the store owns (a scan, a model listing): exported, never imported.
const RUNTIME_PROVIDER_FIELDS: [&str; 8] = [
    "gateway_family",
    "quota_per_unit",
    "display_in_currency",
    "sniff_evidence",
    "model_drift",
    "needs_resniff",
    "sniff_summary",
    "review_pending_since_ms",
];

/// Both bodies reduced to what counts as a difference.
fn comparable(kind: SyncKind, local: &Doc, incoming: &Doc) -> (Value, Value) {
    let (mut local, mut incoming) = (local.body.clone(), incoming.body.clone());
    match kind {
        // Requests carry the mode and AI settings; a provider target's id is per machine.
        SyncKind::Collection | SyncKind::LooseRequests => {
            for body in [&mut local, &mut incoming] {
                strip_provider_ids(body);
            }
        }
        // Ids are machine-local and the name is the identity; `enabled` and `trusted` stay local.
        SyncKind::McpServer => {
            for body in [&mut local, &mut incoming] {
                if let Some(Value::Object(server)) = body.get_mut("server") {
                    server.remove("id");
                    server.remove("enabled");
                }
            }
        }
        // Import never deletes keys, so local keys the files do not list are not differences.
        SyncKind::LlmProvider => {
            let listed: HashSet<String> = incoming
                .get("keys")
                .and_then(Value::as_array)
                .map(|keys| {
                    keys.iter()
                        .filter_map(|key| key.get("id").and_then(Value::as_str).map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(Value::Array(keys)) = local.get_mut("keys") {
                keys.retain(|key| {
                    key.get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| listed.contains(id))
                });
            }
            // An empty secret leaves the stored one, and a known key keeps its local on/off
            // state; what a scan, a balance read or a model listing wrote is this machine's.
            for body in [&mut local, &mut incoming] {
                if let Some(Value::Object(provider)) = body.get_mut("provider") {
                    for field in RUNTIME_PROVIDER_FIELDS {
                        provider.remove(field);
                    }
                }
                if let Some(Value::Array(keys)) = body.get_mut("keys") {
                    for key in keys.iter_mut().filter_map(Value::as_object_mut) {
                        key.remove("enabled");
                        if key.get("secret").and_then(Value::as_str) == Some("") {
                            key.remove("secret");
                        }
                    }
                }
            }
            let blank: HashSet<String> = incoming
                .get("keys")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|key| key.get("secret").is_none())
                .filter_map(|key| key.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect();
            if let Some(Value::Array(keys)) = local.get_mut("keys") {
                for key in keys.iter_mut().filter_map(Value::as_object_mut) {
                    if key
                        .get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| blank.contains(id))
                    {
                        key.remove("secret");
                    }
                }
            }
        }
        _ => {}
    }
    strip_stamps(&mut local);
    strip_stamps(&mut incoming);
    (local, incoming)
}

/// Top-level fields that differ, as text.
fn differing_fields(local: &Value, incoming: &Value) -> String {
    let (Value::Object(local), Value::Object(incoming)) = (local, incoming) else {
        return "content".to_owned();
    };
    let names: BTreeSet<&String> = local.keys().chain(incoming.keys()).collect();
    let differing: Vec<&str> = names
        .into_iter()
        .filter(|name| local.get(*name) != incoming.get(*name))
        .map(String::as_str)
        .collect();
    differing.join(", ")
}

fn skill_flags(doc: &Doc) -> HashMap<String, bool> {
    doc.body
        .get("skills")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|skill| {
            Some((
                skill.get("name")?.as_str()?.to_owned(),
                skill.get("enabled")?.as_bool()?,
            ))
        })
        .collect()
}

/// `Ok(None)` when equal, `Ok(Some(what differs))`, `Err(why the incoming copy is unusable)`.
fn differs(local: &Doc, incoming: &Doc, settings: &Settings) -> Result<Option<String>, String> {
    match incoming.kind {
        SyncKind::Skills => {
            let (local, incoming) = (skill_flags(local), skill_flags(incoming));
            let mut names: Vec<&String> = incoming
                .iter()
                .filter(|(name, enabled)| local.get(*name).is_some_and(|mine| mine != *enabled))
                .map(|(name, _)| name)
                .collect();
            names.sort();
            Ok((!names.is_empty()).then(|| {
                format!(
                    "enabled flag differs for {}",
                    names
                        .into_iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }))
        }
        SyncKind::Settings => {
            let section = syncable_section(&incoming.key)
                .ok_or_else(|| format!("unknown settings section {}", incoming.key))?;
            let wanted = effective_section(
                settings,
                section,
                &secrets::without_search_keys(&incoming.body),
            )?;
            let current = settings.section_value(section);
            let mut fields = (wanted != current).then(|| differing_fields(&current, &wanted));
            // The search keys are not settings: a file value that is set and differs counts.
            let (theirs, mine) = (
                secrets::search_keys(&incoming.body),
                secrets::search_keys(&local.body),
            );
            if (!theirs.0.is_empty() && theirs.0 != mine.0)
                || (!theirs.1.is_empty() && theirs.1 != mine.1)
            {
                fields = Some(match fields {
                    Some(fields) => format!("{fields}, search keys"),
                    None => "search keys".to_owned(),
                });
            }
            Ok(fields)
        }
        kind => {
            let (local, incoming) = comparable(kind, local, incoming);
            Ok((local != incoming).then(|| differing_fields(&local, &incoming)))
        }
    }
}

/// The origin of a provider URL as a label: `host[:port]` for https on its default port, else
/// `scheme://host[:port]`. Parsed with the `url` crate (WHATWG, the same reader the HTTP client
/// uses), so no spelling (`\@`, userinfo, upper case) shows one host to the check and another to
/// the client. A `{{secret:…}}` placeholder stands in for userinfo or query text and is replaced
/// by a dummy before parsing. `None` when the text is not an absolute URL with a host.
fn origin_of(url: &str) -> Option<String> {
    const OPEN: &str = "{{secret:";
    let mut plain = String::with_capacity(url.len());
    let mut rest = url;
    while let Some(start) = rest.find(OPEN) {
        let Some(end) = rest[start..].find("}}") else {
            break;
        };
        plain.push_str(&rest[..start]);
        plain.push('x');
        rest = &rest[start + end + 2..];
    }
    plain.push_str(rest);
    let parsed = url::Url::parse(&plain).ok()?;
    let host = parsed.host_str()?;
    let port = parsed
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Some(if parsed.scheme() == "https" {
        format!("{host}{port}")
    } else {
        format!("{}://{host}{port}", parsed.scheme())
    })
}

/// The label of a provider URL for comparing: an unparsable text is its own label (so only an
/// unchanged text compares equal).
fn origin_label(url: &str) -> String {
    origin_of(url).unwrap_or_else(|| format!("unreadable URL {url}"))
}

/// The origins of a provider document's outbound URLs: the base URL and the custom balance URL,
/// the set the store's `outbound_hosts` compares before it asks to confirm a move.
fn outbound_origins(doc: &Doc) -> BTreeSet<String> {
    ["/provider/base_url", "/provider/custom_balance/url"]
        .iter()
        .filter_map(|pointer| doc.body.pointer(pointer).and_then(Value::as_str))
        .map(origin_label)
        .collect()
}

/// A provider that the incoming document moves to another origin (scheme, host or port) of its
/// base URL, adds an outbound origin (a custom balance URL on a new host), or moves to another
/// proxy than the local one (a proxy set where there was none included). An unreadable incoming
/// URL counts as a move.
fn host_change(local: Option<&Doc>, incoming: &Doc) -> Option<HostChange> {
    if incoming.kind != SyncKind::LlmProvider {
        return None;
    }
    let local = local?;
    let url = |doc: &Doc, field: &str| {
        doc.body
            .pointer(&format!("/provider/{field}"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    if let (Some(from), Some(to)) = (url(local, "base_url"), url(incoming, "base_url")) {
        let (from_host, to_host) = (origin_label(&from), origin_label(&to));
        if from_host != to_host {
            return Some(HostChange { from_host, to_host });
        }
    }
    let known = outbound_origins(local);
    if let Some(to_host) = outbound_origins(incoming)
        .into_iter()
        .find(|origin| !known.contains(origin))
    {
        let from_host = known.into_iter().collect::<Vec<_>>().join(", ");
        return Some(HostChange { from_host, to_host });
    }
    let proxy = |doc: &Doc| {
        url(doc, "proxy_url")
            .filter(|text| !text.is_empty())
            .map_or_else(
                || "no proxy".to_owned(),
                |text| format!("proxy {}", origin_label(&text)),
            )
    };
    let (from_host, to_host) = (proxy(local), proxy(incoming));
    (from_host != to_host).then_some(HostChange { from_host, to_host })
}

/// The command line of a stdio MCP server document (`command` and `args`), if it is one.
fn mcp_command(doc: &Doc) -> Option<(String, Vec<String>)> {
    let transport = doc.body.pointer("/server/transport")?;
    let command = transport.get("command")?.as_str()?.to_owned();
    let args = transport
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some((command, args))
}

/// The base URL a mock server forwards unmatched requests to, when proxying is on.
pub(super) fn mock_proxy_target(server: &MockServer) -> Option<&str> {
    (server.proxy_enabled && !server.proxy_base_url.is_empty())
        .then_some(server.proxy_base_url.as_str())
}

/// The mock server of a mock document, if it parses.
fn mock_server_of(doc: &Doc) -> Option<MockServer> {
    serde_json::from_value::<MockBody>(doc.body.clone())
        .ok()
        .map(|body| body.mock_server)
}

/// What a mock import adds that can send traffic out: a proxy target the local copy did not have,
/// or callback URLs it did not have.
fn mock_outbound_reason(local: Option<&Doc>, incoming: &Doc) -> Option<String> {
    let server = mock_server_of(incoming)?;
    let mine = local.and_then(mock_server_of);
    if let Some(target) = mock_proxy_target(&server)
        && mine.as_ref().and_then(mock_proxy_target) != Some(target)
    {
        return Some(format!(
            "Mock proxy target changes to {target}; proxying stays off until you enable it"
        ));
    }
    let known: HashSet<&str> = mine
        .iter()
        .flat_map(|server| &server.callbacks)
        .map(|callback| callback.url.as_str())
        .collect();
    server
        .callbacks
        .iter()
        .find(|callback| !known.contains(callback.url.as_str()))
        .map(|callback| format!("Mock callback URL changes to {}", callback.url))
}

/// Why importing `incoming` over `local` needs a look before it is applied: a stdio MCP
/// server that is new or runs another command line, a provider moving to another host or proxy,
/// or a mock server with a new proxy target or callback URL.
fn review_reason(
    local: Option<&Doc>,
    incoming: &Doc,
    moved: Option<&HostChange>,
) -> Option<String> {
    if let Some(moved) = moved {
        return Some(format!(
            "Host changes from {} to {}",
            moved.from_host, moved.to_host
        ));
    }
    if incoming.kind == SyncKind::Mock {
        return mock_outbound_reason(local, incoming);
    }
    if incoming.kind != SyncKind::McpServer {
        return None;
    }
    let command = mcp_command(incoming)?;
    match local.and_then(mcp_command) {
        None if local.is_none() => Some(format!("New MCP command: {}", command.0)),
        Some(mine) if mine != command => Some(format!("MCP command changes to {}", command.0)),
        _ => None,
    }
}

/// Classifies every difference between `local` and `incoming` (equal entities are left out) and
/// fills the incoming secrets from local values. A provider the files move to another host gets
/// no local values (a placeholder resolves to empty, which leaves the stored secret; the import
/// switches the keys off) unless its id is in `keep_key_providers`.
/// Warnings name placeholders without a local value. A local-only entity in `in_folder` (it was in
/// the folder at this machine's last export or import) is marked `deleted_in_folder`.
pub(crate) fn compare(
    local: &[Doc],
    incoming: Vec<Doc>,
    settings: &Settings,
    keep_key_providers: &[String],
    in_folder: &HashSet<(SyncKind, String)>,
) -> (Vec<Item>, Vec<String>) {
    let by_key: HashMap<(SyncKind, &str), &Doc> = local
        .iter()
        .map(|doc| ((doc.kind, doc.key.as_str()), doc))
        .collect();
    let mut seen: HashSet<(SyncKind, String)> = HashSet::new();
    let mut items = Vec::new();
    let mut warnings = Vec::new();

    for mut doc in incoming {
        seen.insert((doc.kind, doc.key.clone()));
        let mine = by_key.get(&(doc.kind, doc.key.as_str())).copied();
        let label = format!("{} {}", kind_label(doc.kind), doc.name);
        let moved = host_change(mine, &doc);
        let keep_keys = keep_key_providers.contains(&doc.key);
        // Local secrets never follow a provider to a host the user did not approve.
        let fill_from = match (&moved, keep_keys) {
            (Some(_), false) => None,
            _ => mine.map(|local| &local.body),
        };
        secrets::fill(doc.kind, &label, &mut doc.body, fill_from, &mut warnings);
        let mut local_setting_kept = false;
        if let Some(section) = syncable_section(&doc.key).filter(|_| doc.kind == SyncKind::Settings)
        {
            for name in refused_fields(settings, section, &doc.body) {
                local_setting_kept = true;
                warnings.push(format!(
                    "settings {}: {name} is never imported; the local value stays",
                    doc.key
                ));
            }
        }
        let review = review_reason(mine, &doc, moved.as_ref());
        let entry = |change, detail: String| SyncDiffEntry {
            kind: doc.kind,
            key: doc.key.clone(),
            name: doc.name.clone(),
            change,
            local_updated_at_ms: mine.and_then(|local| local.updated_at_ms.or(local.row_ms)),
            incoming_updated_at_ms: doc.updated_at_ms,
            detail: match &moved {
                Some(_) if !keep_keys => {
                    format!("{detail}; host changes: your key will not be sent to the new host")
                }
                _ => detail,
            },
            host_change: moved.clone(),
            review: review.is_some(),
            review_reason: review.clone(),
            local_setting_kept,
            deleted_in_folder: false,
        };
        let Some(mine) = mine else {
            if doc.kind == SyncKind::McpServer {
                warnings.push(format!(
                    "MCP server {} will be imported disabled: enable it after reviewing its command",
                    doc.name
                ));
            }
            items.push(Item {
                entry: entry(SyncChange::Added, "only in the files".to_owned()),
                local: None,
                incoming: Some(doc.clone()),
                problem: None,
            });
            continue;
        };
        match differs(mine, &doc, settings) {
            // Only local-only fields differ: the row still shows that the local values stay.
            Ok(None) if local_setting_kept => items.push(Item {
                entry: entry(
                    SyncChange::Changed,
                    "only device settings differ; the local values stay".to_owned(),
                ),
                local: Some(mine.clone()),
                incoming: Some(doc.clone()),
                problem: None,
            }),
            Ok(None) => {}
            Ok(Some(fields)) => {
                let newer_local = doc
                    .updated_at_ms
                    .is_some_and(|incoming| mine.merge_time_ms() > incoming);
                let (change, detail) = if newer_local {
                    (
                        SyncChange::Conflict,
                        format!("differs in {fields}; the local copy is newer and is kept"),
                    )
                } else {
                    (
                        SyncChange::Changed,
                        format!("differs in {fields}; the incoming copy replaces the local one"),
                    )
                };
                items.push(Item {
                    entry: entry(change, detail),
                    local: Some(mine.clone()),
                    incoming: Some(doc.clone()),
                    problem: None,
                });
            }
            Err(problem) => {
                warnings.push(format!("{label}: {problem}"));
                items.push(Item {
                    entry: entry(
                        SyncChange::Changed,
                        format!("cannot be imported: {problem}"),
                    ),
                    local: Some(mine.clone()),
                    incoming: Some(doc.clone()),
                    problem: Some(problem),
                });
            }
        }
    }

    for doc in local {
        // Skill folders and settings sections exist on every machine; they are never "removed".
        if matches!(doc.kind, SyncKind::Skills | SyncKind::Settings)
            || seen.contains(&(doc.kind, doc.key.clone()))
        {
            continue;
        }
        let deleted_in_folder = in_folder.contains(&(doc.kind, doc.key.clone()));
        let detail = if deleted_in_folder {
            "deleted in the folder by another machine; deleted here only when removals are applied"
        } else {
            "only in local data; deleted only when removals are applied"
        };
        items.push(Item {
            entry: SyncDiffEntry {
                kind: doc.kind,
                key: doc.key.clone(),
                name: doc.name.clone(),
                change: SyncChange::Removed,
                local_updated_at_ms: doc.updated_at_ms.or(doc.row_ms),
                incoming_updated_at_ms: None,
                detail: detail.to_owned(),
                host_change: None,
                review: false,
                review_reason: None,
                local_setting_kept: false,
                deleted_in_folder,
            },
            local: Some(doc.clone()),
            incoming: None,
            problem: None,
        });
    }
    items.sort_by(|a, b| (a.entry.kind, &a.entry.key).cmp(&(b.entry.kind, &b.entry.key)));
    (items, warnings)
}
