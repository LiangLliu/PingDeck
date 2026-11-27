//! Recording of proxied exchanges, so a session against the real upstream can become routes.
//!
//! Single responsibility: keep the whole response of every proxied request while recording is
//! on, one entry per method and path (the latest wins), bounded in count and bytes. Turning
//! recording into routes is the use-case's business; the recordings live in memory only and end
//! with the server.

use bytes::Bytes;

use super::http::ParsedRequest;
use super::logs::now_ms;
use super::reply::MockReply;

/// Most recordings a server keeps; the oldest leave first.
pub const MAX_RECORDINGS: usize = 200;

/// Most body bytes all recordings of a server hold together; the oldest leave first.
pub const MAX_RECORDING_BYTES: usize = 32 * 1024 * 1024;

/// One proxied exchange as it was relayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockRecording {
    /// The log row of the exchange.
    pub log_id: String,
    pub method: String,
    /// The request path as received (endpoint prefix included).
    pub path: String,
    pub query: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// The whole response body (the proxy refuses bodies above its cap).
    pub body: Bytes,
    pub recorded_at_ms: i64,
}

/// Whether recording is on and what it has kept.
#[derive(Debug, Default)]
pub(super) struct Recordings {
    pub(super) enabled: bool,
    entries: Vec<MockRecording>,
    bytes: usize,
}

impl Recordings {
    /// Switches recording; switching it on starts an empty session.
    pub(super) fn set_enabled(&mut self, enabled: bool) {
        if enabled && !self.enabled {
            self.entries.clear();
            self.bytes = 0;
        }
        self.enabled = enabled;
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Keeps the exchange of `request` and its proxied `reply` (logged as `log_id`) when
    /// recording is on, replacing an earlier one of the same method and path, then drops the
    /// oldest until both bounds hold.
    pub(super) fn record(&mut self, log_id: &str, request: &ParsedRequest, reply: &MockReply) {
        if !self.enabled {
            return;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.method == request.method && entry.path == request.path)
        {
            let replaced = self.entries.remove(index);
            self.bytes -= replaced.body.len();
        }
        self.bytes += reply.body.len();
        self.entries.push(MockRecording {
            log_id: log_id.to_owned(),
            method: request.method.clone(),
            path: request.path.clone(),
            query: request.query.clone(),
            status: reply.status,
            headers: reply.headers.clone(),
            body: reply.body.clone(),
            recorded_at_ms: now_ms(),
        });
        while self.entries.len() > MAX_RECORDINGS || self.bytes > MAX_RECORDING_BYTES {
            if self.entries.is_empty() {
                break;
            }
            let oldest = self.entries.remove(0);
            self.bytes -= oldest.body.len();
        }
    }

    /// Every recording, oldest first.
    pub(super) fn snapshot(&self) -> Vec<MockRecording> {
        self.entries.clone()
    }

    /// The recording made from the log row `log_id`.
    pub(super) fn by_log_id(&self, log_id: &str) -> Option<MockRecording> {
        self.entries
            .iter()
            .find(|entry| entry.log_id == log_id)
            .cloned()
    }
}
