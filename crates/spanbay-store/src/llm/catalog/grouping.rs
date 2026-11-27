//! Which model a catalog row is an offer of, and who released it.
//!
//! Gives every row a `model_key`, a `publisher` and a `publisher_name`, so the list shows one row
//! per model however many providers sell it. Applies
//! [`spanbay_llm_domain::llm::catalog_grouping::model_keys`] (which holds the grouping rules) to
//! the synced catalog rows and writes the keys back.
//!
//! The publisher is the key's part before the first `/`; its name is that family's provider name
//! when the catalog has the family, else the id. User rows never join a model: key
//! `user:<family>/<model_id>`, publisher their family.

use std::collections::HashMap;

use rusqlite::{Connection, params};

use spanbay_llm_domain::llm::catalog_grouping::{GroupingRow, model_keys};
use spanbay_model::model_catalog::CatalogSource;

/// The grouping columns of one stored row.
struct StoredRow {
    family: String,
    model_id: String,
    canonical_id: String,
    family_name: String,
    source: String,
    model_key: String,
    publisher: String,
    publisher_name: String,
}

impl StoredRow {
    fn is_user(&self) -> bool {
        self.source == CatalogSource::User.as_str()
    }
}

/// Whether any row still lacks its key (a database whose rows were written before keys existed) or
/// any pricing row its model.
///
/// # Errors
/// Any `SQLite` failure.
pub(super) fn needs_regroup(conn: &Connection) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM model_catalog WHERE model_key = '')
             OR EXISTS (SELECT 1 FROM model_pricing WHERE model_key = '')",
    )?
    .query_row([], |row| row.get(0))
}

/// Recomputes `model_key`, `publisher` and `publisher_name` of every row and writes the ones
/// that changed, then the model of every pricing row. Run inside the transaction of every catalog
/// write.
///
/// # Errors
/// Any `SQLite` failure.
pub(crate) fn regroup(conn: &Connection) -> rusqlite::Result<()> {
    let rows = read_rows(conn)?;
    let synced: Vec<&StoredRow> = rows.iter().filter(|row| !row.is_user()).collect();
    let mut keys = synced_keys(&synced);
    // The provider name of every synced family.
    let names: HashMap<&str, &str> = synced
        .iter()
        .map(|row| (row.family.as_str(), row.family_name.as_str()))
        .collect();
    let mut update = conn.prepare_cached(
        "UPDATE model_catalog SET model_key = ?3, publisher = ?4, publisher_name = ?5
         WHERE family = ?1 AND model_id = ?2",
    )?;
    for row in &rows {
        let (key, publisher) = if row.is_user() {
            (
                format!("user:{}/{}", row.family, row.model_id),
                row.family.clone(),
            )
        } else {
            let key = keys
                .remove(&(row.family.as_str(), row.model_id.as_str()))
                .unwrap_or_default();
            let publisher = key.split('/').next().unwrap_or_default().to_owned();
            (key, publisher)
        };
        let name = publisher_name(row, &publisher, &names);
        if (key.as_str(), publisher.as_str(), name)
            != (
                row.model_key.as_str(),
                row.publisher.as_str(),
                row.publisher_name.as_str(),
            )
        {
            update.execute(params![row.family, row.model_id, key, publisher, name])?;
        }
    }
    drop(update);
    super::rekey_pricing(conn)
}

fn read_rows(conn: &Connection) -> rusqlite::Result<Vec<StoredRow>> {
    conn.prepare_cached(
        "SELECT family, model_id, canonical_id, family_name, source, model_key, publisher,
                publisher_name
         FROM model_catalog",
    )?
    .query_map([], |row| {
        Ok(StoredRow {
            family: row.get(0)?,
            model_id: row.get(1)?,
            canonical_id: row.get(2)?,
            family_name: row.get(3)?,
            source: row.get(4)?,
            model_key: row.get(5)?,
            publisher: row.get(6)?,
            publisher_name: row.get(7)?,
        })
    })?
    .collect()
}

/// The [`model_keys`] of the synced rows by `(family, model_id)`.
fn synced_keys<'a>(synced: &[&'a StoredRow]) -> HashMap<(&'a str, &'a str), String> {
    let offers: Vec<GroupingRow<'_>> = synced
        .iter()
        .map(|row| GroupingRow {
            family: &row.family,
            model_id: &row.model_id,
            canonical_id: &row.canonical_id,
        })
        .collect();
    synced
        .iter()
        .zip(model_keys(&offers))
        .map(|(row, key)| ((row.family.as_str(), row.model_id.as_str()), key))
        .collect()
}

/// The provider name of `publisher` when a synced family has that id, else the id; a user row
/// whose family no synced row names keeps its own family name.
fn publisher_name<'a>(
    row: &'a StoredRow,
    publisher: &'a str,
    names: &HashMap<&str, &'a str>,
) -> &'a str {
    let name = names.get(publisher).copied().unwrap_or(publisher);
    if row.is_user() && name == publisher {
        row.family_name.as_str()
    } else {
        name
    }
}
