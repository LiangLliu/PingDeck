//! Server-owned provider columns: the scan result, the gateway family, the model-list drift and
//! the re-scan banner. A user save never writes them (see `providers`).

use rusqlite::{Connection, params};

use super::providers::ensure_provider_updated;
use super::{db, enum_text, flag, json_text};
use crate::llm::probe::listing::ModelDrift;
use crate::llm::providers::{MAX_SNIFF_EVIDENCE, SniffPersist};
use crate::llm::wire::GatewayFamily;
use pingdeck_base::error::AppError;

/// Stores the result of a scan (a create-from-sniff or a re-sniff) and clears `needs_resniff`.
/// At most [`MAX_SNIFF_EVIDENCE`] evidence entries are kept; a quota that is not a positive
/// number is dropped.
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist, [`AppError::Database`] otherwise.
pub fn set_provider_sniff(
    connection: &Connection,
    id: &str,
    sniff: &SniffPersist,
) -> Result<(), AppError> {
    let evidence = &sniff.evidence[..sniff.evidence.len().min(MAX_SNIFF_EVIDENCE)];
    let updated = connection
        .execute(
            "UPDATE llm_providers SET gateway_family = ?2, quota_per_unit = ?3, \
                 display_in_currency = ?4, sniff_evidence_json = ?5, sniff_summary_json = ?6, \
                 needs_resniff = 0 WHERE id = ?1",
            params![
                id,
                enum_text(&sniff.family)?,
                sniff
                    .quota_per_unit
                    .filter(|quota| quota.is_finite() && *quota > 0.0),
                sniff.display_in_currency.map(flag),
                json_text(&evidence)?,
                json_text(&sniff.summary)?,
            ],
        )
        .map_err(db)?;
    ensure_provider_updated(updated, id)
}

/// Stores the gateway family a later scan or probe recognised.
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist, [`AppError::Database`] otherwise.
pub fn set_gateway_family(
    connection: &Connection,
    id: &str,
    family: GatewayFamily,
) -> Result<(), AppError> {
    let updated = connection
        .execute(
            "UPDATE llm_providers SET gateway_family = ?2 WHERE id = ?1",
            params![id, enum_text(&family)?],
        )
        .map_err(db)?;
    ensure_provider_updated(updated, id)
}

/// Stores the comparison of a model-list refresh.
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist, [`AppError::Database`] otherwise.
pub(crate) fn set_model_drift(
    connection: &Connection,
    id: &str,
    drift: &ModelDrift,
) -> Result<(), AppError> {
    let updated = connection
        .execute(
            "UPDATE llm_providers SET model_drift_json = ?2 WHERE id = ?1",
            params![id, json_text(drift)?],
        )
        .map_err(db)?;
    ensure_provider_updated(updated, id)
}

/// Removes the stored model-list comparison ("Mark as seen").
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist, [`AppError::Database`] otherwise.
pub(crate) fn clear_model_drift(connection: &Connection, id: &str) -> Result<(), AppError> {
    let updated = connection
        .execute(
            "UPDATE llm_providers SET model_drift_json = NULL WHERE id = ?1",
            params![id],
        )
        .map_err(db)?;
    ensure_provider_updated(updated, id)
}

/// Sets or clears the banner "This host answered differently than when it was added".
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist, [`AppError::Database`] otherwise.
#[cfg(test)]
pub(crate) fn set_needs_resniff(
    connection: &Connection,
    id: &str,
    needs_resniff: bool,
) -> Result<(), AppError> {
    let updated = connection
        .execute(
            "UPDATE llm_providers SET needs_resniff = ?2 WHERE id = ?1",
            params![id, flag(needs_resniff)],
        )
        .map_err(db)?;
    ensure_provider_updated(updated, id)
}

/// Test-only helper: writes a raw `base_url` value directly to exercise redaction of legacy or
/// malformed database rows without loosening save-time URL validation.
#[cfg(feature = "test-support")]
pub fn overwrite_provider_base_url_for_test(
    connection: &Connection,
    id: &str,
    base_url: &str,
) -> Result<(), AppError> {
    let updated = connection
        .execute(
            "UPDATE llm_providers SET base_url = ?2 WHERE id = ?1",
            params![id, base_url],
        )
        .map_err(db)?;
    ensure_provider_updated(updated, id)
}
