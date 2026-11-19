//! Telling the model it is going in circles (first stage).
//!
//! Single responsibility: a small state over the settled tool calls of a run, no I/O. Three
//! signals: the same call with the same result [`REPEAT_NOTICE_AFTER`] times in a row, that many
//! failed calls in a row, and that many calls in a row the user denied. When one fires, the loop
//! appends a line to the end of that call's result: the history only grows at its end, so the
//! request prefix stays stable and the transcript valid. Polling a background process, paging a
//! saved output and rewriting the task list repeat on purpose and never count as repeats. Second
//! stage: a repeat streak that goes on [`STUCK_AFTER_NUDGE`] calls past its nudge marks the run
//! stuck, and the loop ends it as `NoProgress` once the step's results are stored.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::super::limits::REPEAT_NOTICE_AFTER;
use super::super::model::ToolCallStatus;

/// Tools that repeat the same call on purpose.
const REPEAT_EXEMPT: [&str; 3] = ["shell_output", "read_output", "todo_write"];
/// Calls past the repeat nudge after which the run is stuck (the 5th identical call).
const STUCK_AFTER_NUDGE: u32 = 2;
/// Bytes of a result that go into its fingerprint.
const RESULT_PREFIX: usize = 4 * 1024;

/// What fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    Repeat,
    Failures,
    Denials,
}

impl Signal {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Repeat => "repeat",
            Self::Failures => "failures",
            Self::Denials => "denials",
        }
    }
}

/// A fired signal and the line the model reads at the end of the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Nudge {
    pub signal: Signal,
    pub text: String,
}

/// One settled call as the probe sees it.
pub(crate) struct Observed<'a> {
    pub name: &'a str,
    pub arguments: &'a str,
    pub status: ToolCallStatus,
    /// The user answered the approval with Deny.
    pub denied_by_user: bool,
    pub result: &'a str,
}

/// The run's counters.
#[derive(Debug, Default)]
pub(crate) struct ProgressProbe {
    last_call: Option<u64>,
    repeats: u32,
    failures: u32,
    denials: u32,
    /// Lines appended so far in this run.
    pub nudges: u32,
    /// The same call kept its result [`STUCK_AFTER_NUDGE`] times after the repeat nudge.
    pub stuck: bool,
}

/// Sorts object keys recursively so canonical serialization is independent of
/// `serde_json`'s `preserve_order` feature unification across the workspace.
fn sort_json_keys(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, serde_json::Value)> = map
                .into_iter()
                .map(|(k, v)| (k, sort_json_keys(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            serde_json::Value::Object(entries.into_iter().collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sort_json_keys).collect())
        }
        other => other,
    }
}

/// The same arguments in any key order or spacing hash alike.
fn fingerprint(name: &str, arguments: &str, result: &str) -> u64 {
    let canonical = serde_json::from_str::<serde_json::Value>(arguments).map_or_else(
        |_| arguments.to_owned(),
        |value| sort_json_keys(value).to_string(),
    );
    let prefix = &result.as_bytes()[..result.len().min(RESULT_PREFIX)];
    let mut hasher = DefaultHasher::new();
    (name, canonical, prefix).hash(&mut hasher);
    hasher.finish()
}

/// Every [`REPEAT_NOTICE_AFTER`]-th time in a row.
const fn due(count: u32) -> bool {
    count > 0 && count.is_multiple_of(REPEAT_NOTICE_AFTER)
}

impl ProgressProbe {
    /// Counts one settled call; `Some` when this result should end with a nudge.
    pub(crate) fn observe(&mut self, call: &Observed<'_>) -> Option<Nudge> {
        let counted = !matches!(
            call.status,
            ToolCallStatus::Cancelled | ToolCallStatus::Superseded | ToolCallStatus::Expired
        );
        if !counted {
            return None;
        }
        self.denials = if call.denied_by_user {
            self.denials + 1
        } else {
            0
        };
        let failed = matches!(
            call.status,
            ToolCallStatus::Error | ToolCallStatus::Interrupted
        );
        self.failures = if failed { self.failures + 1 } else { 0 };
        if REPEAT_EXEMPT.contains(&call.name) {
            self.last_call = None;
            self.repeats = 0;
        } else {
            let print = fingerprint(call.name, call.arguments, call.result);
            self.repeats = if self.last_call == Some(print) {
                self.repeats + 1
            } else {
                1
            };
            self.last_call = Some(print);
        }
        if self.repeats >= REPEAT_NOTICE_AFTER + STUCK_AFTER_NUDGE {
            self.stuck = true;
        }
        let n = REPEAT_NOTICE_AFTER;
        let nudge = if due(self.denials) {
            Nudge {
                signal: Signal::Denials,
                text: format!(
                    "[System notice] The user denied {n} calls in a row. Stop and ask the user how to proceed."
                ),
            }
        } else if due(self.repeats) {
            Nudge {
                signal: Signal::Repeat,
                text: format!(
                    "[System notice] You called {} with the same arguments {n} times and got the same result. Change approach or tell the user what blocks you.",
                    call.name
                ),
            }
        } else if due(self.failures) {
            Nudge {
                signal: Signal::Failures,
                text: format!(
                    "[System notice] The last {n} tool calls failed. Change approach or tell the user what blocks you."
                ),
            }
        } else {
            return None;
        };
        self.nudges += 1;
        Some(nudge)
    }
}
