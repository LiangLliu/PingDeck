//! The stored plan of a run: it must survive `summary_json` unchanged and read stored rows back.

use super::super::spec::{RunDoc, RunSpec};
use crate::llm::probe::ProbeStatus;
use crate::llm::probe::listing::ModelKind;
use crate::llm::probe::plan::{CheckSelection, CheckSet, PlanItem, SkipReason, plan_run};
use crate::llm::store::{ProbeResult, ProbeRunRow};
use crate::llm::wire::Protocol;

fn item(key: &str, model: &str, kind: ModelKind) -> PlanItem {
    PlanItem {
        provider_id: "p".to_owned(),
        key_id: key.to_owned(),
        model_id: model.to_owned(),
        kind,
        protocols: vec![Protocol::OpenaiChat],
    }
}

fn items() -> Vec<PlanItem> {
    vec![
        item("k1", "chat", ModelKind::Chat),
        item("k1", "embed", ModelKind::Embedding),
        item("k2", "image", ModelKind::Image),
    ]
}

fn row(key: &str, model: &str, test: &str, word: &str, skip: Option<&str>) -> ProbeResult {
    ProbeResult {
        key_id: key.to_owned(),
        model_id: model.to_owned(),
        test: test.to_owned(),
        status_word: word.to_owned(),
        skip_reason: skip.map(str::to_owned),
        message: format!("{test} {word}"),
        ..ProbeResult::default()
    }
}

#[test]
fn run_spec_and_doc_round_trip_and_convert_stored_rows() {
    let items = items();
    let selection = CheckSelection::Set {
        set: CheckSet::Quick,
    };
    let plan = plan_run(&items, &selection);
    let spec = RunSpec::new(&items, &plan, Some(Protocol::Gemini));
    assert!(!spec.is_empty() && RunSpec::default().is_empty());
    assert!(spec.is_embedding("k1", "embed") && !spec.is_embedding("k1", "chat"));

    let doc = RunDoc {
        spec: spec.clone(),
        totals: None,
    };
    let run_row = ProbeRunRow {
        run_id: "r".to_owned(),
        provider_id: None,
        check_set: "quick".to_owned(),
        started_at_ms: 0,
        finished_at_ms: None,
        passed: 0,
        total: 0,
        summary: doc.to_value(),
        previous_run_id: None,
    };
    let back = RunDoc::of(&run_row).spec;
    assert_eq!(back, spec);
    assert_eq!(back.plan(), plan);

    let rows = [
        row("k1", "chat", "chat", "rejected", None),
        row("k1", "chat", "stream", "skipped", Some("chat_failed")),
        row("k9", "chat", "chat", "ready", None),
    ];
    let prev = spec.previous_cells(&rows);
    assert_eq!(prev.len(), 2);
    assert_eq!(prev[0].status, ProbeStatus::Rejected);
    assert_eq!(prev[1].skip, Some(SkipReason::ChatFailed));

    let recorded = spec.recorded_cells(&rows);
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].provider_id, "p");
}

#[test]
fn planned_protocols_exclude_disabled_endpoints() {
    let provider = crate::llm::store::Provider {
        id: "p".to_owned(),
        endpoints: vec![
            crate::llm::wire::ProviderEndpoint {
                protocol: Protocol::OpenaiChat,
                path_prefix: "/v1".to_owned(),
                auth: None,
                url_mode: Default::default(),
                enabled: true,
            },
            crate::llm::wire::ProviderEndpoint {
                protocol: Protocol::AnthropicMessages,
                path_prefix: "/v1".to_owned(),
                auth: None,
                url_mode: Default::default(),
                enabled: false,
            },
        ],
        ..crate::llm::store::Provider::default()
    };
    let mut snapshot = crate::llm::router::RoutingSnapshot::default();
    snapshot.providers.insert(provider.id.clone(), provider);
    let items = items();
    let spec = RunSpec::new(
        &items,
        &plan_run(
            &items,
            &CheckSelection::Set {
                set: CheckSet::Full,
            },
        ),
        None,
    );
    for row in spec.plan_items(&snapshot) {
        assert_eq!(row.protocols, [Protocol::OpenaiChat]);
    }
}
