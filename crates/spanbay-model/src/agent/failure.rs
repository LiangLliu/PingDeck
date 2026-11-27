//! A run's structured failure: the class, and every target that failed with its reason.

use serde::{Deserialize, Serialize};

/// Which row of the failure table a run ended in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentFailureKind {
    /// Every target the model name routes to failed.
    AllTargetsFailed,
    /// The provider rejected the key and no other key or target was left.
    KeyRejected,
    /// The provider refused or answered with nothing.
    RefusedOrEmpty,
    /// The stream stopped talking and a retry did not help.
    Stalled,
    /// An MCP server asked for credentials.
    McpNeedsToken,
    /// The request does not fit the model's context window and shortening did not help.
    ContextFull,
    /// The machine or the provider could not be reached at all.
    Offline,
}

/// One target that failed and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentFailureTarget {
    /// The model name or `provider/model` the chat called.
    pub target: String,
    /// The error text (secrets already redacted by the transport).
    pub reason: String,
}

/// A run's structured failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentFailure {
    /// The class.
    pub kind: AgentFailureKind,
    /// The targets that failed with their reasons, one per attempt in order; a single entry for
    /// the model label when the client reported no attempt list (a stream that broke mid-way).
    pub targets: Vec<AgentFailureTarget>,
}
