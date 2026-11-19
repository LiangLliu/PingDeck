//! The "Models & pricing" use-cases: the catalog list, its models.dev sync (manual and the
//! delayed automatic one), the user's own models and the pricing rows.
//!
//! Single responsibility: the UI-agnostic entry points over `llm::catalog` and
//! `llm::store::pricing`. A sync downloads on the async side, then parses and writes on the
//! blocking pool in one transaction; a failure keeps every old row and records the reason. The
//! automatic sync only runs while the setting `ai.providers.catalog_auto_refresh` is on (read
//! live on every check), starts 90 s after launch and then sleeps until the sync is due (at most
//! 6 h, at least a minute; ten minutes while the setting is off), woken early when the interval
//! changes or a manual sync finishes; it waits 6 h after a failed attempt and stops with the
//! background token. An applied sync announces `CatalogChanged`. Defaults precedence, for the record: a value the provider reported or the user
//! set on a provider model row beats the catalog row, which beats unknown.

use std::time::{Duration, Instant};

use super::balance::{background_token, begin_flight};
use super::price_book::{ModelPriceDetail, PriceBook, ProviderPricingView, UnlistedModel};
use super::{ensure_catalog, with_connection, with_db_blocking};
use crate::app::{AppState, current_time_ms};
use crate::cancel::{CancelToken, until_cancelled};
use crate::error::AppError;
use crate::llm::catalog;
use crate::llm::events::LlmEvent;
use crate::llm::store;
use crate::logging::{LogLevel, log_line};
use pingdeck_model::model_catalog::{
    MODELS_DEV_URL, ModelAlias, ModelCatalogPage, ModelCatalogQuery, ModelCatalogRow,
    ModelCatalogStatus, ModelCatalogSyncReport, ModelPricing, ModelResolution, UserModelInput,
};

/// The automatic sync waits this long after launch (the window and the gateway are up by then).
pub(super) const AUTO_SYNC_FIRST_DELAY: Duration = Duration::from_secs(90);
/// The longest the automatic sync sleeps before it re-checks whether it is due.
pub(super) const AUTO_SYNC_RECHECK: Duration = Duration::from_secs(6 * 60 * 60);
/// How often the automatic sync re-reads the setting while it is off.
const AUTO_SYNC_OFF_RECHECK: Duration = Duration::from_secs(10 * 60);
/// The shortest sleep between two checks (a due time in the past is checked after this).
const AUTO_SYNC_MIN_WAIT: Duration = Duration::from_secs(60);
/// The job name of a catalog sync: one at a time.
const CATALOG_FLIGHT: &str = "catalog";

fn auto_sync_enabled(state: &AppState) -> bool {
    state.settings().ai.providers.catalog_auto_refresh
}

/// One page of the catalog (seeds the bundled snapshot on first use): lab models, the user's own
/// and the models the user's providers serve, each at its default catalog price.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub async fn llm_model_catalog_list_impl(
    state: &AppState,
    query: ModelCatalogQuery,
) -> Result<ModelCatalogPage, AppError> {
    with_db_blocking(state, move |connection| {
        catalog::seed_bundled_if_empty(connection)?;
        let in_use = PriceBook::load(connection)?.in_use();
        catalog::list_page(connection, &query, &in_use)
    })
    .await
}

/// One model's prices: its default, its every-provider row, its catalog offers and its price at
/// every provider of the user's that serves it.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub async fn llm_model_price_detail_impl(
    state: &AppState,
    model_key: String,
) -> Result<ModelPriceDetail, AppError> {
    with_db_blocking(state, move |connection| {
        catalog::seed_bundled_if_empty(connection)?;
        PriceBook::load(connection)?.detail(connection, model_key.trim())
    })
    .await
}

/// The model a provider's model id stands for.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub async fn llm_model_resolve_impl(
    state: &AppState,
    provider_id: Option<String>,
    model_id: String,
) -> Result<ModelResolution, AppError> {
    with_db_blocking(state, move |connection| {
        catalog::seed_bundled_if_empty(connection)?;
        catalog::resolve(connection, provider_id.as_deref(), &model_id)
    })
    .await
}

/// The enabled models of the user's providers that no catalog row matches.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub async fn llm_model_unlisted_impl(state: &AppState) -> Result<Vec<UnlistedModel>, AppError> {
    with_db_blocking(state, move |connection| {
        catalog::seed_bundled_if_empty(connection)?;
        Ok(PriceBook::load(connection)?.unlisted())
    })
    .await
}

/// Saves that a provider's model id is a catalog model.
///
/// # Errors
/// [`AppError::Validation`] for an unknown model or provider, [`AppError::Database`] on failure.
pub async fn llm_model_alias_save_impl(
    state: &AppState,
    alias: ModelAlias,
) -> Result<ModelAlias, AppError> {
    let saved = with_db_blocking(state, move |connection| {
        if store::get_provider(connection, alias.provider_id.trim())?.is_none() {
            return Err(AppError::validation(format!(
                "provider {} not found",
                alias.provider_id
            )));
        }
        store::save_alias(connection, alias, current_time_ms())
    })
    .await?;
    state.llm.touch();
    Ok(saved)
}

/// Removes an alias; `true` when one existed.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub async fn llm_model_alias_delete_impl(
    state: &AppState,
    provider_id: String,
    model_id: String,
) -> Result<bool, AppError> {
    let deleted = with_db_blocking(state, move |connection| {
        store::delete_alias(connection, &provider_id, &model_id)
    })
    .await?;
    if deleted {
        state.llm.touch();
    }
    Ok(deleted)
}

/// Every provider of the user's with its catalog basis.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub async fn llm_provider_pricing_list_impl(
    state: &AppState,
) -> Result<Vec<ProviderPricingView>, AppError> {
    with_db_blocking(state, move |connection| {
        catalog::seed_bundled_if_empty(connection)?;
        Ok(PriceBook::load(connection)?.provider_views())
    })
    .await
}

/// Makes a provider follow a models.dev provider's catalog prices (`None`: the official ones);
/// returns every provider's basis afterwards.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider, [`AppError::Validation`] for an unknown
/// family, [`AppError::Database`] on failure.
pub async fn llm_provider_pricing_save_impl(
    state: &AppState,
    provider_id: String,
    catalog_family: Option<String>,
) -> Result<Vec<ProviderPricingView>, AppError> {
    let views = with_db_blocking(state, move |connection| {
        if store::get_provider(connection, &provider_id)?.is_none() {
            return Err(AppError::not_found(format!(
                "provider {provider_id} not found"
            )));
        }
        store::save_provider_pricing(
            connection,
            &provider_id,
            catalog_family.as_deref(),
            current_time_ms(),
        )?;
        Ok(PriceBook::load(connection)?.provider_views())
    })
    .await?;
    state.llm.touch();
    Ok(views)
}

/// Where the catalog stands: origin, size, last sync and the automatic-sync schedule.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_model_catalog_status_impl(state: &AppState) -> Result<ModelCatalogStatus, AppError> {
    let auto_sync = auto_sync_enabled(state);
    with_connection(state, |connection| {
        ensure_catalog(connection)?;
        catalog::sync_status(connection, auto_sync)
    })
}

/// Downloads `url`, parses it and applies it. Any failure but a cancel is recorded in the sync
/// state, and the catalog is left as it was.
async fn sync_from(
    state: &AppState,
    client: &reqwest::Client,
    url: &str,
    cancel: Option<&CancelToken>,
) -> Result<ModelCatalogSyncReport, AppError> {
    let started = Instant::now();
    let applied = async {
        let body = catalog::download(client, url, cancel).await?;
        with_db_blocking(state, move |connection| {
            catalog::seed_bundled_if_empty(connection)?;
            let now = current_time_ms();
            let parsed = catalog::parse_remote(&body, now)?;
            let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            catalog::apply_sync(connection, &parsed.records, now, elapsed)
        })
        .await
    }
    .await;
    match applied {
        Ok(report) => {
            // Prices are cached per configuration version: a sync can change them.
            state.llm.touch();
            state.llm.emit(LlmEvent::CatalogChanged);
            Ok(report)
        }
        Err(error) => {
            if !error.is_cancelled() {
                let message = error.message().to_owned();
                // Best effort: the original error is what the caller needs.
                let _ = with_db_blocking(state, move |connection| {
                    catalog::record_failure(connection, &message, current_time_ms())
                })
                .await;
            }
            Err(error)
        }
    }
}

/// Syncs the catalog from models.dev now (manual 「立即同步」, also what `llm_catalog_refresh`
/// runs): `GET https://models.dev/api.json`, 30 s, at most 16 MiB, redirects not followed. Only
/// `models.dev` rows are written; user rows and pricing are never touched; rows models.dev no
/// longer lists are kept.
///
/// # Errors
/// [`AppError::Network`] on download failure, [`AppError::Validation`] for a body that is not a
/// catalog, [`AppError::Cancelled`] when cancelled.
pub async fn llm_model_catalog_sync_impl(
    state: &AppState,
    cancel_token: Option<String>,
) -> Result<ModelCatalogSyncReport, AppError> {
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let client = state.llm.http_for(MODELS_DEV_URL, "")?;
    // Single flight both ways: the automatic sync skips its turn while this one holds the name,
    // and this one refuses while another sync holds it (that one emits `CatalogChanged`).
    let Some(flight) = begin_flight(state, CATALOG_FLIGHT.to_owned()) else {
        return Err(AppError::conflict(
            "A model catalog sync is already running. Try again when it finishes.",
        ));
    };
    let synced = sync_from(state, &client, MODELS_DEV_URL, Some(guard.token())).await;
    drop(flight);
    state.llm.services.catalog_wake.notify_one();
    synced
}

/// One check of the automatic sync against `url`: nothing while the setting is off, the next
/// sync is not due or another sync runs, else a sync stopped by `cancel`. `true` when it tried to
/// sync.
pub(super) async fn auto_sync_tick(state: &AppState, url: &str, cancel: &CancelToken) -> bool {
    if !auto_sync_enabled(state) {
        return false;
    }
    let due = with_db_blocking(state, |connection| {
        catalog::seed_bundled_if_empty(connection)?;
        catalog::auto_sync_due(connection, current_time_ms())
    })
    .await;
    if !matches!(due, Ok(true)) {
        return false;
    }
    let Some(_flight) = begin_flight(state, CATALOG_FLIGHT.to_owned()) else {
        return false;
    };
    let client = match state.llm.http_for(url, "") {
        Ok(client) => client,
        Err(error) => {
            log_line(
                LogLevel::Warn,
                "llm.catalog",
                &format!("automatic model catalog sync: {}", error.message()),
            );
            return false;
        }
    };
    if let Err(error) = sync_from(state, &client, url, Some(cancel)).await
        && !error.is_cancelled()
    {
        log_line(
            LogLevel::Warn,
            "llm.catalog",
            &format!("automatic model catalog sync failed: {}", error.message()),
        );
    }
    true
}

/// How long the automatic sync sleeps before its next check: until the sync is due, within
/// [`AUTO_SYNC_MIN_WAIT`] and `recheck`; [`AUTO_SYNC_OFF_RECHECK`] (at most `recheck`) while the
/// setting is off.
async fn next_wait(state: &AppState, recheck: Duration) -> Duration {
    if !auto_sync_enabled(state) {
        return AUTO_SYNC_OFF_RECHECK.min(recheck);
    }
    let next_due = with_db_blocking(state, |connection| {
        catalog::sync_status(connection, true).map(|status| status.next_due_ms)
    })
    .await
    .ok()
    .flatten();
    let Some(next_due) = next_due else {
        return recheck;
    };
    let left = u64::try_from(next_due.saturating_sub(current_time_ms())).unwrap_or(0);
    Duration::from_millis(left).clamp(AUTO_SYNC_MIN_WAIT.min(recheck), recheck)
}

/// Starts the background task of the automatic sync: first check after `first_delay`, then a
/// sleep until the sync is due (at most `recheck`), woken early by `catalog_wake`, each check
/// reading the setting live. Runs on the background token, so shutdown stops it. Never blocks
/// the caller.
pub(super) fn spawn_auto_sync(
    state: &AppState,
    url: &'static str,
    first_delay: Duration,
    recheck: Duration,
) {
    let state = state.clone();
    let token = background_token(&state);
    tokio::spawn(async move {
        if until_cancelled(Some(&token), tokio::time::sleep(first_delay))
            .await
            .is_none()
        {
            return;
        }
        loop {
            auto_sync_tick(&state, url, &token).await;
            let wait = next_wait(&state, recheck).await;
            tokio::select! {
                () = token.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
                () = state.llm.services.catalog_wake.notified() => {}
            }
        }
    });
}

/// Saves the interval between automatic syncs (1, 7 or 30 days) and returns the new status.
///
/// # Errors
/// [`AppError::Validation`] for any other number.
pub fn llm_model_catalog_config_save_impl(
    state: &AppState,
    auto_interval_days: u16,
) -> Result<ModelCatalogStatus, AppError> {
    with_connection(state, |connection| {
        ensure_catalog(connection)?;
        catalog::save_config(connection, auto_interval_days)
    })?;
    // The automatic sync re-reads when it is due.
    state.llm.services.catalog_wake.notify_one();
    llm_model_catalog_status_impl(state)
}

/// Adds or replaces a model of the user's own; a sync never touches it.
///
/// # Errors
/// [`AppError::Validation`] for a bad input, [`AppError::Conflict`] when a synced model has that
/// family and id.
pub fn llm_model_catalog_user_save_impl(
    state: &AppState,
    model: UserModelInput,
) -> Result<ModelCatalogRow, AppError> {
    let row = with_connection(state, |connection| {
        ensure_catalog(connection)?;
        catalog::save_user_model(connection, model, current_time_ms())
    })?;
    state.llm.touch();
    Ok(row)
}

/// Deletes a model of the user's own; `true` when one existed. Synced models stay.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_model_catalog_user_delete_impl(
    state: &AppState,
    family: &str,
    model_id: &str,
) -> Result<bool, AppError> {
    let deleted = with_connection(state, |connection| {
        catalog::delete_user_model(connection, family, model_id)
    })?;
    if deleted {
        state.llm.touch();
    }
    Ok(deleted)
}

/// Saves the pricing of one model for one provider (`provider_id` empty: every provider); see
/// [`store::save_pricing`] for what `dynamic` does. Applies to requests made from now on; rows
/// already logged keep their cost.
///
/// # Errors
/// [`AppError::Validation`] for a bad price, currency or id, [`AppError::NotFound`] for an
/// unknown provider.
pub fn llm_model_pricing_save_impl(
    state: &AppState,
    pricing: ModelPricing,
) -> Result<ModelPricing, AppError> {
    let saved = with_connection(state, |connection| {
        let pricing = pricing.validated()?;
        if !pricing.provider_id.is_empty()
            && store::get_provider(connection, &pricing.provider_id)?.is_none()
        {
            return Err(AppError::not_found(format!(
                "provider {} not found",
                pricing.provider_id
            )));
        }
        store::save_pricing(connection, pricing, current_time_ms())
    })?;
    state.llm.touch();
    if !saved.provider_id.is_empty() {
        state.llm.emit(LlmEvent::ModelsChanged {
            provider_id: saved.provider_id.clone(),
        });
    }
    Ok(saved)
}

/// Removes one pricing row ("back to inheriting"); `true` when one existed.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_model_pricing_delete_impl(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
) -> Result<bool, AppError> {
    let deleted = with_connection(state, |connection| {
        store::delete_pricing(connection, provider_id, model_id)
    })?;
    if deleted {
        state.llm.touch();
        if !provider_id.is_empty() {
            state.llm.emit(LlmEvent::ModelsChanged {
                provider_id: provider_id.to_owned(),
            });
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests;
