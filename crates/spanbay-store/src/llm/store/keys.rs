//! Provider keys: CRUD over `llm_keys` for the UI-facing [`ProviderKey`] (masked hints only),
//! the guarded save that writes the key's secret in the same transaction, and the server-owned
//! balance column. The plaintext side (routing views, reveal, management token) is in
//! `key_secrets`.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::key_secrets::delete_key_secrets;
use super::providers::get_provider;
use super::{ProviderKey, atomic, db, flag, json_text};
use crate::llm::balance_state::{BalanceErrorRecord, BalanceReport, StoredBalance};
use crate::llm::providers::{MAX_KEY_NOTES_CHARS, next_stamp, validate_name};
use crate::llm::wire::Protocol;
use crate::storage::{StorageError, secrets};
use spanbay_base::error::{AppError, ErrorCode};
use spanbay_base::secrets::SecretRef;

/// Longest key group name, in characters (the same limit as provider and key names).
pub const MAX_GROUP_NAME_CHARS: usize = crate::llm::providers::MAX_NAME_CHARS;

/// Columns of a key plus the management token's presence and hint (one join, no per-key query);
/// the order is what `key_from_row` reads.
// The join's secret ref must match `key_secrets::management_token_secret`.
const KEY_SELECT: &str = "SELECT k.id, k.provider_id, k.name, k.secret_hint, k.enabled, k.position, \
     k.created_at_ms, k.updated_at_ms, k.api_user_id, k.balance_json, k.balance_error_json, \
     k.balance_attempt_ms, s.secret_ref IS NOT NULL, COALESCE(s.hint, ''), k.last_test_json, k.notes, \
     k.group_name, k.protocols_json, k.preferred_protocol, k.models_listed_at_ms, \
     (SELECT COUNT(*) FROM llm_key_models m WHERE m.key_id = k.id) \
     FROM llm_keys k LEFT JOIN secrets s \
       ON s.secret_ref = 'llm.key.' || k.id || '.management_token'";

/// A stored protocol list; one that no longer parses, or an empty one, is no list.
pub(super) fn protocols_from_json(text: Option<String>) -> Option<Vec<Protocol>> {
    text.and_then(|text| serde_json::from_str::<Vec<Protocol>>(&text).ok())
        .filter(|list| !list.is_empty())
}

/// The stored balance of a key, or `None` when neither a report nor an error is stored. A cache
/// that no longer parses counts as absent: it is refreshed, never a reason to fail a listing.
fn stored_balance(report: Option<String>, error: Option<String>) -> Option<StoredBalance> {
    let last_good = report.and_then(|text| serde_json::from_str::<BalanceReport>(&text).ok());
    let error = error.and_then(|text| serde_json::from_str::<BalanceErrorRecord>(&text).ok());
    if last_good.is_none() && error.is_none() {
        return None;
    }
    // `stale` is the state at the last write; readers that know the time use `is_stale_at`.
    let stale = error.is_some();
    Some(StoredBalance {
        last_good,
        error,
        stale,
    })
}

fn key_from_row(row: &Row<'_>) -> rusqlite::Result<ProviderKey> {
    Ok(ProviderKey {
        id: row.get(0)?,
        provider_id: row.get(1)?,
        name: row.get(2)?,
        secret_hint: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        position: row.get(5)?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
        api_user_id: row.get(8)?,
        balance: stored_balance(row.get(9)?, row.get(10)?),
        balance_attempt_ms: row.get(11)?,
        has_management_token: row.get(12)?,
        management_token_hint: row.get(13)?,
        last_test: row
            .get::<_, Option<String>>(14)?
            .and_then(|text| serde_json::from_str(&text).ok()),
        notes: Some(row.get(15)?),
        group_name: Some(row.get(16)?),
        protocols: protocols_from_json(row.get(17)?),
        preferred_protocol: row
            .get::<_, Option<String>>(18)?
            .as_deref()
            .and_then(Protocol::parse),
        models_listed_at_ms: row.get(19)?,
        model_count: row
            .get::<_, Option<i64>>(19)?
            .map(|_| row.get::<_, i64>(20))
            .transpose()?
            .map(|count| u32::try_from(count).unwrap_or(u32::MAX)),
        ..ProviderKey::default()
    })
}

/// Keys of `provider_id`, or of every provider when `None`, in order (masked hints only).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_keys(
    connection: &Connection,
    provider_id: Option<&str>,
) -> Result<Vec<ProviderKey>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "{KEY_SELECT} WHERE (?1 IS NULL OR k.provider_id = ?1) \
             ORDER BY k.provider_id, k.position, k.name, k.id"
        ))
        .map_err(db)?;
    let rows = statement
        .query_map(params![provider_id], key_from_row)
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// The key with `id` (masked hints only).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn get_key(connection: &Connection, id: &str) -> Result<Option<ProviderKey>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!("{KEY_SELECT} WHERE k.id = ?1"))
        .map_err(db)?;
    statement
        .query_row(params![id], key_from_row)
        .optional()
        .map_err(db)
}

/// Inserts or updates a key. `secret` is the new plaintext (`Some("")` clears it); `None` keeps
/// the stored one. The secret goes to the secret store, never into `llm_keys`; the secret and the
/// row land together or not at all.
///
/// Writes only the user-editable columns (name, enabled, position, `api_user_id`); the balance
/// and the management token are server-owned and kept (see [`set_key_balance`],
/// [`super::set_key_management_token`]), except that replacing the secret with another one drops
/// the stored balance and test of the old one.
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist, [`AppError::Conflict`] when the key
/// belongs to another provider, [`AppError::Validation`] for a bad name, notes or group name,
/// [`AppError::Database`] otherwise (coded [`ErrorCode::KeyStoreUnavailable`] when the secret
/// store fails).
pub fn save_key(
    connection: &Connection,
    key: &ProviderKey,
    secret: Option<&str>,
    now_ms: i64,
) -> Result<ProviderKey, AppError> {
    save_key_guarded(connection, key, secret, None, now_ms)
}

/// [`save_key`] that refuses when the stored key is not the version the caller read
/// (`expected_updated_at_ms`); `None` saves whatever is stored.
///
/// # Errors
/// As [`save_key`], plus [`AppError::Conflict`] coded `stale_write` when the stored version
/// differs or the key is gone.
pub fn save_key_guarded(
    connection: &Connection,
    key: &ProviderKey,
    secret: Option<&str>,
    expected_updated_at_ms: Option<i64>,
    now_ms: i64,
) -> Result<ProviderKey, AppError> {
    atomic(connection, |connection| {
        save_key_rows(connection, key, secret, expected_updated_at_ms, now_ms)
    })
}

/// A failed read or write of the secret store, coded so the UI can say the vault is down.
fn vault_error(error: StorageError) -> AppError {
    AppError::from(error).coded(ErrorCode::KeyStoreUnavailable, None)
}

fn save_key_rows(
    connection: &Connection,
    key: &ProviderKey,
    secret: Option<&str>,
    expected_updated_at_ms: Option<i64>,
    now_ms: i64,
) -> Result<ProviderKey, AppError> {
    let (stored, position, stamp) =
        check_key_write(connection, key, expected_updated_at_ms, now_ms)?;
    check_key_text(key, stored.as_ref())?;
    let (hint, replaced) = write_key_secret(connection, &key.id, secret, now_ms)?;
    // After the secret write: a vault failure is reported before a bad group name.
    let columns = key_columns(key)?;
    upsert_key_row(connection, key, &hint, position, stamp, &columns)?;
    if secret.is_some() {
        reset_after_secret_change(connection, &key.id, replaced)?;
    }
    get_key(connection, &key.id)?.ok_or_else(|| AppError::internal("key vanished after save"))
}

/// The checks against the stored rows: the provider exists, the key keeps its owner, and the
/// caller read the stored version. Returns the stored key, the position to write and the stamp.
fn check_key_write(
    connection: &Connection,
    key: &ProviderKey,
    expected_updated_at_ms: Option<i64>,
    now_ms: i64,
) -> Result<(Option<ProviderKey>, i64, i64), AppError> {
    if get_provider(connection, &key.provider_id)?.is_none() {
        return Err(AppError::not_found(format!(
            "provider {} not found",
            key.provider_id
        )));
    }
    let stored = get_key(connection, &key.id)?;
    // A key never changes owner: a write naming another provider would hand it that key's secret.
    if stored
        .as_ref()
        .is_some_and(|stored| stored.provider_id != key.provider_id)
    {
        return Err(AppError::conflict(format!(
            "key {} belongs to another provider",
            key.id
        )));
    }
    // A new key sorts after every key the provider has, whatever position it asked for.
    let position = match &stored {
        Some(_) => key.position,
        None => key
            .position
            .max(next_key_position(connection, &key.provider_id)?),
    };
    let stamp = next_stamp(
        "key",
        stored.as_ref().map(|stored| stored.updated_at_ms),
        expected_updated_at_ms,
        now_ms,
    )?;
    Ok((stored, position, stamp))
}

/// The name and notes limits (the group name is checked later, see [`key_columns`]).
fn check_key_text(key: &ProviderKey, stored: Option<&ProviderKey>) -> Result<(), AppError> {
    if let Err(error) = validate_name("key", &key.name) {
        // A name stored before the length limit stays saveable until it is edited.
        if stored.is_none_or(|stored| stored.name.trim() != key.name.trim()) {
            return Err(error);
        }
    }
    if let Some(notes) = &key.notes
        && notes.trim().chars().count() > MAX_KEY_NOTES_CHARS
    {
        return Err(AppError::validation(format!(
            "key notes have at most {MAX_KEY_NOTES_CHARS} characters"
        )));
    }
    Ok(())
}

/// Writes (`Some`), clears (`Some("")`) or keeps (`None`) the key's secret. Returns the masked
/// hint to store on the row and whether the secret was replaced by a different one.
fn write_key_secret(
    connection: &Connection,
    key_id: &str,
    secret: Option<&str>,
    now_ms: i64,
) -> Result<(String, bool), AppError> {
    let secret_ref = SecretRef::llm_key(key_id);
    let replaced = match secret {
        Some(new) => {
            secrets::get(connection, secret_ref.as_str())
                .map_err(vault_error)?
                .as_deref()
                != Some(new.trim())
        }
        None => false,
    };
    let hint = match secret.map(str::trim) {
        Some("") => {
            secrets::delete(connection, secret_ref.as_str()).map_err(vault_error)?;
            String::new()
        }
        Some(secret) => {
            secrets::put(connection, secret_ref.as_str(), secret, now_ms)
                .map_err(vault_error)?
                .hint
        }
        None => {
            secrets::hint(connection, secret_ref.as_str())
                .map_err(vault_error)?
                .hint
        }
    };
    Ok((hint, replaced))
}

/// The normalized optional columns of a key row.
struct KeyColumns<'a> {
    api_user_id: Option<&'a str>,
    group_name: Option<&'a str>,
    /// The sorted, deduplicated protocol list; `None` for no list.
    protocols_json: Option<String>,
}

fn key_columns(key: &ProviderKey) -> Result<KeyColumns<'_>, AppError> {
    let api_user_id = key
        .api_user_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let group_name = key.group_name.as_deref().map(str::trim);
    if group_name.is_some_and(|name| name.chars().count() > MAX_GROUP_NAME_CHARS) {
        return Err(AppError::validation(format!(
            "a key group name has at most {MAX_GROUP_NAME_CHARS} characters"
        )));
    }
    let protocols_json = match key.protocols.as_deref() {
        Some([]) | None => None,
        Some(list) => {
            let mut list = list.to_vec();
            list.sort_unstable();
            list.dedup();
            Some(json_text(&list)?)
        }
    };
    Ok(KeyColumns {
        api_user_id,
        group_name,
        protocols_json,
    })
}

/// The upsert of [`save_key`]: only the user-editable columns. `?9`/`?10` (notes, group) keep the
/// stored value when `NULL`; `?12` says whether `?11` (the protocol list) is written at all.
const UPSERT_KEY_SQL: &str = "INSERT INTO llm_keys (id, provider_id, name, secret_hint, enabled, position, \
                 created_at_ms, updated_at_ms, api_user_id, notes, group_name, protocols_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, COALESCE(?9, ''), COALESCE(?10, ''), ?11) \
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, \
                 secret_hint = excluded.secret_hint, enabled = excluded.enabled, \
                 position = excluded.position, updated_at_ms = excluded.updated_at_ms, \
                 api_user_id = excluded.api_user_id, notes = COALESCE(?9, notes), \
                 group_name = COALESCE(?10, group_name), \
                 protocols_json = CASE WHEN ?12 THEN ?11 ELSE protocols_json END";

fn upsert_key_row(
    connection: &Connection,
    key: &ProviderKey,
    hint: &str,
    position: i64,
    stamp: i64,
    columns: &KeyColumns<'_>,
) -> Result<(), AppError> {
    connection
        .execute(
            UPSERT_KEY_SQL,
            params![
                key.id,
                key.provider_id,
                key.name.trim(),
                hint,
                flag(key.enabled),
                position,
                stamp,
                columns.api_user_id,
                key.notes.as_deref().map(str::trim),
                columns.group_name,
                columns.protocols_json,
                flag(key.protocols.is_some()),
            ],
        )
        .map_err(db)?;
    Ok(())
}

/// After a secret write: a new credential invalidates what the last test said about the old one,
/// and another secret (`replaced`) is another account, so the old balance and model list go.
fn reset_after_secret_change(
    connection: &Connection,
    key_id: &str,
    replaced: bool,
) -> Result<(), AppError> {
    connection
        .execute(
            "UPDATE llm_keys SET last_test_json = NULL, \
                 balance_json = CASE WHEN ?2 THEN NULL ELSE balance_json END, \
                 balance_error_json = CASE WHEN ?2 THEN NULL ELSE balance_error_json END, \
                 balance_attempt_ms = CASE WHEN ?2 THEN NULL ELSE balance_attempt_ms END \
             WHERE id = ?1",
            params![key_id, flag(replaced)],
        )
        .map_err(db)?;
    if replaced {
        // Another secret may sit in another relay group: its old model list is not this one's.
        super::key_models::forget_key_models(connection, key_id)?;
    }
    Ok(())
}

/// The position after the last key of `provider_id` (0 for a provider without keys).
fn next_key_position(connection: &Connection, provider_id: &str) -> Result<i64, AppError> {
    connection
        .prepare_cached(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM llm_keys WHERE provider_id = ?1",
        )
        .and_then(|mut statement| statement.query_row(params![provider_id], |row| row.get(0)))
        .map_err(db)
}

/// `base`, or `base 2`, `base 3`, … when a key of `provider_id` already has that name.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn unique_key_name(
    connection: &Connection,
    provider_id: &str,
    base: &str,
) -> Result<String, AppError> {
    let base = base.trim();
    let taken: Vec<String> = list_keys(connection, Some(provider_id))?
        .into_iter()
        .map(|key| key.name)
        .collect();
    let mut candidate = base.to_owned();
    let mut counter = 2;
    while taken.iter().any(|name| name.trim() == candidate) {
        candidate = format!("{base} {counter}");
        counter += 1;
    }
    Ok(candidate)
}

/// Stores the outcome of a balance refresh: the last good report with its check time,
/// the error of the newest refresh (or none), and when it was attempted. An empty
/// [`StoredBalance`] clears both (the adapter went away).
///
/// # Errors
/// [`AppError::NotFound`] when the key does not exist, [`AppError::Database`] otherwise.
pub fn set_key_balance(
    connection: &Connection,
    key_id: &str,
    balance: &StoredBalance,
    attempt_ms: i64,
) -> Result<(), AppError> {
    let report = balance.last_good.as_ref().map(json_text).transpose()?;
    let error = balance.error.as_ref().map(json_text).transpose()?;
    let updated = connection
        .execute(
            "UPDATE llm_keys SET balance_json = ?2, balance_error_json = ?3, \
                 balance_attempt_ms = ?4 WHERE id = ?1",
            params![key_id, report, error, attempt_ms],
        )
        .map_err(db)?;
    if updated == 0 {
        return Err(AppError::not_found(format!("key {key_id} not found")));
    }
    Ok(())
}

/// When each key last sent a request of any source (gateway, probe, agent), by key id: the "used
/// 2 min ago" of the key rows. The request log's newest row when it still has one (one index seek
/// per key, `idx_llm_request_log_key`); else the local midnight (under `tz_offset_minutes`) of the
/// key's newest day in the daily rollup, which outlives the log's retention. Keys that were
/// never used are absent.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn key_last_used(
    connection: &Connection,
    tz_offset_minutes: i16,
) -> Result<BTreeMap<String, i64>, AppError> {
    let mut last_used = BTreeMap::new();
    let mut days = connection
        .prepare_cached(
            "SELECT key_id, MAX(day) FROM llm_usage_daily WHERE key_id <> '' GROUP BY key_id",
        )
        .map_err(db)?;
    for row in days
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(db)?
    {
        let (id, day) = row.map_err(db)?;
        if let Some(at_ms) = crate::llm::usage::local_midnight_ms(&day, tz_offset_minutes) {
            last_used.insert(id, at_ms);
        }
    }
    let mut statement = connection
        .prepare_cached(
            "SELECT id, (SELECT MAX(ts_ms) FROM llm_request_log WHERE key_id = llm_keys.id) \
             FROM llm_keys",
        )
        .map_err(db)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
        })
        .map_err(db)?;
    for row in rows {
        if let (id, Some(at_ms)) = row.map_err(db)? {
            last_used.insert(id, at_ms);
        }
    }
    Ok(last_used)
}

/// Deletes a key: its listed models and probe results go with it (`ON DELETE CASCADE`), its
/// secrets are deleted here. The row and its secrets go together: a failed secret delete leaves the key as it was, never an orphaned
/// vault entry.
///
/// # Errors
/// [`AppError::NotFound`] when it does not exist.
pub fn delete_key(connection: &Connection, id: &str) -> Result<(), AppError> {
    atomic(connection, |connection| {
        let deleted = connection
            .execute("DELETE FROM llm_keys WHERE id = ?1", params![id])
            .map_err(db)?;
        if deleted == 0 {
            return Err(AppError::not_found(format!("key {id} not found")));
        }
        delete_key_secrets(connection, id)
    })
}

/// Test-only helper: toggles a key's `enabled` flag and stamps `updated_at_ms` directly without
/// advancing the monotonic timestamp.
#[cfg(feature = "test-support")]
pub fn set_key_enabled_at_for_test(
    connection: &Connection,
    id: &str,
    enabled: bool,
    updated_at_ms: i64,
) -> Result<(), AppError> {
    let updated = connection
        .execute(
            "UPDATE llm_keys SET enabled = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id, flag(enabled), updated_at_ms],
        )
        .map_err(db)?;
    if updated == 0 {
        return Err(AppError::not_found(format!("key {id} not found")));
    }
    Ok(())
}
