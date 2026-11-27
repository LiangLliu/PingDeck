//! Page enrichment of history rows: the collection path of each request and, for agent rows,
//! the chat title and turn. One query per page each, never per row.

use std::collections::{BTreeSet, HashMap};

use rusqlite::params;

use spanbay_model::models::{ExecutionHistoryEntry, HistorySource};

use crate::storage::{StorageError, WorkspaceStore};

/// A history entry's collection name and its folders as `(depth, name)`, innermost at depth 0.
#[derive(Default)]
struct EntryPath {
    collection: Option<String>,
    folders: Vec<(i64, String)>,
}

impl WorkspaceStore {
    /// Sets `collection_path` of every entry: the collection name and folder names above the
    /// request its snapshot names, outermost first, in one query for the whole page.
    ///
    /// The snapshot is read inside SQLite (`json_extract`) so list rows, which leave the blob
    /// out, resolve too. A collection that is gone (or a snapshot without one) leaves the path
    /// empty; a folder that is gone ends the chain at the collection.
    pub(super) fn fill_collection_paths(
        &self,
        workspace_id: &str,
        entries: &mut [ExecutionHistoryEntry],
    ) -> Result<(), StorageError> {
        if entries.is_empty() {
            return Ok(());
        }
        let ids_json = serde_json::to_string(
            &entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
        )?;
        let mut statement = self.connection.prepare_cached(
            "
            WITH RECURSIVE
            origin(entry_id, collection_id, folder_id) AS (
                SELECT id, json_extract(request_snapshot_json, '$.collection_id'),
                       json_extract(request_snapshot_json, '$.folder_id')
                FROM execution_history
                WHERE workspace_id = ?1 AND id IN (SELECT value FROM json_each(?2))
                  AND request_snapshot_json IS NOT NULL
            ),
            chain(entry_id, parent_id, name, depth) AS (
                SELECT o.entry_id, f.parent_folder_id, f.name, 0
                FROM origin o
                JOIN folders f ON f.id = o.folder_id AND f.collection_id = o.collection_id
                UNION ALL
                SELECT c.entry_id, f.parent_folder_id, f.name, c.depth + 1
                FROM chain c JOIN folders f ON f.id = c.parent_id
                WHERE c.depth < 64
            )
            SELECT o.entry_id, collection.name, NULL, -1
            FROM origin o
            JOIN collections collection
              ON collection.id = o.collection_id AND collection.workspace_id = ?1
            UNION ALL
            SELECT c.entry_id, NULL, c.name, c.depth FROM chain c
            ",
        )?;
        let rows = statement.query_map(params![workspace_id, ids_json], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut found: HashMap<String, EntryPath> = HashMap::new();
        for row in rows {
            let (entry_id, collection, folder, depth) = row?;
            let slot = found.entry(entry_id).or_default();
            if let Some(collection) = collection {
                slot.collection = Some(collection);
            }
            if let Some(folder) = folder {
                slot.folders.push((depth, folder));
            }
        }
        drop(statement);
        for entry in entries {
            let Some(EntryPath {
                collection: Some(collection),
                mut folders,
            }) = found.remove(entry.id.as_str())
            else {
                continue;
            };
            folders.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
            entry.collection_path = std::iter::once(collection)
                .chain(folders.into_iter().map(|(_, name)| name))
                .collect();
        }

        Ok(())
    }

    /// Sets `origin_label` of every `agent` entry to the title of its chat (`source_id`) and
    /// `origin_turn_id` to the first message of its run, one query each for the whole page; a
    /// deleted chat leaves both empty.
    pub(super) fn fill_origin_labels(
        &self,
        entries: &mut [ExecutionHistoryEntry],
    ) -> Result<(), StorageError> {
        let chats: BTreeSet<&str> = entries
            .iter()
            .filter(|entry| entry.source == HistorySource::Agent && !entry.source_id.is_empty())
            .map(|entry| entry.source_id.as_str())
            .collect();
        if chats.is_empty() {
            return Ok(());
        }
        let chats_json = serde_json::to_string(&chats)?;
        let mut statement = self.connection.prepare_cached(
            "SELECT id, title FROM agent_sessions
             WHERE id IN (SELECT value FROM json_each(?1)) AND deleted_at_ms IS NULL",
        )?;
        let titles = statement
            .query_map(params![chats_json], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        drop(statement);
        // The first message of each run is the turn "View in chat" reveals.
        let runs: BTreeSet<&str> = entries
            .iter()
            .filter(|entry| titles.contains_key(&entry.source_id))
            .filter_map(|entry| entry.run_id.as_deref())
            .collect();
        let mut turns = HashMap::new();
        if !runs.is_empty() {
            let runs_json = serde_json::to_string(&runs)?;
            let mut statement = self.connection.prepare_cached(
                "SELECT run_id, id, MIN(seq) FROM agent_messages
                 WHERE run_id IN (SELECT value FROM json_each(?1)) GROUP BY run_id",
            )?;
            turns = statement
                .query_map(params![runs_json], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<HashMap<_, _>, _>>()?;
        }
        for entry in entries
            .iter_mut()
            .filter(|entry| entry.source == HistorySource::Agent)
        {
            entry.origin_label = titles.get(&entry.source_id).cloned();
            entry.origin_turn_id = entry
                .origin_label
                .as_ref()
                .and(entry.run_id.as_ref())
                .and_then(|run| turns.get(run).cloned());
        }
        Ok(())
    }
}
