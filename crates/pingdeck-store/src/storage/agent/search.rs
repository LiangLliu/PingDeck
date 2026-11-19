//! Message search: across chats (one hit per chat, flagged when only a tool result matched), over
//! the chats list filter, and inside one chat.
//!
//! The index is a contentless trigram FTS5 table with `detail=none` (see the baseline DDL in
//! `storage/schema.rs`): small, but it can only answer "which rows contain all of these
//! three-character pieces". A query is therefore looked up as the AND of its trigrams and every
//! candidate is then verified against `search_text` with a case-insensitive substring test, so a
//! hit really contains the whole query. Queries of one or two characters scan with `LIKE` instead.

use std::collections::{HashMap, HashSet};

use rusqlite::params;

use pingdeck_model::agent::{AgentThreadHit, SessionSearchHit};

use super::super::{StorageError, WorkspaceStore};
use super::messages::{BOUNDARY_SQL, PATH_CTE};
use crate::sqlite::{escape_like, to_u32};

/// Shortest query, in characters, the trigram index can answer.
pub(super) const MIN_FTS_CHARS: usize = 3;
/// Most hits a chat search returns (the command palette shows 20).
const MAX_HITS: u32 = 20;
/// Most hits the in-chat find returns.
const MAX_THREAD_HITS: usize = 500;
/// Most distinct trigrams a lookup constrains: a longer query is still verified in full.
const MAX_TRIGRAMS: usize = 16;
/// Characters of context a snippet keeps on each side of the match.
const SNIPPET_CONTEXT: usize = 40;

/// One character in the form both sides of a comparison are folded to: its first lower-case
/// character (the index folds case the same simple way), so a text and a query keep their
/// character counts.
fn fold_char(character: char) -> char {
    character.to_lowercase().next().unwrap_or(character)
}

fn folded(text: &str) -> Vec<char> {
    text.chars().map(fold_char).collect()
}

/// The character index of the first occurrence of `needle` (already folded) in `text`.
fn find_folded(text: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > text.len() {
        return None;
    }
    text.windows(needle.len())
        .position(|window| window == needle)
}

/// The text around the match at `start` (`len` characters) with the match in `[` `]`, cut to
/// [`SNIPPET_CONTEXT`] characters each side and marked with `…` where text was dropped. Newlines
/// become spaces so a hit stays on one line.
fn snippet(text: &str, start: usize, len: usize) -> String {
    let characters: Vec<char> = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let from = start.saturating_sub(SNIPPET_CONTEXT);
    let to = (start + len + SNIPPET_CONTEXT).min(characters.len());
    let mut out = String::new();
    if from > 0 {
        out.push('…');
    }
    out.extend(&characters[from..start]);
    out.push('[');
    out.extend(&characters[start..(start + len).min(characters.len())]);
    out.push(']');
    out.extend(&characters[(start + len).min(characters.len())..to]);
    if to < characters.len() {
        out.push('…');
    }
    out
}

/// The FTS5 expression that selects every row containing all the trigrams of `query`: each
/// distinct three-character piece of the folded query as its own quoted token, joined by `AND`.
/// `None` for a query shorter than [`MIN_FTS_CHARS`].
fn fts_candidates(query: &[char]) -> Option<String> {
    if query.len() < MIN_FTS_CHARS {
        return None;
    }
    let mut seen: HashSet<&[char]> = HashSet::new();
    let tokens: Vec<String> = query
        .windows(MIN_FTS_CHARS)
        .filter(|window| seen.insert(*window))
        .take(MAX_TRIGRAMS)
        .map(|window| {
            let token: String = window.iter().collect();
            format!("\"{}\"", token.replace('"', "\"\""))
        })
        .collect();
    Some(tokens.join(" AND "))
}

/// One message that may contain the query: its row and text.
struct Candidate {
    session_id: String,
    message_id: String,
    title: String,
    created_at_ms: i64,
    session_updated_at_ms: i64,
    text: String,
    tool: bool,
}

impl WorkspaceStore {
    /// The live top-level chats' messages that may contain `query` (already trimmed, non-empty),
    /// newest first: trigram candidates for three or more characters, a `LIKE` scan otherwise.
    fn search_candidates(&self, query: &str) -> Result<Vec<Candidate>, StorageError> {
        let needle = folded(query);
        let row = |row: &rusqlite::Row<'_>| {
            Ok(Candidate {
                session_id: row.get(0)?,
                message_id: row.get(1)?,
                title: row.get(2)?,
                created_at_ms: row.get(3)?,
                session_updated_at_ms: row.get(4)?,
                text: row.get(5)?,
                tool: row.get(6)?,
            })
        };
        let select = "SELECT m.session_id, m.id, s.title, m.created_at_ms, s.updated_at_ms,
                             m.search_text, m.role = 'tool'
                      FROM agent_messages m JOIN agent_sessions s ON s.id = m.session_id";
        let order = "ORDER BY m.created_at_ms DESC, m.seq DESC";
        let live = "s.deleted_at_ms IS NULL AND s.parent_session_id IS NULL";
        let rows = if let Some(expression) = fts_candidates(&needle) {
            self.connection
                .prepare_cached(&format!(
                    "{select} JOIN agent_messages_fts f ON f.rowid = m.fts_id
                     WHERE agent_messages_fts MATCH ?1 AND {live} {order}"
                ))?
                .query_map(params![expression], row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            self.connection
                .prepare_cached(&format!(
                    "{select} WHERE m.search_text LIKE ?1 ESCAPE '\\' AND {live} {order}"
                ))?
                .query_map(params![format!("%{}%", escape_like(query))], row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    /// Searches the text of every live chat (archived ones included) and returns at most `limit`
    /// hits (capped at 20), one per chat, newest first. A chat's hit is its newest message whose
    /// prose matches; a chat whose only matches are inside tool results gets its newest tool
    /// message with `in_tool_output` set. Prose and tool results are searched as the index holds
    /// them: text in full, each tool result as its head and tail (4 KB), spilled output never.
    /// Queries of three or more characters (after trimming) use the trigram index, shorter ones
    /// scan; both match any substring case-insensitively. A blank query finds nothing.
    /// Tombstoned and sub-agent chats are not searched.
    pub fn search_agent_messages(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<SessionSearchHit>, StorageError> {
        let query = query.trim();
        let limit = limit.clamp(1, MAX_HITS) as usize;
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let needle = folded(query);
        // Per chat: the newest prose match and the newest tool match, in candidate order.
        let mut order: Vec<String> = Vec::new();
        let mut best: HashMap<String, (Option<SessionSearchHit>, Option<SessionSearchHit>)> =
            HashMap::new();
        for candidate in self.search_candidates(query)? {
            let text = folded(&candidate.text);
            let Some(start) = find_folded(&text, &needle) else {
                continue;
            };
            let hit = SessionSearchHit {
                snippet: snippet(&candidate.text, start, needle.len()),
                in_tool_output: candidate.tool,
                session_id: candidate.session_id.clone(),
                message_id: candidate.message_id,
                title: candidate.title,
                ts_ms: candidate.created_at_ms,
                updated_at_ms: candidate.session_updated_at_ms,
            };
            let entry = best.entry(candidate.session_id.clone()).or_insert_with(|| {
                order.push(candidate.session_id.clone());
                (None, None)
            });
            let slot = if candidate.tool {
                &mut entry.1
            } else {
                &mut entry.0
            };
            slot.get_or_insert(hit);
        }
        let mut hits: Vec<SessionSearchHit> = order
            .into_iter()
            .filter_map(|session| best.remove(&session))
            .filter_map(|(prose, tool)| prose.or(tool))
            .collect();
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.ts_ms));
        hits.truncate(limit);
        Ok(hits)
    }

    /// The ids of the chats with a message that contains `query` (the chat list filter), live
    /// top-level chats only; the caller applies the other filters. Verified like
    /// [`WorkspaceStore::search_agent_messages`].
    pub(super) fn chats_containing(&self, query: &str) -> Result<Vec<String>, StorageError> {
        let needle = folded(query);
        let mut ids: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for candidate in self.search_candidates(query)? {
            if seen.contains(&candidate.session_id) {
                continue;
            }
            if find_folded(&folded(&candidate.text), &needle).is_some() {
                seen.insert(candidate.session_id.clone());
                ids.push(candidate.session_id);
            }
        }
        Ok(ids)
    }

    /// Finds `query` in the active path of one chat, in path order, at most 500 hits: the
    /// message, its turn index and whether the match is inside a tool result, so the UI can open
    /// an unloaded turn with a page around it. A blank query finds nothing.
    pub fn find_in_session(
        &self,
        session_id: &str,
        query: &str,
    ) -> Result<Vec<AgentThreadHit>, StorageError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let needle = folded(query);
        let mut statement = self.connection.prepare_cached(&format!(
            "{PATH_CTE}
             SELECT m.id, m.seq, m.role = 'tool', {BOUNDARY_SQL}, m.search_text
             FROM agent_messages m JOIN path ON path.id = m.id
             ORDER BY m.seq"
        ))?;
        let rows = statement.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut hits = Vec::new();
        let mut turn: i64 = -1;
        for row in rows {
            let (message_id, seq, tool, boundary, text) = row?;
            if boundary || turn < 0 {
                turn += 1;
            }
            if hits.len() >= MAX_THREAD_HITS {
                break;
            }
            if let Some(start) = find_folded(&folded(&text), &needle) {
                hits.push(AgentThreadHit {
                    snippet: snippet(&text, start, needle.len()),
                    message_id,
                    seq,
                    turn_index: to_u32(turn),
                    in_tool_output: tool,
                });
            }
        }
        Ok(hits)
    }
}
