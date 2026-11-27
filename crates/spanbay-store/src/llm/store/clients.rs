//! Gateway clients over `llm_gateway_clients`: the record with its token stored as SHA-256 only,
//! the allow-list, and the rotate/revoke/delete rules. Minting and the constant-time token match
//! live in `spanbay_llm_domain::llm::records` (the clients module).

use rusqlite::{Connection, OptionalExtension, Row, params};

#[cfg(test)]
use super::TokenLookup;
use super::{
    ClientHashEntry, ClientInput, ClientWithToken, GatewayClient, MAX_ALLOW_ENTRIES, MAX_CLIENTS,
    MIGRATED_CLIENT_ID, MintedToken, TOUCH_INTERVAL_MS, atomic, db, flag, json_text, mint_token,
};
use crate::llm::client_config::{AppliedFile, ClientApp};
use spanbay_base::crypto::{random_hex, sha256_hex};
use spanbay_base::error::AppError;
use spanbay_llm_domain::llm::records::{
    ClientLimits, ClientPolicyInput, check_client_input, check_client_limits,
};

const CLIENT_COLUMNS: &str = "id, name, token_prefix, token_last4, allow_all, \
     allowed_aliases_json, allowed_models_json, created_at_ms, last_used_at_ms, revoked_at_ms, \
     default_model, unknown_model_to_default";

/// What `allowed_aliases_json` holds: the alias list, or (once a client has limits) an object
/// with the list and the limits. The column keeps its name and type; no schema change.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum AliasColumn {
    List(Vec<String>),
    Envelope {
        #[serde(default)]
        aliases: Vec<String>,
        #[serde(default)]
        limits: ClientLimits,
    },
}

/// The alias list and the limits stored in `allowed_aliases_json`; unreadable text is neither.
fn read_alias_column(text: &str) -> (Vec<String>, ClientLimits) {
    match serde_json::from_str(text) {
        Ok(AliasColumn::List(aliases)) => (aliases, ClientLimits::default()),
        Ok(AliasColumn::Envelope { aliases, limits }) => (aliases, limits),
        Err(_) => (Vec::new(), ClientLimits::default()),
    }
}

/// The `allowed_aliases_json` text of `aliases` and `limits`: a bare list without limits.
fn alias_column(aliases: &[String], limits: &ClientLimits) -> Result<String, AppError> {
    if limits.is_empty() {
        return json_text(&aliases);
    }
    json_text(&serde_json::json!({"aliases": aliases, "limits": limits}))
}

fn client_from_row(row: &Row<'_>) -> rusqlite::Result<GatewayClient> {
    let (aliases, limits) = read_alias_column(&row.get::<_, String>(5)?);
    let models: String = row.get(6)?;
    let id: String = row.get(0)?;
    Ok(GatewayClient {
        migrated: id == MIGRATED_CLIENT_ID,
        id,
        name: row.get(1)?,
        token_prefix: row.get(2)?,
        token_last4: row.get(3)?,
        allow_all: row.get::<_, i64>(4)? != 0,
        allowed_aliases: aliases,
        allowed_models: serde_json::from_str(&models).unwrap_or_default(),
        created_at_ms: row.get(7)?,
        last_used_ms: row.get(8)?,
        revoked_at_ms: row.get(9)?,
        default_model: row.get(10)?,
        unknown_model_to_default: row.get::<_, i64>(11)? != 0,
        limits,
    })
}

/// Refuses a name another record (any state, `except_id` excluded) already has, compared
/// case-insensitively.
fn ensure_name_free(
    connection: &Connection,
    name: &str,
    except_id: Option<&str>,
) -> Result<(), AppError> {
    let wanted = name.to_lowercase();
    let mut statement = connection
        .prepare_cached("SELECT id, name FROM llm_gateway_clients")
        .map_err(db)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(db)?;
    for row in rows {
        let (id, existing) = row.map_err(db)?;
        if except_id != Some(id.as_str()) && existing.to_lowercase() == wanted {
            return Err(AppError::validation(format!(
                "A client named \"{name}\" already exists"
            )));
        }
    }
    Ok(())
}

/// Creates an active client with a fresh token; only the token's hash is stored.
///
/// # Errors
/// [`AppError::Validation`] for a bad name or allow-list, a duplicate name or the 65th record;
/// [`AppError::Internal`] without OS randomness (nothing is created);
/// [`AppError::Database`] on SQLite failure.
pub fn create_client(
    connection: &Connection,
    input: &ClientInput,
    now_ms: i64,
) -> Result<ClientWithToken, AppError> {
    create_client_with(connection, input, now_ms, false)
}

/// [`create_client`] that first deletes revoked records holding the same name
/// (`OnExisting::ReplaceRevoked`); an active record of that name is still refused.
///
/// # Errors
/// As [`create_client`].
pub fn create_client_replacing_revoked(
    connection: &Connection,
    input: &ClientInput,
    now_ms: i64,
) -> Result<ClientWithToken, AppError> {
    create_client_with(connection, input, now_ms, true)
}

/// Deletes the revoked records named `name` (case-insensitively); their log rows keep the name.
fn delete_revoked_named(connection: &Connection, name: &str) -> Result<(), AppError> {
    let wanted = name.to_lowercase();
    let ids: Vec<String> = {
        let mut statement = connection
            .prepare_cached(
                "SELECT id, name FROM llm_gateway_clients WHERE revoked_at_ms IS NOT NULL",
            )
            .map_err(db)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db)?;
        rows.filter_map(Result::ok)
            .filter(|(_, existing)| existing.to_lowercase() == wanted)
            .map(|(id, _)| id)
            .collect()
    };
    let mut delete = connection
        .prepare_cached("DELETE FROM llm_gateway_clients WHERE id = ?1")
        .map_err(db)?;
    for id in ids {
        delete.execute(params![id]).map_err(db)?;
    }
    Ok(())
}

fn create_client_with(
    connection: &Connection,
    input: &ClientInput,
    now_ms: i64,
    replace_revoked: bool,
) -> Result<ClientWithToken, AppError> {
    let checked = check_client_input(input)?;
    let token = mint_token(&checked.name)?;
    let id = format!("cl_{}", random_hex::<6>()?);
    let client = atomic(connection, |transaction| {
        if replace_revoked {
            delete_revoked_named(transaction, &checked.name)?;
        }
        let existing: i64 = transaction
            .query_row("SELECT COUNT(*) FROM llm_gateway_clients", [], |row| {
                row.get(0)
            })
            .map_err(db)?;
        if usize::try_from(existing).unwrap_or(usize::MAX) >= MAX_CLIENTS {
            return Err(AppError::validation(format!(
                "At most {MAX_CLIENTS} clients are allowed; delete a revoked one first"
            )));
        }
        ensure_name_free(transaction, &checked.name, None)?;
        transaction
            .execute(
                "INSERT INTO llm_gateway_clients (id, name, token_sha256, token_prefix, token_last4, \
                     allow_all, allowed_aliases_json, allowed_models_json, created_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    checked.name,
                    sha256_hex(token.token.as_bytes()),
                    token.prefix(),
                    token.last4(),
                    flag(checked.allow_all),
                    json_text(&checked.aliases)?,
                    json_text(&checked.models)?,
                    now_ms,
                ],
            )
            .map_err(db)?;
        get_client(transaction, &id)
    })?;
    Ok(ClientWithToken {
        client,
        token: token.token,
    })
}

/// Seeds a client row with a caller-supplied plaintext token (used by test harnesses).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_client_token(
    connection: &Connection,
    id: &str,
    name: &str,
    token: &str,
) -> Result<(), AppError> {
    let prefix: String = token.chars().take(10).collect();
    connection
        .execute(
            "INSERT INTO llm_gateway_clients (id, name, token_sha256, token_prefix, created_at_ms) \
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![id, name, sha256_hex(token.as_bytes()), prefix],
        )
        .map_err(db)?;
    Ok(())
}

/// Every client, oldest first.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_clients(connection: &Connection) -> Result<Vec<GatewayClient>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {CLIENT_COLUMNS} FROM llm_gateway_clients ORDER BY created_at_ms, id"
        ))
        .map_err(db)?;
    let rows = statement.query_map([], client_from_row).map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// One client.
///
/// # Errors
/// [`AppError::NotFound`] when it does not exist.
pub fn get_client(connection: &Connection, id: &str) -> Result<GatewayClient, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {CLIENT_COLUMNS} FROM llm_gateway_clients WHERE id = ?1"
        ))
        .map_err(db)?;
    statement
        .query_row(params![id], client_from_row)
        .optional()
        .map_err(db)?
        .ok_or_else(|| AppError::not_found(format!("gateway client {id} not found")))
}

/// Saves the name and allow-list of a client (the token and timestamps never change here).
///
/// # Errors
/// [`AppError::Validation`] as for [`create_client`], [`AppError::NotFound`] for an unknown id.
pub fn update_client(
    connection: &Connection,
    id: &str,
    input: &ClientInput,
) -> Result<GatewayClient, AppError> {
    let checked = check_client_input(input)?;
    atomic(connection, |transaction| {
        let current = get_client(transaction, id)?;
        ensure_name_free(transaction, &checked.name, Some(id))?;
        transaction
            .execute(
                "UPDATE llm_gateway_clients SET name = ?2, allow_all = ?3, allowed_aliases_json = ?4, \
                     allowed_models_json = ?5 WHERE id = ?1",
                params![
                    id,
                    checked.name,
                    flag(checked.allow_all),
                    alias_column(&checked.aliases, &current.limits)?,
                    json_text(&checked.models)?,
                ],
            )
            .map_err(db)?;
        get_client(transaction, id)
    })
}

/// Saves a client's limits and, when given, its unknown-name mapping switch (stored with the
/// alias list, see [`AliasColumn`]).
///
/// # Errors
/// [`AppError::Validation`] for limits out of range (`check_client_limits`),
/// [`AppError::NotFound`] for an unknown id.
pub fn set_client_policy(
    connection: &Connection,
    id: &str,
    input: &ClientPolicyInput,
) -> Result<GatewayClient, AppError> {
    let limits = check_client_limits(&input.limits)?;
    atomic(connection, |transaction| {
        let current = get_client(transaction, id)?;
        transaction
            .execute(
                "UPDATE llm_gateway_clients SET allowed_aliases_json = ?2, \
                     unknown_model_to_default = coalesce(?3, unknown_model_to_default) \
                 WHERE id = ?1",
                params![
                    id,
                    alias_column(&current.allowed_aliases, &limits)?,
                    input.unknown_model_to_default.map(flag),
                ],
            )
            .map_err(db)?;
        get_client(transaction, id)
    })
}

/// What a Connect or Update grants the client besides the token.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelGrant {
    /// The model the tool's config now names (`provider_id/model`).
    pub default_model: Option<String>,
    /// Models to add to the allow-list (ignored when the client allows everything).
    pub extend: Vec<String>,
    /// New value of `unknown_model_to_default`; `None` keeps it.
    pub unknown_model_to_default: Option<bool>,
}

fn apply_grant(connection: &Connection, id: &str, grant: &ModelGrant) -> Result<(), AppError> {
    let current = get_client(connection, id)?;
    let mut models = current.allowed_models;
    if !current.allow_all {
        for model in &grant.extend {
            let model = model.trim();
            if !model.is_empty() && !models.iter().any(|known| known == model) {
                models.push(model.to_owned());
            }
        }
        if models.len() + current.allowed_aliases.len() > MAX_ALLOW_ENTRIES {
            return Err(AppError::validation(format!(
                "A client allow-list holds at most {MAX_ALLOW_ENTRIES} entries"
            )));
        }
    }
    connection
        .execute(
            "UPDATE llm_gateway_clients SET allowed_models_json = ?2, \
                 default_model = coalesce(?3, default_model), \
                 unknown_model_to_default = coalesce(?4, unknown_model_to_default) \
             WHERE id = ?1",
            params![
                id,
                json_text(&models)?,
                grant.default_model,
                grant.unknown_model_to_default.map(flag),
            ],
        )
        .map_err(db)?;
    Ok(())
}

/// Applies `grant` to a client in one transaction: its default model, the models its config
/// names joined into the allow-list, the unknown-name mapping.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id, [`AppError::Validation`] for a full allow-list.
pub fn grant_model(
    connection: &Connection,
    id: &str,
    grant: &ModelGrant,
) -> Result<GatewayClient, AppError> {
    atomic(connection, |transaction| {
        apply_grant(transaction, id, grant)?;
        get_client(transaction, id)
    })
}

/// Replaces the token of an active client (the old one stops working at once) and returns the new
/// one once.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id, [`AppError::Validation`] for a revoked client,
/// [`AppError::Internal`] without OS randomness (the old token stays valid).
pub fn rotate_client_token(connection: &Connection, id: &str) -> Result<ClientWithToken, AppError> {
    atomic(connection, |transaction| {
        let current = get_client(transaction, id)?;
        if !current.is_active() {
            return Err(AppError::validation("A revoked client cannot be rotated"));
        }
        let token = mint_token(&current.name)?;
        transaction
            .execute(
                "UPDATE llm_gateway_clients SET token_sha256 = ?2, token_prefix = ?3, token_last4 = ?4, \
                     verified_at_ms = NULL \
                 WHERE id = ?1",
                params![
                    id,
                    sha256_hex(token.token.as_bytes()),
                    token.prefix(),
                    token.last4()
                ],
            )
            .map_err(db)?;
        let client = get_client(transaction, id)?;
        Ok(ClientWithToken {
            client,
            token: token.token,
        })
    })
}

/// Commits a rotation whose token was minted earlier ([`super::mint_token`]) and whose files are
/// already written: swaps the client's token hash and records the ledger rows of every written
/// tool (and `grant`) in ONE transaction, so a failure leaves the old token valid and the ledger
/// unchanged.
/// `expected_sha256` is the hash the caller saw before it minted; a client rotated or revoked
/// meanwhile is refused.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id, [`AppError::Validation`] for a revoked client,
/// [`AppError::Conflict`] when the token changed since `expected_sha256`,
/// [`AppError::Database`] on SQLite failure (nothing is changed).
pub fn commit_rotation(
    connection: &Connection,
    id: &str,
    expected_sha256: &str,
    minted: &MintedToken,
    written: &[(ClientApp, Vec<AppliedFile>)],
    grant: &ModelGrant,
    now_ms: i64,
) -> Result<GatewayClient, AppError> {
    atomic(connection, |transaction| {
        let current = get_client(transaction, id)?;
        if !current.is_active() {
            return Err(AppError::validation("A revoked client cannot be rotated"));
        }
        let changed = transaction
            .execute(
                "UPDATE llm_gateway_clients SET token_sha256 = ?2, token_prefix = ?3, token_last4 = ?4, \
                     verified_at_ms = NULL \
                 WHERE id = ?1 AND token_sha256 = ?5",
                params![
                    id,
                    sha256_hex(minted.token.as_bytes()),
                    minted.prefix(),
                    minted.last4(),
                    expected_sha256
                ],
            )
            .map_err(db)?;
        if changed == 0 {
            return Err(AppError::conflict(
                "The client's token changed while it was being rotated. Try again.",
            ));
        }
        for (app, files) in written {
            super::ledger::insert_rows(transaction, id, *app, files, now_ms)?;
        }
        apply_grant(transaction, id, grant)?;
        get_client(transaction, id)
    })
}

/// Revokes a client: its token is refused from the next lookup, the record stays. Revoking twice
/// keeps the first time.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id.
pub fn revoke_client(
    connection: &Connection,
    id: &str,
    now_ms: i64,
) -> Result<GatewayClient, AppError> {
    atomic(connection, |transaction| {
        get_client(transaction, id)?;
        transaction
            .execute(
                "UPDATE llm_gateway_clients SET revoked_at_ms = coalesce(revoked_at_ms, ?2) \
                 WHERE id = ?1",
                params![id, now_ms],
            )
            .map_err(db)?;
        get_client(transaction, id)
    })
}

/// Deletes a revoked client for good (its log rows keep `client_name`).
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id, [`AppError::Validation`] for an active client.
pub fn delete_client(connection: &Connection, id: &str) -> Result<(), AppError> {
    atomic(connection, |transaction| {
        if get_client(transaction, id)?.is_active() {
            return Err(AppError::validation("Revoke the client before deleting it"));
        }
        transaction
            .execute("DELETE FROM llm_gateway_clients WHERE id = ?1", params![id])
            .map_err(db)?;
        Ok(())
    })
}

/// Every client with its token hash, for the gateway's authentication cache.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn client_hashes_snapshot(connection: &Connection) -> Result<Vec<ClientHashEntry>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {CLIENT_COLUMNS}, token_sha256 FROM llm_gateway_clients ORDER BY created_at_ms, id"
        ))
        .map_err(db)?;
    let rows = statement
        .query_map([], |row| {
            Ok(ClientHashEntry {
                client: client_from_row(row)?,
                token_sha256: row.get(12)?,
            })
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// [`super::match_token`] over the stored clients.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
#[cfg(test)]
pub(crate) fn find_client_by_token(
    connection: &Connection,
    presented: &str,
) -> Result<TokenLookup, AppError> {
    Ok(super::match_token(
        &client_hashes_snapshot(connection)?,
        presented,
    ))
}

/// Records that `id` authenticated a request at `now_ms`, at most once per
/// [`TOUCH_INTERVAL_MS`] per client; returns whether the row was written.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn touch_client(connection: &Connection, id: &str, now_ms: i64) -> Result<bool, AppError> {
    let written = connection
        .execute(
            "UPDATE llm_gateway_clients SET last_used_at_ms = ?2 \
             WHERE id = ?1 AND (last_used_at_ms IS NULL OR last_used_at_ms <= ?3)",
            params![id, now_ms, now_ms.saturating_sub(TOUCH_INTERVAL_MS)],
        )
        .map_err(db)?;
    Ok(written > 0)
}

/// Records that a request passed with `id`'s current token at `at_ms` (a served Verify test or a
/// real request that got a success status); only a later instant replaces an earlier one.
/// Returns whether the row changed.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn mark_client_verified(
    connection: &Connection,
    id: &str,
    at_ms: i64,
) -> Result<bool, AppError> {
    let written = connection
        .execute(
            "UPDATE llm_gateway_clients SET verified_at_ms = ?2 \
             WHERE id = ?1 AND (verified_at_ms IS NULL OR verified_at_ms < ?2)",
            params![id, at_ms],
        )
        .map_err(db)?;
    Ok(written > 0)
}

/// When a request last passed with `id`'s current token; `None` when none has since the token
/// was created or rotated.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn client_verified_at(connection: &Connection, id: &str) -> Result<Option<i64>, AppError> {
    Ok(connection
        .query_row(
            "SELECT verified_at_ms FROM llm_gateway_clients WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db)?
        .flatten())
}

#[cfg(test)]
mod tests;
