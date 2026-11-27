//! Persistence of global settings: one JSON row per section in `app_settings`.

use rusqlite::{Connection, OptionalExtension as _, params};

use spanbay_model::stored_setting::StoredSetting;

use super::{StorageError, WorkspaceStore};

/// The stored JSON text of `section`, unvalidated, for callers that run on a connection of their
/// own instead of the store (the gateway start-up check, which must see the row as stored).
pub fn raw_app_setting_json(
    connection: &Connection,
    section: &str,
) -> Result<Option<String>, StorageError> {
    connection
        .prepare_cached("SELECT value_json FROM app_settings WHERE section = ?1")?
        .query_row(params![section], |row| row.get(0))
        .optional()
        .map_err(Into::into)
}

impl WorkspaceStore {
    pub fn list_app_settings(&self) -> Result<Vec<StoredSetting>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT section, version, value_json FROM app_settings ORDER BY section",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(StoredSetting {
                section: row.get(0)?,
                version: row.get(1)?,
                value_json: row.get(2)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn save_app_setting(
        &self,
        setting: &StoredSetting,
        updated_at_ms: i64,
    ) -> Result<(), StorageError> {
        upsert_app_setting(&self.connection, setting, updated_at_ms)
    }

    /// Upserts several settings sections in one transaction: all of them change or none does (a
    /// savepoint of the caller's transaction when one is open).
    pub fn save_app_settings(
        &self,
        settings: &[StoredSetting],
        updated_at_ms: i64,
    ) -> Result<(), StorageError> {
        self.atomic(|transaction| {
            for setting in settings {
                upsert_app_setting(transaction, setting, updated_at_ms)?;
            }
            Ok(())
        })
    }
}

fn upsert_app_setting(
    connection: &Connection,
    setting: &StoredSetting,
    updated_at_ms: i64,
) -> Result<(), StorageError> {
    connection.execute(
        "
        INSERT INTO app_settings (section, version, value_json, updated_at_ms)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(section) DO UPDATE SET
            version = excluded.version,
            value_json = excluded.value_json,
            updated_at_ms = excluded.updated_at_ms
        ",
        params![
            setting.section,
            setting.version,
            setting.value_json,
            updated_at_ms
        ],
    )?;
    Ok(())
}
