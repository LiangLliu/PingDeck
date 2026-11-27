//! Progress of a flow run: the event stream, its rate limit and the run summary.
//!
//! Single responsibility: turn finished steps into [`FlowEvent`]s a shell can forward, keep that
//! stream under 30 events a second, and condense a finished run into the [`FlowRunSummary`] that
//! is persisted.
//!
//! Design: every finished step is a one-step [`FlowEvent::StepsFinished`]; steps that finish
//! within one coalescing window are merged into a single event carrying all of them (in
//! execution order), so nothing is dropped and the rate stays bounded however fast steps finish.
//! The first step of a run is never delayed, and [`FlowEvent::RunFinished`] flushes what is
//! pending and then closes the stream.

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::events::{EventSink, StreamCoalescer};
use crate::workspace::KeyValue;
use crate::workspace::items::{
    FlowRunResult, FlowRunScopeKind, FlowRunSummary, FlowStepResult, FlowStepSummary,
};

/// Longest flow variable value a stored run summary keeps, in bytes.
pub const SUMMARY_VARIABLE_MAX_BYTES: usize = 4 * 1024;
/// Most flow variables a stored run summary keeps.
pub const SUMMARY_VARIABLES_MAX: usize = 100;

/// Progress of a flow run; events arrive in execution order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum FlowEvent {
    /// These steps finished since the previous event (one step when they are slow).
    StepsFinished {
        run_id: String,
        /// Number of steps the flow has, for "4 of 9".
        total: usize,
        steps: Vec<FlowStepSummary>,
    },
    /// The run ended (passed, failed or cancelled); the last event of the stream.
    RunFinished {
        run_id: String,
        passed: bool,
        cancelled: bool,
    },
}

impl FlowEvent {
    /// The event for step number `index` of `result`'s run having finished: its summary carries
    /// no bodies, headers or traces and only the first line of the error.
    #[must_use]
    pub fn step_finished(
        run_id: &str,
        total: usize,
        index: usize,
        result: &FlowStepResult,
    ) -> Self {
        Self::StepsFinished {
            run_id: run_id.to_owned(),
            total,
            steps: vec![step_summary(index, result)],
        }
    }

    /// The closing event of `result`'s run.
    #[must_use]
    pub fn run_finished(result: &FlowRunResult) -> Self {
        Self::RunFinished {
            run_id: result.run_id.clone(),
            passed: result.passed,
            cancelled: result.cancelled,
        }
    }
}

/// What a progress event and a stored run keep of step number `index`: no bodies, headers or
/// traces, and only the first line of the error.
fn step_summary(index: usize, result: &FlowStepResult) -> FlowStepSummary {
    let failed = result
        .assertion_results
        .iter()
        .filter(|assertion| !assertion.passed)
        .count();
    FlowStepSummary {
        index,
        step_id: result.step_id.clone(),
        step_name: result.step_name.clone(),
        status_code: result.status_code,
        elapsed_ms: result.elapsed_ms,
        passed: result.passed,
        skipped: result.skipped,
        attempts: result.attempts,
        error: result
            .error
            .as_deref()
            .and_then(|error| error.lines().next())
            .map(str::to_owned),
        history_entry_id: result.history_entry_id.clone(),
        assertions_passed: result.assertion_results.len() - failed,
        assertions_failed: failed,
        condition: result.condition.clone(),
        explanation: result.explanation.clone(),
        ai: result.ai.clone(),
        matched_branch_id: result.matched_branch_id.clone(),
        started_offset_ms: result.started_offset_ms,
    }
}

fn is_step_batch(event: &FlowEvent) -> bool {
    matches!(event, FlowEvent::StepsFinished { .. })
}

/// Appends `next`'s steps to `pending` when both belong to the same run.
fn merge_step_batches(pending: &mut FlowEvent, next: FlowEvent) -> Result<(), FlowEvent> {
    match (pending, next) {
        (
            FlowEvent::StepsFinished { run_id, steps, .. },
            FlowEvent::StepsFinished {
                run_id: next_run_id,
                steps: next_steps,
                ..
            },
        ) if *run_id == next_run_id => {
            steps.extend(next_steps);
            Ok(())
        }
        (_, next) => Err(next),
    }
}

/// Delivers the events received from `events` to `sink`, at most 30 a second, until the sender
/// is dropped (after it sent [`FlowEvent::RunFinished`]) or the sink reports that its consumer
/// is gone.
///
/// The merging is the pure [`StreamCoalescer`] state machine; this driver supplies the clock
/// (tokio's, so it follows a paused test clock) and wakes up when a held-back batch is due, so a
/// slow next step never delays the steps that already finished.
pub async fn forward_flow_events(
    mut events: UnboundedReceiver<FlowEvent>,
    sink: &dyn EventSink<FlowEvent>,
) {
    let mut coalescer = StreamCoalescer::new(is_step_batch, merge_step_batches);
    loop {
        let next = match coalescer.next_deadline() {
            Some(deadline) => tokio::select! {
                biased;
                event = events.recv() => event,
                () = tokio::time::sleep_until(deadline.into()) => {
                    if let Some(due) = coalescer.poll(tokio::time::Instant::now().into_std())
                        && !sink.emit(due)
                    {
                        return;
                    }
                    continue;
                }
            },
            None => events.recv().await,
        };
        let Some(event) = next else { break };
        for ready in coalescer.push(event, tokio::time::Instant::now().into_std()) {
            if !sink.emit(ready) {
                return;
            }
        }
    }
    if let Some(rest) = coalescer.flush() {
        sink.emit(rest);
    }
}

/// Condenses a finished run for storage: its scope and environment, what each step did, what
/// failed first, how long it took, and the flow variables it ended with (see
/// [`stored_variables`]).
#[must_use]
pub fn summarize_run(result: &FlowRunResult, finished_at_ms: i64) -> FlowRunSummary {
    let (variables, withheld_variables) = stored_variables(result);
    let failed = result.steps.iter().find(|step| !step.passed);
    // Every step of a run's scope up to where it stopped has a result, so the first result is
    // the scope's first step.
    let first_index = match result.scope.kind {
        FlowRunScopeKind::Only => result.scope.step_index.unwrap_or(0),
        FlowRunScopeKind::Full | FlowRunScopeKind::UpTo => 0,
    };
    FlowRunSummary {
        run_id: result.run_id.clone(),
        flow_id: result.flow_id.clone(),
        started_at_ms: result.started_at_ms,
        finished_at_ms,
        passed: result.passed,
        cancelled: result.cancelled,
        failed_step_id: failed.map(|step| step.step_id.clone()),
        failed_step_name: failed.map(|step| step.step_name.clone()),
        steps_passed: result
            .steps
            .iter()
            .filter(|step| step.passed && !step.skipped)
            .count(),
        steps_total: result.steps_total,
        elapsed_ms: result.elapsed_ms,
        steps: result
            .steps
            .iter()
            .enumerate()
            .map(|(offset, step)| step_summary(first_index + offset, step))
            .collect(),
        scope: result.scope,
        environment_id: result.environment_id.clone(),
        variables: Some(variables),
        withheld_variables,
    }
}

/// The flow variables a stored summary may keep, and the keys of those it must not: a secret
/// variable, a value a redaction pattern or a secret value matches (it would be stored masked,
/// useless as a seed), a value over [`SUMMARY_VARIABLE_MAX_BYTES`], and everything past the first
/// [`SUMMARY_VARIABLES_MAX`] kept variables are withheld.
fn stored_variables(result: &FlowRunResult) -> (Vec<KeyValue>, Vec<String>) {
    let is_secret = |key: &str| result.secret_variables.iter().any(|secret| secret == key);
    let secret_values: Vec<String> = result
        .variables
        .iter()
        .filter(|variable| is_secret(&variable.key) && !variable.value.is_empty())
        .map(|variable| variable.value.clone())
        .collect();
    let mut kept = Vec::new();
    let mut withheld = Vec::new();
    for variable in &result.variables {
        let keep = !is_secret(&variable.key)
            && kept.len() < SUMMARY_VARIABLES_MAX
            && variable.value.len() <= SUMMARY_VARIABLE_MAX_BYTES
            && pingdeck_base::secrets::redact_secrets(&variable.value, &secret_values)
                == variable.value;
        if keep {
            kept.push(KeyValue {
                description: None,
                ..variable.clone()
            });
        } else {
            withheld.push(variable.key.clone());
        }
    }
    (kept, withheld)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::events::COALESCE_WINDOW;

    fn finished(index: usize) -> FlowEvent {
        FlowEvent::StepsFinished {
            run_id: "run_1".into(),
            total: 9,
            steps: vec![FlowStepSummary {
                index,
                step_id: format!("s{index}"),
                step_name: format!("Step {index}"),
                status_code: Some(200),
                elapsed_ms: 1,
                passed: true,
                skipped: false,
                attempts: 1,
                error: None,
                history_entry_id: None,
                assertions_passed: 0,
                assertions_failed: 0,
                condition: None,
                explanation: None,
                ai: None,
                matched_branch_id: None,
                started_offset_ms: None,
            }],
        }
    }

    fn run_finished() -> FlowEvent {
        FlowEvent::RunFinished {
            run_id: "run_1".into(),
            passed: true,
            cancelled: false,
        }
    }

    fn indexes(event: &FlowEvent) -> Vec<usize> {
        match event {
            FlowEvent::StepsFinished { steps, .. } => steps.iter().map(|step| step.index).collect(),
            FlowEvent::RunFinished { .. } => Vec::new(),
        }
    }

    fn coalescer() -> StreamCoalescer<FlowEvent> {
        StreamCoalescer::new(is_step_batch, merge_step_batches)
    }

    #[test]
    fn the_first_step_is_sent_at_once_and_a_burst_becomes_one_batch_per_window() {
        let start = Instant::now();
        let mut coalescer = coalescer();

        let first = coalescer.push(finished(0), start);
        assert_eq!(first.iter().map(indexes).collect::<Vec<_>>(), [vec![0]]);
        for index in 1..=4 {
            let held = coalescer.push(finished(index), start + Duration::from_millis(index as u64));
            assert!(held.is_empty(), "within the window nothing is sent");
        }
        assert_eq!(coalescer.poll(start + Duration::from_millis(10)), None);

        let batch = coalescer.poll(start + COALESCE_WINDOW).unwrap();
        assert_eq!(indexes(&batch), [1, 2, 3, 4]);
    }

    #[test]
    fn run_finished_is_never_delayed_and_flushes_pending_steps_first() {
        let start = Instant::now();
        let mut coalescer = coalescer();
        coalescer.push(finished(0), start);
        coalescer.push(finished(1), start);

        let out = coalescer.push(run_finished(), start + Duration::from_millis(1));

        assert_eq!(out.len(), 2);
        assert_eq!(indexes(&out[0]), [1]);
        assert_eq!(out[1], run_finished());
    }

    #[test]
    fn steps_of_another_run_are_never_merged() {
        let start = Instant::now();
        let mut coalescer = coalescer();
        coalescer.push(finished(0), start);
        coalescer.push(finished(1), start);
        let other = match finished(0) {
            FlowEvent::StepsFinished { total, steps, .. } => FlowEvent::StepsFinished {
                run_id: "run_2".into(),
                total,
                steps,
            },
            other => other,
        };

        let out = coalescer.push(other, start);

        assert_eq!(out.len(), 1, "the held batch of run_1 is released");
        assert_eq!(indexes(&out[0]), [1]);
    }

    #[tokio::test]
    async fn the_driver_keeps_order_sends_the_first_step_alone_and_closes_with_run_finished() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        for index in 0..6 {
            sender.send(finished(index)).unwrap();
        }
        sender.send(run_finished()).unwrap();
        drop(sender);
        let seen = Mutex::new(Vec::new());

        forward_flow_events(receiver, &|event: FlowEvent| {
            seen.lock().unwrap().push(event);
            true
        })
        .await;

        let seen = seen.into_inner().unwrap();
        assert_eq!(indexes(&seen[0]), [0], "the first step is not delayed");
        assert_eq!(seen.last(), Some(&run_finished()));
        let flattened: Vec<usize> = seen.iter().flat_map(indexes).collect();
        assert_eq!(
            flattened,
            [0, 1, 2, 3, 4, 5],
            "no step is lost or reordered"
        );
    }

    #[tokio::test]
    async fn the_driver_releases_a_held_batch_when_its_window_ends_without_new_events() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (out, mut delivered) = tokio::sync::mpsc::unbounded_channel();
        let driver = tokio::spawn(async move {
            forward_flow_events(receiver, &move |event: FlowEvent| out.send(event).is_ok()).await;
        });

        sender.send(finished(0)).unwrap();
        sender.send(finished(1)).unwrap();
        let first = delivered.recv().await.unwrap();
        let second = tokio::time::timeout(Duration::from_secs(10), delivered.recv())
            .await
            .expect("the held step must be released by the window, not by the next event")
            .unwrap();

        assert_eq!(indexes(&first), [0]);
        assert_eq!(indexes(&second), [1]);
        drop(sender);
        driver.await.unwrap();
    }

    #[tokio::test]
    async fn the_driver_stops_when_the_consumer_is_gone() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        sender.send(finished(0)).unwrap();
        sender.send(run_finished()).unwrap();
        let calls = Mutex::new(0);

        forward_flow_events(receiver, &|_: FlowEvent| {
            *calls.lock().unwrap() += 1;
            false
        })
        .await;

        assert_eq!(
            *calls.lock().unwrap(),
            1,
            "no event is sent after a refusal"
        );
    }

    #[test]
    fn the_run_summary_names_the_first_failed_step_counts_executed_passes_and_withholds_secrets() {
        let step = |id: &str, passed: bool, skipped: bool| -> FlowStepResult {
            serde_json::from_value(serde_json::json!({
                "step_id": id, "step_name": id, "request_id": "", "status_code": null,
                "elapsed_ms": 1, "passed": passed, "error": null, "skipped": skipped,
            }))
            .unwrap()
        };
        let result = FlowRunResult {
            run_id: "run_1".into(),
            flow_id: "flow_1".into(),
            started_at_ms: 1_000,
            passed: false,
            cancelled: false,
            elapsed_ms: 40,
            steps_total: 5,
            variables: vec![
                KeyValue {
                    key: "user_id".into(),
                    value: "42".into(),
                    enabled: true,
                    description: None,
                },
                KeyValue {
                    key: "token".into(),
                    value: "tok-secret-123456".into(),
                    enabled: true,
                    description: None,
                },
                KeyValue {
                    key: "echo".into(),
                    value: "sent tok-secret-123456".into(),
                    enabled: true,
                    description: None,
                },
                KeyValue {
                    key: "body".into(),
                    value: "x".repeat(SUMMARY_VARIABLE_MAX_BYTES + 1),
                    enabled: true,
                    description: None,
                },
            ],
            secret_variables: vec!["token".into()],
            steps: vec![
                step("a", true, false),
                step("b", true, true),
                step("c", false, false),
                step("d", false, false),
            ],
            scope: Default::default(),
            environment_id: Some("env_dev".into()),
        };

        let summary = summarize_run(&result, 1_040);
        assert_eq!(summary.environment_id.as_deref(), Some("env_dev"));
        let kept: Vec<_> = summary
            .variables
            .iter()
            .flatten()
            .map(|variable| (variable.key.as_str(), variable.value.as_str()))
            .collect();
        assert_eq!(kept, [("user_id", "42")]);
        assert_eq!(summary.withheld_variables, ["token", "echo", "body"]);

        assert_eq!(summary.failed_step_id.as_deref(), Some("c"));
        assert_eq!(summary.failed_step_name.as_deref(), Some("c"));
        assert_eq!((summary.steps_passed, summary.steps_total), (1, 5));
        assert_eq!(
            (summary.started_at_ms, summary.finished_at_ms),
            (1_000, 1_040)
        );
    }
}
