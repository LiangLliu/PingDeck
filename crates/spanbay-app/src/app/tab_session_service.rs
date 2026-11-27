//! Tab-session wire format and restore projection.
//!
//! A shell remembers its open tabs per workspace. This module owns the stored format (a typed
//! [`TabSession`], current `version` 2), the reader for the legacy version-1 payloads the React
//! workbench wrote (camelCase drafts, tolerant of malformed fields), and the restore rules:
//! a tab whose saved request was deleted keeps only unsaved edits, the active tab falls back to
//! the first one, and a restore merges with tabs the user opened while it was running.
//!
//! Only UI-neutral state is stored: per tab its id, the saved request it shows, the history entry
//! it was opened from, whether it has unsaved edits and — for tabs that cannot be rebuilt from a
//! stored request (dirty edits, new or history tabs) — the draft. Presentation state (the last
//! execution result, multipart pick/validation state, scroll or panel layout) is not stored.
//!
//! A draft's proxy password is never stored: the draft serializes its proxy URL masked. A typed
//! password the request has not saved is therefore lost; the stored tab says so, and the restore
//! removes the password from the draft's proxy URL and flags the tab (`proxy_password_lost`), so
//! a send can neither use the saved request's older password nor the literal mask.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

use spanbay_base::proxy_url;

use super::request_model_service::draft_normalize;
use super::workspace_service::{load_tab_session_impl, save_tab_session_impl};
use super::{AppState, command_error};
use crate::error::AppError;
use crate::workspace::{
    Assertion, AuthConfig, HttpMethod, KeyValue, MAX_TAB_SESSION_BYTES, NetworkOptions,
    ProtocolKind, Request, RequestBody, WorkspaceId,
};

/// The format version [`tab_session_serialize`] writes.
pub const TAB_SESSION_VERSION: u32 = 2;

/// The oldest format [`tab_session_parse`] still reads (the React workbench's original payload).
const LEGACY_VERSION: u32 = 1;

/// The persisted open-tab state of one workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabSession {
    /// Format version; must be [`TAB_SESSION_VERSION`] when saving.
    pub version: u32,
    /// The tab that was in front, if any.
    pub active_tab_id: Option<String>,
    /// Open tabs in strip order.
    pub tabs: Vec<SessionTab>,
}

/// The editor section a tab shows. Ids only: a stored value this build does not know
/// reads back as `None`, so a newer or older payload never costs a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorTab {
    /// Query parameters.
    Params,
    /// Headers.
    Headers,
    /// Authorization.
    Auth,
    /// Body.
    Body,
    /// Assertions.
    Assertions,
    /// Scripts.
    Scripts,
    /// Network options.
    Network,
    /// The request's description.
    Docs,
    /// AI API messages.
    Messages,
    /// AI API parameters.
    Parameters,
    /// AI API tools.
    Tools,
    /// AI API raw JSON.
    RawJson,
}

/// An [`EditorTab`] that reads as `None` when it is not a known id (or not a string).
fn lenient_editor_tab<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<EditorTab>, D::Error> {
    Ok(Option::<Value>::deserialize(deserializer)?
        .and_then(|value| EditorTab::deserialize(value).ok()))
}

/// A flag that reads as `false` when it is not a boolean.
fn lenient_flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(Option::<Value>::deserialize(deserializer)?
        .and_then(|value| value.as_bool())
        .unwrap_or(false))
}

/// `skip_serializing_if` for a flag whose absence reads as `false`.
fn is_false(flag: &bool) -> bool {
    !*flag
}

/// One open tab of a [`TabSession`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTab {
    /// Shell-assigned tab identity.
    pub tab_id: String,
    /// The saved request the tab edits; `None` for new and history tabs.
    pub request_id: Option<String>,
    /// The history entry the tab was opened from.
    pub history_entry_id: Option<String>,
    /// Whether the draft differs from the saved request.
    pub dirty: bool,
    /// The editable request. Saving embeds it only when it cannot be rebuilt from the store
    /// (`dirty` or no `request_id`), so a shell may always pass it.
    pub draft: Option<Request>,
    /// The editor section the tab showed; absent in sessions saved before it existed.
    #[serde(
        default,
        deserialize_with = "lenient_editor_tab",
        skip_serializing_if = "Option::is_none"
    )]
    pub editor_tab: Option<EditorTab>,
    /// Whether the tab is a preview tab (replaced by the next open); absent reads `false`.
    #[serde(default, deserialize_with = "lenient_flag")]
    pub preview: bool,
    /// Set by the stored format only: the stored draft had a typed proxy password, which is not
    /// stored. A shell never needs to send it.
    #[serde(
        default,
        deserialize_with = "lenient_flag",
        skip_serializing_if = "is_false"
    )]
    pub proxy_password_lost: bool,
}

/// A tab ready to show: its draft is always present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoredTab {
    /// Shell-assigned tab identity.
    pub tab_id: String,
    /// The saved request the tab edits; `None` for new, history and orphaned (deleted) tabs.
    pub request_id: Option<String>,
    /// The history entry the tab was opened from.
    pub history_entry_id: Option<String>,
    /// Whether the draft differs from the saved request.
    pub dirty: bool,
    /// The editable request, normalised to the workspace (id and workspace id always set).
    pub draft: Request,
    /// The editor section the tab showed, echoed from the session.
    #[serde(
        default,
        deserialize_with = "lenient_editor_tab",
        skip_serializing_if = "Option::is_none"
    )]
    pub editor_tab: Option<EditorTab>,
    /// Whether the tab is a preview tab, echoed from the session.
    #[serde(default, deserialize_with = "lenient_flag")]
    pub preview: bool,
    /// The draft's proxy password could not be restored (it is never stored) and was removed
    /// from its proxy URL: the user has to enter it again before sending.
    #[serde(
        default,
        deserialize_with = "lenient_flag",
        skip_serializing_if = "is_false"
    )]
    pub proxy_password_lost: bool,
}

/// Why a saved tab was not restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DropReason {
    /// The saved request no longer exists and the tab had no unsaved edits.
    RequestDeleted,
    /// A new or history tab without a stored draft.
    MissingDraft,
    /// The shell already has a tab with this id or for this request open.
    AlreadyOpen,
}

/// A saved tab that was not restored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedTab {
    /// Id of the dropped tab.
    pub tab_id: String,
    /// The request it pointed at, if any.
    pub request_id: Option<String>,
    /// The reason it was dropped.
    pub reason: DropReason,
}

/// A [`TabSession`] projected against the current requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoredSession {
    /// Tabs to open, in order.
    pub tabs: Vec<RestoredTab>,
    /// The tab to focus: the saved one when it survived, else the first tab.
    pub active_tab_id: Option<String>,
    /// Saved tabs that were not restored.
    pub dropped: Vec<DroppedTab>,
}

/// Serializes `session` to the stored payload (current version).
///
/// Drafts of clean saved tabs are left out: they are rebuilt from the stored request on restore.
pub fn tab_session_serialize(session: &TabSession) -> Result<String, AppError> {
    if session.version != TAB_SESSION_VERSION {
        return Err(AppError::validation(format!(
            "Cannot save a tab session with format version {}; this build writes version {TAB_SESSION_VERSION}.",
            session.version
        )));
    }
    #[derive(Serialize)]
    struct Stored<'a> {
        version: u32,
        active_tab_id: Option<&'a str>,
        tabs: Vec<StoredTab<'a>>,
    }
    #[derive(Serialize)]
    struct StoredTab<'a> {
        tab_id: &'a str,
        request_id: Option<&'a str>,
        history_entry_id: Option<&'a str>,
        dirty: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        draft: Option<&'a Request>,
        #[serde(skip_serializing_if = "Option::is_none")]
        editor_tab: Option<EditorTab>,
        #[serde(skip_serializing_if = "is_false")]
        preview: bool,
        #[serde(skip_serializing_if = "is_false")]
        proxy_password_lost: bool,
    }
    let stored = Stored {
        version: session.version,
        active_tab_id: session.active_tab_id.as_deref(),
        tabs: session
            .tabs
            .iter()
            .map(|tab| {
                let draft = tab
                    .draft
                    .as_ref()
                    .filter(|_| tab.dirty || tab.request_id.is_none());
                StoredTab {
                    tab_id: &tab.tab_id,
                    request_id: tab.request_id.as_deref(),
                    history_entry_id: tab.history_entry_id.as_deref(),
                    dirty: tab.dirty,
                    draft,
                    editor_tab: tab.editor_tab,
                    preview: tab.preview,
                    // The draft serializes its proxy URL masked: a typed password does not survive.
                    proxy_password_lost: tab.proxy_password_lost
                        || draft.is_some_and(typed_proxy_password),
                }
            })
            .collect(),
    };
    serde_json::to_string(&stored).map_err(|error| {
        AppError::validation(format!("Could not serialize the tab session: {error}"))
    })
}

/// Parses a stored payload (version 1 or 2) into the current [`TabSession`].
///
/// Like the React workbench, an entry that is malformed is skipped, and a draft with bad or
/// missing fields falls back to the default of each field. A payload that is not JSON, has no
/// `tabs` array or an unknown `version` is an error (a newer version is
/// [`AppError::Unsupported`]) so callers never overwrite what they cannot read.
pub fn tab_session_parse(text: &str) -> Result<TabSession, AppError> {
    if text.len() > MAX_TAB_SESSION_BYTES {
        return Err(unreadable(&format!(
            "it is {} bytes, over the {MAX_TAB_SESSION_BYTES} byte limit",
            text.len()
        )));
    }
    let Value::Object(root) =
        serde_json::from_str(text).map_err(|error| unreadable(&error.to_string()))?
    else {
        return Err(unreadable("it is not a JSON object"));
    };
    let Some(Value::Array(entries)) = root.get("tabs") else {
        return Err(unreadable("it has no tab list"));
    };
    match root.get("version").and_then(Value::as_u64) {
        Some(1) => Ok(TabSession {
            version: TAB_SESSION_VERSION,
            active_tab_id: string_field(&root, "active"),
            tabs: entries.iter().filter_map(legacy_tab).collect(),
        }),
        Some(2) => Ok(TabSession {
            version: TAB_SESSION_VERSION,
            active_tab_id: string_field(&root, "active_tab_id"),
            tabs: entries
                .iter()
                .filter_map(|entry| SessionTab::deserialize(entry).ok())
                .collect(),
        }),
        Some(newer) if newer > u64::from(TAB_SESSION_VERSION) => {
            Err(AppError::unsupported(format!(
                "The saved tab session was written by a newer SpanBay (format version {newer}; this build reads versions {LEGACY_VERSION} to {TAB_SESSION_VERSION})."
            )))
        }
        _ => Err(unreadable("its format version is not supported")),
    }
}

/// Loads, parses and restores the saved tabs of a workspace; `None` when nothing is saved.
pub fn tab_session_load(
    state: &AppState,
    workspace_id: &str,
) -> Result<Option<RestoredSession>, AppError> {
    let Some(text) =
        load_tab_session_impl(state, workspace_id.to_owned())?.filter(|text| !text.is_empty())
    else {
        return Ok(None);
    };
    let session = tab_session_parse(&text)?;
    tab_session_restore(state, workspace_id, &session).map(Some)
}

/// Serializes `session` and stores it as the workspace's tab session (8 MiB cap).
pub fn tab_session_save(
    state: &AppState,
    workspace_id: &str,
    session: &TabSession,
) -> Result<(), AppError> {
    save_tab_session_impl(
        state,
        workspace_id.to_owned(),
        tab_session_serialize(session)?,
    )
}

/// Projects `session` against the workspace's current requests.
pub fn tab_session_restore(
    state: &AppState,
    workspace_id: &str,
    session: &TabSession,
) -> Result<RestoredSession, AppError> {
    let workspace_id = WorkspaceId::new(workspace_id).map_err(command_error)?;
    let requests = state
        .store()
        .list_requests(workspace_id.as_str())
        .map_err(command_error)?;
    restore_tabs(session, &workspace_id, &requests)
}

/// Adds restored tabs to the tabs the shell already has open (it may have opened some while the
/// restore was running): a restored tab is skipped when its id or its request is already open
/// or was already kept from the restored list itself. The active tab is the shell's, else the
/// restored one, else the first tab, so tabs never end up without one.
#[must_use]
pub fn tab_session_merge(
    open_tabs: Vec<RestoredTab>,
    open_active_tab_id: Option<String>,
    restored: RestoredSession,
) -> RestoredSession {
    let mut seen_ids: HashSet<String> = open_tabs.iter().map(|tab| tab.tab_id.clone()).collect();
    let mut seen_requests: HashSet<String> = open_tabs
        .iter()
        .filter_map(|tab| tab.request_id.clone())
        .collect();
    let mut tabs = open_tabs;
    let mut dropped = restored.dropped;
    for tab in restored.tabs {
        let duplicate = seen_ids.contains(&tab.tab_id)
            || tab
                .request_id
                .as_ref()
                .is_some_and(|id| seen_requests.contains(id));
        if duplicate {
            dropped.push(DroppedTab {
                tab_id: tab.tab_id,
                request_id: tab.request_id,
                reason: DropReason::AlreadyOpen,
            });
        } else {
            seen_ids.insert(tab.tab_id.clone());
            seen_requests.extend(tab.request_id.clone());
            tabs.push(tab);
        }
    }
    let present = |id: Option<String>| id.filter(|id| tabs.iter().any(|tab| &tab.tab_id == id));
    let active_tab_id = present(open_active_tab_id)
        .or_else(|| present(restored.active_tab_id))
        .or_else(|| tabs.first().map(|tab| tab.tab_id.clone()));
    RestoredSession {
        tabs,
        active_tab_id,
        dropped,
    }
}

/// The restore rules against an explicit request list.
///
/// A clean tab is rebuilt from the stored request (a saved draft may be stale); the saved draft
/// is used only for dirty tabs, which hold edits the store does not have.
fn restore_tabs(
    session: &TabSession,
    workspace_id: &WorkspaceId,
    requests: &[Request],
) -> Result<RestoredSession, AppError> {
    let by_id: HashMap<&str, &Request> = requests
        .iter()
        .map(|request| (request.id.as_str(), request))
        .collect();
    let pin = |draft: Request| draft_normalize(draft, workspace_id.as_str());
    let mut tabs = Vec::new();
    let mut dropped = Vec::new();
    for entry in &session.tabs {
        let draft = entry.draft.clone().map(pin).transpose()?;
        // A stored draft whose proxy password is gone: the stored flag, or a masked password in
        // a tab no saved request backs (no stored password exists for it).
        let backed = entry
            .request_id
            .as_deref()
            .is_some_and(|id| by_id.contains_key(id));
        let draft_lost = |draft: &Request| {
            entry.proxy_password_lost || (!backed && masked_proxy_password(draft))
        };
        let restored = match entry.request_id.as_deref() {
            Some(request_id) => match by_id.get(request_id) {
                // The saved request was deleted elsewhere: keep unsaved edits only.
                None => entry.dirty.then_some(draft).flatten().map(|draft| {
                    let lost = draft_lost(&draft);
                    RestoredTab {
                        tab_id: entry.tab_id.clone(),
                        request_id: None,
                        history_entry_id: None,
                        dirty: true,
                        draft: forget_proxy_password(draft, lost),
                        editor_tab: entry.editor_tab,
                        preview: entry.preview,
                        proxy_password_lost: lost,
                    }
                }),
                Some(request) => {
                    let (draft, lost) = match draft.filter(|_| entry.dirty) {
                        Some(edited) => {
                            let lost = draft_lost(&edited);
                            (forget_proxy_password(edited, lost), lost)
                        }
                        None => (pin((*request).clone())?, false),
                    };
                    Some(RestoredTab {
                        tab_id: entry.tab_id.clone(),
                        request_id: Some(request_id.to_owned()),
                        history_entry_id: entry.history_entry_id.clone(),
                        dirty: entry.dirty,
                        draft,
                        editor_tab: entry.editor_tab,
                        preview: entry.preview,
                        proxy_password_lost: lost,
                    })
                }
            },
            None => draft.map(|draft| {
                let lost = draft_lost(&draft);
                RestoredTab {
                    tab_id: entry.tab_id.clone(),
                    request_id: None,
                    history_entry_id: entry.history_entry_id.clone(),
                    dirty: entry.dirty,
                    draft: forget_proxy_password(draft, lost),
                    editor_tab: entry.editor_tab,
                    preview: entry.preview,
                    proxy_password_lost: lost,
                }
            }),
        };
        match restored {
            Some(tab) => tabs.push(tab),
            None => dropped.push(DroppedTab {
                tab_id: entry.tab_id.clone(),
                request_id: entry.request_id.clone(),
                reason: if entry.request_id.is_some() {
                    DropReason::RequestDeleted
                } else {
                    DropReason::MissingDraft
                },
            }),
        }
    }
    let active_tab_id = session
        .active_tab_id
        .clone()
        .filter(|id| tabs.iter().any(|tab| &tab.tab_id == id))
        .or_else(|| tabs.first().map(|tab| tab.tab_id.clone()));
    Ok(RestoredSession {
        tabs,
        active_tab_id,
        dropped,
    })
}

/// The draft's proxy URL carries a typed password (one the store does not hold).
fn typed_proxy_password(draft: &Request) -> bool {
    draft
        .network_options
        .proxy_url
        .as_deref()
        .is_some_and(|url| proxy_url::split(url).password.is_some())
}

/// The draft's proxy URL carries the mask in place of its password.
fn masked_proxy_password(draft: &Request) -> bool {
    draft
        .network_options
        .proxy_url
        .as_deref()
        .is_some_and(|url| proxy_url::split(url).is_masked)
}

/// `draft` with the password removed from its proxy URL when `lost`.
fn forget_proxy_password(mut draft: Request, lost: bool) -> Request {
    if lost && let Some(url) = draft.network_options.proxy_url.as_mut() {
        *url = proxy_url::without_password(url);
    }
    draft
}

fn unreadable(reason: &str) -> AppError {
    AppError::validation(format!("The saved tab session cannot be read: {reason}."))
}

fn string_field(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// One version-1 entry, or `None` when the React workbench would have discarded it.
fn legacy_tab(entry: &Value) -> Option<SessionTab> {
    let entry = entry.as_object()?;
    let draft = match entry.get("draft") {
        None => None,
        Some(Value::Object(draft)) => Some(legacy_draft(draft)?),
        // The workbench accepted any non-null object here and normalised it to defaults.
        Some(Value::Array(_)) => Some(legacy_draft(&Map::new())?),
        Some(_) => return None,
    };
    Some(SessionTab {
        tab_id: string_field(entry, "tabId")?,
        request_id: match entry.get("requestId")? {
            Value::Null => None,
            Value::String(id) => Some(id.clone()),
            _ => return None,
        },
        history_entry_id: string_field(entry, "historyEntryId"),
        dirty: entry.get("dirty")?.as_bool()?,
        draft,
        editor_tab: None,
        preview: false,
        proxy_password_lost: false,
    })
}

/// A version-1 (camelCase `RequestDraft`) draft as a [`Request`], field by field: a value of the
/// wrong type is replaced by that field's default, array items of the wrong shape are dropped.
fn legacy_draft(draft: &Map<String, Value>) -> Option<Request> {
    let pick = |key: &str| draft.get(key);
    let string = |key: &str| kept::<String>(pick(key), json!(""));
    let id_or_null = |key: &str| kept::<String>(pick(key), Value::Null);
    let mut network_options = Map::new();
    if let Some(Value::Object(raw)) = pick("networkOptions") {
        // Every `NetworkOptions` field is independent, so each entry is checked on its own
        // (one deserialization per key, not one over the whole map per key).
        for (key, value) in raw {
            let single = Value::Object(Map::from_iter([(key.clone(), value.clone())]));
            if NetworkOptions::deserialize(&single).is_ok() {
                network_options.insert(key.clone(), value.clone());
            }
        }
    }
    let position = pick("position")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_f64().map(|float| float as i64))
        })
        .unwrap_or(0);
    let mut request = Map::new();
    request.insert("id".into(), string("id"));
    request.insert("workspace_id".into(), string("workspaceId"));
    request.insert("collection_id".into(), id_or_null("collectionId"));
    request.insert("folder_id".into(), id_or_null("folderId"));
    request.insert(
        "name".into(),
        kept::<String>(pick("name"), json!("Untitled Request")),
    );
    request.insert(
        "protocol".into(),
        kept::<ProtocolKind>(pick("protocol"), json!("HttpRest")),
    );
    request.insert(
        "method".into(),
        kept::<HttpMethod>(pick("method"), json!("Get")),
    );
    request.insert("url".into(), string("url"));
    request.insert(
        "query_params".into(),
        items::<KeyValue>(pick("queryParams")),
    );
    request.insert(
        "path_variables".into(),
        items::<KeyValue>(pick("pathVariables")),
    );
    request.insert("headers".into(), items::<KeyValue>(pick("headers")));
    request.insert(
        "auth".into(),
        kept::<AuthConfig>(pick("auth"), json!("Inherit")),
    );
    request.insert(
        "body".into(),
        kept::<RequestBody>(pick("body"), json!("None")),
    );
    request.insert("network_options".into(), Value::Object(network_options));
    request.insert("assertions".into(), items::<Assertion>(pick("assertions")));
    request.insert("description".into(), string("description"));
    request.insert("pre_request_script".into(), string("preRequestScript"));
    request.insert("test_script".into(), string("testScript"));
    request.insert("position".into(), json!(position));
    Request::deserialize(Value::Object(request)).ok()
}

/// `value` when it deserializes as `T`, else `default`.
fn kept<T: DeserializeOwned>(value: Option<&Value>, default: Value) -> Value {
    value
        .filter(|value| T::deserialize(*value).is_ok())
        .cloned()
        .unwrap_or(default)
}

/// The items of an array value that deserialize as `T`; anything else is an empty array.
fn items<T: DeserializeOwned>(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Array(values)) => Value::Array(
            values
                .iter()
                .filter(|value| T::deserialize(*value).is_ok())
                .cloned()
                .collect(),
        ),
        _ => Value::Array(Vec::new()),
    }
}

#[cfg(test)]
mod tests;
