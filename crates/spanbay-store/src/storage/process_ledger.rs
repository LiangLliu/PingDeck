//! Persistence for `process_ledger`: one row per process group the core spawned and has not seen
//! end, handed over at the next start so leftovers can be reaped.

use rusqlite::params;

use super::{StorageError, WorkspaceStore};
use spanbay_base::time::now_ms;
use spanbay_model::agent::ledger::{LedgerEntry, LedgerKind};

impl WorkspaceStore {
    pub fn ledger_record(&self, entry: &LedgerEntry) -> Result<(), StorageError> {
        self.connection
            .prepare_cached(
                "INSERT OR REPLACE INTO process_ledger (pid, pgid, started, kind, label, recorded_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                entry.pid,
                entry.pgid,
                entry.started,
                entry.kind.as_str(),
                entry.label,
                now_ms()
            ])?;
        Ok(())
    }

    pub fn ledger_remove(&self, pid: u32, started: &str) -> Result<(), StorageError> {
        self.connection
            .prepare_cached("DELETE FROM process_ledger WHERE pid = ?1 AND started = ?2")?
            .execute(params![pid, started])?;
        Ok(())
    }

    /// Reads and deletes every record, oldest first (by `recorded_ms`, then `pid`). A row whose
    /// kind this build does not know is dropped.
    ///
    /// The read and the delete are not one transaction. That is safe because only the store
    /// connection writes this table, under the store lock the caller holds for the whole call;
    /// pooled connections never touch it.
    pub fn ledger_take_all(&self) -> Result<Vec<LedgerEntry>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT pid, pgid, started, kind, label FROM process_ledger ORDER BY recorded_ms, pid",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        self.connection.execute("DELETE FROM process_ledger", [])?;
        Ok(rows
            .into_iter()
            .filter_map(|(pid, pgid, started, kind, label)| {
                Some(LedgerEntry {
                    pid,
                    pgid,
                    started,
                    kind: LedgerKind::parse(&kind)?,
                    label,
                })
            })
            .collect())
    }
}
