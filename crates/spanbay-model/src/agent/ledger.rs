//! Rows of the process ledger: one process group the core started.

use serde::{Deserialize, Serialize};

/// What a recorded group was running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerKind {
    /// A `shell` tool call, in the foreground or in the background.
    Tool,
    /// An MCP stdio server.
    Mcp,
}

impl LedgerKind {
    /// The text stored in the table.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Mcp => "mcp",
        }
    }

    /// The kind stored as `text`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "tool" => Some(Self::Tool),
            "mcp" => Some(Self::Mcp),
            _ => None,
        }
    }
}

/// One recorded process group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// The group leader's process id.
    pub pid: u32,
    /// The process group id (the leader's own id for every group the core starts).
    pub pgid: u32,
    /// The leader's start time as [`process_start_time`] reads it.
    pub started: String,
    /// What the group was running.
    pub kind: LedgerKind,
    /// A label for diagnostics (the call id or the MCP server id).
    pub label: String,
}
