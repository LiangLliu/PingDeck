//! Telling the model it is going in circles (doom-loop protection).
//!
//! Single responsibility: a small state over the settled tool calls of a run, no I/O. Signals:
//! - exact repeat: the same call with the same result [`REPEAT_NOTICE_AFTER`] times in a row
//!   (nudge at 3, stuck at 5), or the same failing call with identical arguments (ignoring volatile
//!   timestamps/PIDs in stderr/stdout),
//! - oscillating multi-step cycle: period-2 (`A → B → A → B → A → B`) or period-3 (`A → B → C`
//!   repeated 3 times, stuck at 4),
//! - consecutive failures or user denials: nudge every [`REPEAT_NOTICE_AFTER`], hard-stop stuck
//!   circuit breaker at `2 * REPEAT_NOTICE_AFTER` (6 in a row).
//!
//! `todo_write` updates never reset repeat/cycle state (preventing an agent from evading loop
//! detection by rewriting todos between stuck retries). `shell_output` and `read_output` allow
//! normal polling but still catch dead-EOF polling loops after 6 identical calls in a row.

use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::super::limits::REPEAT_NOTICE_AFTER;
use super::super::model::ToolCallStatus;

/// Polling tools that legitimately repeat a few times before hitting an identical-EOF loop.
const POLLING_TOOLS: [&str; 2] = ["shell_output", "read_output"];
/// Bookkeeping tools that do not count as repeats and must not wipe an active repeat/cycle streak.
const BOOKKEEPING_TOOLS: [&str; 1] = ["todo_write"];
/// Calls past the repeat nudge after which the run is stuck (the 5th identical call).
const STUCK_AFTER_NUDGE: u32 = 2;
/// Consecutive identical polling results before warning about a dead-EOF poll loop.
const POLLING_NOTICE_AFTER: u32 = REPEAT_NOTICE_AFTER * 2;
/// Consecutive failures or user denials before tripping the hard-stop circuit breaker.
const STREAK_HARD_STOP: u32 = REPEAT_NOTICE_AFTER * 2;
/// Recent non-bookkeeping fingerprints kept for period-2 / period-3 cycle detection.
const HISTORY_WINDOW: usize = 16;
/// Full repetitions of a period-2/3 cycle before nudging; one more cycle trips `stuck`.
const CYCLE_NOTICE_REPS: usize = 3;
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
    /// Identical `(name, canonical_args)` failing streak, resilient to volatile output timestamps.
    last_failed_args: Option<u64>,
    failed_arg_repeats: u32,
    /// Identical polling `(name, canonical_args, result)` streak for `shell_output`/`read_output`.
    last_poll: Option<u64>,
    poll_repeats: u32,
    /// Recent non-exempt call fingerprints for multi-step oscillating cycle detection.
    history: VecDeque<u64>,
    failures: u32,
    denials: u32,
    /// Lines appended so far in this run.
    pub nudges: u32,
    /// The run hit a hard doom-loop circuit breaker and should stop as `NoProgress`.
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

fn canonical_args(arguments: &str) -> String {
    serde_json::from_str::<serde_json::Value>(arguments).map_or_else(
        |_| arguments.to_owned(),
        |value| sort_json_keys(value).to_string(),
    )
}

/// The same arguments in any key order or spacing hash alike.
fn fingerprint(name: &str, canonical: &str, result: &str) -> u64 {
    let prefix = &result.as_bytes()[..result.len().min(RESULT_PREFIX)];
    let mut hasher = DefaultHasher::new();
    (name, canonical, prefix).hash(&mut hasher);
    hasher.finish()
}

fn args_fingerprint(name: &str, canonical: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    (name, canonical).hash(&mut hasher);
    hasher.finish()
}

/// Detects how many consecutive full repetitions of a period-2 or period-3 cycle end at the tail
/// of `history`, returning `(period, repetitions)` when `repetitions >= 2` and the cycle contains
/// at least two distinct fingerprints (period-1 exact repeats are tracked separately).
fn tail_cycle(history: &VecDeque<u64>) -> Option<(usize, usize)> {
    let len = history.len();
    for period in [2usize, 3usize] {
        if len < period * 2 {
            continue;
        }
        let pat_start = len - period;
        let first = history[pat_start];
        if (1..period).all(|offset| history[pat_start + offset] == first) {
            continue;
        }
        let mut reps = 1;
        while len >= period * (reps + 1) {
            let chunk_start = len - period * (reps + 1);
            let matches = (0..period)
                .all(|offset| history[chunk_start + offset] == history[pat_start + offset]);
            if matches {
                reps += 1;
            } else {
                break;
            }
        }
        if reps >= CYCLE_NOTICE_REPS {
            return Some((period, reps));
        }
    }
    None
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

        let mut cycle_detected: Option<(usize, usize)> = None;
        let mut poll_stuck_notice = false;
        let mut failed_args_notice = false;

        if BOOKKEEPING_TOOLS.contains(&call.name) {
            // Keep existing repeat/cycle streaks intact so rewriting todos cannot mask a loop.
        } else if POLLING_TOOLS.contains(&call.name) {
            let canon = canonical_args(call.arguments);
            let print = fingerprint(call.name, &canon, call.result);
            self.poll_repeats = if self.last_poll == Some(print) {
                self.poll_repeats + 1
            } else {
                1
            };
            self.last_poll = Some(print);
            if self.poll_repeats >= POLLING_NOTICE_AFTER + STUCK_AFTER_NUDGE {
                self.stuck = true;
            }
            if self.poll_repeats > 0 && self.poll_repeats.is_multiple_of(POLLING_NOTICE_AFTER) {
                poll_stuck_notice = true;
            }
        } else {
            self.last_poll = None;
            self.poll_repeats = 0;
            let canon = canonical_args(call.arguments);
            let print = fingerprint(call.name, &canon, call.result);
            self.repeats = if self.last_call == Some(print) {
                self.repeats + 1
            } else {
                1
            };
            self.last_call = Some(print);

            if failed {
                let arg_print = args_fingerprint(call.name, &canon);
                self.failed_arg_repeats = if self.last_failed_args == Some(arg_print) {
                    self.failed_arg_repeats + 1
                } else {
                    1
                };
                self.last_failed_args = Some(arg_print);
                if self.failed_arg_repeats >= REPEAT_NOTICE_AFTER + STUCK_AFTER_NUDGE {
                    self.stuck = true;
                }
                if due(self.failed_arg_repeats) {
                    failed_args_notice = true;
                }
            } else {
                self.last_failed_args = None;
                self.failed_arg_repeats = 0;
            }

            if self.history.len() == HISTORY_WINDOW {
                self.history.pop_front();
            }
            self.history.push_back(print);
            cycle_detected = tail_cycle(&self.history);
            if let Some((_, reps)) = cycle_detected
                && reps > CYCLE_NOTICE_REPS
            {
                self.stuck = true;
            }
        }

        if self.repeats >= REPEAT_NOTICE_AFTER + STUCK_AFTER_NUDGE
            || self.failures >= STREAK_HARD_STOP
            || self.denials >= STREAK_HARD_STOP
        {
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
        } else if due(self.repeats) || failed_args_notice {
            Nudge {
                signal: Signal::Repeat,
                text: format!(
                    "[System notice] You called {} with the same arguments {n} times and got the same result. Change approach or tell the user what blocks you.",
                    call.name
                ),
            }
        } else if let Some((period, reps)) = cycle_detected {
            Nudge {
                signal: Signal::Repeat,
                text: format!(
                    "[System notice] You repeated the same {period}-step tool sequence {reps} times without progress. Change approach or tell the user what blocks you."
                ),
            }
        } else if poll_stuck_notice {
            Nudge {
                signal: Signal::Repeat,
                text: format!(
                    "[System notice] You polled {} {} times with identical output. Stop polling and either inspect the process or tell the user what blocks you.",
                    call.name, self.poll_repeats
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

#[cfg(test)]
mod tests {
    use super::*;

    fn obs<'a>(
        name: &'a str,
        arguments: &'a str,
        status: ToolCallStatus,
        result: &'a str,
    ) -> Observed<'a> {
        Observed {
            name,
            arguments,
            status,
            denied_by_user: false,
            result,
        }
    }

    #[test]
    fn detects_period_two_cycles_and_todo_write_cannot_reset_streak() {
        let mut probe = ProgressProbe::default();
        // A -> B -> todo_write -> A -> B -> A -> B (3 cycles => nudge), then A -> B (4th => stuck)
        for rep in 1..=4 {
            let a_nudge = probe.observe(&obs(
                "read_file",
                r#"{"path":"a"}"#,
                ToolCallStatus::Ok,
                "same-a",
            ));
            if rep < 4 {
                assert!(a_nudge.is_none(), "rep {rep}");
            } else {
                // Seventh entry [A,B,A,B,A,B,A] already forms [B,A] x 3 at the tail.
                assert_eq!(a_nudge.unwrap().signal, Signal::Repeat);
            }
            assert!(!probe.stuck, "rep {rep} after A");
            // Interleaved todo_write must not break the cycle.
            assert!(
                probe
                    .observe(&obs(
                        "todo_write",
                        r#"{"todos":[]}"#,
                        ToolCallStatus::Ok,
                        "ok"
                    ))
                    .is_none()
            );
            let nudge = probe.observe(&obs(
                "grep",
                r#"{"pattern":"x"}"#,
                ToolCallStatus::Ok,
                "same-b",
            ));
            if rep < 3 {
                assert!(nudge.is_none(), "rep {rep}");
                assert!(!probe.stuck);
            } else if rep == 3 {
                assert_eq!(nudge.unwrap().signal, Signal::Repeat);
                assert!(!probe.stuck);
            } else {
                assert_eq!(nudge.unwrap().signal, Signal::Repeat);
                assert!(probe.stuck);
            }
        }
    }

    #[test]
    fn failing_same_args_ignores_volatile_output_and_hard_stops_failure_and_denial_streaks() {
        let mut probe = ProgressProbe::default();
        for i in 1..=5 {
            let err = format!("failed at ts={i} pid={}", 1000 + i);
            let nudge = probe.observe(&obs(
                "shell",
                r#"{"command":"cargo test"}"#,
                ToolCallStatus::Error,
                &err,
            ));
            if i == 3 {
                assert_eq!(nudge.unwrap().signal, Signal::Repeat);
                assert!(!probe.stuck);
            } else if i == 5 {
                assert!(probe.stuck);
            }
        }

        // Distinct failing calls still trip the hard-stop circuit breaker at 6 in a row.
        let mut failures = ProgressProbe::default();
        for i in 1..=6 {
            let args = format!(r#"{{"path":"f{i}"}}"#);
            let nudge =
                failures.observe(&obs("read_file", &args, ToolCallStatus::Error, "missing"));
            if i == 3 || i == 6 {
                assert_eq!(nudge.unwrap().signal, Signal::Failures);
            }
            assert_eq!(failures.stuck, i == 6);
        }

        // User denials trip hard-stop at 6 in a row.
        let mut denials = ProgressProbe::default();
        for i in 1..=6 {
            let args = format!(r#"{{"path":"d{i}"}}"#);
            let nudge = denials.observe(&Observed {
                name: "write_file",
                arguments: &args,
                status: ToolCallStatus::Denied,
                denied_by_user: true,
                result: "denied",
            });
            if i == 3 || i == 6 {
                assert_eq!(nudge.unwrap().signal, Signal::Denials);
            }
            assert_eq!(denials.stuck, i == 6);
        }
    }

    #[test]
    fn dead_eof_polling_nudges_at_six_and_sticks_at_eight() {
        let mut probe = ProgressProbe::default();
        for i in 1..=8 {
            let nudge = probe.observe(&obs(
                "shell_output",
                r#"{"id":"p1"}"#,
                ToolCallStatus::Ok,
                "still running, 0 new bytes",
            ));
            assert_eq!(nudge.is_some(), i == 6, "poll {i}");
            assert_eq!(probe.stuck, i == 8, "poll {i}");
        }
    }
}
