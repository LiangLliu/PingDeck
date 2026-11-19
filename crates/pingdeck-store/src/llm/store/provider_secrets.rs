//! The secrets of a provider: extra request headers, custom balance-query headers and the proxy
//! password.
//!
//! Single responsibility: keep header *values* out of `llm_providers` (they go to the secret
//! store; the row keeps names and `{{secret:...}}` placeholders), resolve them again for sending,
//! and delete them with the provider. A value never appears in an error, a log line or a DTO.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, params};

use super::providers::get_provider;
use super::{Provider, atomic, db, json_text};
use crate::llm::balance_state::{CustomBalanceQuery, CustomHeaderRef, ResolvedHeaders};
use crate::llm::providers::origin_label;
use crate::llm::wire::Quirks;
use crate::storage::secrets;
use pingdeck_base::error::AppError;
use pingdeck_base::secrets::SecretRef;

/// What opens a placeholder; a header value holding it must be exactly this provider's own.
const PLACEHOLDER_OPEN: &str = "{{secret:";

pub(super) fn header_secret(provider_id: &str, name: &str) -> SecretRef {
    SecretRef(format!("llm.{provider_id}.header.{name}"))
}

pub(super) fn balance_header_secret(provider_id: &str, name: &str) -> SecretRef {
    SecretRef(format!("llm.{provider_id}.balance.header.{name}"))
}

pub(super) fn proxy_secret(provider_id: &str) -> SecretRef {
    SecretRef(format!("llm.provider.{provider_id}.proxy_password"))
}

/// Moves the password in `proxy_url` into the secret store under the provider's proxy ref and
/// returns the masked URL (keeping any stored password when the URL is already masked).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub(super) fn split_proxy_password(
    connection: &Connection,
    provider_id: &str,
    proxy_url: &str,
    now_ms: i64,
) -> Result<String, AppError> {
    let proxy_url = proxy_url.trim();
    let split = pingdeck_base::proxy_url::split(proxy_url);
    let reference = proxy_secret(provider_id);
    if let Some(password) = split.password {
        secrets::put(connection, reference.as_str(), &password, now_ms)?;
    } else if !split.is_masked {
        secrets::delete(connection, reference.as_str())?;
    }
    Ok(split.masked)
}

/// Puts the stored password back into a masked proxy URL, for the HTTP clients; the DTO still
/// serializes it masked.
pub(super) fn hydrate_proxy(
    connection: &Connection,
    provider: &mut Provider,
) -> Result<(), AppError> {
    if provider.proxy_url.is_empty() {
        return Ok(());
    }
    if let Some(password) = secrets::get(connection, proxy_secret(&provider.id).as_str())? {
        provider.proxy_url = pingdeck_base::proxy_url::restore(&provider.proxy_url, &password);
    }
    Ok(())
}

/// Replaces every plaintext header value of `quirks` by its placeholder and stores the value
/// under [`header_secret`]; deletes the secrets of headers that are gone. Placeholders of the
/// provider's own headers pass through; any other `{{secret:...}}` is refused, so a header can
/// never read another provider's or key's secret.
///
/// # Errors
/// [`AppError::Validation`] naming the header (never its value).
pub(super) fn split_extra_headers(
    connection: &Connection,
    provider_id: &str,
    quirks: &Quirks,
    now_ms: i64,
) -> Result<Quirks, AppError> {
    let mut split = quirks.clone();
    for (name, value) in &mut split.extra_headers {
        let own = header_secret(provider_id, name);
        if *value == own.placeholder() {
            continue;
        }
        if value.contains(PLACEHOLDER_OPEN) {
            return Err(AppError::validation(format!(
                "the {name} header cannot reference a secret"
            )));
        }
        if value.is_empty() {
            secrets::delete(connection, own.as_str())?;
            continue;
        }
        secrets::put(connection, own.as_str(), value, now_ms)?;
        *value = own.placeholder();
    }
    if let Some(stored) = stored_quirks(connection, provider_id)? {
        for name in stored.extra_headers.keys() {
            if !split.extra_headers.contains_key(name) {
                secrets::delete(connection, header_secret(provider_id, name).as_str())?;
            }
        }
    }
    Ok(split)
}

fn stored_quirks(connection: &Connection, provider_id: &str) -> Result<Option<Quirks>, AppError> {
    let text: Option<String> = connection
        .query_row(
            "SELECT quirks_json FROM llm_providers WHERE id = ?1",
            params![provider_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db)?;
    // An unreadable stored value only means there is nothing to compare against.
    Ok(text.and_then(|text| serde_json::from_str(&text).ok()))
}

/// The extra headers of `provider` with every placeholder resolved, ready to send.
///
/// # Errors
/// [`AppError::Validation`] naming the header when its secret is missing or its value references
/// a secret that is not the header's own.
pub fn resolve_provider_headers(
    connection: &Connection,
    provider: &Provider,
) -> Result<BTreeMap<String, String>, AppError> {
    let mut resolved = BTreeMap::new();
    for (name, value) in &provider.quirks.extra_headers {
        let own = header_secret(&provider.id, name);
        let text = if *value == own.placeholder() {
            secrets::get(connection, own.as_str())?.ok_or_else(|| {
                AppError::validation(format!("the secret of the {name} header is missing"))
            })?
        } else if value.contains(PLACEHOLDER_OPEN) {
            return Err(AppError::validation(format!(
                "the {name} header references a secret that is not its own"
            )));
        } else {
            value.clone()
        };
        resolved.insert(name.clone(), text);
    }
    Ok(resolved)
}

fn stored_balance_headers(
    connection: &Connection,
    provider_id: &str,
) -> Result<Vec<String>, AppError> {
    Ok(stored_custom_balance(connection, provider_id)?
        .map(|query| {
            query
                .headers
                .into_iter()
                .map(|header| header.name)
                .collect()
        })
        .unwrap_or_default())
}

/// The custom balance query stored for `provider_id`: names and references, never values.
pub(super) fn stored_custom_balance(
    connection: &Connection,
    provider_id: &str,
) -> Result<Option<CustomBalanceQuery>, AppError> {
    let text: Option<Option<String>> = connection
        .query_row(
            "SELECT custom_balance_json FROM llm_providers WHERE id = ?1",
            params![provider_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db)?;
    Ok(text
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok()))
}

/// The stored form of `query` for [`super::save_provider`]: canonical secret references, no
/// resolved values, validated; the header secrets of a query it no longer lists are deleted.
///
/// A header whose value is not stored yet is kept as is: the balance runner reports the missing
/// secret, and an import that carries the query but not the value still saves the provider.
///
/// # Errors
/// [`AppError::Validation`] for a query [`CustomBalanceQuery::validate`] refuses.
pub(super) fn stored_custom_balance_json(
    connection: &Connection,
    provider_id: &str,
    query: Option<&CustomBalanceQuery>,
    base_url: &str,
    same_base: bool,
) -> Result<Option<String>, AppError> {
    let before = stored_custom_balance(connection, provider_id)?;
    let previous: Vec<String> = before
        .iter()
        .flat_map(|query| query.headers.iter().map(|header| header.name.clone()))
        .collect();
    let stored = query
        .map(|query| {
            let mut stored = canonical_balance_query(provider_id, query);
            // A cross-host confirmation is this machine's, given for one balance URL beside
            // one base URL. A save keeps it only when both stay; a caller's (or a file's) flag
            // never counts on its own.
            stored.cross_host = stored.is_cross_host(std::slice::from_ref(&base_url.to_owned()))
                && same_base
                && before.as_ref().is_some_and(|before| {
                    before.cross_host && origin_label(&before.url) == origin_label(&query.url)
                });
            stored.validate().map_err(AppError::validation)?;
            Ok::<_, AppError>(stored)
        })
        .transpose()?;
    delete_dropped_balance_secrets(connection, provider_id, &previous, stored.as_ref())?;
    stored.as_ref().map(json_text).transpose()
}

/// `query` as stored: no resolved values, every header pointing at its canonical secret ref.
/// `cross_host` is the input's; the caller decides it.
fn canonical_balance_query(provider_id: &str, query: &CustomBalanceQuery) -> CustomBalanceQuery {
    let mut stored = query.clone();
    stored.resolved_headers = ResolvedHeaders::default();
    stored.headers = query
        .headers
        .iter()
        .map(|header| CustomHeaderRef {
            name: header.name.clone(),
            secret_ref: balance_header_secret(provider_id, &header.name)
                .as_str()
                .to_owned(),
        })
        .collect();
    stored
}

/// Deletes the header secrets of the `previous` header names that `stored` no longer lists.
fn delete_dropped_balance_secrets(
    connection: &Connection,
    provider_id: &str,
    previous: &[String],
    stored: Option<&CustomBalanceQuery>,
) -> Result<(), AppError> {
    let kept: BTreeSet<&str> = stored
        .iter()
        .flat_map(|query| query.headers.iter().map(|header| header.name.as_str()))
        .collect();
    for name in previous.iter().filter(|name| !kept.contains(name.as_str())) {
        secrets::delete(
            connection,
            balance_header_secret(provider_id, name).as_str(),
        )?;
    }
    Ok(())
}

/// Saves (or, with `None`, removes) the custom balance query of a provider.
///
/// `header_values` maps a header name to its new plaintext value; it goes to the secret store
/// under `llm.<provider_id>.balance.header.<NAME>` and the stored JSON keeps the name and that
/// reference only. A header of the query without a value here keeps its stored secret; one that
/// has none yet is an error. Secrets of headers the query no longer lists are deleted.
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist, [`AppError::Validation`] for an
/// invalid query or a header without a value (naming the header, never its value).
pub fn set_custom_balance(
    connection: &Connection,
    provider_id: &str,
    query: Option<&CustomBalanceQuery>,
    header_values: &BTreeMap<String, String>,
    now_ms: i64,
) -> Result<(), AppError> {
    atomic(connection, |connection| {
        let Some(provider) = get_provider(connection, provider_id)? else {
            return Err(AppError::not_found(format!(
                "provider {provider_id} not found"
            )));
        };
        let previous = stored_balance_headers(connection, provider_id)?;
        let stored = match query {
            None => None,
            Some(query) => {
                let mut stored = canonical_balance_query(provider_id, query);
                stored.validate().map_err(AppError::validation)?;
                // A query on another host than the provider's needs the user's confirmation, which
                // is recorded; on the provider's own host the flag is cleared.
                let cross_host = stored.is_cross_host(std::slice::from_ref(&provider.base_url));
                if cross_host && !query.cross_host {
                    return Err(AppError::confirmation_required(
                        "the balance URL is on another host than the provider: confirm to save it",
                    ));
                }
                stored.cross_host = cross_host;
                for header in &stored.headers {
                    match header_values.get(&header.name) {
                        Some(value) => {
                            secrets::put(connection, &header.secret_ref, value, now_ms)?;
                        }
                        None if secrets::get(connection, &header.secret_ref)?.is_some() => {}
                        None => {
                            return Err(AppError::validation(format!(
                                "the {} header needs a value",
                                header.name
                            )));
                        }
                    }
                }
                Some(stored)
            }
        };
        delete_dropped_balance_secrets(connection, provider_id, &previous, stored.as_ref())?;
        let json = stored.as_ref().map(json_text).transpose()?;
        connection
            .execute(
                "UPDATE llm_providers SET custom_balance_json = ?2 WHERE id = ?1",
                params![provider_id, json],
            )
            .map_err(db)?;
        Ok(())
    })
}

/// The saved custom balance query of a provider with its header values resolved from the secret
/// store, ready for the balance runner; `None` when the provider has no query.
///
/// # Errors
/// [`AppError::Validation`] naming the header whose secret is missing.
pub fn resolved_custom_balance(
    connection: &Connection,
    provider_id: &str,
) -> Result<Option<CustomBalanceQuery>, AppError> {
    let Some(mut query) = stored_custom_balance(connection, provider_id)? else {
        return Ok(None);
    };
    let mut headers = Vec::with_capacity(query.headers.len());
    for header in &query.headers {
        let value = secrets::get(
            connection,
            balance_header_secret(provider_id, &header.name).as_str(),
        )?
        .ok_or_else(|| {
            AppError::validation(format!(
                "the secret of the {} header is missing",
                header.name
            ))
        })?;
        headers.push((header.name.clone(), value));
    }
    query.resolved_headers = ResolvedHeaders(headers);
    Ok(Some(query))
}

/// The masked hints of a provider's write-only header secrets. Never a value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderHints {
    /// Extra request header name -> hint of its secret (empty when unset).
    pub extra: BTreeMap<String, String>,
    /// Custom balance query header name -> hint of its secret (empty when unset).
    pub balance: BTreeMap<String, String>,
}

/// The [`HeaderHints`] of `provider`: one entry per extra header and per header of its saved
/// custom balance query, the masked hint of the stored secret or an empty string when none is.
///
/// # Errors
/// [`AppError::Database`] when the secret store cannot be read.
pub fn provider_header_hints(
    connection: &Connection,
    provider: &Provider,
) -> Result<HeaderHints, AppError> {
    let hint_of = |reference: &SecretRef| -> Result<String, AppError> {
        Ok(secrets::hint(connection, reference.as_str())?.hint)
    };
    let mut hints = HeaderHints::default();
    for name in provider.quirks.extra_headers.keys() {
        hints
            .extra
            .insert(name.clone(), hint_of(&header_secret(&provider.id, name))?);
    }
    for header in provider
        .custom_balance
        .iter()
        .flat_map(|query| &query.headers)
    {
        hints.balance.insert(
            header.name.clone(),
            hint_of(&balance_header_secret(&provider.id, &header.name))?,
        );
    }
    Ok(hints)
}

/// Deletes every secret of a provider's own (extra headers, custom balance headers); the secrets
/// of its keys are deleted by the key code.
pub(super) fn delete_provider_secrets(
    connection: &Connection,
    provider_id: &str,
) -> Result<(), AppError> {
    if let Some(quirks) = stored_quirks(connection, provider_id)? {
        for name in quirks.extra_headers.keys() {
            secrets::delete(connection, header_secret(provider_id, name).as_str())?;
        }
    }
    for name in stored_balance_headers(connection, provider_id)? {
        secrets::delete(
            connection,
            balance_header_secret(provider_id, &name).as_str(),
        )?;
    }
    secrets::delete(connection, proxy_secret(provider_id).as_str())?;
    Ok(())
}
