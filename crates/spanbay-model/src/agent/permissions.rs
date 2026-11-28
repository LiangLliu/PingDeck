//! Permission rows and the approval card the UI renders.

use serde::{Deserialize, Serialize};

use crate::agent::{ApprovalScope, approvals::RuleContext, sandbox::SandboxStatus};

/// What a per-tool setting or a stored rule does to a matching call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionAction {
    /// Run without asking.
    Allow,
    /// Always ask first.
    Ask,
    /// Refuse the call.
    Block,
}

impl PermissionAction {
    /// The text stored in the database (`allow`, `ask`, `block`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Block => "block",
        }
    }

    /// The action stored as `text`, or `None` for an unknown value.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "block" => Some(Self::Block),
            _ => None,
        }
    }
}

/// A saved permission rule: a narrow grant created from the approval menu ("this chat", "this
/// folder"), or a legacy block/ask row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    /// Unique rule id.
    pub id: String,
    /// The chat the rule is limited to ("this chat").
    pub session_id: Option<String>,
    /// The folder the rule is limited to ("this folder").
    pub workspace_dir: Option<String>,
    /// The tool's exact name (or a glob with `*` for a legacy row).
    pub tool_pattern: String,
    /// What the rule matches in the call's argument subject: an argv prefix for `shell`
    /// (`cargo test *`, see [`derive_command_rule`]), a folder glob for file writes, a host for
    /// `fetch_url`, `METHOD host` for `send_request`, the provider for `run_probe`; `None` only for
    /// a trusted-server MCP tool in one chat.
    pub arg_pattern: Option<String>,
    /// What the rule does.
    pub action: PermissionAction,
    /// When the rule was created, Unix milliseconds.
    pub created_at_ms: i64,
}

/// How risky a command is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// A known read-only command.
    Low,
    /// Anything else.
    Medium,
    /// Deletes, overwrites, publishes or leaves the folder.
    High,
}

/// One chain, substitution or redirection operator of a command, with its position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorHit {
    /// The operator text (`&&`, `||`, `;`, `|`, `&`, `$(`, `` ` ``, `>`, `>>`, `<`, `<<`, or
    /// `\n` for a newline).
    pub op: String,
    /// Character offset of its first character in the command.
    pub start: usize,
}

impl OperatorHit {
    /// Byte length of the operator in the source command (`\n` is displayed as `\n` (2 chars)
    /// but occupies 1 byte in the raw command string).
    #[must_use]
    pub fn source_len(&self) -> usize {
        if self.op == "\\n" { 1 } else { self.op.len() }
    }
}

/// A short label the approval card shows next to a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskTag {
    /// The command chains, substitutes or redirects ("Chained command").
    ChainedCommand,
    /// The system has no usable sandbox ("No sandbox on this system").
    NoSandbox,
    /// The chat chose to run without the sandbox ("Sandbox off").
    SandboxOff,
}

/// Everything the approval card shows about a call, computed in the core. Carried by the
/// approval-required notification, so the board sees it without opening the chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalCard {
    /// Low, Medium or High.
    pub risk: RiskLevel,
    /// Why the risk is High ("deletes files").
    pub risk_reason: Option<String>,
    /// The tags to draw.
    pub tags: Vec<RiskTag>,
    /// The chain, substitution and redirection operators with their character positions in the
    /// command (empty for a tool that is not `shell`).
    pub operators: Vec<OperatorHit>,
    /// "Asks because: …": set when a forced Ask decided.
    pub why: Option<String>,
    /// "Saved rule not used: …": a stored rule matched but could not be used.
    pub rule_not_used: Option<String>,
    /// The sandbox self-test result.
    pub sandbox: SandboxStatus,
    /// One line for the board.
    pub summary: String,
    /// The scopes the approval menu offers; `Once` is always first.
    pub offered_scopes: Vec<ApprovalScope>,
    /// The rule a "this chat" or "this folder" answer would store (`cargo test *`, a host, a
    /// folder); `None` when the scope stores the tool itself or no scope is offered.
    pub rule_label: Option<String>,
    /// Why the "this chat" and "this folder" items are disabled, when they are.
    pub scope_disabled_reason: Option<String>,
    /// The decision layer that asked (`forced_ask`, `mode_default`, …), for "Why you're asked".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// A stable code of a forced Ask's reason (`mcp_untrusted`, `url_provenance`,
    /// `protected_path`, …); the UI words it, else it shows `why`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_code: Option<String>,
}

impl ApprovalCard {
    /// What the answer needs to leave a rule behind: the offered scopes and the pattern.
    #[must_use]
    pub fn rule_context(
        &self,
        session_id: &str,
        workspace_dir: Option<&str>,
        tool: &str,
    ) -> RuleContext {
        RuleContext {
            session_id: session_id.to_owned(),
            workspace_dir: workspace_dir.map(str::to_owned),
            tool: tool.to_owned(),
            pattern: self.rule_label.clone(),
            offered: self.offered_scopes.clone(),
        }
    }
}
