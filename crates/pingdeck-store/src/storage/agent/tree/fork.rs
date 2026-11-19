//! Branching a path of the message tree into a new chat.

use rusqlite::{Connection, params};

use pingdeck_model::agent::AgentSession;
use pingdeck_model::hub::HubUsage;
use pingdeck_model::variables::uuid_v4;

use super::{restore_plan, set_head_row};
use crate::sqlite::i64_from_usize;
use crate::storage::agent::invalid_data;
use crate::storage::agent::sessions::{load_session, upsert_session};
use crate::storage::{StorageError, WorkspaceStore};

/// `(id, seq, compacted_through_seq)` of each message from the root of `source_id` through
/// `through_id`, oldest first; empty when `through_id` is not a message of `source_id`.
fn ancestor_path(
    connection: &Connection,
    source_id: &str,
    through_id: &str,
) -> Result<Vec<(String, i64, Option<i64>)>, StorageError> {
    connection
        .prepare_cached(
            "WITH RECURSIVE anc(id, parent_id) AS (
                 SELECT id, parent_id FROM agent_messages WHERE id = ?2 AND session_id = ?1
                 UNION ALL
                 SELECT m.id, m.parent_id FROM agent_messages m JOIN anc ON m.id = anc.parent_id
                 WHERE m.session_id = ?1
             ) SELECT m.id, m.seq, m.compacted_through_seq
               FROM agent_messages m JOIN anc ON anc.id = m.id ORDER BY m.seq",
        )?
        .query_map(params![source_id, through_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

/// The session row a branch of `source_id` through `through_id` is stored as: `branch` with its
/// lineage set and the head, tombstone, sub-agent links, task list, usage and own instructions
/// reset.
fn branch_template(source_id: &str, through_id: &str, branch: &AgentSession) -> AgentSession {
    AgentSession {
        forked_from: Some(source_id.to_owned()),
        forked_from_message_id: Some(through_id.to_owned()),
        head_message_id: None,
        deleted_at_ms: None,
        parent_session_id: None,
        parent_run_id: None,
        todos: Vec::new(),
        total_usage: HubUsage::default(),
        total_cost_micro_usd: 0,
        last_context_tokens: 0,
        // A branch starts with the core's prompt alone (the same rule as Full access and the
        // plan): the source chat's own instructions stay with the source.
        instructions: None,
        ..branch.clone()
    }
}

/// One message of a path being copied into a branch session.
struct PathCopy<'a> {
    /// The message copied.
    old_id: &'a str,
    /// The id of the copy.
    new_id: &'a str,
    seq: i64,
    /// The copy of the previous message on the path; `None` for the root.
    parent: Option<&'a str>,
    /// The copy's `compacted_through_seq`.
    covered: Option<i64>,
}

/// Copies one message of a path into `branch_id` with its attachment links and branch plan.
/// The copy belongs to no run, has branch index 0 and forgets the remembered active child.
fn copy_path_message(
    connection: &Connection,
    branch_id: &str,
    copy: &PathCopy<'_>,
) -> Result<(), StorageError> {
    connection.execute(
        "
        INSERT INTO agent_messages (
            id, session_id, seq, role, parts_json, usage_json, model, error, search_text,
            created_at_ms, run_id, cost_micro_usd, cost_currency,
            parent_id, branch_index, partial, meta_json, compacted_through_seq
        )
        SELECT ?1, ?2, ?3, role, parts_json, usage_json, model, error, search_text,
               created_at_ms, NULL, cost_micro_usd, cost_currency,
               ?4, 0, partial, json_remove(meta_json, '$.active_child'), ?5
        FROM agent_messages WHERE id = ?6
        ",
        params![
            copy.new_id,
            branch_id,
            copy.seq,
            copy.parent,
            copy.covered,
            copy.old_id
        ],
    )?;
    connection.execute(
        "INSERT INTO agent_message_attachments (message_id, attachment_id, ord)
         SELECT ?1, attachment_id, ord FROM agent_message_attachments WHERE message_id = ?2",
        params![copy.new_id, copy.old_id],
    )?;
    connection.execute(
        "INSERT INTO agent_branch_plans (message_id, todos_json)
         SELECT ?1, todos_json FROM agent_branch_plans WHERE message_id = ?2",
        params![copy.new_id, copy.old_id],
    )?;
    Ok(())
}

impl WorkspaceStore {
    /// Branch into a new chat: stores `branch` as a new session (`forked_from` and
    /// `forked_from_message_id` are set here; the head, task list, usage and lineage fields are
    /// reset) holding a copy of the path from the root of `source_id` through
    /// `through_message_id`, in one transaction. Copies get new ids, consecutive `seq`, re-linked
    /// parents, branch index 0 and no run, together with their attachment links; the new
    /// session's head is the copy of `through_message_id`. Returns the stored session, or `None`
    /// when `through_message_id` is not a message of `source_id`; `branch.id` must be new.
    pub fn branch_session(
        &self,
        source_id: &str,
        through_message_id: &str,
        branch: &AgentSession,
    ) -> Result<Option<AgentSession>, StorageError> {
        self.atomic(|transaction| {
            let path = ancestor_path(transaction, source_id, through_message_id)?;
            if path.is_empty() {
                return Ok(None);
            }
            if load_session(transaction, &branch.id)?.is_some() {
                return Err(invalid_data(format!(
                    "chat session {} already exists",
                    branch.id
                )));
            }
            let branch = branch_template(source_id, through_message_id, branch);
            upsert_session(transaction, &branch)?;
            let new_ids: Vec<String> = path.iter().map(|_| format!("am_{}", uuid_v4())).collect();
            let old_seqs: Vec<i64> = path.iter().map(|(_, seq, _)| *seq).collect();
            for (index, (old_id, _, covered)) in path.iter().enumerate() {
                // The copy's compaction range ends at the copy of the last message the original's
                // ended at; `seq` is renumbered 1.., so that is the count of copies up to it.
                let covered = covered
                    .map(|covered| i64_from_usize(old_seqs.partition_point(|seq| *seq <= covered)))
                    .filter(|covered| *covered > 0);
                let copy = PathCopy {
                    old_id,
                    new_id: &new_ids[index],
                    seq: i64_from_usize(index + 1),
                    parent: index
                        .checked_sub(1)
                        .map(|previous| new_ids[previous].as_str()),
                    covered,
                };
                copy_path_message(transaction, &branch.id, &copy)?;
            }
            set_head_row(transaction, &branch.id, new_ids.last().map(String::as_str))?;
            restore_plan(transaction, &branch.id)?;
            load_session(transaction, &branch.id)
        })
    }
}
