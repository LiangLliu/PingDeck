//! AI requests in collection runs, flow steps and draft sends: they run through the AI
//! run service, carry model, finish reason and cost, and are judged by their HTTP assertions.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::support::{Fx, WS, body_for, pong, upstream};
use crate::app::{
    ExecuteDraftRequestInput, RunCollectionInput, flow_run_impl, flow_save_impl,
    request_execute_draft_impl, run_collection_impl,
};
use crate::events::NoEvents;
use crate::llm::usage::PriceOverride;
use crate::llm::wire::Protocol;
use crate::workspace::models::{AiStepSummary, HistoryCheckSummary, HistoryEntryId};
use crate::workspace::{Assertion, Collection, Flow, FlowStep, Request, WorkspaceId};

const COLLECTION: &str = "col_ai";

/// Files `request` in the fixture collection, with one assertion that passes and one that fails.
fn file(fx: &Fx, request: &mut Request) {
    let collection = Collection::new(COLLECTION, WS, "AI evals").unwrap();
    let store = fx.state.store();
    store.save_collection(&collection).unwrap();
    request.collection_id = Some(collection.id);
    request.assertions = vec![
        Assertion::StatusEquals { value: 200 },
        Assertion::StatusEquals { value: 201 },
    ];
    store.save_request(request).unwrap();
}

fn run_input() -> RunCollectionInput {
    serde_json::from_value(serde_json::json!({"workspace_id": WS, "collection_id": COLLECTION}))
        .unwrap()
}

/// What one entry point reports of the AI request: status, AI summary, assertion verdicts and
/// the history row it wrote.
struct Seen {
    status: Option<u16>,
    ai: Option<AiStepSummary>,
    verdicts: Vec<bool>,
    passed: bool,
    history: Option<String>,
}

async fn through_runner(fx: &Fx, _request: &Request) -> Seen {
    let result = run_collection_impl(&fx.state, run_input(), None, &NoEvents)
        .await
        .unwrap();
    let item = result.items.into_iter().next().unwrap();
    Seen {
        status: item.status_code,
        ai: item.ai,
        verdicts: item.assertion_results.iter().map(|a| a.passed).collect(),
        passed: item.passed,
        history: item.history_entry_id,
    }
}

async fn through_flow(fx: &Fx, request: &Request) -> Seen {
    let step: FlowStep = serde_json::from_value(serde_json::json!({
        "id": "s1", "name": "Ask", "delay_ms": 0, "continue_on_error": false, "position": 0,
        "request_id": request.id.as_str(),
    }))
    .unwrap();
    let flow = Flow {
        id: "flow_ai".into(),
        workspace_id: WorkspaceId::new(WS).unwrap(),
        name: "AI flow".into(),
        description: String::new(),
        steps: vec![step],
        updated_at_ms: 0,
        options: Default::default(),
    };
    flow_save_impl(&fx.state, flow).unwrap();
    let result = flow_run_impl(
        &fx.state,
        WS.into(),
        "flow_ai".into(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        &NoEvents,
    )
    .await
    .unwrap();
    let step = result.steps.into_iter().next().unwrap();
    Seen {
        status: step.status_code,
        ai: step.ai,
        verdicts: step.assertion_results.iter().map(|a| a.passed).collect(),
        passed: step.passed,
        history: step.history_entry_id,
    }
}

async fn through_draft(fx: &Fx, request: &Request) -> Seen {
    let draft = serde_json::from_value(serde_json::to_value(request).unwrap()).unwrap();
    let input = ExecuteDraftRequestInput {
        request: draft,
        ..Default::default()
    };
    let response = request_execute_draft_impl(&fx.state, input, None)
        .await
        .unwrap();
    Seen {
        status: Some(response.status_code),
        ai: response.ai,
        verdicts: response
            .assertion_results
            .iter()
            .map(|a| a.passed)
            .collect(),
        passed: response.assertion_results.iter().all(|a| a.passed),
        history: response.history_id,
    }
}

fn price(provider: &str) -> PriceOverride {
    PriceOverride {
        provider_id: provider.to_owned(),
        model_id: "m".to_owned(),
        input_per_mtok: 1.0,
        output_per_mtok: 2.0,
        cache_read_per_mtok: None,
        cache_write_per_mtok: None,
        reasoning_per_mtok: None,
        currency: "USD".to_owned(),
        updated_at_ms: 0,
    }
}

#[tokio::test]
async fn runner_flow_and_draft_send_run_an_ai_request_and_judge_it_by_its_assertions() {
    type Entry = for<'a> fn(
        &'a Fx,
        &'a Request,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Seen> + 'a>>;
    let entries: [(&str, Entry); 3] = [
        ("runner", |fx, request| {
            Box::pin(through_runner(fx, request))
        }),
        ("flow", |fx, request| Box::pin(through_flow(fx, request))),
        ("draft", |fx, request| Box::pin(through_draft(fx, request))),
    ];
    for (name, entry) in entries {
        let fake = upstream(Protocol::OpenaiChat, pong()).await;
        let fx = Fx::new();
        let provider = fx.chat_provider("Relay", fake.port);
        fx.save_price(price(&provider));
        let body = body_for(Protocol::OpenaiChat, "m", false);
        let mut request = fx.provider_request("r1", &provider, Protocol::OpenaiChat, "m", &body);
        file(&fx, &mut request);

        let seen = entry(&fx, &request).await;

        assert_eq!(fake.requests().len(), 1, "{name}");
        assert_eq!(seen.status, Some(200), "{name}");
        let ai = seen.ai.unwrap_or_else(|| panic!("{name}: no AI summary"));
        assert_eq!(ai.model, "m", "{name}");
        assert_eq!(ai.finish_reason.as_deref(), Some("stop"), "{name}");
        assert!(ai.cost_micro.is_some_and(|micro| micro > 0), "{name}");
        assert_eq!(ai.cost_currency.as_deref(), Some("USD"), "{name}");
        assert_eq!(ai.error, None, "{name}");
        assert_eq!(seen.verdicts, [true, false], "{name}");
        assert!(!seen.passed, "{name}");
        // The history row counts the failed assertion.
        let id = HistoryEntryId::new(seen.history.unwrap()).unwrap();
        let entry = fx
            .state
            .store()
            .get_execution_history(WS, &id)
            .unwrap()
            .unwrap();
        assert_eq!(
            entry.check_summary,
            Some(HistoryCheckSummary {
                passed: 1,
                failed: 1
            }),
            "{name}"
        );
    }
}

#[tokio::test]
async fn an_unreachable_ai_request_fails_its_runner_row_and_links_the_failure_row() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let fx = Fx::new();
    let provider = fx.chat_provider("Relay", port);
    let body = body_for(Protocol::OpenaiChat, "m", false);
    let mut request = fx.provider_request("r1", &provider, Protocol::OpenaiChat, "m", &body);
    file(&fx, &mut request);

    let result = run_collection_impl(&fx.state, run_input(), None, &NoEvents)
        .await
        .unwrap();

    assert_eq!((result.total, result.failed), (1, 1));
    let item = &result.items[0];
    assert_eq!(item.status_code, None);
    let error = item.error.as_deref().unwrap();
    assert!(error.starts_with("Cannot reach 127.0.0.1"), "{error}");
    let id = HistoryEntryId::new(item.history_entry_id.as_deref().unwrap()).unwrap();
    assert!(
        fx.state
            .store()
            .get_execution_history(WS, &id)
            .unwrap()
            .is_some()
    );
}
