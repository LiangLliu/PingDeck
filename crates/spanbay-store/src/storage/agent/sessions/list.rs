//! The chats list: one filtered page of session summaries, pinned first and then newest first,
//! paged by a keyset cursor.

use rusqlite::params;

use spanbay_model::agent::{AgentSessionFilter, AgentSessionPage, AgentSessionSummary};

use crate::sqlite::escape_like;
use crate::storage::agent::invalid_data;
use crate::storage::agent::runs::ACTIVE_RUN_STATUSES;
use crate::storage::agent::search::MIN_FTS_CHARS;
use crate::storage::{StorageError, WorkspaceStore};

/// Preview length of a session summary, in characters.
const PREVIEW_CHARS: usize = 160;
/// Rows per page when the caller names no limit.
const DEFAULT_PAGE: u32 = 50;
/// Most rows per page.
const MAX_PAGE: u32 = 200;

/// Where the next list page starts: after the row with this pin rank (0 pinned, 1 not), change
/// time and id in list order.
struct ListCursor {
    rank: i64,
    updated_at_ms: i64,
    id: String,
}

impl ListCursor {
    fn encode(&self) -> String {
        format!("{}:{}:{}", self.rank, self.updated_at_ms, self.id)
    }

    fn decode(text: &str) -> Result<Self, StorageError> {
        let malformed = || invalid_data(format!("malformed session list cursor {text:?}"));
        // The id is last and may itself contain `:`, so split at most twice.
        let mut parts = text.splitn(3, ':');
        let (Some(rank), Some(updated), Some(id)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(malformed());
        };
        Ok(Self {
            rank: rank.parse().map_err(|_| malformed())?,
            updated_at_ms: updated.parse().map_err(|_| malformed())?,
            id: id.to_owned(),
        })
    }
}

/// How a text query narrows the list.
struct TextFilter {
    /// The `LIKE` pattern of the title match (`?1`); `None` without a query.
    title_pattern: Option<String>,
    /// The predicate that adds the chats whose messages match, appended to the title match.
    text_match: &'static str,
    /// The chats with a verified message match as a JSON array (`?8`), for a query the trigram
    /// index can answer.
    matching_chats: Option<String>,
}

/// The list query for a `text_match` predicate of [`TextFilter`].
fn list_sql(text_match: &str) -> String {
    format!(
        "
        SELECT s.id, s.title, s.target_json, s.updated_at_ms, s.pinned_at_ms IS NOT NULL,
               s.archived_at_ms IS NOT NULL, s.workspace_dir, s.parent_session_id,
               EXISTS (SELECT 1 FROM agent_runs r
                       WHERE r.session_id = s.id AND r.status IN {ACTIVE_RUN_STATUSES}),
               (SELECT COUNT(*) FROM agent_messages m WHERE m.session_id = s.id),
               COALESCE((SELECT m.search_text FROM agent_messages m
                         WHERE m.session_id = s.id AND m.search_text <> ''
                           AND m.role IN ('user', 'assistant')
                         ORDER BY m.seq DESC LIMIT 1), ''),
               s.spanbay_workspace_id,
               (SELECT name FROM workspaces WHERE id = s.spanbay_workspace_id),
               s.forked_from,
               COALESCE((SELECT SUM(fc.added) FROM agent_file_changes fc
                         WHERE fc.session_id = s.id AND fc.status IN ('pending', 'conflict')), 0),
               COALESCE((SELECT SUM(fc.removed) FROM agent_file_changes fc
                         WHERE fc.session_id = s.id AND fc.status IN ('pending', 'conflict')), 0)
        FROM agent_sessions s
        WHERE (?1 IS NULL OR s.title LIKE ?1 ESCAPE '\\' {text_match})
          AND (?2 = 0 OR s.pinned_at_ms IS NOT NULL)
          AND (?3 = 1 OR s.archived_at_ms IS NULL)
          AND (?4 IS NULL OR s.workspace_dir = ?4)
          AND (?5 = 0 OR EXISTS (SELECT 1 FROM agent_runs r
                                 WHERE r.session_id = s.id
                                   AND r.status IN {ACTIVE_RUN_STATUSES}))
          AND (?6 = 1 OR ?15 = 1 OR s.deleted_at_ms IS NULL)
          AND (?15 = 0 OR s.deleted_at_ms IS NOT NULL)
          AND s.parent_session_id IS ?7
          AND (?13 IS NULL OR s.spanbay_workspace_id = ?13)
          AND (?14 = 0 OR s.workspace_dir IS NULL)
          AND (?9 IS NULL OR
               (s.pinned_at_ms IS NULL, -s.updated_at_ms, s.id) > (?9, -?10, ?11))
        ORDER BY s.pinned_at_ms IS NULL, s.updated_at_ms DESC, s.id
        LIMIT ?12
        "
    )
}

/// A summary row of [`list_sql`] and its target column, still JSON.
fn summary_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(AgentSessionSummary, Option<String>)> {
    Ok((
        AgentSessionSummary {
            id: row.get(0)?,
            title: row.get(1)?,
            target: None,
            updated_at_ms: row.get(3)?,
            pinned: row.get(4)?,
            archived: row.get(5)?,
            workspace_dir: row.get(6)?,
            parent_session_id: row.get(7)?,
            running: row.get(8)?,
            message_count: row.get(9)?,
            preview: row
                .get::<_, String>(10)?
                .chars()
                .take(PREVIEW_CHARS)
                .collect(),
            spanbay_workspace_id: row.get(11)?,
            spanbay_workspace_name: row.get(12)?,
            forked_from: row.get(13)?,
            pending_added: row.get(14)?,
            pending_removed: row.get(15)?,
        },
        row.get::<_, Option<String>>(2)?,
    ))
}

/// Cuts the extra row `sessions` was fetched with (`limit + 1` rows) and returns the cursor of
/// the next page; `None` on the last page.
fn next_cursor(sessions: &mut Vec<AgentSessionSummary>, limit: u32) -> Option<String> {
    if sessions.len() <= limit as usize {
        return None;
    }
    sessions.truncate(limit as usize);
    sessions.last().map(|last| {
        ListCursor {
            rank: i64::from(!last.pinned),
            updated_at_ms: last.updated_at_ms,
            id: last.id.clone(),
        }
        .encode()
    })
}

impl WorkspaceStore {
    /// One page of session summaries, pinned first and then newest first.
    ///
    /// Tombstoned sessions, archived sessions and sub-agent sessions are left out unless the
    /// filter asks for them. `query` keeps sessions whose title or any message text contains it
    /// (three or more characters use the trigram index); `only_running` keeps sessions with an
    /// active run. `cursor` is a previous page's `next_cursor`; `limit` defaults to 50 and is
    /// capped at 200.
    pub fn list_agent_sessions(
        &self,
        filter: &AgentSessionFilter,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<AgentSessionPage, StorageError> {
        let limit = limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
        // A malformed cursor fails before any query runs.
        let cursor = cursor.map(ListCursor::decode).transpose()?;
        let query = filter
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty());
        let text = self.text_filter(query)?;
        let mut statement = self.connection.prepare_cached(&list_sql(text.text_match))?;
        // `?8` is bound even when the query is too short for the index: SQLite ignores a
        // numbered parameter the statement text does not mention.
        let rows = statement.query_map(
            params![
                text.title_pattern,
                filter.pinned_only,
                filter.include_archived,
                filter.workspace_dir,
                filter.only_running,
                filter.include_deleted,
                filter.parent_session_id,
                text.matching_chats,
                cursor.as_ref().map(|cursor| cursor.rank),
                cursor.as_ref().map(|cursor| cursor.updated_at_ms),
                cursor.as_ref().map(|cursor| cursor.id.clone()),
                i64::from(limit) + 1,
                filter.spanbay_workspace_id,
                filter.no_folder,
                filter.deleted_only,
            ],
            summary_from_row,
        )?;
        let mut sessions = Vec::new();
        for row in rows {
            let (mut summary, target_json) = row?;
            summary.target = target_json
                .map(|json| serde_json::from_str(&json))
                .transpose()?;
            sessions.push(summary);
        }
        let next_cursor = next_cursor(&mut sessions, limit);
        Ok(AgentSessionPage {
            sessions,
            next_cursor,
        })
    }

    /// The [`TextFilter`] of a trimmed, non-empty `query`; matches everything for `None`.
    fn text_filter(&self, query: Option<&str>) -> Result<TextFilter, StorageError> {
        let Some(query) = query else {
            return Ok(TextFilter {
                title_pattern: None,
                text_match: "",
                matching_chats: None,
            });
        };
        let title_pattern = Some(format!("%{}%", escape_like(query)));
        let indexed = query.chars().count() >= MIN_FTS_CHARS;
        let text_match = if indexed {
            "OR s.id IN (SELECT value FROM json_each(?8))"
        } else {
            "OR EXISTS (SELECT 1 FROM agent_messages m WHERE m.session_id = s.id
                       AND m.search_text LIKE ?1 ESCAPE '\\')"
        };
        // The chats with a verified message match: the trigram index only narrows (see `search`).
        let matching_chats = if indexed {
            Some(serde_json::to_string(&self.chats_containing(query)?)?)
        } else {
            None
        };
        Ok(TextFilter {
            title_pattern,
            text_match,
            matching_chats,
        })
    }
}
