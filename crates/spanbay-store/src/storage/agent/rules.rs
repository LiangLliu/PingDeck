//! Permission rules (`agent_permission_rules`): per-tool Allow / Ask / Block decisions.

use std::path::Path;

use rusqlite::params;

use spanbay_model::agent::permissions::{PermissionAction, PermissionRule};
use spanbay_model::variables::uuid_v4;

use spanbay_base::time::now_ms;

use super::super::{StorageError, WorkspaceStore};
use super::{invalid, invalid_data, require_session};

/// Longest tool pattern, in bytes.
const MAX_TOOL_PATTERN: usize = 200;
/// Longest argument pattern, in bytes.
const MAX_ARG_PATTERN: usize = 1024;
/// Longest workspace folder, in bytes.
const MAX_WORKSPACE_DIR: usize = 4096;

/// First words a stored `shell` rule may never start with: a prefix that is a shell or an
/// interpreter alone would allow any program.
const INTERPRETERS: [&str; 11] = [
    "sh", "bash", "zsh", "fish", "dash", "ksh", "env", "xargs", "node", "ruby", "perl",
];
/// Most words of a stored `shell` prefix, as the approval card shows them.
const MAX_SHELL_PREFIX_WORDS: usize = 3;

/// Whether `word` (a command's first word, any path) names a shell or an interpreter.
fn is_interpreter(word: &str) -> bool {
    let name = word.rsplit('/').next().unwrap_or(word);
    INTERPRETERS.contains(&name) || name.starts_with("python")
}

/// A `shell` rule's argv prefix: one to three plain words, no flag, no shell syntax, not a shell
/// or an interpreter alone; a trailing lone `*` means "and any arguments".
fn validate_shell_prefix(pattern: &str) -> Result<(), StorageError> {
    let words: Vec<&str> = pattern.split_whitespace().collect();
    let words = match words.split_last() {
        Some((&"*", rest)) => rest,
        _ => &words[..],
    };
    let bad = words.is_empty()
        || words.len() > MAX_SHELL_PREFIX_WORDS
        || words.iter().any(|word| {
            word.starts_with('-')
                || word.contains('*')
                || word.chars().any(|c| ";&|`$()<>\\\"'".contains(c))
        })
        || is_interpreter(words[0]);
    if bad {
        return Err(invalid_data(
            "a shell rule is one to three plain words (no flags, no shell syntax) that do not \
             start a shell or an interpreter",
        ));
    }
    Ok(())
}

/// What an Allow rule must look like: it names one tool, never a wildcard; an MCP tool
/// is allowed for a chat only; `create_request` is never stored; `run_probe` is chat-scoped;
/// every other tool needs a narrow argument pattern (allowing a whole tool is the per-tool
/// setting, which only read-only tools offer).
fn validate_allow(rule: &PermissionRule) -> Result<(), StorageError> {
    let tool = rule.tool_pattern.as_str();
    if tool.contains('*') {
        return Err(invalid_data("an allow rule names one tool, not a pattern"));
    }
    if tool == "create_request" {
        return Err(invalid_data(
            "create_request is approved once, never stored",
        ));
    }
    if (tool == "run_probe" || tool.starts_with("mcp__")) && rule.session_id.is_none() {
        return Err(invalid_data("this tool can only be allowed for one chat"));
    }
    if tool.starts_with("mcp__") {
        return Ok(());
    }
    let Some(pattern) = rule.arg_pattern.as_deref() else {
        return Err(invalid_data(
            "an allow rule needs a narrow argument pattern; allowing a whole tool is the \
             per-tool setting of a read-only tool",
        ));
    };
    if tool == "shell" {
        validate_shell_prefix(pattern)?;
    }
    Ok(())
}

/// Rejects a rule that could never match, or whose fields are not safe to store. Tool names are
/// `[A-Za-z0-9_.:-]` with `*` as the only wildcard; an argument pattern is any printable text.
/// A rule belongs to a chat or a folder (per-tool settings replace global rules), and an Allow
/// rule is narrow ([`validate_allow`]).
fn validate_rule(rule: &PermissionRule) -> Result<(), StorageError> {
    if rule.session_id.is_none() && rule.workspace_dir.is_none() {
        return Err(invalid_data(
            "a rule belongs to a chat or a folder; per-tool settings replace global rules",
        ));
    }
    if rule.action == PermissionAction::Allow {
        validate_allow(rule)?;
    }
    let tool = &rule.tool_pattern;
    if tool.is_empty() || tool.len() > MAX_TOOL_PATTERN {
        return Err(invalid_data(format!(
            "tool_pattern must be 1 to {MAX_TOOL_PATTERN} characters"
        )));
    }
    if !tool
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-' | '*'))
    {
        return Err(invalid_data(
            "tool_pattern may only hold letters, digits, _ . : - and the * wildcard",
        ));
    }
    if let Some(pattern) = &rule.arg_pattern
        && (pattern.is_empty()
            || pattern.len() > MAX_ARG_PATTERN
            || pattern.chars().any(char::is_control))
    {
        return Err(invalid_data(format!(
            "arg_pattern must be 1 to {MAX_ARG_PATTERN} printable characters"
        )));
    }
    if let Some(dir) = &rule.workspace_dir
        && (dir.len() > MAX_WORKSPACE_DIR || dir.contains('\0') || !Path::new(dir).is_absolute())
    {
        return Err(invalid_data("workspace_dir must be an absolute path"));
    }
    Ok(())
}

impl WorkspaceStore {
    /// The rules that can apply to `session_id`: every global rule, every workspace rule and,
    /// when given, that session's rules; oldest first.
    pub fn list_permission_rules(
        &self,
        session_id: Option<&str>,
    ) -> Result<Vec<PermissionRule>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "
            SELECT id, session_id, workspace_dir, tool_pattern, arg_pattern, action, created_at_ms
            FROM agent_permission_rules
            WHERE session_id IS NULL OR session_id = ?1
            ORDER BY created_at_ms, id
            ",
        )?;
        let rows = statement.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;
        let mut rules = Vec::new();
        for row in rows {
            let (id, session_id, workspace_dir, tool_pattern, arg_pattern, action, created_at_ms) =
                row?;
            rules.push(PermissionRule {
                id,
                session_id,
                workspace_dir,
                tool_pattern,
                arg_pattern,
                action: PermissionAction::parse(&action)
                    .ok_or_else(|| invalid("action", action))?,
                created_at_ms,
            });
        }
        Ok(rules)
    }

    /// Validates and upserts a rule, returning what is stored. A blank `id` gets a fresh one and
    /// a `created_at_ms` of 0 becomes now; an update keeps the original creation time and may
    /// change everything else. A rule for a session that does not exist is invalid.
    pub fn save_permission_rule(
        &self,
        rule: &PermissionRule,
    ) -> Result<PermissionRule, StorageError> {
        validate_rule(rule)?;
        if let Some(session_id) = &rule.session_id {
            require_session(&self.connection, session_id)?;
        }
        let id = if rule.id.trim().is_empty() {
            format!("perm_{}", uuid_v4())
        } else {
            rule.id.clone()
        };
        // RETURNING: an update keeps the stored creation time, which may differ from the input.
        let created_at_ms = self.connection.query_row(
            "
            INSERT INTO agent_permission_rules (
                id, session_id, workspace_dir, tool_pattern, arg_pattern, action, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                session_id = excluded.session_id,
                workspace_dir = excluded.workspace_dir,
                tool_pattern = excluded.tool_pattern,
                arg_pattern = excluded.arg_pattern,
                action = excluded.action
            RETURNING created_at_ms
            ",
            params![
                id,
                rule.session_id,
                rule.workspace_dir,
                rule.tool_pattern,
                rule.arg_pattern,
                rule.action.as_str(),
                if rule.created_at_ms == 0 {
                    now_ms()
                } else {
                    rule.created_at_ms
                },
            ],
            |row| row.get(0),
        )?;
        Ok(PermissionRule {
            id,
            created_at_ms,
            ..rule.clone()
        })
    }

    /// Deletes a rule; returns whether it existed.
    pub fn delete_permission_rule(&self, id: &str) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "DELETE FROM agent_permission_rules WHERE id = ?1",
            params![id],
        )? > 0)
    }
}
