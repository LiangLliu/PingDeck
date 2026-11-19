//! The capture policy: which log rows get a capture.

use serde::{Deserialize, Serialize};

use super::CaptureMode;

/// Where a log row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSource {
    /// A client called the local gateway.
    Gateway,
    /// An AI Chat run.
    Agent,
    /// A test bench or sniffer request.
    Probe,
    /// An AI API mode request from the API client.
    Api,
}

/// The `outcome` column of a log row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogOutcome {
    /// The call finished normally.
    Success,
    /// The call failed.
    Failure,
    /// The caller went away.
    Canceled,
}

/// Whether a capture row is written for an exchange.
///
/// An `api` row is captured when the request's "Keep this exchange in the log" option
/// (`keep_api_exchange`, default on) is on, under every policy including `Off`. For the other
/// sources: `Off` never, `All` always, `ErrorsOnly` when `status >= 400`, the outcome is not
/// success, or any attempt failed.
#[must_use]
pub fn should_capture(
    mode: CaptureMode,
    source: CaptureSource,
    status: u16,
    outcome: LogOutcome,
    any_attempt_failed: bool,
    keep_api_exchange: bool,
) -> bool {
    if source == CaptureSource::Api {
        return keep_api_exchange;
    }
    match mode {
        CaptureMode::Off => false,
        CaptureMode::All => true,
        CaptureMode::ErrorsOnly => {
            status >= 400 || outcome != LogOutcome::Success || any_attempt_failed
        }
    }
}
