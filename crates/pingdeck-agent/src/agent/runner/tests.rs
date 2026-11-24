//! Tests of the run loop over a scripted model and an in-memory host: no database, no network, no
//! sleeping (time moves when a test advances the manual clock).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::FutureExt;
use serde_json::json;
use tokio::sync::Barrier;

use super::fakes::{
    Behavior, FakeBackend, FakeHost, ManualClock, Step, Stored, call_turn, context_length_failure,
    finish, says, start, text, text_turn, until, usage,
};
use super::{
    AskInfo, CANCEL_GRACE, CompactionPolicy, CostCap, ModelInfo, RunInput, STALL_TIMEOUT, Verdict,
    approval_timed_out_text, run_agent,
};
use crate::agent::approvals::{ApprovalAnswer, ApprovalOutcome};
use crate::agent::model::{
    AgentEvent, AgentRole, AgentRunResult, AgentStopReason, ApprovalDecision, ApprovalScope,
    ReasoningEffort, ToolCallStatus,
};
use crate::agent::registry::CostDecision;
use crate::agent::tools::{ToolClass, ToolOutput, ToolSpec};
use pingdeck_base::cancel::CancelToken;
use pingdeck_base::error::AppError;
use pingdeck_llm::llm::capabilities::{Capability, CapabilitySource};
use pingdeck_llm::llm::client::{AttemptSummary, ChatError, ChatErrorKind, ChatTarget};
use pingdeck_llm::llm::hub::{HubEvent, HubFinish, HubPart, HubRole};
use pingdeck_llm::llm::usage::{Price, PriceResolution, PriceSource};

fn input() -> RunInput {
    RunInput {
        target: ChatTarget::Named {
            name: "t".into(),
            fallbacks: Vec::new(),
        },
        model_label: "m".into(),
        system: Some("sys".into()),
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

/// The default input, for tests that never change it.
fn base() -> &'static RunInput {
    static BASE: std::sync::LazyLock<RunInput> = std::sync::LazyLock::new(input);
    &BASE
}

const READ_ARGS: &str = r#"{"path":"a"}"#;

fn read_host(clock: &Arc<ManualClock>) -> FakeHost {
    FakeHost::new(clock).say(AgentRole::User, "hi").tool(
        "read",
        ToolClass::Read,
        true,
        Verdict::Run,
        says("file body"),
    )
}

fn ask(reason: &str) -> Verdict {
    Verdict::Ask(AskInfo {
        reason: reason.into(),
        class: ToolClass::Exec,
        read_only: false,
        destructive: false,
        preview: None,
        network: false,
        card: None,
        risk: "medium",
    })
}

fn allow() -> ApprovalOutcome {
    ApprovalOutcome::Answered(ApprovalAnswer {
        decision: ApprovalDecision::Allow,
        scope: ApprovalScope::Once,
        message: None,
        edited_arguments: None,
        allow_network_once: false,
    })
}

/// Runs the loop with a real-time guard: a stuck loop fails the test instead of hanging it.
async fn run(
    backend: &FakeBackend,
    host: &FakeHost,
    input: &RunInput,
) -> Result<AgentRunResult, AppError> {
    run_cancellable(backend, host, input, &CancelToken::new()).await
}

async fn run_cancellable(
    backend: &FakeBackend,
    host: &FakeHost,
    input: &RunInput,
    cancel: &CancelToken,
) -> Result<AgentRunResult, AppError> {
    tokio::time::timeout(
        Duration::from_secs(20),
        run_agent(backend, host, input, cancel),
    )
    .await
    .expect("the run loop is stuck")
}

fn has_event(host: &FakeHost, wanted: impl Fn(&AgentEvent) -> bool) -> bool {
    host.events().iter().any(wanted)
}

fn texts(host: &FakeHost) -> String {
    host.events()
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn last_request_texts(backend: &FakeBackend) -> Vec<(HubRole, String)> {
    let requests = backend.requests.lock().unwrap();
    requests
        .last()
        .unwrap()
        .messages
        .iter()
        .map(|message| {
            (
                message.role,
                pingdeck_llm::llm::hub::text_of(&message.content),
            )
        })
        .collect()
}

#[tokio::test]
async fn text_streaming_partial_saves_usage_timings_and_stop_reasons() {
    let clock = ManualClock::new();
    let backend = FakeBackend::new(&clock)
        .script(vec![
            start(),
            Step::Advance(Duration::from_millis(1_500)),
            text("a"),
            Step::Advance(Duration::from_secs(2)),
            text("b"),
            Step::Advance(Duration::from_secs(2)),
            text("c"),
            usage(10, 5),
            finish(HubFinish::Stop),
        ])
        .script(vec![
            start(),
            text("cut"),
            usage(10, 4096),
            finish(HubFinish::Length),
        ]);
    let host = FakeHost::new(&clock).say(AgentRole::User, "hi");
    let mut inp = input();
    inp.reasoning = Some(ReasoningEffort::High);
    inp.native_web_search = true;
    inp.model.context_limit = Some(1_000);
    inp.model
        .capabilities
        .apply(Capability::WebSearch, true, CapabilitySource::Declared);
    inp.tools = vec![ToolSpec {
        name: "read".into(),
        description: "reads".into(),
        input_schema: json!({"type": "object"}),
        read_only: true,
        destructive: false,
        source: crate::agent::tools::ToolSource::Builtin,
        class: ToolClass::Read,
        parallel_safe: true,
    }];

    let result = run(&backend, &host, &inp).await.unwrap();
    assert_eq!(
        (result.stop_reason, result.iterations, texts(&host).as_str()),
        (AgentStopReason::Completed, 1, "abc")
    );
    assert!(host.state.partial_saves.load(Ordering::SeqCst) >= 1);
    assert_eq!(host.state.attachment_resolutions.load(Ordering::SeqCst), 1);
    assert!(has_event(&host, |e| matches!(
        e,
        AgentEvent::Usage {
            ttft_ms: Some(1_500),
            stream_ms: Some(4_000),
            ..
        }
    )));

    let length_res = run(&backend, &host, base()).await.unwrap();
    assert_eq!(length_res.stop_reason, AgentStopReason::OutputLimit);
}

#[tokio::test]
async fn tool_execution_parallel_concurrency_ordering_and_redaction() {
    let secret = "sk-live-0123456789abcdef";
    let clock = ManualClock::new();
    let barrier = Arc::new(Barrier::new(2));
    let par: Behavior = {
        let barrier = Arc::clone(&barrier);
        Arc::new(move |_, call| {
            let barrier = Arc::clone(&barrier);
            async move {
                barrier.wait().await;
                ToolOutput::text(format!("res {} {secret}", call.id))
            }
            .boxed()
        })
    };
    let host = FakeHost::new(&clock)
        .say(AgentRole::User, "hi")
        .tool("read", ToolClass::Read, true, Verdict::Run, par)
        .tool("write", ToolClass::Edit, false, Verdict::Run, says("wrote"))
        .tool(
            "shell",
            ToolClass::Exec,
            false,
            Verdict::Refuse("blocked".into()),
            says("never"),
        )
        .redacting(secret);
    let backend = FakeBackend::new(&clock)
        .script(call_turn(&[
            ("call_0", "read", READ_ARGS),
            ("call_0", "read", READ_ARGS),
            ("c2", "write", "{}"),
            ("c3", "shell", "{}"),
            ("c4", "unknown_tool", "{}"),
        ]))
        .script(text_turn("done"));
    let result = run(&backend, &host, base()).await.unwrap();
    assert_eq!(result.iterations, 2);
    assert_eq!(host.state.max_active.load(Ordering::SeqCst), 2);
    let statuses: Vec<_> = host.results().into_iter().map(|(_, s)| s).collect();
    assert_eq!(
        statuses,
        [
            ToolCallStatus::Ok,
            ToolCallStatus::Ok,
            ToolCallStatus::Ok,
            ToolCallStatus::Blocked,
            ToolCallStatus::Error,
        ]
    );
    let first_id = &host.results()[0].0;
    assert!(!host.result_text(first_id).unwrap().contains(secret));
    assert_eq!(host.audit_states("c3").last(), Some(&"output-denied"));
}

fn edited(arguments: &str) -> ApprovalOutcome {
    ApprovalOutcome::Answered(ApprovalAnswer {
        decision: ApprovalDecision::Allow,
        scope: ApprovalScope::Once,
        message: None,
        edited_arguments: Some(arguments.into()),
        allow_network_once: false,
    })
}

fn verdict_by_arguments(call: &super::ToolCall) -> Verdict {
    if call.arguments.contains("bad") {
        Verdict::Invalid("outside the folder".into())
    } else if call.arguments.contains("never") {
        Verdict::Refuse("blocked".into())
    } else if call.arguments.contains("edited") {
        Verdict::Run
    } else {
        ask("edits files")
    }
}

#[tokio::test]
async fn a_call_that_cannot_run_is_audited_without_a_decider() {
    let clock = ManualClock::new();
    let host = FakeHost::new(&clock).say(AgentRole::User, "hi").tool_with(
        "shell",
        verdict_by_arguments,
        says("ran"),
    );
    let backend = FakeBackend::new(&clock)
        .script(call_turn(&[
            ("bad", "shell", r#"{"command":"bad"}"#),
            ("never", "shell", r#"{"command":"never"}"#),
        ]))
        .script(text_turn("done"));
    run(&backend, &host, base()).await.unwrap();
    let audits = host.state.audits.lock().unwrap();
    let decided_by = |id: &str| {
        audits
            .iter()
            .rev()
            .find(|audit| audit.call_id == id)
            .unwrap()
            .decided_by
            .clone()
    };
    // Invalid arguments were decided by no policy; a refusal still names the mode.
    assert_eq!(decided_by("bad"), None);
    assert_eq!(decided_by("never").as_deref(), Some("mode"));
}

#[tokio::test]
async fn the_exit_status_a_tool_reports_lands_on_the_audit_row_of_a_call_that_ran() {
    let clock = ManualClock::new();
    let exits: Behavior = Arc::new(|_, call| {
        async move {
            let code = if call.arguments.contains("fail") {
                2
            } else {
                0
            };
            ToolOutput::error("[exit code]").with_exit_code(Some(code))
        }
        .boxed()
    });
    let host = FakeHost::new(&clock).say(AgentRole::User, "hi").tool_with(
        "shell",
        |_| Verdict::Run,
        exits,
    );
    let backend = FakeBackend::new(&clock)
        .script(call_turn(&[
            ("failed", "shell", r#"{"command":"fail"}"#),
            ("passed", "shell", r#"{"command":"ok"}"#),
        ]))
        .script(text_turn("done"));
    run(&backend, &host, base()).await.unwrap();
    let audits = host.state.audits.lock().unwrap();
    let exit_of = |id: &str| {
        audits
            .iter()
            .rev()
            .find(|audit| audit.call_id == id)
            .unwrap()
            .exit_code
    };
    assert_eq!((exit_of("failed"), exit_of("passed")), (Some(2), Some(0)));
    // The rows written before the call finished carry none.
    assert!(
        audits
            .iter()
            .filter(|audit| audit.state == "input-available")
            .all(|audit| audit.exit_code.is_none())
    );
}

#[tokio::test]
async fn tool_approvals_denials_timeouts_edited_args_and_max_iterations() {
    let clock = ManualClock::new();
    let denied = ApprovalOutcome::Answered(ApprovalAnswer {
        decision: ApprovalDecision::Deny,
        scope: ApprovalScope::Once,
        message: Some("use staging".into()),
        edited_arguments: None,
        allow_network_once: false,
    });
    let host = FakeHost::new(&clock)
        .say(AgentRole::User, "hi")
        .tool_with("shell", verdict_by_arguments, says("ran"))
        .approve_with(allow())
        .approve_with(denied)
        .approve_with(ApprovalOutcome::TimedOut)
        .approve_with(edited(r#"{"command":"edited"}"#))
        .approve_with(edited(r#"{"command":"bad"}"#))
        .approve_with(edited(r#"{"command":"never"}"#));
    let backend = FakeBackend::new(&clock)
        .script(call_turn(&[
            ("c1", "shell", r#"{"command":"orig"}"#),
            ("c2", "shell", r#"{"command":"orig"}"#),
            ("c3", "shell", r#"{"command":"orig"}"#),
            ("c4", "shell", r#"{"command":"orig"}"#),
            ("c5", "shell", r#"{"command":"orig"}"#),
            ("c6", "shell", r#"{"command":"orig"}"#),
        ]))
        .script(call_turn(&[("c7", "shell", r#"{"command":"edited"}"#)]));
    let mut inp = input();
    inp.max_iterations = 2;
    inp.wrap_up_last_step = false;
    let res = run(&backend, &host, &inp).await.unwrap();
    assert_eq!(res.stop_reason, AgentStopReason::MaxIterations);
    assert_eq!(host.state.approvals_asked.lock().unwrap().len(), 6);
    assert_eq!(
        host.results()
            .into_iter()
            .map(|(_, s)| s)
            .collect::<Vec<_>>(),
        [
            ToolCallStatus::Ok,
            ToolCallStatus::Denied,
            ToolCallStatus::ApprovalTimedOut,
            ToolCallStatus::Ok,
            ToolCallStatus::Error,
            ToolCallStatus::Blocked,
            ToolCallStatus::Ok,
        ]
    );
    assert_eq!(
        host.result_text("c3").as_deref(),
        Some(approval_timed_out_text(crate::agent::limits::APPROVAL_TIMEOUT_S).as_str())
    );
}

#[tokio::test]
async fn steering_cancellation_and_crash_recovery() {
    let clock = ManualClock::new();
    let steer_when_run: Behavior = Arc::new(|state, _| {
        async move {
            state
                .steer
                .lock()
                .unwrap()
                .push("actually use b".to_owned());
            ToolOutput::text("file body")
        }
        .boxed()
    });
    let orphan = Stored {
        id: "a1".into(),
        role: AgentRole::Assistant,
        parts: vec![HubPart::ToolCall {
            id: "c9".into(),
            name: "read".into(),
            arguments: "{}".into(),
            signature: None,
            origin: None,
        }],
        partial: false,
        error: None,
        steered: false,
        failure: None,
    };
    let host = FakeHost::new(&clock)
        .say(AgentRole::User, "hi")
        .push_messages(vec![orphan])
        .say(AgentRole::User, "continue")
        .tool("read", ToolClass::Read, true, Verdict::Run, steer_when_run);
    let backend = FakeBackend::new(&clock)
        .script(call_turn(&[("c1", "read", READ_ARGS)]))
        .script(text_turn("using b"));
    run(&backend, &host, base()).await.unwrap();
    assert_eq!(
        last_request_texts(&backend).last().cloned(),
        Some((HubRole::User, "actually use b".to_owned()))
    );

    // Cancellation of a running tool answers remaining calls as Cancelled
    let hang: Behavior = Arc::new(|_, _| std::future::pending().boxed());
    let cancel_host = FakeHost::new(&clock).say(AgentRole::User, "hi").tool(
        "shell",
        ToolClass::Exec,
        false,
        Verdict::Run,
        hang,
    );
    let cancel_backend =
        FakeBackend::new(&clock).script(call_turn(&[("c1", "shell", "{}"), ("c2", "shell", "{}")]));
    let cancel = CancelToken::new();
    let (res, ()) = tokio::join!(
        run_cancellable(&cancel_backend, &cancel_host, base(), &cancel),
        async {
            until(|| !cancel_host.ran().is_empty()).await;
            cancel.cancel();
            until(|| clock.waiting() > 0).await;
            clock.advance(CANCEL_GRACE);
        }
    );
    assert!(res.unwrap_err().is_cancelled());
    assert_eq!(
        cancel_host.results(),
        vec![
            ("c1".to_owned(), ToolCallStatus::Cancelled),
            ("c2".to_owned(), ToolCallStatus::Cancelled),
        ]
    );
}

#[tokio::test]
async fn stalls_errors_and_context_compaction() {
    let clock = ManualClock::new();
    let backend = FakeBackend::new(&clock)
        .script(vec![start(), Step::Hang])
        .script(vec![context_length_failure()])
        .script(text_turn("now it fits"))
        .summary(Ok("the summary"));
    let mut host = FakeHost::new(&clock);
    // Long enough that the summary is shorter than what it replaces (else it is not stored).
    for turn in 0..4 {
        host = host
            .say(AgentRole::User, &format!("q {turn} {}", "x".repeat(200)))
            .say(AgentRole::Assistant, &format!("a {turn}"));
    }
    let host = host.say(AgentRole::User, "current");
    let mut inp = input();
    inp.compaction.keep_turns = 1;
    let (res, ()) = tokio::join!(run(&backend, &host, &inp), async {
        until(|| clock.waiting() > 0).await;
        clock.advance(STALL_TIMEOUT);
    });
    assert_eq!(res.unwrap().iterations, 1);
    assert_eq!(backend.summaries.lock().unwrap().len(), 1);
    assert_eq!(backend.request_count(), 3);
    assert!(has_event(&host, |e| matches!(
        e,
        AgentEvent::ContextCompacted { .. }
    )));

    // Failed walk records attempt summaries on run failure
    let failed = ChatError::new(
        ChatErrorKind::Failed,
        AppError::network("every upstream attempt failed; last: 503"),
    )
    .with_attempts(vec![AttemptSummary {
        target: "A/m".to_owned(),
        reason: "429".to_owned(),
        transient: Some(false),
    }]);
    let fail_backend = FakeBackend::new(&clock).script(vec![Step::Fail(failed)]);
    let fail_host = FakeHost::new(&clock).say(AgentRole::User, "hi");
    assert!(run(&fail_backend, &fail_host, base()).await.is_err());
    assert_eq!(
        fail_host
            .state
            .failures
            .lock()
            .unwrap()
            .pop()
            .unwrap()
            .targets[0]
            .target,
        "A/m"
    );
}

#[tokio::test]
async fn an_assistant_message_records_the_effort_its_call_sent() {
    // (session effort, the model has a reasoning control) -> what the message records
    for (effort, reasoning, recorded) in [
        (
            Some(ReasoningEffort::High),
            true,
            Some(ReasoningEffort::High),
        ),
        (Some(ReasoningEffort::High), false, None),
        (None, true, None),
    ] {
        let clock = ManualClock::new();
        let backend = FakeBackend::new(&clock).script(text_turn("ok"));
        let host = FakeHost::new(&clock).say(AgentRole::User, "hi");
        let mut inp = input();
        inp.reasoning = effort;
        inp.model
            .capabilities
            .apply(Capability::Reasoning, reasoning, CapabilitySource::Declared);
        run(&backend, &host, &inp).await.unwrap();
        assert_eq!(*host.state.saved_efforts.lock().unwrap(), vec![recorded]);
    }
}

#[tokio::test]
async fn pricing_served_models_and_per_currency_cost_caps() {
    let clock = ManualClock::new();
    let mut served = call_turn(&[("c1", "read", READ_ARGS)]);
    served.insert(
        0,
        Step::Event(HubEvent::Served {
            provider_id: "backup".into(),
            model: "m2".into(),
            attempts: Vec::new(),
        }),
    );
    let backend = FakeBackend::new(&clock)
        .script(served)
        .script(call_turn(&[("c2", "read", READ_ARGS)]))
        .script(text_turn("never reached"));
    let host = read_host(&clock)
        .cost_decision(Some(CostDecision::Continue))
        .cost_decision(Some(CostDecision::Stop));
    host.state
        .other_spend
        .lock()
        .unwrap()
        .push(("CNY".to_owned(), 15_000_000));
    host.state.served_models.lock().unwrap().insert(
        "backup/m2".into(),
        ModelInfo {
            price: PriceResolution::Known(Price {
                input_per_mtok: 1_000_000.0,
                output_per_mtok: 1_000_000.0,
                cache_read_per_mtok: None,
                cache_write_per_mtok: None,
                reasoning_per_mtok: None,
                currency: "USD".into(),
                source: PriceSource::Override,
                per_call: None,
            }),
            ..ModelInfo::default()
        },
    );
    let mut inp = input();
    let mut primary = host.state.served_models.lock().unwrap()["backup/m2"]
        .price
        .known()
        .cloned()
        .unwrap();
    primary.input_per_mtok /= 2.0;
    primary.output_per_mtok /= 2.0;
    inp.model.price = PriceResolution::Known(primary);
    inp.cost_caps = vec![
        CostCap {
            micro: 10_000_000,
            currency: "CNY".into(),
        },
        CostCap {
            micro: 30_000_000,
            currency: "USD".into(),
        },
    ];
    let result = run(&backend, &host, &inp).await.unwrap();
    assert_eq!(result.stop_reason, AgentStopReason::CostLimit);
    assert_eq!(
        *host.state.cost_pauses.lock().unwrap(),
        vec![
            (15_000_000, 10_000_000, "CNY".to_owned()),
            (42_000_000, 30_000_000, "USD".to_owned()),
        ]
    );
    assert_eq!(
        *host.state.usage_currencies.lock().unwrap(),
        vec![Some("USD".to_owned()), Some("USD".to_owned())]
    );
}

#[tokio::test]
async fn tool_batches_cap_parallelism_and_serialize_unsafe_calls() {
    let calls: Vec<_> = (1..=10)
        .map(|n| (format!("c{n}"), "read", READ_ARGS))
        .collect();
    let refs: Vec<_> = calls
        .iter()
        .map(|(id, name, args)| (id.as_str(), *name, *args))
        .collect();
    for (parallel_safe, expected) in [(true, super::MAX_PARALLEL_CALLS), (false, 1)] {
        let clock = ManualClock::new();
        let busy: Behavior = Arc::new(|_, _| {
            async {
                for _ in 0..5 {
                    tokio::task::yield_now().await;
                }
                ToolOutput::text("ok")
            }
            .boxed()
        });
        let host = FakeHost::new(&clock).say(AgentRole::User, "hi").tool(
            "read",
            ToolClass::Read,
            parallel_safe,
            Verdict::Run,
            busy,
        );
        let backend = FakeBackend::new(&clock)
            .script(call_turn(&refs))
            .script(text_turn("done"));
        run(&backend, &host, base()).await.unwrap();
        assert_eq!(host.state.max_active.load(Ordering::SeqCst), expected);
        assert_eq!(
            host.results()
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            calls
                .iter()
                .map(|(id, _, _)| id.clone())
                .collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn cancelled_streams_keep_partial_text_and_steers_supersede_pending_calls() {
    let clock = ManualClock::new();
    let backend = FakeBackend::new(&clock).script(vec![start(), text("partial"), Step::Hang]);
    let host = FakeHost::new(&clock).say(AgentRole::User, "hi");
    let cancel = CancelToken::new();
    let (res, ()) = tokio::join!(run_cancellable(&backend, &host, base(), &cancel), async {
        until(|| texts(&host) == "partial").await;
        cancel.cancel();
    });
    assert!(res.unwrap_err().is_cancelled());
    let assistant = host.messages().pop().unwrap();
    assert_eq!(pingdeck_llm::llm::hub::text_of(&assistant.parts), "partial");
    assert!(!assistant.partial && assistant.error.is_some());

    let host = FakeHost::new(&clock)
        .say(AgentRole::User, "hi")
        .tool("shell", ToolClass::Exec, false, ask("x"), says("never"))
        .tool("read", ToolClass::Read, true, Verdict::Run, says("body"))
        .approve_with(ApprovalOutcome::Superseded);
    let backend = FakeBackend::new(&clock)
        .script(call_turn(&[
            ("c1", "shell", "{}"),
            ("c2", "read", READ_ARGS),
        ]))
        .script(text_turn("ok"));
    run(&backend, &host, base()).await.unwrap();
    assert!(host.ran().is_empty());
    assert_eq!(
        host.results(),
        vec![
            ("c1".to_owned(), ToolCallStatus::Superseded),
            ("c2".to_owned(), ToolCallStatus::Superseded)
        ]
    );
}

#[tokio::test]
async fn failed_streams_store_only_finished_prose_and_auto_compaction_precedes_the_call() {
    for provider_event in [false, true] {
        let clock = ManualClock::new();
        let mut steps = call_turn(&[("c1", "read", READ_ARGS)]);
        steps.pop();
        steps.push(if provider_event {
            Step::Event(HubEvent::Error {
                message: "overloaded".into(),
                kind: None,
            })
        } else {
            Step::Fail(ChatError::new(
                ChatErrorKind::MidStream,
                AppError::network("cut"),
            ))
        });
        let backend = FakeBackend::new(&clock).script(steps);
        let host = read_host(&clock);
        assert!(run(&backend, &host, base()).await.is_err());
        assert!(host.ran().is_empty());
        assert!(
            host.messages()
                .last()
                .unwrap()
                .parts
                .iter()
                .all(|part| matches!(part, HubPart::Text { .. }))
        );
    }
    let clock = ManualClock::new();
    let mut host = FakeHost::new(&clock);
    for _ in 0..4 {
        host = host
            .say(AgentRole::User, &"x".repeat(400))
            .say(AgentRole::Assistant, &"y".repeat(400));
    }
    let host = host.say(AgentRole::User, "current");
    let backend = FakeBackend::new(&clock)
        .script(text_turn("ok"))
        .summary(Ok("short"));
    let mut inp = input();
    inp.compaction = CompactionPolicy {
        auto: true,
        threshold_pct: 50,
        keep_turns: 1,
    };
    inp.model.context_limit = Some(1_000);
    run(&backend, &host, &inp).await.unwrap();
    assert_eq!(backend.summaries.lock().unwrap().len(), 1);
    assert_eq!(last_request_texts(&backend).len(), 2);
}

/// A walk that ended in transient failures (every target answered 502).
fn transient_502(retried: bool) -> Step {
    let mut error = ChatError::new(
        ChatErrorKind::Failed,
        AppError::network("every upstream attempt failed; last: 502"),
    );
    error.status = Some(502);
    error.retried = retried;
    Step::Fail(error.with_attempts(vec![AttemptSummary {
        target: "A/m".to_owned(),
        reason: "502".to_owned(),
        transient: Some(true),
    }]))
}

fn notice_args(host: &FakeHost, wanted: &str) -> Vec<std::collections::BTreeMap<String, String>> {
    host.events()
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::Notice {
                code: Some(code),
                args,
                ..
            } if code == wanted => Some(args.unwrap_or_default()),
            _ => None,
        })
        .collect()
}

// A transient failure before content is retried twice (2 s, then 6 s, ±25 %); an error the
// client already retried in place is not retried again.
#[tokio::test]
async fn transient_failures_are_retried_unless_the_client_already_did() {
    let clock = ManualClock::new();
    let backend = FakeBackend::new(&clock)
        .script(vec![transient_502(false)])
        .script(vec![transient_502(false)])
        .script(text_turn("ok"));
    let host = FakeHost::new(&clock).say(AgentRole::User, "hi");
    let (res, ()) = tokio::join!(run(&backend, &host, base()), async {
        for _ in 0..2 {
            until(|| clock.waiting() > 0).await;
            clock.advance(Duration::from_secs(8));
        }
    });
    assert_eq!(res.unwrap().stop_reason, AgentStopReason::Completed);
    assert_eq!(backend.request_count(), 3);
    let retries = notice_args(&host, "retrying");
    assert_eq!(retries.len(), 2);
    assert_eq!(retries[1]["attempt"], "2");
    assert_eq!(retries[1]["reason"], "HTTP 502");
    let first: u64 = retries[0]["delay_s"].parse().unwrap();
    let second: u64 = retries[1]["delay_s"].parse().unwrap();
    assert!(
        (2..=3).contains(&first) && (5..=8).contains(&second),
        "{retries:?}"
    );
    assert_eq!(
        super::stream::retry_delay(1, None, 0),
        Duration::from_millis(1_500)
    );
    assert_eq!(
        super::stream::retry_delay(2, None, 500),
        Duration::from_millis(7_500)
    );
    assert_eq!(
        super::stream::retry_delay(1, Some(90_000), 0),
        Duration::from_secs(30)
    );

    let backend = FakeBackend::new(&clock).script(vec![transient_502(true)]);
    let host = FakeHost::new(&clock).say(AgentRole::User, "hi");
    assert!(run(&backend, &host, base()).await.is_err());
    assert_eq!(backend.request_count(), 1);
    assert!(notice_args(&host, "retrying").is_empty());
}

// The last step keeps the tool definitions but forbids calls and asks for a summary that is
// never stored; a call made anyway is not run and the run ends at the step limit.
#[tokio::test]
async fn the_last_step_forbids_tool_calls_and_ends_at_the_step_limit() {
    let clock = ManualClock::new();
    let backend = FakeBackend::new(&clock)
        .script(call_turn(&[("c1", "read", READ_ARGS)]))
        .script(call_turn(&[("c2", "read", READ_ARGS)]));
    let host = read_host(&clock);
    let mut inp = input();
    inp.max_iterations = 2;
    inp.tools = vec![ToolSpec {
        name: "read".into(),
        description: "reads".into(),
        input_schema: json!({"type": "object"}),
        read_only: true,
        destructive: false,
        source: crate::agent::tools::ToolSource::Builtin,
        class: ToolClass::Read,
        parallel_safe: true,
    }];
    let res = run(&backend, &host, &inp).await.unwrap();
    assert_eq!(res.stop_reason, AgentStopReason::MaxIterations);
    {
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests[0].tool_choice, None);
        let last = &requests[1];
        assert_eq!(
            last.tool_choice,
            Some(pingdeck_llm::llm::hub::HubToolChoice::None)
        );
        assert_eq!(last.tools.len(), 1);
        assert_eq!(last.messages.last().unwrap().text(), super::WRAP_UP_PROMPT);
    }
    assert_eq!(host.ran().len(), 1);
    assert_eq!(
        host.results().last().cloned(),
        Some(("c2".to_owned(), ToolCallStatus::Cancelled))
    );
    assert_eq!(
        host.result_text("c2").as_deref(),
        Some(super::STEP_LIMIT_TEXT)
    );
    assert!(host.messages().iter().all(|m| {
        !pingdeck_llm::llm::hub::text_of(&m.parts).contains("This is your last step")
    }));
    assert_eq!(notice_args(&host, "step_limit_summary").len(), 1);
}

// At the output cap, a call whose arguments were cut off is answered without running; the
// complete calls before it run, and the run ends at the output limit (the queue then pauses).
#[tokio::test]
async fn a_call_cut_off_by_the_output_cap_is_not_run() {
    let clock = ManualClock::new();
    let mut steps = call_turn(&[
        ("c1", "read", READ_ARGS),
        ("c2", "read", r#"{"path":"b","conte"#),
    ]);
    steps.pop();
    steps.push(finish(HubFinish::Length));
    let backend = FakeBackend::new(&clock).script(steps);
    let host = read_host(&clock);
    let res = run(&backend, &host, base()).await.unwrap();
    assert_eq!(res.stop_reason, AgentStopReason::OutputLimit);
    assert!(res.stop_reason.pauses_queue());
    assert_eq!(host.ran(), [("read".to_owned(), READ_ARGS.to_owned())]);
    assert_eq!(
        host.result_text("c2").as_deref(),
        Some(super::TRUNCATED_CALL_TEXT)
    );
    assert_eq!(backend.request_count(), 1);
}

// The third identical call with the same result (arguments in any key order or spacing)
// ends with a nudge at the end of its own result; the fifth ends the run as `NoProgress`, its
// result stored, and the queue waits.
#[tokio::test]
async fn the_third_identical_call_ends_with_a_nudge_and_the_fifth_stops_the_run() {
    let clock = ManualClock::new();
    let backend = FakeBackend::new(&clock)
        .script(call_turn(&[("c1", "read", r#"{"path":"a","n":1}"#)]))
        .script(call_turn(&[("c2", "read", r#"{ "n": 1, "path": "a" }"#)]))
        .script(call_turn(&[("c3", "read", r#"{"path":"a","n":1}"#)]))
        .script(call_turn(&[("c4", "read", r#"{"path":"a","n":1}"#)]))
        .script(call_turn(&[("c5", "read", r#"{"path":"a","n":1}"#)]))
        .script(text_turn("never reached"));
    let host = read_host(&clock);
    let result = run(&backend, &host, base()).await.unwrap();
    assert_eq!(result.stop_reason, AgentStopReason::NoProgress);
    assert!(result.stop_reason.pauses_queue());
    assert_eq!(backend.request_count(), 5);
    assert_eq!(host.result_text("c5").as_deref(), Some("file body"));
    assert_eq!(host.result_text("c2").as_deref(), Some("file body"));
    let third = host.result_text("c3").unwrap();
    assert!(third.starts_with("file body"), "{third}");
    assert!(
        third.contains("You called read with the same arguments 3 times"),
        "{third}"
    );
    assert_eq!(notice_args(&host, "no_progress")[0]["signal"], "repeat");
}

#[test]
fn stop_reasons_that_leave_the_queue_waiting() {
    let paused: Vec<_> = [
        AgentStopReason::Completed,
        AgentStopReason::MaxIterations,
        AgentStopReason::CostLimit,
        AgentStopReason::OutputLimit,
        AgentStopReason::NoProgress,
    ]
    .into_iter()
    .filter(|reason| reason.pauses_queue())
    .collect();
    assert_eq!(
        paused,
        [
            AgentStopReason::MaxIterations,
            AgentStopReason::CostLimit,
            AgentStopReason::OutputLimit,
            AgentStopReason::NoProgress
        ]
    );
}

// Scenario "30 KiB of shell output, 32K window": the model sees the start and a
// `read_output` handle; the kept full text is redacted and starts with exactly the shown bytes;
// the audit row names the handle.
#[tokio::test]
async fn a_cut_result_is_kept_redacted_and_the_model_gets_a_handle() {
    let clock = ManualClock::new();
    let secret = "sk-live-0123456789abcdef";
    let body: &'static str =
        Box::leak(format!("{secret}\n{}", "x".repeat(30 * 1024)).into_boxed_str());
    let cap = crate::agent::context::result_cap_bytes(Some(32_000));
    let host = FakeHost::new(&clock)
        .say(AgentRole::User, "build it")
        .tool("shell", ToolClass::Exec, false, Verdict::Run, says(body))
        .tool("small", ToolClass::Read, false, Verdict::Run, says("short"))
        .redacting(secret)
        .capped(cap);
    let backend = FakeBackend::new(&clock)
        .script(call_turn(&[("c1", "shell", "{}"), ("c2", "small", "{}")]))
        .script(text_turn("done"));
    run(&backend, &host, base()).await.unwrap();

    let seen = host.result_text("c1").unwrap();
    assert!(seen.len() <= cap, "{} > {cap}", seen.len());
    assert!(!seen.contains(secret));
    let kept = host.state.kept.lock().unwrap().clone();
    assert_eq!(kept.len(), 1, "only the cut result is kept");
    let (handle, full) = &kept[0];
    assert_eq!(handle, "c1");
    assert!(!full.contains(secret) && full.len() > 30 * 1024);
    let (shown, marker) = seen.rsplit_once('\n').unwrap();
    let (head, rest) = shown.split_once("\n[… ").unwrap();
    let (_, tail) = rest.split_once(" bytes omitted …]\n").unwrap();
    assert!(full.starts_with(head));
    assert!(full.ends_with(tail));
    assert_eq!(
        marker,
        crate::agent::tool_output_files::truncation_marker(
            "c1",
            head.len() as u64,
            full.len() as u64
        )
    );
    let audits = host.state.audits.lock().unwrap();
    let last = audits
        .iter()
        .rev()
        .find(|audit| audit.call_id == "c1")
        .unwrap();
    assert!(last.truncated);
    assert_eq!(last.spill_ref.as_deref(), Some("c1"));
    assert_eq!(last.output_bytes, full.len() as u64);
    let small = audits
        .iter()
        .rev()
        .find(|audit| audit.call_id == "c2")
        .unwrap();
    assert!(!small.truncated && small.spill_ref.is_none());
}

// The run's model reads images and has a large window; it fails before any content
// and a fallback that reads no images and has a small window answers. The fallback's request
// carries a note instead of the image and the turn's tool results cut to its window.
#[tokio::test]
async fn a_fallback_gets_the_request_shaped_for_its_own_facts() {
    use pingdeck_llm::llm::capabilities::ModelCapabilities;
    use pingdeck_llm::llm::client::TargetFacts;
    use pingdeck_llm::llm::hub::{HubImage, HubMessage};

    let clock = ManualClock::new();
    let mut small = ModelCapabilities::default();
    small.apply(Capability::Vision, false, CapabilitySource::UserOverride);
    let backend = FakeBackend::new(&clock)
        .script(call_turn(&[("c1", "read", READ_ARGS)]))
        .script_fallback(
            TargetFacts {
                provider_id: "backup".into(),
                model: "small".into(),
                context_limit: Some(8_000),
                capabilities: small,
            },
            text_turn("done"),
        );
    let body: &'static str = Box::leak("x".repeat(40_000).into_boxed_str());
    let host = FakeHost::new(&clock)
        .push_messages(vec![Stored {
            id: "u1".into(),
            role: AgentRole::User,
            parts: vec![
                HubPart::text("what is in it?"),
                HubPart::Image {
                    image: HubImage::Url {
                        url: "https://example.test/x.png".into(),
                    },
                },
            ],
            partial: false,
            error: None,
            steered: false,
            failure: None,
        }])
        .tool("read", ToolClass::Read, true, Verdict::Run, says(body));
    let mut inp = input();
    inp.model.context_limit = Some(1_000_000);
    inp.model
        .capabilities
        .apply(Capability::Vision, true, CapabilitySource::UserOverride);
    let result = run(&backend, &host, &inp).await.unwrap();
    assert_eq!(result.stop_reason, AgentStopReason::Completed);

    let images = |messages: &[HubMessage]| {
        messages
            .iter()
            .flat_map(|message| &message.content)
            .filter(|part| matches!(part, HubPart::Image { .. }))
            .count()
    };
    let requests = backend.requests.lock().unwrap();
    assert_eq!(images(&requests[0].messages), 1, "the run's model reads it");
    assert_eq!(images(&requests[1].messages), 0);
    let fallback = &requests[1].messages;
    assert!(
        pingdeck_llm::llm::hub::text_of(&fallback[0].content)
            .contains("[Image omitted: small cannot read images.]")
    );
    let result_bytes: usize = fallback
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|part| match part {
            HubPart::ToolResult { content, .. } => {
                Some(pingdeck_llm::llm::hub::text_of(content).len())
            }
            _ => None,
        })
        .sum();
    assert!(
        result_bytes < 9_000,
        "{result_bytes} bytes for an 8k window"
    );
    assert_eq!(host.result_text("c1").unwrap().len(), 40_000);
    assert_eq!(notice_args(&host, "fallback")[0]["to"], "small");
}

// `max_run_tokens` pauses like a cost cap and moves on by one cap per Continue;
// every model call carries the run key and its own step's message id.
#[tokio::test]
async fn the_token_cap_pauses_the_run_and_each_call_carries_the_run_key() {
    let clock = ManualClock::new();
    let backend = FakeBackend::new(&clock)
        .script(call_turn(&[("c1", "read", READ_ARGS)]))
        .script(call_turn(&[("c2", "read", READ_ARGS)]))
        .script(text_turn("never reached"));
    let host = read_host(&clock)
        .cost_decision(Some(CostDecision::Continue))
        .cost_decision(Some(CostDecision::Stop));
    let mut inp = input();
    inp.max_run_tokens = Some(25);
    let result = run(&backend, &host, &inp).await.unwrap();
    assert_eq!(result.stop_reason, AgentStopReason::CostLimit);
    assert_eq!(
        *host.state.cost_pauses.lock().unwrap(),
        vec![(28, 25, "tokens".to_owned()), (56, 50, "tokens".to_owned())]
    );
    let metas = backend.metas.lock().unwrap();
    assert_eq!(metas.len(), 2);
    assert!(metas.iter().all(|meta| meta.run_id == "run-1"
        && meta.purpose == pingdeck_llm::llm::client::CallPurpose::Step
        && meta.message_id.is_some()));
    assert_ne!(metas[0].message_id, metas[1].message_id);
}
