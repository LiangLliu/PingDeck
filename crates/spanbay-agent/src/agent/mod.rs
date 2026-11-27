//! The chat/agent harness: sessions, the tool-calling runner, MCP client, web search, skills.
//!
//! Single responsibility: everything an agent conversation needs that is not LLM transport
//! (`crate::llm`) or storage plumbing (`workspace::storage::agent`). The use-cases live in
//! `app::agent_service`.

pub mod approvals;
pub mod attachments;
pub mod attention;
pub mod compaction;
pub mod context;
pub mod dto;
pub mod failure;
pub mod fetch;
pub mod gate;
pub mod git_head;
pub mod instructions;
pub mod limits;
pub mod mcp;
pub use spanbay_model::agent as model;
pub mod permissions;
pub mod process_ledger;
pub mod prompt;
pub mod registry;
pub mod runner;
pub mod runtime;
pub mod sandbox;
pub mod search;
pub mod secret_guard;
pub mod skills;
#[cfg(test)]
pub(crate) mod test_dir;
pub mod tool_output_files;
pub mod tools;
pub mod untrusted;

/// A random, time-ordered entity id (`am_<hex>`), the shape every table expects.
///
/// The agent crate mints its own ids and must not depend on the app layer.
pub fn new_entity_id(prefix: &str) -> String {
    spanbay_base::id::prefixed_hex::<8>(prefix)
}
