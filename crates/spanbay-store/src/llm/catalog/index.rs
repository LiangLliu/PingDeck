//! In-memory lookup over the stored catalog, for callers that resolve many ids in one pass.
//!
//! Answers [`super::lookup_record`] from hash maps loaded once, so a 400-model list costs one
//! table read instead of up to a dozen queries per model under the store lock. The maps pick
//! among rows that share an id exactly as the SQL does ([`super::TIE_ORDER`]).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

use rusqlite::Connection;

use super::{COLUMNS, CatalogEntry, ModelRecord, db_error, lookup_ids, row_record};
use spanbay_base::error::AppError;

/// Rank of a row among rows with the same id: first-party first, then priced, then family.
/// Mirrors [`super::TIE_ORDER`]: the two must change together; the test
/// `index_matches_sql_lookup_for_every_fallback_form` checks their parity.
fn tie_key(record: &ModelRecord) -> (bool, bool, &str) {
    (
        !record.is_first_party(),
        !record.has_price(),
        record.family.as_str(),
    )
}

/// Points `key` of `map` at the row at `position` unless the row it points at already ranks
/// first by `cmp` (a tie keeps the earlier row).
fn keep_best(
    map: &mut HashMap<String, usize>,
    key: String,
    position: usize,
    records: &[ModelRecord],
    cmp: &dyn Fn(&ModelRecord, &ModelRecord) -> Ordering,
) {
    match map.entry(key) {
        Entry::Occupied(mut slot) => {
            if cmp(&records[position], &records[*slot.get()]) == Ordering::Less {
                slot.insert(position);
            }
        }
        Entry::Vacant(slot) => {
            slot.insert(position);
        }
    }
}

/// The whole catalog with the lookup forms of [`super::lookup_record`] precomputed.
#[derive(Debug, Default)]
pub struct CatalogIndex {
    records: Vec<ModelRecord>,
    /// Exact id -> best row.
    exact: HashMap<String, usize>,
    /// ASCII-lowercased id -> best row (ties by id).
    lower: HashMap<String, usize>,
    /// ASCII-lowercased text after any `/` of an id -> best row (shortest id first).
    suffix: HashMap<String, usize>,
}

impl CatalogIndex {
    /// Reads every stored row.
    ///
    /// # Errors
    /// [`AppError::database`] on any `SQLite` failure.
    pub fn load(conn: &Connection) -> Result<Self, AppError> {
        let read = || -> rusqlite::Result<Vec<ModelRecord>> {
            conn.prepare_cached(&format!("SELECT {COLUMNS} FROM model_catalog"))?
                .query_map([], row_record)?
                .collect()
        };
        Ok(Self::new(read().map_err(db_error)?))
    }

    /// Indexes `records`, in any order.
    fn new(records: Vec<ModelRecord>) -> Self {
        let by_tie = |a: &ModelRecord, b: &ModelRecord| tie_key(a).cmp(&tie_key(b));
        let by_tie_then_id = |a: &ModelRecord, b: &ModelRecord| {
            by_tie(a, b).then_with(|| a.model_id.cmp(&b.model_id))
        };
        let chars = |record: &ModelRecord| record.model_id.chars().count();
        let by_length_then_tie = |a: &ModelRecord, b: &ModelRecord| {
            chars(a).cmp(&chars(b)).then_with(|| by_tie_then_id(a, b))
        };
        let mut exact = HashMap::new();
        let mut lower = HashMap::new();
        let mut suffix = HashMap::new();
        for (position, record) in records.iter().enumerate() {
            let id = record.model_id.as_str();
            let lowered = id.to_ascii_lowercase();
            keep_best(&mut exact, id.to_owned(), position, &records, &by_tie);
            for (slash, _) in lowered.match_indices('/') {
                keep_best(
                    &mut suffix,
                    lowered[slash + 1..].to_owned(),
                    position,
                    &records,
                    &by_length_then_tie,
                );
            }
            keep_best(&mut lower, lowered, position, &records, &by_tie_then_id);
        }
        Self {
            records,
            exact,
            lower,
            suffix,
        }
    }

    fn forms(&self, id: &str) -> Option<&ModelRecord> {
        let lower = id.to_ascii_lowercase();
        let position = self
            .exact
            .get(id)
            .or_else(|| self.lower.get(&lower))
            .or_else(|| self.suffix.get(&lower))?;
        self.records.get(*position)
    }

    /// The same answer as [`super::lookup_record`] on the stored rows: for each of
    /// `lookup_ids`, exact id, case-insensitive id, then id ending in `/<id>` (shortest
    /// first).
    #[must_use]
    pub fn lookup_record(&self, model_id: &str) -> Option<&ModelRecord> {
        lookup_ids(model_id).find_map(|id| self.forms(id))
    }

    /// [`CatalogIndex::lookup_record`] as the frozen [`CatalogEntry`] view.
    #[must_use]
    pub fn lookup(&self, model_id: &str) -> Option<CatalogEntry> {
        self.lookup_record(model_id).map(CatalogEntry::from)
    }
}
