//! The run limits table (PRD §6 "Run limits (normative)"): the one place
//! the numbers that bound a run are written in code.
//!
//! Single responsibility: constants. The registry, the process registry, the queue and the
//! Settings defaults all equal these.

use std::time::Duration;

/// Concurrent top-level runs (Settings range 1 to 8).
pub const MAX_CONCURRENT_RUNS: u8 = 3;
/// Sub-agents (`task` calls) per run (Settings range 1 to 8).
pub const MAX_SUBAGENTS: u8 = 3;
/// Steps per run (Settings range 1 to 100).
pub const MAX_STEPS: u32 = 20;
/// Approval wait of a run with no attached UI, in seconds (Settings range 60 to 3,600).
pub const APPROVAL_TIMEOUT_S: u32 = 600;
/// A model stream that sent no byte for this long after its first event has stalled; before the
/// first event the client's first-byte timeout and failover decide.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(90);
/// Times a model call that failed transiently before any content is made again.
pub const TRANSIENT_RETRIES: u32 = 2;
/// Identical tool calls with the same result in a row before the model is told it is going in
/// circles.
pub const REPEAT_NOTICE_AFTER: u32 = 3;
/// After the machine wakes, a stream that sent no byte for this long is retried.
pub const WAKE_STALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Auto-compact threshold, percent of the model's maximum input (Settings range 50 to 95).
pub const AUTO_COMPACT_PCT: u8 = 85;
/// Newest turns compaction keeps verbatim (Settings range 1 to 20).
pub const KEEP_RECENT_TURNS: u8 = 2;
/// Events a run's replay buffer holds.
pub const REPLAY_EVENTS: usize = 4_096;
/// Bytes a run's replay buffer holds.
pub const REPLAY_BYTES: usize = 1024 * 1024;
/// Running background processes per chat.
pub const PROCESSES_PER_CHAT: usize = 4;
/// Running background processes in the app.
pub const PROCESSES_TOTAL: usize = 8;
/// Longest life of a background process.
pub const PROCESS_LIFETIME: Duration = Duration::from_secs(8 * 60 * 60);
/// Messages a chat's queue holds (the store enforces it).
pub const QUEUE_MESSAGES: usize = 5;
/// Bytes of one tool call's full output kept on disk.
pub const TOOL_OUTPUT_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Bytes of full tool output kept per chat; its oldest files go first.
pub const TOOL_OUTPUT_CHAT_BYTES: u64 = 64 * 1024 * 1024;
/// Bytes of full tool output kept app-wide; the least recently used chat goes first.
pub const TOOL_OUTPUT_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Context window, in tokens, assumed for a model whose window is unknown: pruning and
/// compaction judge by it.
pub const UNKNOWN_WINDOW_TOKENS: u64 = 128_000;
/// `send_request` calls one run may make.
pub const SENDS_PER_RUN: u32 = 20;
/// `fetch_url` calls one run may make.
pub const FETCHES_PER_RUN: u32 = 40;
/// Bytes of a saved request's body `get_request` shows; the rest stays behind a `read_output`
/// handle.
pub const GET_REQUEST_BODY_BYTES: usize = 8 * 1024;
// Kept literal for `cargo xtask check-docs`; the store's cap must stay equal.
const _: () = assert!(QUEUE_MESSAGES == pingdeck_model::agent::MAX_QUEUED_TURNS);

#[cfg(test)]
mod tests {
    use super::*;
    use pingdeck_model::settings::AiAgentSettings;

    #[test]
    fn settings_defaults_match_run_limits() {
        let agent = AiAgentSettings::default();
        assert_eq!(agent.max_concurrent_runs, MAX_CONCURRENT_RUNS);
        assert_eq!(agent.max_subagents, MAX_SUBAGENTS);
        assert_eq!(agent.max_tool_iterations, MAX_STEPS);
        assert_eq!(agent.approval_timeout_s, APPROVAL_TIMEOUT_S);
        assert_eq!(agent.compact_threshold_pct, AUTO_COMPACT_PCT);
        assert_eq!(agent.keep_recent_turns, KEEP_RECENT_TURNS);
        // PRD "Tokens per run | off".
        assert_eq!(agent.max_run_tokens, None);
    }
}
