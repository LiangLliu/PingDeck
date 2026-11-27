//! Providers: CRUD over `llm_providers` (secrets go to the secret store, key rows are in `keys`).
//! Pure provider types and URL/header/name validation live in
//! `spanbay_llm_domain::llm::providers`; the server-owned columns are written in `provider_state`.

use rusqlite::{Connection, Row, params};
use serde::de::DeserializeOwned;

use super::identity::{Purged, bury, provider_name_holder, purge_references, rename_recorded};
use super::key_secrets::{delete_key_secrets, management_token_secret};
use super::keys::list_keys;
use super::provider_secrets::{
    delete_provider_secrets, hydrate_proxy, split_extra_headers, split_proxy_password,
    stored_custom_balance_json,
};
use super::{atomic, db, enum_from_text, enum_text, flag, from_json, json_text};
use crate::llm::providers::{
    Provider, SaveProviderOptions, heal_new_api_tie, next_stamp, origin_label, outbound_hosts,
    unknown_family, validate_base_url, validate_extra_headers, validate_name,
    validate_provider_chars, validate_proxy_url, validate_relative_path,
};
use crate::llm::wire::{GatewayFamily, Quirks};
use crate::storage::secrets;
use spanbay_base::error::{AppError, ErrorCode};

const PROVIDER_COLUMNS: &str = "id, name, preset_id, category, base_url, auth, endpoints_json, \
     models_endpoint, quirks_json, enabled, position, notes, created_at_ms, updated_at_ms, proxy_url, \
     gateway_family, quota_per_unit, display_in_currency, sniff_evidence_json, custom_balance_json, \
     primary_protocol, model_drift_json, needs_resniff, sniff_summary_json, balance_off, \
     key_strategy, key_cooldown, review_pending_since_ms";

/// A server-owned cache column as `T`; a value that no longer parses counts as absent, so one bad
/// cache never fails a listing.
fn cached<T: DeserializeOwned>(text: Option<String>) -> Option<T> {
    text.and_then(|text| serde_json::from_str(&text).ok())
}

/// Reads a row selected with [`PROVIDER_COLUMNS`]. The columns read before the struct is built go
/// first, then the JSON columns, then the rest in field order: the first failure is reported.
fn provider_from_row(row: &Row<'_>) -> Result<Provider, AppError> {
    let auth: String = row.get(5).map_err(db)?;
    let endpoints: String = row.get(6).map_err(db)?;
    let quirks: String = row.get(8).map_err(db)?;
    let id: String = row.get(0).map_err(db)?;
    let family: String = row.get(15).map_err(db)?;
    let evidence: Option<String> = row.get(18).map_err(db)?;
    let custom_balance: Option<String> = row.get(19).map_err(db)?;
    let primary: Option<String> = row.get(20).map_err(db)?;
    let drift: Option<String> = row.get(21).map_err(db)?;
    let summary: Option<String> = row.get(23).map_err(db)?;
    let strategy: String = row.get(25).map_err(db)?;
    let mut provider = Provider {
        // A bad auth text is a corrupt row (Database), not an unknown enum to default.
        auth: from_json(&format!("\"{auth}\""), "provider auth")?,
        endpoints: from_json(&endpoints, "provider endpoints")?,
        quirks: from_json(&quirks, "provider quirks")?,
        id,
        name: row.get(1).map_err(db)?,
        preset_id: row.get(2).map_err(db)?,
        category: row.get(3).map_err(db)?,
        base_url: row.get(4).map_err(db)?,
        models_endpoint: row.get(7).map_err(db)?,
        enabled: row.get::<_, i64>(9).map_err(db)? != 0,
        position: row.get(10).map_err(db)?,
        notes: row.get(11).map_err(db)?,
        created_at_ms: row.get(12).map_err(db)?,
        updated_at_ms: row.get(13).map_err(db)?,
        proxy_url: row.get(14).map_err(db)?,
        gateway_family: enum_from_text(&family).unwrap_or(GatewayFamily::UnknownCompatible),
        quota_per_unit: row.get(16).map_err(db)?,
        display_in_currency: row
            .get::<_, Option<i64>>(17)
            .map_err(db)?
            .map(|value| value != 0),
        sniff_evidence: cached(evidence).unwrap_or_default(),
        custom_balance: cached(custom_balance),
        primary_protocol: primary.as_deref().and_then(enum_from_text),
        model_drift: cached(drift),
        needs_resniff: row.get::<_, i64>(22).map_err(db)? != 0,
        sniff_summary: cached(summary),
        balance_off: row.get::<_, i64>(24).map_err(db)? != 0,
        key_strategy: Some(enum_from_text(&strategy).unwrap_or_default()),
        key_cooldown: Some(row.get::<_, i64>(26).map_err(db)? != 0),
        review_pending_since_ms: row.get(27).map_err(db)?,
    };
    heal_new_api_tie(&mut provider);
    Ok(provider)
}

/// Every provider, in display order.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure or unreadable stored JSON.
pub fn list_providers(connection: &Connection) -> Result<Vec<Provider>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {PROVIDER_COLUMNS} FROM llm_providers ORDER BY position, name, id"
        ))
        .map_err(db)?;
    let mut rows = statement.query([]).map_err(db)?;
    let mut providers = Vec::new();
    while let Some(row) = rows.next().map_err(db)? {
        providers.push(provider_from_row(row)?);
    }
    for provider in &mut providers {
        hydrate_proxy(connection, provider)?;
    }
    Ok(providers)
}

/// The provider with `id`, its proxy password restored for the HTTP clients.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure or unreadable stored JSON.
pub fn get_provider(connection: &Connection, id: &str) -> Result<Option<Provider>, AppError> {
    // The cached statement is released before `hydrate_proxy` reads the secret store.
    let mut provider = {
        let mut statement = connection
            .prepare_cached(&format!(
                "SELECT {PROVIDER_COLUMNS} FROM llm_providers WHERE id = ?1"
            ))
            .map_err(db)?;
        let mut rows = statement.query(params![id]).map_err(db)?;
        rows.next()
            .map_err(db)?
            .map(provider_from_row)
            .transpose()?
    };
    if let Some(provider) = &mut provider {
        hydrate_proxy(connection, provider)?;
    }
    Ok(provider)
}

/// Inserts or updates a provider (matched by id); `created_at_ms` of an existing row is kept.
///
/// # Errors
/// As [`save_provider_with`] without a version check or a host-change confirmation.
pub fn save_provider(
    connection: &Connection,
    provider: &Provider,
    now_ms: i64,
) -> Result<Provider, AppError> {
    save_provider_guarded(connection, provider, None, now_ms)
}

/// [`save_provider`] that refuses when the stored row is not the version the caller read.
///
/// # Errors
/// As [`save_provider_with`] without a host-change confirmation.
pub(crate) fn save_provider_guarded(
    connection: &Connection,
    provider: &Provider,
    expected_updated_at_ms: Option<i64>,
    now_ms: i64,
) -> Result<Provider, AppError> {
    let options = SaveProviderOptions {
        expected_updated_at_ms,
        confirm_host_change: false,
        keep_former_name: false,
    };
    save_provider_with(connection, provider, options, now_ms)
}

/// After a confirmed host change: the provider's keys are switched off (their values stay), and
/// what was learnt about the old host goes.
fn forget_old_host(connection: &Connection, id: &str, stamp: i64) -> Result<(), AppError> {
    for key in list_keys(connection, Some(id))? {
        secrets::delete(connection, management_token_secret(&key.id).as_str())?;
    }
    connection
        .execute(
            "UPDATE llm_keys SET enabled = 0, api_user_id = NULL, balance_json = NULL, \
                 balance_error_json = NULL, balance_attempt_ms = NULL, \
                 updated_at_ms = MAX(?2, updated_at_ms + 1) WHERE provider_id = ?1",
            params![id, stamp],
        )
        .map_err(db)?;
    connection
        .execute(
            "UPDATE llm_providers SET gateway_family = ?2, quota_per_unit = NULL, \
                 display_in_currency = NULL, sniff_evidence_json = '[]', sniff_summary_json = NULL, \
                 needs_resniff = 1 WHERE id = ?1",
            params![id, enum_text(&unknown_family())?],
        )
        .map_err(db)?;
    Ok(())
}

/// The checked enum columns of a provider row.
struct ProviderEnums {
    auth: String,
    /// The primary protocol, only when one of the endpoints speaks it.
    primary_protocol: Option<String>,
    key_strategy: Option<String>,
}

/// Every check of a provider that needs no transaction: the name (a stored over-long name stays
/// saveable), its characters and uniqueness, the base URL, endpoints, paths, extra headers and
/// proxy URL. Returns the encoded enum columns.
fn validate_provider_input(
    connection: &Connection,
    provider: &Provider,
) -> Result<ProviderEnums, AppError> {
    if let Err(error) = validate_name("provider", &provider.name) {
        let unchanged = !provider.name.trim().is_empty()
            && get_provider(connection, &provider.id)?
                .is_some_and(|stored| stored.name.trim() == provider.name.trim());
        if !unchanged {
            return Err(error);
        }
    }
    validate_provider_chars(provider.name.trim())?;
    if let Some(holder) = provider_name_holder(connection, &provider.id, &provider.name)? {
        return Err(AppError::conflict(format!(
            "a provider named {} already exists",
            provider.name.trim()
        ))
        .coded(ErrorCode::NameTaken, holder));
    }
    if provider.base_url.trim().is_empty() {
        return Err(AppError::validation("a provider needs a base URL"));
    }
    if provider.endpoints.is_empty() {
        return Err(AppError::validation(
            "a provider needs at least one protocol endpoint",
        ));
    }
    validate_base_url(&provider.base_url)?;
    if let Some(endpoint) = provider.models_endpoint.as_deref() {
        validate_relative_path("the models endpoint", endpoint)?;
    }
    for endpoint in &provider.endpoints {
        validate_relative_path("an endpoint path prefix", &endpoint.path_prefix)?;
        if endpoint.path_prefix.contains('?') {
            return Err(AppError::validation(
                "an endpoint path prefix must not contain a query",
            ));
        }
    }
    validate_extra_headers(&provider.quirks.extra_headers)?;
    if let Some(relay) = &provider.quirks.relay_pricing {
        relay.check()?;
    }
    validate_proxy_url(&provider.proxy_url)?;
    let auth = enum_text(&provider.auth)?;
    let primary_protocol = provider
        .primary_protocol
        .filter(|primary| {
            provider
                .endpoints
                .iter()
                .any(|endpoint| endpoint.protocol == *primary)
        })
        .map(|primary| enum_text(&primary))
        .transpose()?;
    let key_strategy = provider.key_strategy.as_ref().map(enum_text).transpose()?;
    Ok(ProviderEnums {
        auth,
        primary_protocol,
        key_strategy,
    })
}

/// The outbound hosts `provider` uses that the stored row does not (none for a new provider).
fn added_hosts(stored: Option<&Provider>, provider: &Provider) -> Vec<String> {
    match stored {
        Some(stored) => {
            let before = outbound_hosts(stored);
            outbound_hosts(provider)
                .into_iter()
                .filter(|host| !before.contains(host))
                .collect()
        }
        None => Vec::new(),
    }
}

/// [`save_provider`] with every check: the version the caller read, and a move to an outbound
/// host the stored row does not use, which needs `confirm_host_change`.
///
/// # Errors
/// [`AppError::Validation`] for a bad name, URL, endpoint, header or proxy URL;
/// [`AppError::Conflict`] coded [`ErrorCode::NameTaken`] when another provider holds the name,
/// coded `stale_write` when the stored version differs, coded
/// [`ErrorCode::HostChangeNeedsConfirm`] for an unconfirmed new outbound host;
/// [`AppError::Database`] otherwise.
pub fn save_provider_with(
    connection: &Connection,
    provider: &Provider,
    options: SaveProviderOptions,
    now_ms: i64,
) -> Result<Provider, AppError> {
    let enums = validate_provider_input(connection, provider)?;
    atomic(connection, |connection| {
        let stored = get_provider(connection, &provider.id)?;
        let stamp = next_stamp(
            "provider",
            stored.as_ref().map(|stored| stored.updated_at_ms),
            options.expected_updated_at_ms,
            now_ms,
        )?;
        let added = added_hosts(stored.as_ref(), provider);
        if !added.is_empty() && !options.confirm_host_change {
            return Err(AppError::conflict(format!(
                "this change sends the provider's requests to {}: confirm it",
                added.join(", ")
            ))
            .coded(ErrorCode::HostChangeNeedsConfirm, added.join(",")));
        }
        let same_base = stored.as_ref().is_some_and(|stored| {
            origin_label(&stored.base_url) == origin_label(&provider.base_url)
        });
        let quirks = split_extra_headers(connection, &provider.id, &provider.quirks, now_ms)?;
        let proxy_url =
            split_proxy_password(connection, &provider.id, &provider.proxy_url, now_ms)?;
        let custom_balance = stored_custom_balance_json(
            connection,
            &provider.id,
            provider.custom_balance.as_ref(),
            &provider.base_url,
            same_base,
        )?;
        let row = ProviderRow {
            enums: &enums,
            quirks: &quirks,
            proxy_url: &proxy_url,
            custom_balance,
            stamp,
        };
        upsert_provider_row(connection, provider, &row)?;
        if !added.is_empty() {
            forget_old_host(connection, &provider.id, stamp)?;
        }
        if let Some(stored) = stored
            .as_ref()
            .filter(|s| s.name.trim() != provider.name.trim())
        {
            let old = options.keep_former_name.then_some(stored.name.as_str());
            // Former names expire by the wall clock, not the caller's (possibly imported) stamp.
            rename_recorded(
                connection,
                &provider.id,
                old,
                &provider.name,
                spanbay_base::time::now_ms(),
            )?;
        }
        get_provider(connection, &provider.id)?
            .ok_or_else(|| AppError::internal("provider vanished after save"))
    })
}

/// The upsert of [`save_provider_with`]: the user-editable columns. `?13` stamps both
/// `created_at_ms` (new rows only) and `updated_at_ms`; `?18`/`?19` (key strategy and cooldown)
/// keep the stored value when `NULL`.
const UPSERT_PROVIDER_SQL: &str = "INSERT INTO llm_providers (id, name, preset_id, category, base_url, auth, endpoints_json, \
             models_endpoint, quirks_json, enabled, position, notes, created_at_ms, updated_at_ms, \
             proxy_url, primary_protocol, balance_off, custom_balance_json, key_strategy, key_cooldown, \
             review_pending_since_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, ?14, ?15, ?16, ?17, \
             COALESCE(?18, 'primary'), COALESCE(?19, 1), ?20) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, preset_id = excluded.preset_id, \
             category = excluded.category, base_url = excluded.base_url, auth = excluded.auth, \
             endpoints_json = excluded.endpoints_json, models_endpoint = excluded.models_endpoint, \
             quirks_json = excluded.quirks_json, enabled = excluded.enabled, \
             position = excluded.position, notes = excluded.notes, \
             updated_at_ms = excluded.updated_at_ms, proxy_url = excluded.proxy_url, \
             primary_protocol = excluded.primary_protocol, balance_off = excluded.balance_off, \
             custom_balance_json = excluded.custom_balance_json, \
             key_strategy = COALESCE(?18, key_strategy), key_cooldown = COALESCE(?19, key_cooldown)";

/// What a save derived from the payload beside the provider itself.
struct ProviderRow<'a> {
    enums: &'a ProviderEnums,
    /// The quirks with every header value replaced by its placeholder.
    quirks: &'a Quirks,
    /// The proxy URL with its password masked.
    proxy_url: &'a str,
    custom_balance: Option<String>,
    stamp: i64,
}

fn upsert_provider_row(
    connection: &Connection,
    provider: &Provider,
    row: &ProviderRow<'_>,
) -> Result<(), AppError> {
    connection
        .execute(
            UPSERT_PROVIDER_SQL,
            params![
                provider.id,
                provider.name.trim(),
                provider.preset_id,
                provider.category,
                provider.base_url.trim(),
                row.enums.auth,
                json_text(&provider.endpoints)?,
                provider.models_endpoint,
                json_text(row.quirks)?,
                flag(provider.enabled),
                provider.position,
                provider.notes,
                row.stamp,
                row.proxy_url,
                row.enums.primary_protocol,
                flag(provider.balance_off),
                row.custom_balance,
                row.enums.key_strategy,
                provider.key_cooldown.map(flag),
                provider.review_pending_since_ms,
            ],
        )
        .map_err(db)?;
    Ok(())
}

/// Deletes a provider with its keys, models, fallback chains and probe results, and every secret
/// of the provider and its keys; in the same transaction its id is buried (never reused) and the
/// rows that name it by string go ([`purge_references`]).
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist, [`AppError::Database`] otherwise.
pub fn delete_provider(connection: &Connection, id: &str) -> Result<Purged, AppError> {
    atomic(connection, |connection| {
        if get_provider(connection, id)?.is_none() {
            return Err(AppError::not_found(format!("provider {id} not found")));
        }
        for key in list_keys(connection, Some(id))? {
            delete_key_secrets(connection, &key.id)?;
        }
        delete_provider_secrets(connection, id)?;
        let purged = purge_references(connection, id, None)?;
        let deleted = connection
            .execute("DELETE FROM llm_providers WHERE id = ?1", params![id])
            .map_err(db)?;
        ensure_provider_updated(deleted, id)?;
        bury(connection, id)?;
        Ok(purged)
    })
}

/// `NotFound` for provider `id` when a statement that targets it by id changed no row.
pub(super) fn ensure_provider_updated(updated: usize, id: &str) -> Result<(), AppError> {
    if updated == 0 {
        return Err(AppError::not_found(format!("provider {id} not found")));
    }
    Ok(())
}
