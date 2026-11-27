//! A key's secret-store side: the plaintext views for routing and sync ([`ResolvedKey`], never
//! serialized, redacted `Debug`), reveal, the management token and secret deletion.
//!
//! The API key lives under `llm.key.<id>`, the console token under
//! `llm.key.<id>.management_token`. Neither ever enters a DTO: only masked hints do.

use rusqlite::Connection;

use super::ProviderKey;
use super::keys::{get_key, list_keys};
use crate::storage::secrets;
use spanbay_base::error::AppError;
use spanbay_base::secrets::SecretRef;

/// A key with its secrets loaded, for routing and sync only: it is never serialized and prints
/// `***`, so a stray `{:?}` or a DTO cannot leak a secret.
#[derive(Clone, PartialEq, Default)]
pub struct ResolvedKey {
    pub key: ProviderKey,
    /// The plaintext secret; empty when none is stored.
    pub secret: String,
    /// The plaintext management token, when one is stored (for balance adapters that need a
    /// console credential).
    pub management_token: Option<String>,
}

/// Id suffix of [`ResolvedKey::keyless`]: `~` never appears in a stored key id.
const KEYLESS_SUFFIX: &str = "~keyless";

/// Whether `key_id` names an implicit keyless target (no `llm_keys` row behind it).
#[must_use]
pub(crate) fn is_keyless(key_id: &str) -> bool {
    key_id.ends_with(KEYLESS_SUFFIX)
}

impl ResolvedKey {
    /// The implicit target of a provider that takes no key (`AuthStyle::None`): enabled, without
    /// a secret, named after the provider. It exists only in routing snapshots.
    #[must_use]
    pub fn keyless(provider_id: &str) -> Self {
        Self {
            key: ProviderKey {
                id: format!("{provider_id}{KEYLESS_SUFFIX}"),
                provider_id: provider_id.to_owned(),
                name: "No key".to_owned(),
                enabled: true,
                ..ProviderKey::default()
            },
            ..Self::default()
        }
    }
}

impl std::fmt::Debug for ResolvedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedKey")
            .field("key", &self.key)
            .field("secret", &"***")
            .field(
                "management_token",
                &self.management_token.as_ref().map(|_| "***"),
            )
            .finish()
    }
}

/// The secret ref of a key's management token (the key listing joins on the same text).
pub(super) fn management_token_secret(key_id: &str) -> SecretRef {
    SecretRef(format!("llm.key.{key_id}.management_token"))
}

/// Deletes every secret of a key (its API key and its management token).
pub(super) fn delete_key_secrets(connection: &Connection, key_id: &str) -> Result<(), AppError> {
    secrets::delete(connection, SecretRef::llm_key(key_id).as_str())?;
    secrets::delete(connection, management_token_secret(key_id).as_str())?;
    Ok(())
}

/// Every key with its secrets loaded, in order: the routing snapshot and config sync need the
/// plaintext, nothing that is serialized does.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_resolved_keys(connection: &Connection) -> Result<Vec<ResolvedKey>, AppError> {
    list_keys(connection, None)?
        .into_iter()
        .map(|key| resolve_key(connection, key))
        .collect()
}

/// The key with `id`, with its secrets loaded.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn get_resolved_key(
    connection: &Connection,
    id: &str,
) -> Result<Option<ResolvedKey>, AppError> {
    get_key(connection, id)?
        .map(|key| resolve_key(connection, key))
        .transpose()
}

fn resolve_key(connection: &Connection, key: ProviderKey) -> Result<ResolvedKey, AppError> {
    let secret =
        secrets::get(connection, SecretRef::llm_key(&key.id).as_str())?.unwrap_or_default();
    let management_token = if key.has_management_token {
        secrets::get(connection, management_token_secret(&key.id).as_str())?
            .filter(|token| !token.is_empty())
    } else {
        None
    };
    Ok(ResolvedKey {
        key,
        secret,
        management_token,
    })
}

/// The plaintext secret of key `id`, for the user who asked to see it. Never logged; the errors
/// never contain it.
///
/// # Errors
/// [`AppError::NotFound`] when the key does not exist or holds no secret, [`AppError::Database`]
/// on SQLite failure.
pub fn reveal_key(connection: &Connection, id: &str) -> Result<String, AppError> {
    if get_key(connection, id)?.is_none() {
        return Err(AppError::not_found(format!("key {id} not found")));
    }
    secrets::get(connection, SecretRef::llm_key(id).as_str())?
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| AppError::not_found("the key holds no secret"))
}

/// Stores or clears the management (console) token of a key, write-only: `None` or a blank token
/// removes it.
///
/// # Errors
/// [`AppError::NotFound`] when the key does not exist, [`AppError::Database`] otherwise.
pub fn set_key_management_token(
    connection: &Connection,
    key_id: &str,
    token: Option<&str>,
    now_ms: i64,
) -> Result<(), AppError> {
    if get_key(connection, key_id)?.is_none() {
        return Err(AppError::not_found(format!("key {key_id} not found")));
    }
    let secret_ref = management_token_secret(key_id);
    match token.map(str::trim).filter(|token| !token.is_empty()) {
        Some(token) => {
            secrets::put(connection, secret_ref.as_str(), token, now_ms)?;
        }
        None => {
            secrets::delete(connection, secret_ref.as_str())?;
        }
    }
    Ok(())
}
