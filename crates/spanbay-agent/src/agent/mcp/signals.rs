//! What an MCP server tells the client without being asked.
//!
//! Single responsibility: the notifications both transports hand over, kept where a request and
//! the manager can read them: `notifications/tools/list_changed` marks the cached tool list stale,
//! and `notifications/progress` for a request that asked for progress (its `progressToken` is the
//! request id) restarts that request's timeout. Progress for a token no request is waiting on is
//! ignored, so a server cannot grow the table.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::Value;

use super::lock;

/// The notifications of one connection.
#[derive(Debug, Default)]
pub(super) struct Signals {
    /// `notifications/tools/list_changed` arrived since the last `tools/list` began.
    tools_changed: AtomicBool,
    /// The last progress of each request waiting with a progress token, by request id.
    progress: Mutex<HashMap<u64, Instant>>,
}

impl Signals {
    /// Takes one server-to-client notification (a message with a `method` and no `id`).
    pub(super) fn notice(&self, message: &Value) {
        match message.get("method").and_then(Value::as_str) {
            Some("notifications/tools/list_changed") => {
                self.tools_changed.store(true, Ordering::SeqCst);
            }
            Some("notifications/progress") => {
                let token = message.pointer("/params/progressToken");
                // Our tokens are request ids; a server may echo them as a number or a string.
                let id = token.and_then(|token| {
                    token
                        .as_u64()
                        .or_else(|| token.as_str().and_then(|text| text.parse().ok()))
                });
                if let Some(id) = id
                    && let Some(last) = lock(&self.progress).get_mut(&id)
                {
                    *last = Instant::now();
                }
            }
            _ => {}
        }
    }

    /// Starts waiting for progress of request `id`; the returned guard stops it.
    pub(super) fn track(&self, id: u64) -> ProgressGuard<'_> {
        lock(&self.progress).insert(id, Instant::now());
        ProgressGuard { signals: self, id }
    }

    /// When request `id` last reported progress (or started waiting).
    pub(super) fn last_progress(&self, id: u64) -> Option<Instant> {
        lock(&self.progress).get(&id).copied()
    }

    /// Whether the server said its tool list changed, clearing the mark: the caller lists again.
    pub(super) fn take_tools_changed(&self) -> bool {
        self.tools_changed.swap(false, Ordering::SeqCst)
    }

    /// Whether the server said its tool list changed since the last `tools/list` began.
    pub(super) fn tools_changed(&self) -> bool {
        self.tools_changed.load(Ordering::SeqCst)
    }
}

/// Stops tracking the progress of one request when dropped.
pub(super) struct ProgressGuard<'a> {
    signals: &'a Signals,
    id: u64,
}

impl Drop for ProgressGuard<'_> {
    fn drop(&mut self) {
        lock(&self.signals.progress).remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn progress_counts_only_for_a_waiting_request_and_list_changes_are_taken_once() {
        let signals = Signals::default();
        // Nobody waits on token 7: nothing is recorded.
        signals
            .notice(&json!({"method": "notifications/progress", "params": {"progressToken": 7}}));
        assert!(signals.last_progress(7).is_none());
        let guard = signals.track(7);
        let started = signals.last_progress(7).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        signals
            .notice(&json!({"method": "notifications/progress", "params": {"progressToken": "7"}}));
        assert!(signals.last_progress(7).unwrap() > started);
        drop(guard);
        assert!(signals.last_progress(7).is_none());

        assert!(!signals.take_tools_changed());
        signals.notice(&json!({"method": "notifications/tools/list_changed"}));
        assert!(signals.tools_changed());
        assert!(signals.take_tools_changed());
        assert!(!signals.tools_changed());
    }
}
