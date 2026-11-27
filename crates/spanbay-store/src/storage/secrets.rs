//! Raw CRUD on the `secrets` table, one row per logical reference, plus the pruning of transport
//! secrets whose request or certificate is gone. The free functions take a [`Connection`] so the
//! LLM store, which runs on its own connections, shares them with [`WorkspaceStore`]; the trait
//! callers use lives in [`spanbay_base::secrets`]. Fixed-shape statements are cached: key
//! resolution reads a secret on every gateway request.

use rusqlite::{Connection, OptionalExtension, params};

use super::{StorageError, WorkspaceStore};
use spanbay_base::secrets::{SecretHint, mask_secret_hint};

/// The stored value of `secret_ref`, if any.
pub fn get(connection: &Connection, secret_ref: &str) -> Result<Option<String>, StorageError> {
    Ok(connection
        .prepare_cached("SELECT value_text FROM secrets WHERE secret_ref = ?1")?
        .query_row(params![secret_ref], |row| row.get(0))
        .optional()?)
}

/// Upserts `plaintext` under `secret_ref` and returns its masked hint.
pub fn put(
    connection: &Connection,
    secret_ref: &str,
    plaintext: &str,
    now_ms: i64,
) -> Result<SecretHint, StorageError> {
    let hint = mask_secret_hint(plaintext);
    connection
        .prepare_cached(
            "INSERT INTO secrets (secret_ref, value_text, hint, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(secret_ref) DO UPDATE SET
            value_text = excluded.value_text,
            hint = excluded.hint,
            updated_at_ms = excluded.updated_at_ms",
        )?
        .execute(params![secret_ref, plaintext, hint, now_ms])?;
    Ok(SecretHint {
        secret_ref: secret_ref.to_owned(),
        has_secret: true,
        hint,
        updated_at_ms: now_ms,
    })
}

/// Removes `secret_ref`; returns whether it existed.
pub fn delete(connection: &Connection, secret_ref: &str) -> Result<bool, StorageError> {
    Ok(connection
        .prepare_cached("DELETE FROM secrets WHERE secret_ref = ?1")?
        .execute(params![secret_ref])?
        > 0)
}

/// The masked summary of `secret_ref`; `has_secret` is false when nothing is stored.
pub fn hint(connection: &Connection, secret_ref: &str) -> Result<SecretHint, StorageError> {
    let row = connection
        .prepare_cached("SELECT hint, updated_at_ms FROM secrets WHERE secret_ref = ?1")?
        .query_row(params![secret_ref], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .optional()?;
    Ok(match row {
        Some((hint, updated_at_ms)) => SecretHint {
            secret_ref: secret_ref.to_owned(),
            has_secret: true,
            hint,
            updated_at_ms,
        },
        None => SecretHint {
            secret_ref: secret_ref.to_owned(),
            has_secret: false,
            hint: String::new(),
            updated_at_ms: 0,
        },
    })
}

/// Every stored value of at least `min_chars` characters (for redaction needles).
pub fn values_at_least(
    connection: &Connection,
    min_chars: usize,
) -> Result<Vec<String>, StorageError> {
    let mut statement = connection.prepare_cached("SELECT value_text FROM secrets")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    // Characters, not bytes: the redaction floor is a character count.
    rows.filter(|value| {
        value
            .as_ref()
            .map_or(true, |value| value.chars().count() >= min_chars)
    })
    .collect::<Result<_, _>>()
    .map_err(Into::into)
}

impl WorkspaceStore {
    pub fn secret_get(&self, secret_ref: &str) -> Result<Option<String>, StorageError> {
        get(&self.connection, secret_ref)
    }

    #[cfg(test)]
    pub(crate) fn secret_put(
        &self,
        secret_ref: &str,
        plaintext: &str,
        now_ms: i64,
    ) -> Result<SecretHint, StorageError> {
        put(&self.connection, secret_ref, plaintext, now_ms)
    }

    pub fn secret_hint(&self, secret_ref: &str) -> Result<SecretHint, StorageError> {
        hint(&self.connection, secret_ref)
    }

    #[cfg(test)]
    pub(crate) fn secret_values_at_least(
        &self,
        min_chars: usize,
    ) -> Result<Vec<String>, StorageError> {
        values_at_least(&self.connection, min_chars)
    }

    /// Deletes transport secrets whose request, TLS certificate or workspace (OAuth 2.0 tokens)
    /// no longer exists.
    pub fn prune_orphan_transport_secrets(&self) -> Result<usize, StorageError> {
        // `request.<id>.proxy_password`: the id starts at 9 = 1 + len("request.") and is
        // len - 23 characters long, 23 = len("request.") + len(".proxy_password").
        let requests = self.connection.execute(
            "DELETE FROM secrets WHERE secret_ref LIKE 'request.%.proxy_password'
                   AND substr(secret_ref, 9, length(secret_ref) - 23) NOT IN (SELECT id FROM requests)",
            [],
        )?;
        let certificates = self.connection.execute(
            "DELETE FROM secrets WHERE secret_ref LIKE 'tls.%.%.passphrase'
                   AND secret_ref NOT IN (
                       SELECT 'tls.' || workspace_id || '.' || id || '.passphrase' FROM tls_certificates)",
            [],
        )?;
        // `oauth.<workspace>.<digest>.*`: kept while a workspace owns the prefix.
        let oauth = self.connection.execute(
            "DELETE FROM secrets WHERE secret_ref LIKE 'oauth.%'
                   AND NOT EXISTS (
                       SELECT 1 FROM workspaces w
                       WHERE substr(secret_ref, 1, length(w.id) + 7) = 'oauth.' || w.id || '.')",
            [],
        )?;
        Ok(requests + certificates + oauth)
    }
}
