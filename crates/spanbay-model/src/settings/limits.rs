//! Retention limits of the gateway log and the response captures, and what a capture records.

use serde::{Deserialize, Serialize};

/// Request-log rows kept (not a setting).
pub const LOG_MAX_ROWS: u32 = 5_000;
/// Days request-log rows are kept (not a setting).
pub const LOG_MAX_DAYS: u16 = 30;
/// Days captured bodies are kept (not a setting).
pub const CAPTURE_MAX_DAYS: u16 = 7;
/// Bytes of captured bodies kept in total, oldest first (not a setting).
pub const CAPTURE_MAX_BYTES: u64 = 200 * 1024 * 1024;

/// Which gateway exchanges are captured (request and response bodies, redacted) for debugging.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Capture nothing.
    Off,
    /// Capture only exchanges that failed.
    #[default]
    ErrorsOnly,
    /// Capture every exchange.
    All,
}
