//! Execution history and cookie jar use-cases.
//!
//! Single responsibility: page, search, restore and delete saved executions,
//! and manage the workspace cookie jar (list, export, import, edit, expire).

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::platform::{normalize_display_path, write_private_file};
use crate::workspace::models::{cookie_expiry_at, format_cookie_date, parse_cookie_date};
use crate::workspace::{
    CookieJarEntry, CookieJarEntryId, CookieJarExport, CookieJarExportSaveResult, CookieJarImport,
    CookieJarImportOpenFileResult, CookieJarImportResult, EnvironmentId, ExecutionHistoryEntry,
    HistoryEntryId, RequestId, WorkspaceId, WorkspaceStore, validation::validate_id,
};

use crate::error::AppError;

use super::import_export_service::{MAX_IMPORT_FILE_BYTES, read_capped};
use super::{AppState, command_error};

/// The "not found in workspace" error shared by every by-id history/cookie use-case; an id
/// owned by another workspace is indistinguishable from an unknown one.
fn not_found_in_workspace(kind: &str, id: &str, workspace_id: &str) -> AppError {
    AppError::not_found(format!("{kind} {id} not found in workspace {workspace_id}"))
}

/// Loads one history entry of `workspace_id`; unknown or foreign ids are `NotFound`.
pub fn history_get_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
) -> Result<ExecutionHistoryEntry, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let history_entry_id = HistoryEntryId::new(&id).map_err(command_error)?;
    let store = state.store();
    store
        .get_execution_history(workspace.as_str(), &history_entry_id)
        .map_err(command_error)?
        .ok_or_else(|| not_found_in_workspace("history entry", &id, &workspace_id))
}

/// Deletes several history entries of `workspace_id`, returning only the ids actually deleted;
/// ids unknown or owned by another workspace are skipped and left intact.
pub fn history_delete_many_impl(
    state: &AppState,
    workspace_id: String,
    ids: Vec<String>,
) -> Result<Vec<String>, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let history_entry_ids = ids
        .iter()
        .map(|id| HistoryEntryId::new(id).map_err(command_error))
        .collect::<Result<Vec<_>, _>>()?;
    let store = state.store();
    store
        .delete_execution_history_entries(workspace.as_str(), &history_entry_ids)
        .map(|deleted_ids| {
            deleted_ids
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        })
        .map_err(command_error)
}

/// Deletes every history entry of a workspace, returning the deleted ids.
pub fn history_clear_impl(state: &AppState, workspace_id: String) -> Result<Vec<String>, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let store = state.store();
    store
        .delete_all_execution_history(workspace_id.as_str())
        .map(|deleted_ids| {
            deleted_ids
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        })
        .map_err(command_error)
}

/// Deletes the rows of a workspace that `filter` selects ("Clear filtered") and returns
/// how many were removed. The predicate is the one `history_page_impl` lists with, so the count
/// the shell confirmed (the page's `total`) is the number deleted; an empty filter clears the
/// whole workspace.
pub fn history_clear_filtered_impl(
    state: &AppState,
    workspace_id: String,
    filter: crate::workspace::HistoryFilter,
) -> Result<u64, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    state
        .store()
        .delete_filtered_execution_history(workspace_id.as_str(), &filter)
        .map_err(command_error)
}

/// Restores deleted history entries, skipping rows whose workspace is gone and clearing
/// request/environment references that no longer resolve. A batch larger than what retention
/// could keep (every source budget plus the mock rows) is cut to that size; the store
/// re-bounds each row's payloads and prunes afterwards.
pub fn history_restore_many_impl(
    state: &AppState,
    mut entries: Vec<ExecutionHistoryEntry>,
) -> Result<Vec<String>, AppError> {
    let store = state.store();
    entries.truncate(store.history_capacity());
    let entries = entries
        .into_iter()
        .map(validated_history_restore_entry)
        .collect::<Result<Vec<_>, _>>()?;
    let mut restorable_entries = Vec::new();

    for mut entry in entries {
        if store
            .get_workspace(&entry.workspace_id)
            .map_err(command_error)?
            .is_none()
        {
            continue;
        }
        clear_stale_history_refs(&store, &mut entry)?;
        restorable_entries.push(entry);
    }

    store
        .restore_execution_history_entries(restorable_entries)
        .map(|restored_ids| {
            restored_ids
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        })
        .map_err(command_error)
}

/// Clears the `request_id`/`environment_id` of `entry` when the referenced row is missing or
/// belongs to another workspace, so the history row never points at a foreign record.
pub(crate) fn clear_stale_history_refs(
    store: &WorkspaceStore,
    entry: &mut ExecutionHistoryEntry,
) -> Result<(), AppError> {
    if let Some(request_id) = &entry.request_id {
        let stale = store
            .get_request(&entry.workspace_id, request_id)
            .map_err(command_error)?
            .is_none();
        if stale {
            entry.request_id = None;
        }
    }
    if let Some(environment_id) = &entry.environment_id {
        let stale = store
            .get_environment(&entry.workspace_id, environment_id)
            .map_err(command_error)?
            .is_none();
        if stale {
            entry.environment_id = None;
        }
    }
    Ok(())
}

/// Re-validates the ids and timestamp of a history entry received from the shell.
fn validated_history_restore_entry(
    mut entry: ExecutionHistoryEntry,
) -> Result<ExecutionHistoryEntry, AppError> {
    entry.id = HistoryEntryId::new(entry.id.as_str()).map_err(command_error)?;
    entry.workspace_id = WorkspaceId::new(entry.workspace_id.as_str()).map_err(command_error)?;
    entry.request_id = entry
        .request_id
        .map(|request_id| RequestId::new(request_id.as_str()).map_err(command_error))
        .transpose()?;
    entry.environment_id = entry
        .environment_id
        .map(|environment_id| EnvironmentId::new(environment_id.as_str()).map_err(command_error))
        .transpose()?;
    entry.executed_at =
        validate_id("execution timestamp", &entry.executed_at).map_err(command_error)?;

    Ok(entry)
}

/// Flags `entry` expired when its `Max-Age`/`Expires` is in the past now; a missing entry
/// (unknown id or owned by another workspace) is `NotFound`.
fn flagged_or_not_found(
    entry: Option<CookieJarEntry>,
    id: &str,
    workspace_id: &str,
) -> Result<CookieJarEntry, AppError> {
    entry
        .map(|entry| entry.with_expiry_flag(SystemTime::now()))
        .ok_or_else(|| not_found_in_workspace("cookie", id, workspace_id))
}

/// Lists the cookies of a workspace, each flagged `expired` when its `Max-Age`/`Expires` is in
/// the past at listing time.
pub fn cookie_jar_list_impl(
    state: &AppState,
    workspace_id: String,
) -> Result<Vec<CookieJarEntry>, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let store = state.store();
    let now = SystemTime::now();
    Ok(store
        .list_cookie_jar(workspace_id.as_str())
        .map_err(command_error)?
        .into_iter()
        .map(|entry| entry.with_expiry_flag(now))
        .collect())
}

/// Builds the exportable snapshot of a workspace's cookie jar.
pub fn cookie_jar_export_impl(
    state: &AppState,
    workspace_id: String,
) -> Result<CookieJarExport, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let store = state.store();
    let cookies = store
        .list_cookie_jar(workspace_id.as_str())
        .map_err(command_error)?;
    CookieJarExport::new(workspace_id.as_str(), cookies).map_err(command_error)
}

/// Writes the cookie jar export to the chosen path (owner-only permissions, it holds session
/// secrets); a `None` path reports the cancel with the cookie count and writes nothing.
pub fn cookie_jar_export_save_to_path_impl(
    state: &AppState,
    workspace_id: String,
    selected_path: Option<PathBuf>,
) -> Result<CookieJarExportSaveResult, AppError> {
    let export = cookie_jar_export_impl(state, workspace_id)?;
    let cookie_count = export.cookies.len();
    let workspace_id = export.workspace_id.as_str().to_owned();

    let Some(path) = selected_path else {
        return CookieJarExportSaveResult::canceled(workspace_id, cookie_count)
            .map_err(command_error);
    };

    let json = serde_json::to_string_pretty(&export).map_err(command_error)?;
    write_private_file(&path, json).map_err(command_error)?;
    state.note_user_file(&path);

    CookieJarExportSaveResult::saved(workspace_id, normalize_display_path(&path), cookie_count)
        .map_err(command_error)
}

/// Default file name offered by the export dialog.
pub fn cookie_jar_export_save_suggested_filename(workspace_id: &str) -> Result<String, AppError> {
    let workspace_id = WorkspaceId::new(workspace_id).map_err(command_error)?;
    Ok(format!(
        "pingdeck-cookie-jar-{}.json",
        workspace_id.as_str()
    ))
}

/// Imports cookies into a workspace: a PingDeck cookie jar export (JSON, starts with `{`) or a
/// Netscape `cookies.txt` (what browsers, curl and wget write; the Add cookie dialog sends one
/// too). Cookies with the same name, domain and path are replaced.
pub fn cookie_jar_import_impl(
    state: &AppState,
    workspace_id: String,
    payload: String,
) -> Result<CookieJarImportResult, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let entries = if payload.trim_start().starts_with('{') {
        let import = serde_json::from_str::<CookieJarImport>(&payload)
            .map_err(|error| AppError::validation(format!("invalid Cookie Jar JSON: {error}")))?;
        import
            .into_entries_for_workspace(workspace_id.as_str())
            .map_err(command_error)?
    } else {
        netscape_cookie_entries(workspace_id.as_str(), &payload, &cookie_jar_timestamp())?
    };
    let store = state.store();
    let imported_ids = store
        .import_cookie_jar_entries(&entries)
        .map_err(command_error)?;

    CookieJarImportResult::new(workspace_id.as_str(), imported_ids).map_err(command_error)
}

/// The cookies of a Netscape `cookies.txt`: one cookie per line, seven tab-separated fields
/// (domain, include subdomains, path, secure, expiry in Unix seconds with 0 for a session
/// cookie, name, value); a `#HttpOnly_` prefix marks an HttpOnly cookie, other `#` lines and
/// blank lines are skipped. A line that does not fit fails the whole import with its number.
fn netscape_cookie_entries(
    workspace_id: &str,
    text: &str,
    timestamp: &str,
) -> Result<Vec<CookieJarEntry>, AppError> {
    let mut entries = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        let (line, http_only) = match line.strip_prefix("#HttpOnly_") {
            Some(rest) => (rest, true),
            None => (line, false),
        };
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let invalid = |why: &str| {
            AppError::validation(format!(
                "cookies.txt line {}: {why}; expected domain, subdomains, path, secure, expiry, name and value separated by tabs",
                index + 1
            ))
        };
        let fields: Vec<&str> = line.split('\t').collect();
        // A cookie with an empty value may lose its trailing tab.
        let (domain, subdomains, path, secure, expiry, name, value) = match fields.as_slice() {
            [domain, subdomains, path, secure, expiry, name, value] => {
                (*domain, *subdomains, *path, *secure, *expiry, *name, *value)
            }
            [domain, subdomains, path, secure, expiry, name] => {
                (*domain, *subdomains, *path, *secure, *expiry, *name, "")
            }
            _ => return Err(invalid("not 7 fields")),
        };
        let flag = |text: &str, what: &str| match text.trim() {
            text if text.eq_ignore_ascii_case("TRUE") => Ok(true),
            text if text.eq_ignore_ascii_case("FALSE") => Ok(false),
            _ => Err(invalid(&format!("{what} must be TRUE or FALSE"))),
        };
        let subdomains = flag(subdomains, "subdomains")?;
        let secure = flag(secure, "secure")?;
        let expiry = expiry
            .trim()
            .parse::<i64>()
            .map_err(|_| invalid("expiry must be Unix seconds"))?;
        let expires = u64::try_from(expiry)
            .ok()
            .filter(|seconds| *seconds > 0)
            .map(|seconds| {
                format_cookie_date(UNIX_EPOCH + std::time::Duration::from_secs(seconds))
            });
        let domain = domain.trim().trim_start_matches('.');
        let entry = CookieJarEntry::from_resolved_cookie(
            workspace_id,
            None,
            name.trim(),
            value,
            domain,
            path.trim(),
            !subdomains,
            expires.as_deref(),
            None,
            secure,
            http_only,
            None,
            timestamp,
        )
        .map_err(|error| invalid(&error.to_string()))?;
        entries.push(entry);
    }
    if entries.is_empty() {
        return Err(AppError::validation(
            "No cookies found: paste a PingDeck cookie jar export (JSON) or a Netscape cookies.txt",
        ));
    }
    Ok(entries)
}

/// Imports cookies from a file the shell picked; `None` means the dialog was canceled.
pub fn cookie_jar_import_open_file_from_path_impl(
    state: &AppState,
    workspace_id: String,
    selected_path: Option<PathBuf>,
) -> Result<CookieJarImportOpenFileResult, AppError> {
    WorkspaceId::new(&workspace_id).map_err(command_error)?;

    let Some(path) = selected_path else {
        return CookieJarImportOpenFileResult::canceled(workspace_id).map_err(command_error);
    };

    let payload = read_capped(&path, MAX_IMPORT_FILE_BYTES, "Cookie Jar import file")?;
    let result = cookie_jar_import_impl(state, workspace_id.clone(), payload)?;

    CookieJarImportOpenFileResult::imported(
        workspace_id,
        normalize_display_path(&path),
        result.imported_ids,
    )
    .map_err(command_error)
}

/// Replaces the value of one cookie of `workspace_id`; unknown or foreign ids are `NotFound`.
pub fn cookie_jar_update_value_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
    value: String,
) -> Result<CookieJarEntry, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let cookie_jar_entry_id = CookieJarEntryId::new(&id).map_err(command_error)?;
    let updated_at = cookie_jar_timestamp();
    let updated = state
        .store()
        .update_cookie_jar_entry_value(
            workspace.as_str(),
            &cookie_jar_entry_id,
            &value,
            &updated_at,
        )
        .map_err(command_error)?;
    flagged_or_not_found(updated, &id, &workspace_id)
}

/// Validates a cookie `SameSite` value and normalizes its capitalization.
fn normalize_cookie_same_site(value: Option<String>) -> Result<Option<String>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return Ok(None);
    }

    match trimmed {
        "Lax" | "Strict" | "None" => Ok(Some(trimmed.to_owned())),
        _ => Err(AppError::validation(format!(
            "invalid cookie SameSite value: {trimmed}"
        ))),
    }
}

/// Updates a cookie's `Secure`, `HttpOnly` and `SameSite` flags; unknown or foreign ids are
/// `NotFound`.
pub fn cookie_jar_update_flags_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
    secure: bool,
    http_only: bool,
    same_site: Option<String>,
) -> Result<CookieJarEntry, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let cookie_jar_entry_id = CookieJarEntryId::new(&id).map_err(command_error)?;
    let same_site = normalize_cookie_same_site(same_site)?;
    let updated_at = cookie_jar_timestamp();
    let updated = state
        .store()
        .update_cookie_jar_entry_flags(
            workspace.as_str(),
            &cookie_jar_entry_id,
            secure,
            http_only,
            same_site.as_deref(),
            &updated_at,
        )
        .map_err(command_error)?;
    flagged_or_not_found(updated, &id, &workspace_id)
}

/// Validates a cookie `Expires` date (the forms [`parse_cookie_date`] reads); blank clears it.
fn normalize_cookie_expires(value: Option<String>) -> Result<Option<String>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return Ok(None);
    }

    parse_cookie_date(trimmed)
        .map(|_| Some(trimmed.to_owned()))
        .ok_or_else(|| AppError::validation(format!("invalid cookie Expires value: {trimmed}")))
}

/// Validates a cookie `Max-Age` (integer seconds); blank clears it.
fn normalize_cookie_max_age(value: Option<String>) -> Result<Option<String>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return Ok(None);
    }

    trimmed
        .parse::<i64>()
        .map(|_| Some(trimmed.to_owned()))
        .map_err(|_| AppError::validation(format!("invalid cookie Max-Age value: {trimmed}")))
}

/// Updates a cookie's expiry; unknown or foreign ids are `NotFound`. Like a response cookie, the
/// result is stored as an absolute `Expires` fixed now: a `Max-Age` counts from this edit and
/// wins over `Expires` (RFC 6265 §5.3); both blank makes a session cookie.
pub fn cookie_jar_update_expiry_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
    expires: Option<String>,
    max_age: Option<String>,
) -> Result<CookieJarEntry, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let cookie_jar_entry_id = CookieJarEntryId::new(&id).map_err(command_error)?;
    let expires = normalize_cookie_expires(expires)?;
    let max_age = normalize_cookie_max_age(max_age)?;
    let expires = cookie_expiry_at(expires.as_deref(), max_age.as_deref(), SystemTime::now())
        .map(format_cookie_date);
    let updated_at = cookie_jar_timestamp();
    let updated = state
        .store()
        .update_cookie_jar_entry_expiry(
            workspace.as_str(),
            &cookie_jar_entry_id,
            expires.as_deref(),
            None,
            &updated_at,
        )
        .map_err(command_error)?;
    flagged_or_not_found(updated, &id, &workspace_id)
}

/// Trims a cookie key field and rejects a blank one.
fn normalize_cookie_key_field(label: &'static str, value: String) -> Result<String, AppError> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return Err(AppError::validation(format!(
            "cookie {label} cannot be blank"
        )));
    }

    Ok(trimmed.to_owned())
}

/// Trims a cookie domain, drops leading dots and lowercases it.
fn normalize_cookie_key_domain(value: String) -> Result<String, AppError> {
    let domain = normalize_cookie_key_field("domain", value)?
        .trim_start_matches('.')
        .to_ascii_lowercase();

    normalize_cookie_key_field("domain", domain)
}

/// Trims a cookie path and guarantees the leading `/`.
fn normalize_cookie_key_path(value: String) -> Result<String, AppError> {
    let path = normalize_cookie_key_field("path", value)?;

    if path.starts_with('/') {
        Ok(path)
    } else {
        Ok(format!("/{path}"))
    }
}

/// Renames a cookie or moves it to another domain/path; unknown or foreign ids are `NotFound`.
pub fn cookie_jar_update_key_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
    name: String,
    domain: String,
    path: String,
) -> Result<CookieJarEntry, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let cookie_jar_entry_id = CookieJarEntryId::new(&id).map_err(command_error)?;
    let name = normalize_cookie_key_field("name", name)?;
    let domain = normalize_cookie_key_domain(domain)?;
    let path = normalize_cookie_key_path(path)?;
    let updated_at = cookie_jar_timestamp();
    let updated = state
        .store()
        .update_cookie_jar_entry_key(
            workspace.as_str(),
            &cookie_jar_entry_id,
            &name,
            &domain,
            &path,
            &updated_at,
        )
        .map_err(command_error)?;
    flagged_or_not_found(updated, &id, &workspace_id)
}

/// Toggles whether a cookie is host-only; unknown or foreign ids are `NotFound`.
pub fn cookie_jar_update_scope_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
    host_only: bool,
) -> Result<CookieJarEntry, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let cookie_jar_entry_id = CookieJarEntryId::new(&id).map_err(command_error)?;
    let updated_at = cookie_jar_timestamp();
    let updated = state
        .store()
        .update_cookie_jar_entry_scope(
            workspace.as_str(),
            &cookie_jar_entry_id,
            host_only,
            &updated_at,
        )
        .map_err(command_error)?;
    flagged_or_not_found(updated, &id, &workspace_id)
}

/// Deletes several cookies of `workspace_id`, returning only the ids actually deleted; ids
/// unknown or owned by another workspace are skipped and left intact.
pub fn cookie_jar_delete_many_impl(
    state: &AppState,
    workspace_id: String,
    ids: Vec<String>,
) -> Result<Vec<String>, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let cookie_jar_entry_ids = ids
        .into_iter()
        .map(|id| CookieJarEntryId::new(&id).map_err(command_error))
        .collect::<Result<Vec<_>, _>>()?;
    let store = state.store();
    store
        .delete_cookie_jar_entries(workspace.as_str(), &cookie_jar_entry_ids)
        .map(|deleted_ids| {
            deleted_ids
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        })
        .map_err(command_error)
}

/// Restores deleted cookies, skipping foreign workspaces and clearing stale source requests.
pub fn cookie_jar_restore_many_impl(
    state: &AppState,
    entries: Vec<CookieJarEntry>,
) -> Result<Vec<String>, AppError> {
    let mut entries = entries
        .into_iter()
        .map(validated_cookie_jar_restore_entry)
        .collect::<Result<Vec<_>, _>>()?;
    let store = state.store();
    let mut restorable_entries = Vec::new();

    for mut entry in entries.drain(..) {
        if store
            .get_workspace(&entry.workspace_id)
            .map_err(command_error)?
            .is_none()
        {
            continue;
        }

        let clear_source_request_id = if let Some(request_id) = entry.source_request_id.as_ref() {
            store
                .get_request(&entry.workspace_id, request_id)
                .map_err(command_error)?
                .is_none()
        } else {
            false
        };

        if clear_source_request_id {
            entry.source_request_id = None;
        }

        restorable_entries.push(entry);
    }

    store
        .restore_cookie_jar_entries(&restorable_entries)
        .map(|restored_ids| {
            restored_ids
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        })
        .map_err(command_error)
}

/// Re-validates the ids of a cookie received from the shell for undo.
fn validated_cookie_jar_restore_entry(
    mut entry: CookieJarEntry,
) -> Result<CookieJarEntry, AppError> {
    entry.id = CookieJarEntryId::new(entry.id.as_str()).map_err(command_error)?;
    entry.workspace_id = WorkspaceId::new(entry.workspace_id.as_str()).map_err(command_error)?;
    entry.source_request_id = entry
        .source_request_id
        .map(|request_id| RequestId::new(request_id.as_str()))
        .transpose()
        .map_err(command_error)?;
    Ok(entry)
}

/// Deletes the workspace's expired cookies, returning their ids.
pub fn cookie_jar_delete_expired_impl(
    state: &AppState,
    workspace_id: String,
) -> Result<Vec<String>, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let store = state.store();
    store
        .delete_expired_cookie_jar_entries(workspace_id.as_str(), SystemTime::now())
        .map(|ids| {
            ids.into_iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        })
        .map_err(command_error)
}

/// Fresh unique history entry id for an execution of `request_id`.
#[must_use]
pub(crate) fn execution_history_id(request_id: &RequestId) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("hist_{}_{}", request_id.as_str(), nanos)
}

/// Fresh run id shared by the history rows (and flow-run record) of one collection run or flow
/// run: wall-clock nanoseconds plus a process-wide counter, so two runs started in the same
/// instant still differ.
#[must_use]
pub(crate) fn new_run_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("run_{nanos}_{sequence}")
}

/// Current time as the Unix-millisecond text history rows store.
#[must_use]
pub(crate) fn execution_history_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

/// Timestamp stamped on cookie edits (Unix milliseconds as text).
#[must_use]
pub(crate) fn cookie_jar_timestamp() -> String {
    execution_history_timestamp()
}

/// Which rows a HAR export takes: the selected ids, or every row the list filter matches.
#[derive(Debug, Clone)]
pub enum HistoryExportScope {
    Ids(Vec<String>),
    Filter(crate::workspace::HistoryFilter),
}

/// What `history_har_export` answers: whether a file was written, where, and how many entries.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HistoryExportResult {
    pub saved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub entries: u32,
}

/// File name the HAR save dialog offers.
pub const HISTORY_EXPORT_FILE_NAME: &str = "pingdeck-history.har";

/// Renders the scope's rows as HAR 1.2 JSON and returns it with the entry count. Credentials are
/// masked with the core's rules (`pingdeck_base::secrets`): secret headers by name, userinfo and
/// secret query values in URLs, known token shapes in bodies. Bodies are the stored preview; a cut
/// one says so in `comment` and `_pingdeck.truncated`. `_pingdeck.manifest` holds the entry count
/// and the SHA-256 of the serialized `entries` array, so an edited file can be told apart.
pub fn history_export_har_impl(
    state: &AppState,
    workspace_id: String,
    scope: HistoryExportScope,
) -> Result<(String, usize), AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let rows = {
        let store = state.store();
        let ids: Vec<HistoryEntryId> = match scope {
            HistoryExportScope::Ids(ids) => ids
                .iter()
                .map(|id| HistoryEntryId::new(id).map_err(command_error))
                .collect::<Result<_, _>>()?,
            HistoryExportScope::Filter(filter) => {
                let mut ids = Vec::new();
                let mut cursor = None;
                loop {
                    let page = store
                        .query_execution_history(workspace.as_str(), &filter, 200, cursor.as_ref())
                        .map_err(command_error)?;
                    ids.extend(page.entries.into_iter().map(|entry| entry.id));
                    match page.next_cursor {
                        Some(next) => cursor = Some(next),
                        None => break,
                    }
                }
                ids
            }
        };
        let mut rows = Vec::with_capacity(ids.len());
        for id in &ids {
            // Ids of another workspace or already deleted are skipped, never loaded.
            if let Some(entry) = store
                .get_execution_history(workspace.as_str(), id)
                .map_err(command_error)?
            {
                rows.push(entry);
            }
        }
        rows
    };
    let entries: Vec<serde_json::Value> = rows.iter().map(har_entry).collect();
    let digest =
        pingdeck_base::crypto::sha256_hex(&serde_json::to_vec(&entries).map_err(command_error)?);
    let count = entries.len();
    let har = serde_json::json!({
        "log": {
            "version": "1.2",
            "creator": { "name": "PingDeck", "version": env!("CARGO_PKG_VERSION") },
            "entries": entries,
            "_pingdeck": { "manifest": { "entries": count, "sha256": digest } },
        }
    });
    let json = serde_json::to_string_pretty(&har).map_err(command_error)?;
    Ok((json, count))
}

/// Writes the HAR of `scope` to the chosen path (owner-only permissions); `None` is a cancelled
/// save dialog and writes nothing.
pub fn history_export_save_to_path_impl(
    state: &AppState,
    workspace_id: String,
    scope: HistoryExportScope,
    selected_path: Option<PathBuf>,
) -> Result<HistoryExportResult, AppError> {
    let Some(path) = selected_path else {
        return Ok(HistoryExportResult {
            saved: false,
            path: None,
            entries: 0,
        });
    };
    let (json, count) = history_export_har_impl(state, workspace_id, scope)?;
    write_private_file(&path, json).map_err(command_error)?;
    state.note_user_file(&path);
    Ok(HistoryExportResult {
        saved: true,
        path: Some(normalize_display_path(&path)),
        entries: u32::try_from(count).unwrap_or(u32::MAX),
    })
}

fn har_text(text: &str) -> String {
    pingdeck_base::secrets::redact_secrets(text, &[])
}

/// Masks the userinfo of a URL, then its secret query values and token shapes.
fn har_url(url: &str) -> String {
    let masked = match url.split_once("://") {
        Some((scheme, rest)) => {
            let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            match rest[..authority_end].rfind('@') {
                Some(at) => format!(
                    "{scheme}://{}@{}",
                    pingdeck_base::secrets::REDACTED,
                    &rest[at + 1..]
                ),
                None => url.to_owned(),
            }
        }
        None => url.to_owned(),
    };
    har_text(&masked)
}

fn har_headers<'a>(headers: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<serde_json::Value> {
    headers
        .map(|(name, value)| {
            let value = if pingdeck_base::secrets::is_secret_header_name(name) {
                pingdeck_base::secrets::REDACTED.to_owned()
            } else {
                har_text(value)
            };
            serde_json::json!({ "name": name, "value": value })
        })
        .collect()
}

fn header_value<'a>(headers: &'a [crate::workspace::KeyValue], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.key.eq_ignore_ascii_case(name))
        .map(|h| h.value.split(';').next().unwrap_or("").trim())
}

fn har_entry(entry: &ExecutionHistoryEntry) -> serde_json::Value {
    let http_version = if entry.protocol.is_empty() {
        "HTTP/1.1"
    } else {
        entry.protocol.as_str()
    };
    let (method, url, request_headers, request_body) = match &entry.sent_request {
        Some(sent) => (
            sent.method.clone(),
            sent.url.clone(),
            sent.headers.as_slice(),
            Some(sent.body_preview.as_str()).filter(|body| !body.is_empty()),
        ),
        None => (
            format!("{:?}", entry.method).to_ascii_uppercase(),
            entry.request_url.clone(),
            entry
                .request_snapshot
                .as_ref()
                .map_or(&[][..], |request| request.headers.as_slice()),
            None,
        ),
    };
    let url = har_url(&url);
    let query: Vec<serde_json::Value> = url::Url::parse(&url).map_or_else(
        |_| Vec::new(),
        |parsed| {
            parsed
                .query_pairs()
                .map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
                .collect()
        },
    );
    let mut request = serde_json::json!({
        "method": method,
        "url": url,
        "httpVersion": http_version,
        "cookies": [],
        "headers": har_headers(request_headers.iter().filter(|h| h.enabled || entry.sent_request.is_some()).map(|h| (h.key.as_str(), h.value.as_str()))),
        "queryString": query,
        "headersSize": -1,
        "bodySize": -1,
    });
    if let Some(body) = request_body {
        request["postData"] = serde_json::json!({
            "mimeType": header_value(request_headers, "content-type").unwrap_or(""),
            "text": har_text(body),
        });
    }
    let shown = entry.response_body_preview.len();
    let mut content = serde_json::json!({
        "size": entry.body_size_bytes,
        "mimeType": header_value(&entry.response_headers, "content-type").unwrap_or(""),
        "text": har_text(&entry.response_body_preview),
    });
    if entry.response_body_truncated {
        content["comment"] = serde_json::Value::String(format!(
            "Stored preview: the first {shown} of {} bytes.",
            entry.body_size_bytes
        ));
    }
    let (wait, receive) = entry
        .timing
        .as_ref()
        .map_or((entry.elapsed_ms, 0), |timing| {
            (timing.request_ms, timing.response_ms)
        });
    let mut har = serde_json::json!({
        "startedDateTime": entry.executed_at,
        "time": entry.elapsed_ms,
        "request": request,
        "response": {
            "status": entry.status_code,
            "statusText": entry.status_text,
            "httpVersion": http_version,
            "cookies": [],
            "headers": har_headers(entry.response_headers.iter().map(|h| (h.key.as_str(), h.value.as_str()))),
            "content": content,
            "redirectURL": "",
            "headersSize": -1,
            "bodySize": entry.body_size_bytes,
        },
        "cache": {},
        "timings": { "send": 0, "wait": wait, "receive": receive },
        "_pingdeck": {
            "id": entry.id.as_str(),
            "source": entry.source,
            "sourceName": entry.source_name,
            "environment": entry.environment_name,
            "checks": entry.check_summary,
            "truncated": entry.response_body_truncated,
        },
    });
    if let (Some(kind), message) = (&entry.error_kind, &entry.error_message) {
        har["comment"] = serde_json::Value::String(
            [Some(kind.as_str()), message.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(": "),
        );
    }
    har
}
