//! History reads: keyset paging, counts, run summaries and the full row of one entry.

use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params, params_from_iter, types::Value};

use pingdeck_model::history_filter::{HistoryCursor, HistoryFilter, HistoryRunSummary};
use pingdeck_model::models::{ExecutionHistoryEntry, HistoryEntryId};

use super::HistoryRows;
use super::filter::{Clause, FAILED_ROW_SQL, filter_clause};
use super::row::{
    SELECT_FULL_ROW_BY_ID, StoredExecutionHistoryEntry, source_from_storage,
    stored_execution_history_row,
};
use crate::sqlite::to_u64;
use crate::storage::{StorageError, WorkspaceStore};

/// Largest page the store serves; the caller fetches one extra row to learn whether more follow.
const MAX_PAGE_ROWS: u32 = 200;

/// Columns of a list query, in [`stored_execution_history_row`] order: the heavy blobs
/// (snapshot, response headers, sent request, call chain) are left out; `get_execution_history`
/// loads them.
const LIST_COLUMNS: &str = "
    id, workspace_id, request_id, environment_id, result_kind, request_name, method, request_url,
    status_code, status_text, final_url, elapsed_ms, body_size_bytes, attempt_count,
    response_body_preview, response_body_truncated, error_kind, error_message, executed_at,
    NULL AS request_snapshot_json, source, source_name, source_id, run_id, protocol, timing_json,
    environment_name, NULL AS response_headers_json, response_headers_truncated,
    NULL AS sent_request_json, NULL AS call_chain_json, connect_phase, check_passed, check_failed,
    origin_ref
";

impl WorkspaceStore {
    /// Newest entries of a workspace, without the heavy blobs.
    #[cfg(any(test, feature = "test-support"))]
    pub fn list_execution_history(
        &self,
        workspace_id: &str,
        limit: u32,
    ) -> Result<Vec<ExecutionHistoryEntry>, StorageError> {
        Ok(self
            .query_execution_history(workspace_id, &HistoryFilter::default(), limit, None)?
            .entries)
    }

    /// Pages through a workspace's history, newest first, under `filter`.
    ///
    /// The page resumes strictly after `cursor` in `(executed_at, id)` descending order, so rows
    /// inserted while paging can neither repeat nor be skipped. `limit` is clamped to
    /// `1..=200`. The first page (no cursor) also reports the filter's total row count.
    pub fn query_execution_history(
        &self,
        workspace_id: &str,
        filter: &HistoryFilter,
        limit: u32,
        cursor: Option<&HistoryCursor>,
    ) -> Result<HistoryRows, StorageError> {
        self.query_history_page(workspace_id, filter, None, limit, cursor)
    }

    /// Pages through the rows of one run, newest first, under the same `filter` and
    /// keyset rules as [`Self::query_execution_history`]; the filter narrows the children.
    /// `total` (first page) counts the matching children and `runs` is empty.
    pub fn query_run_children(
        &self,
        workspace_id: &str,
        run_id: &str,
        filter: &HistoryFilter,
        limit: u32,
        cursor: Option<&HistoryCursor>,
    ) -> Result<HistoryRows, StorageError> {
        self.query_history_page(workspace_id, filter, Some(run_id), limit, cursor)
    }

    fn query_history_page(
        &self,
        workspace_id: &str,
        filter: &HistoryFilter,
        run_id: Option<&str>,
        limit: u32,
        cursor: Option<&HistoryCursor>,
    ) -> Result<HistoryRows, StorageError> {
        let page_size = limit.clamp(1, MAX_PAGE_ROWS);
        let mut clause = filter_clause(workspace_id, filter);
        if let Some(run_id) = run_id {
            clause.push_eq("run_id", Value::Text(run_id.to_owned()));
        }
        // The count covers the whole filter, so it is taken before the cursor narrows the page.
        let total = if cursor.is_none() {
            Some(self.count_matching(&clause)?)
        } else {
            None
        };
        let workspace_total = match total {
            Some(total) if run_id.is_none() && filter.is_empty() => Some(total),
            Some(_) if run_id.is_none() => {
                Some(self.count_execution_history(workspace_id, &HistoryFilter::default())?)
            }
            _ => None,
        };
        if let Some(cursor) = cursor {
            let at = clause.bind(Value::Text(cursor.executed_at.clone()));
            let id = clause.bind(Value::Text(cursor.id.clone()));
            clause.sql.push_str(&format!(
                " AND (executed_at < ?{at} OR (executed_at = ?{at} AND id < ?{id}))"
            ));
        }
        let limit_index = clause.bind(Value::Integer(i64::from(page_size) + 1));
        // `prepare`, not `prepare_cached`: the filter shapes outnumber the statement cache.
        let mut statement = self.connection.prepare(&format!(
            "SELECT {LIST_COLUMNS} FROM execution_history WHERE {}
             ORDER BY executed_at DESC, id DESC LIMIT ?{limit_index}",
            clause.sql
        ))?;
        let rows = statement.query_map(
            params_from_iter(clause.params.iter()),
            stored_execution_history_row,
        )?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?.into_domain()?);
        }
        drop(statement);

        let next_cursor = if entries.len() > page_size as usize {
            entries.truncate(page_size as usize);
            entries.last().map(|entry| HistoryCursor {
                executed_at: entry.executed_at.clone(),
                id: entry.id.as_str().to_owned(),
            })
        } else {
            None
        };
        let runs = if run_id.is_none() {
            self.run_summaries(workspace_id, filter, &entries)?
        } else {
            Vec::new()
        };
        self.fill_collection_paths(workspace_id, &mut entries)?;
        self.fill_origin_labels(&mut entries)?;

        Ok(HistoryRows {
            entries,
            next_cursor,
            total,
            workspace_total,
            runs,
        })
    }

    /// Rows of `workspace_id` that `filter` matches.
    fn count_execution_history(
        &self,
        workspace_id: &str,
        filter: &HistoryFilter,
    ) -> Result<u64, StorageError> {
        self.count_matching(&filter_clause(workspace_id, filter))
    }

    fn count_matching(&self, clause: &Clause) -> Result<u64, StorageError> {
        let count: i64 = self.connection.query_row(
            &format!(
                "SELECT COUNT(*) FROM execution_history WHERE {}",
                clause.sql
            ),
            params_from_iter(clause.params.iter()),
            |row| row.get(0),
        )?;

        Ok(to_u64(count))
    }

    /// Totals of every run with a row in `entries`, over the rows `filter` keeps.
    fn run_summaries(
        &self,
        workspace_id: &str,
        filter: &HistoryFilter,
        entries: &[ExecutionHistoryEntry],
    ) -> Result<Vec<HistoryRunSummary>, StorageError> {
        let run_ids: BTreeSet<&str> = entries
            .iter()
            .filter_map(|entry| entry.run_id.as_deref())
            .collect();
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut clause = filter_clause(workspace_id, filter);
        let placeholders = run_ids
            .iter()
            .map(|run_id| format!("?{}", clause.bind(Value::Text((*run_id).to_owned()))))
            .collect::<Vec<_>>()
            .join(", ");
        let mut statement = self.connection.prepare(&format!(
            "
            SELECT run_id, source, source_name, source_id,
                   COUNT(*),
                   SUM(CASE WHEN {FAILED_ROW_SQL} THEN 1 ELSE 0 END),
                   MIN(executed_at), MAX(executed_at)
            FROM execution_history
            WHERE {} AND run_id IN ({placeholders})
            GROUP BY run_id
            ORDER BY MAX(executed_at) DESC, run_id DESC
            ",
            clause.sql
        ))?;
        let rows = statement.query_map(params_from_iter(clause.params.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        let mut runs = Vec::new();
        for row in rows {
            let (run_id, source, source_name, source_id, total, failed, first, last) = row?;
            runs.push(HistoryRunSummary {
                source: source_from_storage("execution_history", &run_id, &source)?,
                run_id,
                source_name,
                source_id,
                total: to_u64(total),
                failed: to_u64(failed),
                first_executed_at: first,
                last_executed_at: last,
            });
        }

        Ok(runs)
    }

    /// Loads one history entry of `workspace_id` by id, including its request snapshot and the
    /// stored response details.
    ///
    /// An entry owned by another workspace is reported as `None`, never loaded.
    pub fn get_execution_history(
        &self,
        workspace_id: &str,
        id: &HistoryEntryId,
    ) -> Result<Option<ExecutionHistoryEntry>, StorageError> {
        let mut statement = self.connection.prepare_cached(SELECT_FULL_ROW_BY_ID)?;
        let row = statement
            .query_row(
                params![workspace_id, id.as_str()],
                stored_execution_history_row,
            )
            .optional()?;
        let mut entry = row
            .map(StoredExecutionHistoryEntry::into_domain)
            .transpose()?;
        if let Some(entry) = entry.as_mut() {
            self.fill_collection_paths(workspace_id, std::slice::from_mut(entry))?;
            self.fill_origin_labels(std::slice::from_mut(entry))?;
        }
        Ok(entry)
    }
}
