//! Engine layer of the demo/core conformance checks.
//!
//! Three scripted runs (a plain answer, a tool call behind an approval, a cancel mid-answer) go
//! through the run loop over its test doubles; the script and the kind and `seq` of every event
//! are frozen in `fixtures/agent-transcripts/<name>.json`. `src/api/demo/chat/transcripts.test.ts`
//! plays the same scripts on the demo's engine and keeps a table of the known differences. A
//! changed transcript is stale until regenerated with
//! `UPDATE_AGENT_TRANSCRIPTS=1 cargo test -p pingdeck-agent --locked transcripts`.
//!
//! The loop does not open or close a run: `agent_service/run.rs` emits `RunStarted`,
//! `UserMessage` and `QueuedUpdated` before it (`execute`) and the one `Done` or `Error` after it
//! (`finish`). The transcript writes that frame around the loop's own events.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use super::fakes::{FakeBackend, FakeHost, ManualClock, Step, finish, says, start, text, until};
use super::{AskInfo, CompactionPolicy, ModelInfo, RunInput, Verdict, run_agent};
use crate::agent::approvals::{ApprovalAnswer, ApprovalOutcome};
use crate::agent::model::{
    AgentEvent, AgentRole, AgentRunResult, AgentStopReason, ApprovalDecision, ApprovalScope,
};
use crate::agent::tools::ToolClass;
use pingdeck_base::cancel::CancelToken;
use pingdeck_base::error::AppError;
use pingdeck_llm::llm::client::ChatTarget;
use pingdeck_llm::llm::hub::{HubEvent, HubFinish, HubUsage};

/// What the user said, what the tools do, what the model streams and how the user answers.
#[derive(Serialize)]
struct Script {
    user: &'static str,
    tools: Vec<Tool>,
    turns: Vec<Turn>,
    /// One answer per approval, in order: `allow` or `deny`.
    approvals: Vec<&'static str>,
    /// The user stops the run once the hanging turn streamed its text.
    cancel: bool,
}

#[derive(Serialize)]
struct Tool {
    name: &'static str,
    /// The permission gate asks the user before the call runs.
    ask: bool,
    output: &'static str,
}

/// One model call: its text fragments and tool calls; `hang` keeps the stream open and silent
/// after the text (no usage, no finish).
#[derive(Serialize)]
struct Turn {
    text: Vec<&'static str>,
    calls: Vec<Call>,
    usage: [u64; 2],
    hang: bool,
}

#[derive(Serialize)]
struct Call {
    id: &'static str,
    name: &'static str,
    arguments: &'static str,
}

/// The frame `agent_service/run.rs::execute` emits before the loop runs.
const OPENING: [&str; 3] = ["RunStarted", "UserMessage", "QueuedUpdated"];

/// The terminal event `agent_service/run.rs::finish` emits for the loop's outcome.
fn terminal(outcome: &Result<AgentRunResult, AppError>) -> &'static str {
    match outcome {
        Ok(result) if result.stop_reason == AgentStopReason::CostLimit => "Error",
        Ok(_) => "Done",
        Err(_) => "Error",
    }
}

fn steps(turn: &Turn) -> Vec<Step> {
    let mut steps = vec![start()];
    steps.extend(turn.text.iter().map(|piece| text(piece)));
    if turn.hang {
        steps.push(Step::Hang);
        return steps;
    }
    for (index, call) in turn.calls.iter().enumerate() {
        let index = u32::try_from(index).unwrap();
        steps.push(Step::Event(HubEvent::ToolCallStart {
            index,
            id: call.id.into(),
            name: call.name.into(),
            signature: None,
        }));
        steps.push(Step::Event(HubEvent::ToolCallDelta {
            index,
            arguments: call.arguments.into(),
        }));
    }
    steps.push(Step::Event(HubEvent::Usage {
        usage: HubUsage {
            input_tokens: Some(turn.usage[0]),
            output_tokens: Some(turn.usage[1]),
            ..HubUsage::default()
        },
    }));
    steps.push(finish(if turn.calls.is_empty() {
        HubFinish::Stop
    } else {
        HubFinish::ToolCalls
    }));
    steps
}

fn ask() -> Verdict {
    Verdict::Ask(AskInfo {
        reason: "Runs a command.".into(),
        class: ToolClass::Exec,
        read_only: false,
        destructive: false,
        preview: None,
        network: false,
        card: None,
        risk: "medium",
    })
}

fn answer(decision: &str) -> ApprovalOutcome {
    ApprovalOutcome::Answered(ApprovalAnswer {
        decision: if decision == "allow" {
            ApprovalDecision::Allow
        } else {
            ApprovalDecision::Deny
        },
        scope: ApprovalScope::Once,
        message: None,
        edited_arguments: None,
        allow_network_once: false,
    })
}

fn input() -> RunInput {
    RunInput {
        target: ChatTarget::Named {
            name: "t".into(),
            fallbacks: Vec::new(),
        },
        model_label: "m".into(),
        system: None,
        tools: Vec::new(),
        native_web_search: false,
        reasoning: None,
        max_iterations: 20,
        model: ModelInfo::default(),
        compaction: CompactionPolicy {
            auto: false,
            threshold_pct: 85,
            keep_turns: 2,
        },
        cost_caps: Vec::new(),
        wrap_up_last_step: true,
        prompt_cache: None,
        max_run_tokens: None,
    }
}

/// Plays `script` on the loop and returns the kinds of the run's events, frame included.
async fn play(script: &Script) -> Vec<String> {
    let clock: Arc<ManualClock> = ManualClock::new();
    let mut host = FakeHost::new(&clock)
        .say(AgentRole::User, script.user)
        .announcing();
    for tool in &script.tools {
        let verdict = if tool.ask { ask() } else { Verdict::Run };
        host = host.tool(
            tool.name,
            ToolClass::Exec,
            false,
            verdict,
            says(tool.output),
        );
    }
    for decision in &script.approvals {
        host = host.approve_with(answer(decision));
    }
    let backend = script
        .turns
        .iter()
        .fold(FakeBackend::new(&clock), |backend, turn| {
            backend.script(steps(turn))
        });
    let streamed: usize = script.turns.iter().map(|turn| turn.text.len()).sum();
    let cancel = CancelToken::new();
    let input = input();
    let run = tokio::time::timeout(
        Duration::from_secs(20),
        run_agent(&backend, &host, &input, &cancel),
    );
    let (outcome, ()) = tokio::join!(run, async {
        if script.cancel {
            until(|| {
                host.events()
                    .iter()
                    .filter(|event| matches!(event, AgentEvent::TextDelta { .. }))
                    .count()
                    == streamed
            })
            .await;
            cancel.cancel();
        }
    });
    let outcome = outcome.expect("the run loop is stuck");
    let kind = |event: &AgentEvent| {
        serde_json::to_value(event).unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    OPENING
        .iter()
        .map(|kind| (*kind).to_owned())
        .chain(host.events().iter().map(kind))
        .chain([terminal(&outcome).to_owned()])
        .collect()
}

fn sort_json_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .into_iter()
                .map(|(k, v)| (k, sort_json_keys(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_json_keys).collect()),
        other => other,
    }
}

/// Compares the transcript of `script` with its fixture, or writes it under
/// `UPDATE_AGENT_TRANSCRIPTS`. Every event of a run's hub takes the next `seq` from 1.
async fn verify(name: &str, note: &str, script: Script) {
    let events: Vec<Value> = play(&script)
        .await
        .into_iter()
        .enumerate()
        .map(|(index, kind)| json!({ "seq": index + 1, "kind": kind }))
        .collect();
    let payload =
        sort_json_keys(json!({ "name": name, "note": note, "script": script, "events": events }));
    let rendered = format!("{}\n", serde_json::to_string_pretty(&payload).unwrap());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/agent-transcripts")
        .join(format!("{name}.json"));
    if fs::read_to_string(&path).ok().as_deref() == Some(rendered.as_str()) {
        return;
    }
    if std::env::var_os("UPDATE_AGENT_TRANSCRIPTS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, rendered).unwrap();
        return;
    }
    panic!(
        "{} is stale: regenerate with `UPDATE_AGENT_TRANSCRIPTS=1 cargo test -p pingdeck-agent --locked transcripts`, then update the known differences in src/api/demo/chat/transcripts.test.ts",
        path.display()
    );
}

#[tokio::test]
async fn plain_answer() {
    let script = Script {
        user: "hi",
        tools: Vec::new(),
        turns: vec![Turn {
            text: vec!["Hel", "lo."],
            calls: Vec::new(),
            usage: [10, 5],
            hang: false,
        }],
        approvals: Vec::new(),
        cancel: false,
    };
    verify(
        "plain-answer",
        "One model call that answers in two fragments.",
        script,
    )
    .await;
}

#[tokio::test]
async fn tool_approval() {
    let script = Script {
        user: "run the tests",
        tools: vec![Tool {
            name: "shell",
            ask: true,
            output: "14 passed",
        }],
        turns: vec![
            Turn {
                text: vec!["Running the tests."],
                calls: vec![Call {
                    id: "c1",
                    name: "shell",
                    arguments: r#"{"command":"cargo test"}"#,
                }],
                usage: [20, 8],
                hang: false,
            },
            Turn {
                text: vec!["All 14 passed."],
                calls: Vec::new(),
                usage: [30, 6],
                hang: false,
            },
        ],
        approvals: vec!["allow"],
        cancel: false,
    };
    verify(
        "tool-approval",
        "A shell call the user allows once, then the answer.",
        script,
    )
    .await;
}

#[tokio::test]
async fn cancel() {
    let script = Script {
        user: "hi",
        tools: Vec::new(),
        turns: vec![Turn {
            text: vec!["Part", "ial"],
            calls: Vec::new(),
            usage: [0, 0],
            hang: true,
        }],
        approvals: Vec::new(),
        cancel: true,
    };
    verify(
        "cancel",
        "The user stops the run while the model is mid-answer.",
        script,
    )
    .await;
}
