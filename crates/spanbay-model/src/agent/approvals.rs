//! What an approval answer needs to leave a rule behind.

use crate::agent::ApprovalScope;

/// What a saved permission rule for the answer's scope is made from. Built by
/// [`ApprovalCard::rule_context`](crate::agent::permissions::ApprovalCard::rule_context), so the scopes
/// the card offers and the rule an answer stores come from one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleContext {
    /// The chat of the run.
    pub session_id: String,
    /// The chat's folder.
    pub workspace_dir: Option<String>,
    /// The tool's function name.
    pub tool: String,
    /// The narrow rule a chat or folder scope stores (`cargo test *`, a host, a folder glob);
    /// `None` stores the tool itself (a trusted MCP tool) or nothing.
    pub pattern: Option<String>,
    /// The scopes the card offered; an answer outside them is refused.
    pub offered: Vec<ApprovalScope>,
}
