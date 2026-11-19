//! SQL over `llm_provider_models`: row mapping, list, get, save, delete (and the test-only
//! merge). The record types live in `pingdeck_llm_domain::llm::records`.

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{
    FactColumns, FactSource, ModelRef, ModelSource, ProviderModel, db, enum_from_text, enum_text,
    flag, json_text, to_i64, to_u64,
};
use crate::llm::capabilities::{Capability, CapabilitySource, CapabilityState, ModelCapabilities};
use crate::llm::probe::listing::Presence;
use pingdeck_base::error::AppError;

/// Reads a row selected with [`MODEL_COLUMNS`]; columns are taken by index.
pub(super) fn model_from_row(row: &Row<'_>) -> rusqlite::Result<ProviderModel> {
    let role: Option<String> = row.get(3)?;
    let num = |index: usize| -> rusqlite::Result<Option<u64>> {
        Ok(row.get::<_, Option<i64>>(index)?.map(to_u64))
    };
    let (listed_context, listed_max_output, measured_context) = (num(12)?, num(13)?, num(14)?);
    let (context, context_source) = FactColumns {
        declared_value: num(4)?,
        declared: row.get::<_, i64>(15)? != 0,
        measured: measured_context,
        listed: listed_context,
    }
    .resolve();
    let (max_output, max_output_source) = FactColumns {
        declared_value: num(5)?,
        declared: row.get::<_, i64>(16)? != 0,
        measured: None,
        listed: listed_max_output,
    }
    .resolve();
    let capabilities: ModelCapabilities =
        serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default();
    let (vision, tools) = legacy_flags(&capabilities);
    let presence: String = row.get(9)?;
    Ok(ProviderModel {
        provider_id: row.get(0)?,
        model_id: row.get(1)?,
        source: ModelSource::parse(&row.get::<_, String>(2)?),
        role: role.as_deref().and_then(enum_from_text),
        context,
        max_output,
        context_source,
        max_output_source,
        listed_context,
        listed_max_output,
        measured_context,
        listed_confirmed_at_ms: row.get(17)?,
        vision,
        tools,
        notes: row.get(6)?,
        enabled: row.get::<_, i64>(7)? != 0,
        capabilities,
        presence: enum_from_text(&presence).unwrap_or(Presence::Listed),
        first_seen_ms: row.get(10)?,
        new_cleared_at_ms: row.get(11)?,
        fallbacks: Vec::new(),
    })
}

pub(super) const MODEL_COLUMNS: &str = "provider_id, model_id, source, role, context, max_output, \
     notes, enabled, capabilities_json, presence, first_seen_ms, new_cleared_at_ms, listed_context, \
     listed_max_output, measured_context, context_declared, max_output_declared, \
     listed_confirmed_at_ms";

/// Models of `provider_id`, or of every provider when `None`, ordered by provider then model id.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_models(
    connection: &Connection,
    provider_id: Option<&str>,
) -> Result<Vec<ProviderModel>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {MODEL_COLUMNS} FROM llm_provider_models \
             WHERE (?1 IS NULL OR provider_id = ?1) ORDER BY provider_id, model_id"
        ))
        .map_err(db)?;
    let rows = statement
        .query_map(params![provider_id], model_from_row)
        .map_err(db)?;
    let mut models: Vec<ProviderModel> = rows.collect::<Result<_, _>>().map_err(db)?;
    let mut chains = super::routing::list_fallbacks(connection)?;
    for model in &mut models {
        if let Some(chain) = chains.remove(&ModelRef {
            provider_id: model.provider_id.clone(),
            model: model.model_id.clone(),
        }) {
            model.fallbacks = chain;
        }
    }
    Ok(models)
}

/// The model `model_id` of `provider_id`, with its fallback chain.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn get_model(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<Option<ProviderModel>, AppError> {
    // The cached statement is released before the chain is read.
    let model = {
        let mut statement = connection
            .prepare_cached(&format!(
                "SELECT {MODEL_COLUMNS} FROM llm_provider_models WHERE provider_id = ?1 AND model_id = ?2"
            ))
            .map_err(db)?;
        statement
            .query_row(params![provider_id, model_id], model_from_row)
            .optional()
            .map_err(db)?
    };
    let Some(mut model) = model else {
        return Ok(None);
    };
    model.fallbacks = super::routing::fallbacks_of(connection, provider_id, model_id)?;
    Ok(Some(model))
}

/// The legacy `(vision, tools)` flags a matrix implies.
pub(super) fn legacy_flags(capabilities: &ModelCapabilities) -> (bool, bool) {
    (
        capabilities.known(Capability::Vision) == Some(true),
        capabilities.known(Capability::Tools) == Some(true),
    )
}

/// Inserts or updates one model entry (matched by provider and model id).
///
/// Updates the user-editable columns (source, role, context, max output, notes, enabled). The
/// server-owned ones (`presence`, `first_seen_ms`, `new_cleared_at_ms`) and the stored matrix are
/// kept on update: the matrix changes through [`super::set_model_capabilities_override`],
/// [`super::record_measured`] and [`super::apply_model_listing`], so a stale payload cannot erase
/// a measurement. A `vision` or `tools` flag only adds a catalogue-strength fact where the matrix
/// has no opinion. A new row takes the payload's matrix, presence and `first_seen_ms`.
///
/// # Errors
/// [`AppError::Validation`] for a blank id, [`AppError::Database`] otherwise (an unknown provider
/// violates the foreign key).
pub fn save_model(connection: &Connection, model: &ProviderModel) -> Result<(), AppError> {
    let model_id = model.model_id.trim();
    if model_id.is_empty() {
        return Err(AppError::validation("a model needs an id"));
    }
    let role = model.role.as_ref().map(enum_text).transpose()?;
    // One statement: the stored matrix is read and merged where the row is written, so a
    // measurement another connection commits cannot be overwritten by a stale read.
    let (context, context_declared, listed_context) = save_fact(
        model.context,
        model.context_source,
        model.source,
        model.listed_context,
    );
    let (max_output, max_output_declared, listed_max_output) = save_fact(
        model.max_output,
        model.max_output_source,
        model.source,
        model.listed_max_output,
    );
    let catalog_fact = json_text(&CapabilityState {
        supported: true,
        source: CapabilitySource::Catalog,
        ..CapabilityState::default()
    })?;
    connection
        .execute(
            SAVE_MODEL_SQL,
            params![
                model.provider_id,
                model_id,
                model.source.as_str(),
                role,
                context.map(to_i64),
                max_output.map(to_i64),
                flag(model.vision),
                flag(model.tools),
                model.notes,
                flag(model.enabled),
                json_text(&model.capabilities)?,
                enum_text(&model.presence)?,
                model.first_seen_ms,
                model.new_cleared_at_ms,
                catalog_fact,
                flag(context_declared),
                flag(max_output_declared),
                listed_context.map(to_i64),
                listed_max_output.map(to_i64),
                model.measured_context.map(to_i64),
                model.listed_confirmed_at_ms,
            ],
        )
        .map_err(db)?;
    Ok(())
}

/// What [`save_model`] stores of one fact: the user's value, whether it is declared, and the
/// listed value a new row starts with. A payload `value` is the user's when its source says
/// `declared` (or, without a source, on a manual row); otherwise it is the resolved value read
/// back, and an update returns the fact to automatic.
fn save_fact(
    value: Option<u64>,
    source: Option<FactSource>,
    model_source: ModelSource,
    listed: Option<u64>,
) -> (Option<u64>, bool, Option<u64>) {
    let declared = value.is_some()
        && match source {
            Some(FactSource::Declared) => true,
            None => model_source == ModelSource::Manual,
            Some(_) => false,
        };
    if declared {
        return (value, true, listed);
    }
    let listed = listed.or(value.filter(|_| matches!(source, None | Some(FactSource::Listed))));
    (None, false, listed)
}

/// The upsert of [`save_model`]. `?7`/`?8` are the payload's `vision`/`tools` flags, `?11` its
/// matrix (used by a new row), `?15` a catalogue-strength "supported" fact. The matrix is the
/// stored one (a row that fails to parse counts as empty) or the payload's; a flag then fills the
/// vision or tools bit only where its source is still `unknown`.
const SAVE_MODEL_SQL: &str = "\
WITH prior(json) AS (
    SELECT CASE WHEN json_valid(capabilities_json) THEN capabilities_json ELSE '{}' END
    FROM llm_provider_models WHERE provider_id = ?1 AND model_id = ?2),
base(json) AS (SELECT COALESCE((SELECT json FROM prior), ?11)),
with_vision(json) AS (
    SELECT CASE WHEN ?7 AND COALESCE(json_extract(json, '$.vision.source'), 'unknown') = 'unknown'
        THEN json_set(json, '$.vision', json(?15)) ELSE json END FROM base),
merged(json) AS (
    SELECT CASE WHEN ?8 AND COALESCE(json_extract(json, '$.tools.source'), 'unknown') = 'unknown'
        THEN json_set(json, '$.tools', json(?15)) ELSE json END FROM with_vision)
INSERT INTO llm_provider_models (provider_id, model_id, source, role, context, max_output,
    notes, enabled, capabilities_json, presence, first_seen_ms, new_cleared_at_ms,
    context_declared, max_output_declared, listed_context, listed_max_output, measured_context,
    listed_confirmed_at_ms)
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?9, ?10, json, ?12, ?13, ?14, ?16, ?17, ?18, ?19, ?20, ?21
FROM merged WHERE true
ON CONFLICT(provider_id, model_id) DO UPDATE SET source = excluded.source, role = excluded.role,
    context = excluded.context, max_output = excluded.max_output, notes = excluded.notes,
    enabled = excluded.enabled, capabilities_json = excluded.capabilities_json,
    context_declared = excluded.context_declared,
    max_output_declared = excluded.max_output_declared";

/// Deletes one model entry.
///
/// # Errors
/// [`AppError::NotFound`] when it does not exist.
pub fn delete_model(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<(), AppError> {
    let deleted = connection
        .execute(
            "DELETE FROM llm_provider_models WHERE provider_id = ?1 AND model_id = ?2",
            params![provider_id, model_id],
        )
        .map_err(db)?;
    if deleted == 0 {
        return Err(AppError::not_found(format!("model {model_id} not found")));
    }
    Ok(())
}

/// Merges a fresh model list of one `source` into a provider: entries of that source that are no
/// longer listed are removed, new ones are added enabled, existing ones keep their `enabled` flag
/// and refresh their metadata. Entries of other sources are untouched (a manual entry is never
/// overwritten by a live list).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
#[cfg(test)]
pub(crate) fn merge_models(
    connection: &Connection,
    provider_id: &str,
    source: ModelSource,
    models: &[ProviderModel],
) -> Result<(), AppError> {
    super::atomic(connection, |transaction| {
        let existing = list_models(transaction, Some(provider_id))?;
        for old in existing.iter().filter(|m| m.source == source) {
            if !models.iter().any(|m| m.model_id == old.model_id) {
                delete_model(transaction, provider_id, &old.model_id)?;
            }
        }
        for model in models {
            let kept = existing.iter().find(|m| m.model_id == model.model_id);
            if kept.is_some_and(|old| old.source != source && source == ModelSource::Live) {
                // A preset or manual entry already describes it; the live list only confirms it.
                continue;
            }
            let mut model = model.clone();
            model.provider_id = provider_id.to_owned();
            model.source = source;
            if let Some(old) = kept {
                model.enabled = old.enabled;
            }
            save_model(transaction, &model)?;
        }
        Ok(())
    })
}
