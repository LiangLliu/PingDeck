//! "Add provider" from a scan in one transaction, and the scan's key checks as stored results.
//!
//! Single responsibility: write everything a scan decided (provider row, identity and evidence,
//! keys with their secrets and first balance, model rows, the Connectivity and Auth results) so
//! that it all lands or none of it does. Which models start enabled and what the status of the
//! new provider reads is the caller's and `llm::status`'s business.

use rusqlite::{Connection, OptionalExtension, params};

use super::ProbeResult;
use super::keys::{save_key, set_key_balance};
use super::model_listing::{ListedModel, insert_models_from_sniff};
use super::models::save_model;
use super::pricing::save_pricing;
use super::probe::persist_cell;
use super::provider_state::set_provider_sniff;
use super::providers::{get_provider, save_provider};
use super::{ModelSource, Provider, ProviderKey, ProviderModel, SniffPersist, atomic, db};
use crate::llm::balance_state::StoredBalance;
use crate::llm::failure::FailureKind;
use crate::llm::probe::run::CellRecord;
use crate::llm::probe::{ProbeStatus, ProbeTest};
use spanbay_base::error::AppError;
use spanbay_model::model_catalog::ModelPricing;

/// What one of the scan's key checks found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyCheck {
    pub ok: bool,
    /// `None` when no response arrived.
    pub status: Option<u16>,
    pub latency_ms: Option<u64>,
}

/// The two key-scoped results a scan yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SniffChecks {
    /// The scan's report id; the results are stored with run id `sniff:<report_id>`.
    pub report_id: String,
    /// Does the host answer (the models request or the first route probe reached it).
    pub connectivity: KeyCheck,
    pub auth: KeyCheck,
}

fn sniff_run_id(report_id: &str) -> String {
    format!("sniff:{report_id}")
}

/// The model the key-scoped results of `provider_id` hang on: its first enabled model, else its
/// first model, else none (key-scoped checks carry the key's first model, as probe runs do).
fn first_model(connection: &Connection, provider_id: &str) -> Result<String, AppError> {
    Ok(connection
        .query_row(
            "SELECT model_id FROM llm_provider_models WHERE provider_id = ?1 \
             ORDER BY enabled DESC, model_id LIMIT 1",
            params![provider_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db)?
        .unwrap_or_default())
}

fn check_cell(
    provider_id: &str,
    key_id: &str,
    model_id: &str,
    test: ProbeTest,
    endpoint: &str,
    check: &KeyCheck,
    now_ms: i64,
) -> CellRecord {
    let failure_kind = (!check.ok).then_some(match check.status {
        None => FailureKind::Network,
        Some(401 | 403) => FailureKind::Auth,
        Some(429) => FailureKind::RateLimit,
        Some(status) if status >= 500 => FailureKind::ProviderError,
        Some(_) => FailureKind::Unknown,
    });
    let result = ProbeResult {
        key_id: key_id.to_owned(),
        model_id: model_id.to_owned(),
        endpoint: endpoint.to_owned(),
        test: test.as_str().to_owned(),
        ok: check.ok,
        status: check.status,
        latency_ms: check.latency_ms,
        failure_kind,
        message: "Checked by the provider scan".to_owned(),
        checked_at_ms: now_ms,
        ..ProbeResult::default()
    };
    CellRecord {
        provider_id: provider_id.to_owned(),
        key_id: key_id.to_owned(),
        model_id: model_id.to_owned(),
        test,
        status: ProbeStatus::from_result(&result).unwrap_or(ProbeStatus::Unreachable),
        reason: String::new(),
        skip: None,
        result: Some(result),
        capability: None,
    }
}

/// Stores the Connectivity and Auth results of a scan for `key_id` as measured results of run
/// `sniff:<report_id>` (endpoints `base` and `models`, statuses derived like any probe result),
/// so a new provider reads Unverified or Needs attention from them.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure (an unknown key violates the foreign key).
pub(crate) fn record_sniff_checks(
    connection: &Connection,
    provider_id: &str,
    key_id: &str,
    checks: &SniffChecks,
    now_ms: i64,
) -> Result<(), AppError> {
    atomic(connection, |connection| {
        let model_id = first_model(connection, provider_id)?;
        let run_id = sniff_run_id(&checks.report_id);
        for (test, endpoint, check) in [
            (ProbeTest::Connectivity, "base", &checks.connectivity),
            (ProbeTest::Auth, "models", &checks.auth),
        ] {
            let cell = check_cell(
                provider_id,
                key_id,
                &model_id,
                test,
                endpoint,
                check,
                now_ms,
            );
            persist_cell(connection, &run_id, &cell, now_ms)?;
        }
        Ok(())
    })
}

/// A key added together with its provider.
#[derive(Debug, Clone, PartialEq)]
pub struct NewKeyFromSniff {
    /// The key row; its provider id is forced to the new provider's.
    pub key: ProviderKey,
    /// The plaintext secret, written to the secret store.
    pub secret: Option<String>,
    /// The first balance finding of the scan.
    pub balance: Option<StoredBalance>,
    pub checks: Option<SniffChecks>,
}

/// Everything "Add provider" saves.
#[derive(Debug, Clone, PartialEq)]
pub struct NewProviderFromSniff {
    /// The provider row (user-editable columns: name, base URL, auth, endpoints, quirks, primary
    /// protocol, position, ...).
    pub provider: Provider,
    pub sniff: SniffPersist,
    pub keys: Vec<NewKeyFromSniff>,
    /// The listed models with their `enabled` flag; a deselected row is saved disabled.
    pub models: Vec<(ListedModel, bool)>,
    /// Models the user typed by hand, saved as `manual` entries.
    pub manual_models: Vec<ProviderModel>,
    /// The prices the host's own model list declared, saved as the provider's `custom` rows of
    /// those models (`provider_id` is set here).
    pub declared_prices: Vec<ModelPricing>,
}

/// Creates a provider from a scan in ONE transaction: the row, the identity and
/// evidence, the keys (secrets through the secret store, first balance, key checks) and the model
/// rows (`first_seen_ms = now_ms`). Any failure rolls all of it back, so nothing of a failed or
/// discarded scan stays. When the caller already holds a transaction this is a savepoint of it.
///
/// # Errors
/// [`AppError::Conflict`] when the provider id exists already, [`AppError::Validation`] for an
/// invalid provider, key or model, [`AppError::Database`] otherwise.
pub fn create_provider_from_sniff(
    connection: &Connection,
    new: &NewProviderFromSniff,
    now_ms: i64,
) -> Result<Provider, AppError> {
    atomic(connection, |connection| {
        if get_provider(connection, &new.provider.id)?.is_some() {
            return Err(AppError::conflict(format!(
                "provider {} exists already",
                new.provider.id
            )));
        }
        let saved = save_provider(connection, &new.provider, now_ms)?;
        set_provider_sniff(connection, &saved.id, &new.sniff)?;
        insert_models_from_sniff(connection, &saved.id, &new.models, now_ms)?;
        for manual in &new.manual_models {
            let model = ProviderModel {
                provider_id: saved.id.clone(),
                source: ModelSource::Manual,
                first_seen_ms: manual.first_seen_ms.or(Some(now_ms)),
                ..manual.clone()
            };
            save_model(connection, &model)?;
        }
        for declared in &new.declared_prices {
            save_pricing(
                connection,
                ModelPricing {
                    provider_id: saved.id.clone(),
                    ..declared.clone()
                },
                now_ms,
            )?;
        }
        for added in &new.keys {
            let key = ProviderKey {
                provider_id: saved.id.clone(),
                ..added.key.clone()
            };
            save_key(connection, &key, added.secret.as_deref(), now_ms)?;
            if let Some(balance) = &added.balance {
                set_key_balance(connection, &key.id, balance, now_ms)?;
            }
            if let Some(checks) = &added.checks {
                record_sniff_checks(connection, &saved.id, &key.id, checks, now_ms)?;
            }
        }
        get_provider(connection, &saved.id)?
            .ok_or_else(|| AppError::internal("provider vanished after create"))
    })
}
