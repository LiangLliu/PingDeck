//! The model catalog: facts and prices of models, sourced from models.dev.
//!
//! Persists and queries the `model_catalog` rows. `records` seeds the bundled snapshot and
//! applies a sync, `sync_state` keeps the sync schedule and status, `user_models` edits the
//! user's own rows, `list` pages through the rows, `grouping` says which model each row is an
//! offer of and who released it, `offers` reads a model's offers and its default catalog price,
//! `index` answers lookups from memory and `resolve` maps a provider's model id to a model.
//!
//! This file answers "which row describes this model id?" ([`lookup`], [`lookup_record`],
//! [`search`]) and shares the row mapping. Normalisation is limited to the lookup fallbacks (case,
//! `/<id>` suffix, date or `@version` suffix). Parsing and downloading the models.dev `api.json`
//! is [`spanbay_llm_domain::llm::models_dev`]; the user's pricing is `llm::store::pricing`.

use rusqlite::{Connection, OptionalExtension, Row, params};

pub use spanbay_model::catalog_entry::CatalogEntry;
pub use spanbay_model::model_catalog::ModelRecord;
use spanbay_model::model_catalog::{CatalogKind, CatalogSource};

use spanbay_base::error::AppError;
use spanbay_llm_domain::llm::models_dev;

use crate::sqlite::{escape_like, i64_from_usize};

mod grouping;
mod index;
mod list;
mod offers;
mod records;
mod resolve;
mod sync_state;
#[cfg(test)]
mod tests;
mod user_models;

#[cfg(test)]
pub(crate) use grouping::regroup;
pub use index::CatalogIndex;
pub use list::list_page;
pub(crate) use list::{PRICING_COLUMNS, model_row, pricing_from_row};
pub use models_dev::{download, parse_remote};
pub use offers::ModelOffers;
pub(crate) use offers::{all_offers, offers_of};
pub use records::{apply_sync, seed_bundled_if_empty};
pub(crate) use resolve::rekey_pricing;
pub use resolve::resolve;
pub use sync_state::{
    CatalogStatus, auto_sync_due, record_failure, save_config, status, sync_status,
};
pub use user_models::{delete_user_model, save_user_model};

/// The `model_catalog` columns in the order [`row_record`] reads them. The `INSERT` of
/// `records::insert_sql` lists them and `records::insert` binds them in this same order: change
/// all three together.
const COLUMNS: &str = "family, family_name, model_id, name, kind, context_tokens, max_output_tokens, reasoning, vision, tools, prompt_caching, canonical_id, input_per_mtok, output_per_mtok, cache_read_per_mtok, cache_write_per_mtok, reasoning_per_mtok, currency, source, synced_at_ms, release_date, input_modalities";

/// The order of rows that share a model id: first-party rows first (no canonical id, or one under
/// the row's own family), then rows that carry a price, then the family id.
///
/// `index::tie_key` ranks rows the same way in memory: the two must change together; the test
/// `index_matches_sql_lookup_for_every_fallback_form` checks their parity.
const TIE_ORDER: &str = "CASE WHEN canonical_id = '' OR substr(canonical_id, 1, length(family) + 1) = family || '/' THEN 0 ELSE 1 END, CASE WHEN input_per_mtok IS NOT NULL AND output_per_mtok IS NOT NULL THEN 0 ELSE 1 END, family";

/// A `SQLite` failure of a catalog query as [`AppError::Database`].
fn db_error(error: rusqlite::Error) -> AppError {
    AppError::database(format!("Model catalog database error: {error}"))
}

/// A token limit column; a negative value reads as unknown.
fn tokens(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    Ok(row
        .get::<_, Option<i64>>(index)?
        .and_then(|v| u64::try_from(v).ok()))
}

/// Maps a row selected with [`COLUMNS`].
fn row_record(row: &Row<'_>) -> rusqlite::Result<ModelRecord> {
    Ok(ModelRecord {
        family: row.get(0)?,
        family_name: row.get(1)?,
        model_id: row.get(2)?,
        name: row.get(3)?,
        kind: CatalogKind::parse(&row.get::<_, String>(4)?).unwrap_or_default(),
        context_tokens: tokens(row, 5)?,
        max_output_tokens: tokens(row, 6)?,
        reasoning: row.get(7)?,
        vision: row.get(8)?,
        tools: row.get(9)?,
        prompt_caching: row.get(10)?,
        canonical_id: row.get(11)?,
        input_per_mtok: row.get(12)?,
        output_per_mtok: row.get(13)?,
        cache_read_per_mtok: row.get(14)?,
        cache_write_per_mtok: row.get(15)?,
        reasoning_per_mtok: row.get(16)?,
        currency: row.get(17)?,
        source: CatalogSource::parse(&row.get::<_, String>(18)?).unwrap_or_default(),
        synced_at_ms: row.get(19)?,
        release_date: row.get(20)?,
        input_modalities: row
            .get::<_, Option<String>>(21)?
            .unwrap_or_default()
            .split(',')
            .filter(|modality| !modality.is_empty())
            .map(str::to_owned)
            .collect(),
    })
}

/// Drops a trailing `@version`, `-YYYY-MM-DD` or `-YYYYMMDD` from `id`.
fn strip_version(id: &str) -> Option<&str> {
    if let Some((base, _)) = id.rsplit_once('@') {
        return (!base.is_empty()).then_some(base);
    }
    ["-DDDD-DD-DD", "-DDDDDDDD"]
        .into_iter()
        .find_map(|pattern| {
            let tail_matches = id.len() > pattern.len()
                && id
                    .bytes()
                    .rev()
                    .zip(pattern.bytes().rev())
                    .all(|(byte, want)| {
                        if want == b'D' {
                            byte.is_ascii_digit()
                        } else {
                            byte == want
                        }
                    });
            // The matched tail is ASCII, so the cut falls on a char boundary.
            tail_matches
                .then(|| id.get(..id.len() - pattern.len()))
                .flatten()
        })
}

/// Exact id, then case-insensitive, then a `/<id>` suffix match (shortest id wins); rows that
/// share the id are ranked by [`TIE_ORDER`].
fn lookup_forms(conn: &Connection, id: &str) -> rusqlite::Result<Option<ModelRecord>> {
    let lower = id.to_ascii_lowercase();
    let suffix = format!("/{lower}");
    let suffix_chars = i64_from_usize(suffix.chars().count());
    let steps: [(String, &[&dyn rusqlite::ToSql]); 3] = [
        (format!("model_id = ?1 ORDER BY {TIE_ORDER}"), &[&id]),
        (
            format!("lower(model_id) = ?1 ORDER BY {TIE_ORDER}, model_id"),
            &[&lower],
        ),
        (
            format!(
                "lower(substr(model_id, -?1)) = ?2 ORDER BY length(model_id), {TIE_ORDER}, model_id"
            ),
            &[&suffix_chars, &suffix],
        ),
    ];
    for (clause, args) in steps {
        let sql = format!("SELECT {COLUMNS} FROM model_catalog WHERE {clause} LIMIT 1");
        let found = conn
            .prepare_cached(&sql)?
            .query_row(args, row_record)
            .optional()?;
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

/// The ids [`lookup_record`] tries, in order: `model_id` itself, then without a date or
/// `@version` suffix, then the same two for the text after its last `/` (a relay's `vendor/` or
/// Gemini's `models/` prefix that the catalogue row does not carry).
fn lookup_ids(model_id: &str) -> impl Iterator<Item = &str> {
    let id = model_id.trim();
    let tail = id
        .rsplit_once('/')
        .map(|(_, tail)| tail)
        .filter(|tail| !tail.is_empty());
    [
        Some(id),
        strip_version(id),
        tail,
        tail.and_then(strip_version),
    ]
    .into_iter()
    .flatten()
}

/// Finds the row that describes `model_id`: for each of `lookup_ids`, exact id,
/// case-insensitive id, then id ending in `/<id>` (shortest first). Rows of several families
/// share most ids; `TIE_ORDER` picks among them.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub fn lookup_record(conn: &Connection, model_id: &str) -> Result<Option<ModelRecord>, AppError> {
    for id in lookup_ids(model_id) {
        if let Some(record) = lookup_forms(conn, id).map_err(db_error)? {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

/// [`lookup_record`] as the frozen [`CatalogEntry`] view.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub fn lookup(conn: &Connection, model_id: &str) -> Result<Option<CatalogEntry>, AppError> {
    Ok(lookup_record(conn, model_id)?.map(|record| CatalogEntry::from(&record)))
}

/// One entry per model id that contains `query` (case-insensitive) — the best family's row, by
/// `TIE_ORDER` — ordered by id.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub fn search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<CatalogEntry>, AppError> {
    let escaped = escape_like(query.trim());
    let limit = i64_from_usize(limit);
    let run = || -> rusqlite::Result<Vec<CatalogEntry>> {
        conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM model_catalog m
             WHERE model_id LIKE ?1 ESCAPE '\\'
               AND m.rowid = (SELECT o.rowid FROM model_catalog o WHERE o.model_id = m.model_id
                              ORDER BY {TIE_ORDER} LIMIT 1)
             ORDER BY model_id LIMIT ?2"
        ))?
        .query_map(params![format!("%{escaped}%"), limit], |row| {
            row_record(row).map(|record| CatalogEntry::from(&record))
        })?
        .collect()
    };
    run().map_err(db_error)
}
