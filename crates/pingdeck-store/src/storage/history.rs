//! Execution history persistence. This module writes: insert, per-source retention, deletes,
//! the Undo take and restore, and the scrub of a value turned into a secret. Reads live in
//! `query` (paging, counts, run summaries) with `enrich` (collection paths, chat titles),
//! `filter` builds the shared `WHERE` clause and `row` maps the columns.

mod enrich;
mod filter;
mod query;
mod row;

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use pingdeck_base::secrets::MIN_REDACTION_CHARS;
use pingdeck_model::history_filter::{HistoryCursor, HistoryFilter, HistoryRunSummary};
use pingdeck_model::models::{
    EnvironmentId, ExecutionHistoryEntry, HISTORY_BODY_PREVIEW_LIMIT_BYTES,
    HISTORY_CALL_CHAIN_LIMIT_BYTES, HISTORY_REQUEST_SNAPSHOT_LIMIT_BYTES, HistoryEntryId,
    HistoryOriginRef, HistorySource, RequestId, bounded_body_preview, capped_masked_headers,
};

use filter::filter_clause;
use row::{
    SELECT_FULL_ROW_BY_ID, execution_history_result_kind_to_storage, history_columns,
    stored_execution_history_row,
};

use crate::sqlite::{i64_from_u128, i64_from_usize, to_u64};
use crate::storage::requests::http_method_to_storage;
use crate::storage::{StorageError, WorkspaceStore};

/// Default execution history rows kept per workspace and source (`data.history_retention`
/// overrides it).
pub const HISTORY_RETENTION_PER_WORKSPACE: usize = 2000;

/// Mock-hit rows kept per workspace: a noisy mock cannot flush request history.
pub const HISTORY_MOCK_RETENTION: usize = 500;

/// The sources that each keep their own `data.history_retention` budget per workspace, so a
/// collection run, a flow or the agent never flushes the rows of another source. Runner and flow
/// rows are kept and dropped by whole runs ([`RUN_BUCKETS`]); the others row by row. Mock hits
/// have their own fixed budget ([`HISTORY_MOCK_RETENTION`]).
pub const HISTORY_ROW_BUCKETS: [HistorySource; 4] = [
    HistorySource::Request,
    HistorySource::Agent,
    HistorySource::Runner,
    HistorySource::Flow,
];

/// Sources whose rows are counted and evicted by run: the newest run is always kept whole, older
/// runs stay while the rows kept so far fit the budget. A row without a run id is its own run.
const RUN_BUCKETS: [HistorySource; 2] = [HistorySource::Runner, HistorySource::Flow];

/// The run keys of one workspace (`?1`) and source (`?2`) beyond a budget of `?3` rows, newest
/// run first: everything after the first run whose cumulative row count passes the budget, the
/// newest run excepted.
const EVICTED_RUNS_SQL: &str = "
    WITH runs AS (
        SELECT COALESCE(run_id, id) AS run_key, COUNT(*) AS n, MAX(executed_at) AS last_at
        FROM execution_history
        WHERE workspace_id = ?1 AND source = ?2
        GROUP BY run_key
    ),
    ranked AS (
        SELECT run_key, n,
               SUM(n) OVER (ORDER BY last_at DESC, run_key DESC ROWS UNBOUNDED PRECEDING) AS kept,
               ROW_NUMBER() OVER (ORDER BY last_at DESC, run_key DESC) AS rank
        FROM runs
    )
    SELECT run_key, n FROM ranked WHERE rank > 1 AND kept > ?3
";

/// Registers the SQL functions the history search uses on `connection`: `pd_casefold(text)`, the
/// Unicode lowercase of a text (SQLite's `lower()` only folds ASCII); `NULL` stays `NULL`.
///
/// # Errors
/// [`StorageError::Sqlite`] when SQLite refuses the registration.
pub(super) fn register_search_functions(connection: &Connection) -> Result<(), StorageError> {
    use rusqlite::functions::FunctionFlags;
    use rusqlite::types::ValueRef;
    connection.create_scalar_function(
        "pd_casefold",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            Ok(match context.get_raw(0) {
                ValueRef::Text(text) => Some(String::from_utf8_lossy(text).to_lowercase()),
                _ => None,
            })
        },
    )?;
    Ok(())
}

/// How a row is inserted: [`Insert::Always`] for a live row, [`Insert::UnlessPresent`] for a
/// restored one whose id may exist again.
#[derive(Clone, Copy)]
enum Insert {
    Always,
    UnlessPresent,
}

/// An insert of one whole row, binding `?1..?35` in `history_columns!` order.
macro_rules! insert_history {
    ($verb:literal) => {
        concat!(
            $verb,
            " INTO execution_history (",
            history_columns!(),
            ")
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                     ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32,
                     ?33, ?34, ?35)"
        )
    };
}

impl Insert {
    const fn sql(self) -> &'static str {
        match self {
            Self::Always => insert_history!("INSERT"),
            Self::UnlessPresent => insert_history!("INSERT OR IGNORE"),
        }
    }
}

/// One page of history rows with its continuation and the run totals of its rows.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRows {
    /// At most the requested number of rows, newest first.
    pub entries: Vec<ExecutionHistoryEntry>,
    /// Where the next page starts; `None` on the last page.
    pub next_cursor: Option<HistoryCursor>,
    /// Rows the filter matches in total; only computed for the first page (no cursor).
    pub total: Option<u64>,
    /// Rows of the whole workspace, whatever the filter; first page of a list query only. Tells
    /// "nothing in this range" from "nothing ever recorded".
    pub workspace_total: Option<u64>,
    /// Totals of every run that has a row on this page, over the rows the filter keeps.
    pub runs: Vec<HistoryRunSummary>,
}

impl WorkspaceStore {
    /// Inserts a history entry and prunes its source to the retention limit.
    pub fn save_execution_history(
        &self,
        entry: &ExecutionHistoryEntry,
    ) -> Result<(), StorageError> {
        self.save_execution_history_batch(std::slice::from_ref(entry))
    }

    /// Inserts history entries in one transaction (the mock-hit writer batches this way) and
    /// prunes each touched workspace and source once, after the inserts.
    pub fn save_execution_history_batch(
        &self,
        entries: &[ExecutionHistoryEntry],
    ) -> Result<(), StorageError> {
        if entries.is_empty() {
            return Ok(());
        }
        self.atomic(|transaction| {
            let mut touched: BTreeSet<(&str, &'static str)> = BTreeSet::new();
            for entry in entries {
                insert_row(transaction, Insert::Always, entry)?;
                touched.insert((entry.workspace_id.as_str(), entry.source.as_str()));
            }
            for (workspace_id, source) in touched {
                self.prune_source(transaction, workspace_id, source)?;
            }
            Ok(())
        })
    }

    /// Deletes the rows of a workspace that `filter` selects and returns how many were removed;
    /// the predicate is the one paging and counting use.
    pub fn delete_filtered_execution_history(
        &self,
        workspace_id: &str,
        filter: &HistoryFilter,
    ) -> Result<u64, StorageError> {
        let clause = filter_clause(workspace_id, filter);
        let removed = self.connection.execute(
            &format!("DELETE FROM execution_history WHERE {}", clause.sql),
            params_from_iter(clause.params.iter()),
        )?;

        Ok(removed as u64)
    }

    /// Deletes one history entry of `workspace_id`; returns whether it existed there.
    #[cfg(test)]
    pub(crate) fn delete_execution_history_entry(
        &self,
        workspace_id: &str,
        id: &HistoryEntryId,
    ) -> Result<bool, StorageError> {
        Ok(!self
            .delete_execution_history_entries(workspace_id, std::slice::from_ref(id))?
            .is_empty())
    }

    /// Deletes the given history entries of `workspace_id` in one transaction and returns the
    /// ids removed; ids unknown or owned by another workspace are skipped and left intact.
    pub fn delete_execution_history_entries(
        &self,
        workspace_id: &str,
        ids: &[HistoryEntryId],
    ) -> Result<Vec<HistoryEntryId>, StorageError> {
        self.atomic(|transaction| {
            let mut delete = transaction
                .prepare("DELETE FROM execution_history WHERE workspace_id = ?1 AND id = ?2")?;
            let mut deleted_ids = Vec::new();
            for id in ids {
                if delete.execute(params![workspace_id, id.as_str()])? > 0 {
                    deleted_ids.push(id.clone());
                }
            }
            Ok(deleted_ids)
        })
    }

    /// Deletes every history entry of a workspace and returns the ids removed.
    pub fn delete_all_execution_history(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<HistoryEntryId>, StorageError> {
        self.atomic(|transaction| {
            let deleted_ids = {
                let mut statement = transaction.prepare_cached(
                    "SELECT id FROM execution_history WHERE workspace_id = ?1 ORDER BY executed_at ASC, id ASC",
                )?;
                let rows =
                    statement.query_map(params![workspace_id], |row| row.get::<_, String>(0))?;
                let mut ids = Vec::new();
                for row in rows {
                    ids.push(HistoryEntryId::new(&row?)?);
                }
                ids
            };
            transaction.execute(
                "DELETE FROM execution_history WHERE workspace_id = ?1",
                params![workspace_id],
            )?;
            Ok(deleted_ids)
        })
    }

    /// Re-inserts previously deleted history entries, skipping ids that already exist.
    ///
    /// The rows come from the webview, so each one is re-bounded like a live row
    /// (`bound_restored_entry`) and the touched workspaces and sources are pruned to their
    /// retention afterwards. Only the ids that were really inserted *and* survived the prune are
    /// reported.
    pub fn restore_execution_history_entries(
        &self,
        entries: Vec<ExecutionHistoryEntry>,
    ) -> Result<Vec<HistoryEntryId>, StorageError> {
        self.atomic(|transaction| {
            let mut restored_ids = Vec::new();
            let mut touched: BTreeSet<(String, &'static str)> = BTreeSet::new();

            for mut entry in entries {
                bound_restored_entry(&mut entry);
                if insert_row(transaction, Insert::UnlessPresent, &entry)? > 0 {
                    touched.insert((
                        entry.workspace_id.as_str().to_owned(),
                        entry.source.as_str(),
                    ));
                    restored_ids.push(entry.id);
                }
            }
            for (workspace_id, source) in touched {
                self.prune_source(transaction, &workspace_id, source)?;
            }
            let mut exists =
                transaction.prepare("SELECT 1 FROM execution_history WHERE id = ?1")?;
            let mut surviving = Vec::with_capacity(restored_ids.len());
            for id in restored_ids {
                if exists.exists(params![id.as_str()])? {
                    surviving.push(id);
                }
            }
            Ok(surviving)
        })
    }

    /// Loads the full rows of `ids` in `workspace_id` and deletes them in one transaction (the
    /// Undo hold keeps what was read); ids unknown or owned by another workspace are skipped.
    pub fn take_execution_history_entries(
        &self,
        workspace_id: &str,
        ids: &[HistoryEntryId],
    ) -> Result<Vec<ExecutionHistoryEntry>, StorageError> {
        self.atomic(|transaction| {
            let mut taken = Vec::new();
            let mut select = transaction.prepare_cached(SELECT_FULL_ROW_BY_ID)?;
            let mut delete = transaction.prepare_cached(
                "DELETE FROM execution_history WHERE workspace_id = ?1 AND id = ?2",
            )?;
            for id in ids {
                let row = select
                    .query_row(
                        params![workspace_id, id.as_str()],
                        stored_execution_history_row,
                    )
                    .optional()?;
                if let Some(row) = row {
                    taken.push(row.into_domain()?);
                    delete.execute(params![workspace_id, id.as_str()])?;
                }
            }
            Ok(taken)
        })
    }

    /// The most rows the retention keeps in one workspace: each budgeted source plus the mock
    /// hits (a run may run past its source budget until the next run starts).
    #[must_use]
    pub fn history_capacity(&self) -> usize {
        self.history_retention
            .saturating_mul(HISTORY_ROW_BUCKETS.len())
            .saturating_add(HISTORY_MOCK_RETENTION)
    }

    /// How many rows a retention of `keep` per source would delete: in `workspace_id`, or summed
    /// over every workspace when `None` (retention is one global value applied to each workspace
    /// and source on its next insert). Runner and flow rows count by whole runs, like the prune.
    pub fn history_prune_count(
        &self,
        workspace_id: Option<&str>,
        keep: usize,
    ) -> Result<u64, StorageError> {
        let workspaces: Vec<String> = match workspace_id {
            Some(workspace_id) => vec![workspace_id.to_owned()],
            None => {
                let mut statement = self
                    .connection
                    .prepare_cached("SELECT DISTINCT workspace_id FROM execution_history")?;
                statement
                    .query_map([], |row| row.get(0))?
                    .collect::<Result<_, _>>()?
            }
        };
        let mut total = 0_u64;
        for workspace_id in &workspaces {
            for source in HISTORY_ROW_BUCKETS {
                total += if RUN_BUCKETS.contains(&source) {
                    evicted_runs(&self.connection, workspace_id, source.as_str(), keep)?
                        .iter()
                        .map(|(_, rows)| *rows)
                        .sum::<u64>()
                } else {
                    source_rows(&self.connection, workspace_id, source.as_str())?
                        .saturating_sub(keep as u64)
                };
            }
        }
        Ok(total)
    }

    /// Rewrites `value` to `replacement` in the stored request definitions and sent requests of
    /// `workspace_id`'s history (the value became a secret, so earlier rows must stop carrying
    /// its plaintext) and returns how many rows changed. Values shorter than the redaction floor
    /// are left alone: they would match unrelated text. The JSON columns are parsed and only
    /// their string values are rewritten, so a match never breaks the JSON around it; a column
    /// that does not parse (a row synced or imported broken) gets the JSON-escaped value replaced
    /// in its raw text instead of failing the whole scrub.
    pub fn scrub_history_value(
        &self,
        workspace_id: &str,
        value: &str,
        replacement: &str,
    ) -> Result<u64, StorageError> {
        if value.chars().count() < MIN_REDACTION_CHARS {
            return Ok(0);
        }
        // The value as it appears inside a JSON string literal.
        let encoded = serde_json::to_string(value)?;
        let needle = &encoded[1..encoded.len() - 1];
        let encoded_replacement = serde_json::to_string(replacement)?;
        let scrub = Scrub {
            value,
            replacement,
            needle,
            encoded_replacement: &encoded_replacement[1..encoded_replacement.len() - 1],
        };
        self.atomic(|transaction| {
            let rows: Vec<ScrubRow> = {
                let mut statement = transaction.prepare(
                    "SELECT id, request_snapshot_json, sent_request_json, request_url, final_url
                     FROM execution_history
                     WHERE workspace_id = ?1
                       AND (instr(request_snapshot_json, ?2) > 0
                            OR instr(sent_request_json, ?2) > 0
                            OR instr(request_url, ?3) > 0
                            OR instr(final_url, ?3) > 0)",
                )?;
                statement
                    .query_map(params![workspace_id, needle, value], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })?
                    .collect::<Result<_, _>>()?
            };
            let mut update = transaction.prepare(
                "UPDATE execution_history
                 SET request_snapshot_json = ?2, sent_request_json = ?3, request_url = ?4,
                     final_url = ?5
                 WHERE id = ?1",
            )?;
            for (id, snapshot, sent, request_url, final_url) in &rows {
                update.execute(params![
                    id,
                    snapshot.as_deref().map(|json| scrub.json(json)),
                    sent.as_deref().map(|json| scrub.json(json)),
                    request_url.replace(value, replacement),
                    final_url.replace(value, replacement),
                ])?;
            }
            Ok(rows.len() as u64)
        })
    }

    /// Prunes one workspace's rows of one source to its budget; a cheap row count first skips
    /// the work while the source is within it. Cached: it runs after every insert.
    fn prune_source(
        &self,
        connection: &Connection,
        workspace_id: &str,
        source: &str,
    ) -> Result<(), StorageError> {
        let keep = if source == HistorySource::Mock.as_str() {
            HISTORY_MOCK_RETENTION
        } else {
            self.history_retention
        };
        if source_rows(connection, workspace_id, source)? <= keep as u64 {
            return Ok(());
        }
        if RUN_BUCKETS.iter().any(|bucket| bucket.as_str() == source) {
            let mut delete = connection.prepare_cached(
                "DELETE FROM execution_history
                 WHERE workspace_id = ?1 AND source = ?2 AND COALESCE(run_id, id) = ?3",
            )?;
            for (run_key, _) in evicted_runs(connection, workspace_id, source, keep)? {
                delete.execute(params![workspace_id, source, run_key])?;
            }
        } else {
            connection
                .prepare_cached(
                    "DELETE FROM execution_history
                     WHERE workspace_id = ?1 AND source = ?2
                       AND id NOT IN (
                           SELECT id FROM execution_history
                           WHERE workspace_id = ?1 AND source = ?2
                           ORDER BY executed_at DESC, id DESC
                           LIMIT ?3
                       )",
                )?
                .execute(params![workspace_id, source, i64_from_usize(keep)])?;
        }
        Ok(())
    }
}

/// The columns a scrub rewrites: id, request snapshot, sent request, request URL, final URL.
type ScrubRow = (String, Option<String>, Option<String>, String, String);

/// Rows of one workspace and source (served by the `workspace, source` index).
fn source_rows(
    connection: &Connection,
    workspace_id: &str,
    source: &str,
) -> Result<u64, StorageError> {
    let count: i64 = connection
        .prepare_cached(
            "SELECT COUNT(*) FROM execution_history WHERE workspace_id = ?1 AND source = ?2",
        )?
        .query_row(params![workspace_id, source], |row| row.get(0))?;
    Ok(to_u64(count))
}

/// The runs of a run-counted source beyond `keep` rows, with their row counts.
fn evicted_runs(
    connection: &Connection,
    workspace_id: &str,
    source: &str,
    keep: usize,
) -> Result<Vec<(String, u64)>, StorageError> {
    let mut statement = connection.prepare_cached(EVICTED_RUNS_SQL)?;
    let rows = statement.query_map(params![workspace_id, source, i64_from_usize(keep)], |row| {
        Ok((row.get::<_, String>(0)?, to_u64(row.get::<_, i64>(1)?)))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// `json` with every occurrence of `value` inside its string values replaced by `replacement`.
/// One value to rewrite in history rows, plain and as it appears inside a JSON string literal.
struct Scrub<'a> {
    value: &'a str,
    replacement: &'a str,
    needle: &'a str,
    encoded_replacement: &'a str,
}

impl Scrub<'_> {
    /// `json` with the value rewritten in its string values; raw text that does not parse gets
    /// the JSON-escaped value replaced instead.
    fn json(&self, json: &str) -> String {
        match serde_json::from_str::<serde_json::Value>(json) {
            Ok(mut node) => {
                scrub_json_strings(&mut node, self.value, self.replacement);
                serde_json::to_string(&node)
                    .unwrap_or_else(|_| json.replace(self.needle, self.encoded_replacement))
            }
            Err(_) => json.replace(self.needle, self.encoded_replacement),
        }
    }
}

fn scrub_json_strings(node: &mut serde_json::Value, value: &str, replacement: &str) {
    fn walk(node: &mut serde_json::Value, value: &str, replacement: &str) {
        match node {
            serde_json::Value::String(text) if text.contains(value) => {
                *text = text.replace(value, replacement);
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, value, replacement);
                }
            }
            serde_json::Value::Object(fields) => {
                for field in fields.values_mut() {
                    walk(field, value, replacement);
                }
            }
            _ => {}
        }
    }
    walk(node, value, replacement);
}

/// Re-applies the live-row size limits to an entry the webview sent back: response body preview,
/// masked and capped response headers, sent request, and drops a request snapshot or call chain
/// that exceeds its stored limit.
fn bound_restored_entry(entry: &mut ExecutionHistoryEntry) {
    if entry.response_body_preview.len() > HISTORY_BODY_PREVIEW_LIMIT_BYTES {
        entry.response_body_preview = bounded_body_preview(
            &entry.response_body_preview,
            HISTORY_BODY_PREVIEW_LIMIT_BYTES,
        )
        .0;
        entry.response_body_truncated = true;
    }
    let (headers, cut) = capped_masked_headers(&entry.response_headers);
    entry.response_headers = headers;
    entry.response_headers_truncated |= cut;
    if let Some(sent) = entry.sent_request.as_mut() {
        sent.headers = capped_masked_headers(&sent.headers).0;
        if sent.body_preview.len() > HISTORY_BODY_PREVIEW_LIMIT_BYTES {
            sent.body_preview =
                bounded_body_preview(&sent.body_preview, HISTORY_BODY_PREVIEW_LIMIT_BYTES).0;
        }
    }
    if entry.request_snapshot.as_ref().is_some_and(|snapshot| {
        !serde_json::to_string(snapshot)
            .is_ok_and(|json| json.len() <= HISTORY_REQUEST_SNAPSHOT_LIMIT_BYTES)
    }) {
        entry.request_snapshot = None;
    }
    if entry.call_chain.as_ref().is_some_and(|chain| {
        !serde_json::to_string(chain).is_ok_and(|json| json.len() <= HISTORY_CALL_CHAIN_LIMIT_BYTES)
    }) {
        entry.call_chain = None;
    }
}

/// Inserts `entry` as `mode` says; returns the rows inserted. Cached: every executed request
/// comes through here.
fn insert_row(
    connection: &Connection,
    mode: Insert,
    entry: &ExecutionHistoryEntry,
) -> Result<usize, StorageError> {
    let snapshot = entry
        .request_snapshot
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let timing = entry
        .timing
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let headers = (!entry.response_headers.is_empty())
        .then(|| serde_json::to_string(&entry.response_headers))
        .transpose()?;
    let sent_request = entry
        .sent_request
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let call_chain = entry
        .call_chain
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    Ok(connection.prepare_cached(mode.sql())?.execute(params![
        entry.id.as_str(),
        entry.workspace_id.as_str(),
        entry.request_id.as_ref().map(RequestId::as_str),
        entry.environment_id.as_ref().map(EnvironmentId::as_str),
        execution_history_result_kind_to_storage(&entry.result_kind),
        entry.request_name,
        http_method_to_storage(entry.method),
        entry.request_url,
        i64::from(entry.status_code),
        entry.status_text,
        entry.final_url,
        i64_from_u128(entry.elapsed_ms),
        i64_from_usize(entry.body_size_bytes),
        i64::from(entry.attempt_count.max(1)),
        entry.response_body_preview,
        entry.response_body_truncated,
        entry.error_kind,
        entry.error_message,
        entry.executed_at,
        snapshot,
        entry.source.as_str(),
        entry.source_name,
        entry.source_id,
        entry.run_id,
        entry.protocol,
        timing,
        entry.environment_name,
        headers,
        entry.response_headers_truncated,
        sent_request,
        call_chain,
        entry.connect_phase,
        entry.check_summary.map(|summary| i64::from(summary.passed)),
        entry.check_summary.map(|summary| i64::from(summary.failed)),
        entry.origin_ref.as_ref().map(HistoryOriginRef::to_storage),
    ])?)
}
