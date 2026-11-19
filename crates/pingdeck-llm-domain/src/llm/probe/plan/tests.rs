use super::*;
use crate::llm::probe::test_support::{chat_item, embedding_item, plan_item, set};
use ProbeStatus as S;
use ProbeTest as T;

fn runnable(plan: &ProbePlan, row: usize) -> Vec<ProbeTest> {
    plan.rows[row]
        .cells
        .iter()
        .filter(|cell| cell.skip.is_none())
        .map(|cell| cell.test)
        .collect()
}

#[test]
fn quick_standard_and_full_for_a_chat_model() {
    let items = [chat_item("m")];
    let quick = plan_run(&items, &set(CheckSet::Quick));
    assert_eq!(quick.keys[0].checks, [T::Connectivity, T::Auth]);
    assert_eq!(runnable(&quick, 0), [T::Chat, T::Stream]);
    let standard = plan_run(&items, &set(CheckSet::Standard));
    assert_eq!(
        runnable(&standard, 0),
        [
            T::Chat,
            T::Stream,
            T::Tools,
            T::JsonSchema,
            T::Vision,
            T::Reasoning
        ]
    );
    // The 8 checks of the Standard set: 2 key-scoped + 6 model-scoped.
    assert_eq!(standard.runnable_cells(), 8);
    let full = plan_run(&items, &set(CheckSet::Full));
    assert_eq!(
        runnable(&full, 0),
        [
            T::Chat,
            T::Stream,
            T::Tools,
            T::JsonSchema,
            T::Vision,
            T::Reasoning,
            T::Cache,
            T::Search
        ]
    );
}

#[test]
fn pdf_is_planned_only_for_messages_and_gemini_providers() {
    for (protocol, expected) in [
        (Protocol::AnthropicMessages, true),
        (Protocol::Gemini, true),
        (Protocol::OpenaiChat, false),
        (Protocol::OpenaiResponses, false),
    ] {
        let items = [plan_item("p", "k", "m", ModelKind::Chat, &[protocol])];
        let plan = plan_run(&items, &set(CheckSet::Full));
        assert_eq!(
            runnable(&plan, 0).contains(&T::Pdf),
            expected,
            "{protocol:?}"
        );
        let standard = plan_run(&items, &set(CheckSet::Standard));
        assert!(!runnable(&standard, 0).contains(&T::Pdf));
        let custom = plan_run(
            &items,
            &CheckSelection::Custom {
                tests: vec![T::Pdf],
            },
        );
        assert_eq!(runnable(&custom, 0).contains(&T::Pdf), expected);
    }
}

#[test]
fn an_embedding_model_runs_three_checks_in_every_set() {
    let items = [embedding_item("e")];
    for chosen in [CheckSet::Quick, CheckSet::Standard, CheckSet::Full] {
        let plan = plan_run(&items, &set(chosen));
        assert_eq!(runnable(&plan, 0), [T::Embeddings], "{chosen:?}");
        // Connectivity, Auth and Embeddings: 3 checks.
        assert_eq!(plan.runnable_cells(), 3);
        for cell in &plan.rows[0].cells {
            if cell.test != T::Embeddings {
                assert_eq!(
                    cell.skip,
                    Some(SkipReason::EmbeddingModel),
                    "{:?}",
                    cell.test
                );
            }
        }
    }
    let standard = plan_run(&items, &set(CheckSet::Standard));
    assert_eq!(standard.rows[0].cells.len(), 7);
}

#[test]
fn a_decision_only_provider_runs_the_decision_probe_in_every_set() {
    let items = [plan_item(
        "p",
        "k",
        "jev-latest",
        ModelKind::Chat,
        &[Protocol::DecisionSystemone],
    )];
    for chosen in [CheckSet::Quick, CheckSet::Standard, CheckSet::Full] {
        let plan = plan_run(&items, &set(chosen));
        assert_eq!(runnable(&plan, 0), [T::Decision], "{chosen:?}");
        assert!(plan.rows[0].cells.iter().all(|cell| cell.skip.is_none()));
    }
    let two = [plan_item(
        "p",
        "k",
        "m",
        ModelKind::Chat,
        &[Protocol::DecisionSystemone, Protocol::DecisionOpenai],
    )];
    assert_eq!(
        runnable(&plan_run(&two, &set(CheckSet::Full)), 0),
        [T::Decision]
    );
}

#[test]
fn a_chat_model_of_a_chat_provider_runs_the_chat_sets() {
    for protocols in [
        vec![Protocol::OpenaiChat],
        vec![Protocol::OpenaiChat, Protocol::DecisionSystemone],
    ] {
        let items = [plan_item("p", "k", "m", ModelKind::Chat, &protocols)];
        for chosen in [CheckSet::Quick, CheckSet::Standard, CheckSet::Full] {
            let plan = plan_run(&items, &set(chosen));
            assert!(
                !runnable(&plan, 0).contains(&T::Decision),
                "{protocols:?} {chosen:?}"
            );
        }
        let standard = plan_run(&items, &set(CheckSet::Standard));
        assert_eq!(
            runnable(&standard, 0),
            [
                T::Chat,
                T::Stream,
                T::Tools,
                T::JsonSchema,
                T::Vision,
                T::Reasoning
            ]
        );
    }
}

#[test]
fn a_decision_model_of_a_chat_and_decision_provider_runs_only_the_decision_probe() {
    let protocols = [Protocol::OpenaiChat, Protocol::DecisionSystemone];
    for id in ["jev-latest", "typesafe/jev-1.14", "Kev-4B"] {
        let items = [plan_item("p", "k", id, ModelKind::Chat, &protocols)];
        for chosen in [CheckSet::Quick, CheckSet::Standard, CheckSet::Full] {
            let plan = plan_run(&items, &set(chosen));
            assert_eq!(runnable(&plan, 0), [T::Decision], "{id} {chosen:?}");
            assert!(plan.rows[0].cells.iter().all(|cell| cell.skip.is_none()));
        }
    }
    // A chat model of the same provider keeps the chat set.
    let items = [plan_item("p", "k", "gpt-6.1", ModelKind::Chat, &protocols)];
    assert_eq!(
        runnable(&plan_run(&items, &set(CheckSet::Standard)), 0),
        [
            T::Chat,
            T::Stream,
            T::Tools,
            T::JsonSchema,
            T::Vision,
            T::Reasoning
        ]
    );
    // Without a decision endpoint the id alone plans nothing new.
    let chat_only = [plan_item(
        "p",
        "k",
        "jev-latest",
        ModelKind::Chat,
        &[Protocol::OpenaiChat],
    )];
    assert!(!runnable(&plan_run(&chat_only, &set(CheckSet::Standard)), 0).contains(&T::Decision));
}

#[test]
fn a_decision_only_embedding_row_keeps_its_embeddings_cell() {
    let items = [plan_item(
        "p",
        "k",
        "e",
        ModelKind::Embedding,
        &[Protocol::DecisionSystemone],
    )];
    let plan = plan_run(&items, &set(CheckSet::Standard));
    assert_eq!(runnable(&plan, 0), [T::Embeddings]);
}

#[test]
fn key_checks_run_once_per_key() {
    let mut items: Vec<PlanItem> = (0..38).map(|n| chat_item(&format!("m{n}"))).collect();
    let plan = plan_run(&items, &set(CheckSet::Standard));
    // 1 Connectivity and 1 Auth, not 38 each.
    assert_eq!(plan.keys.len(), 1);
    assert_eq!(plan.keys[0].checks, [T::Connectivity, T::Auth]);
    assert!(
        plan.rows
            .iter()
            .all(|row| row.cells.iter().all(|cell| !cell.test.is_key_scoped()))
    );
    // Two keys of the same provider, "All keys": 76 rows and two key plans.
    let second: Vec<PlanItem> = items
        .iter()
        .map(|row| PlanItem {
            key_id: "k2".to_owned(),
            ..row.clone()
        })
        .collect();
    items.extend(second);
    let plan = plan_run(&items, &set(CheckSet::Standard));
    assert_eq!(plan.rows.len(), 76);
    assert_eq!(plan.keys.len(), 2);
    assert_eq!(plan.keys[1].first_item, 38);
}

#[test]
fn the_same_key_id_under_two_providers_is_two_keys() {
    let items = [
        plan_item("a", "k", "m", ModelKind::Chat, &[Protocol::OpenaiChat]),
        plan_item("b", "k", "m", ModelKind::Chat, &[Protocol::OpenaiChat]),
    ];
    assert_eq!(plan_run(&items, &set(CheckSet::Quick)).keys.len(), 2);
}

#[test]
fn custom_selection_orders_cells_and_ignores_context() {
    let items = [chat_item("m")];
    let plan = plan_run(
        &items,
        &CheckSelection::Custom {
            tests: vec![T::Tools, T::Models, T::Chat, T::Context, T::Auth, T::Tools],
        },
    );
    assert_eq!(plan.keys[0].checks, [T::Auth, T::Models]);
    assert_eq!(runnable(&plan, 0), [T::Chat, T::Tools]);
}

#[test]
fn test_key_has_only_key_checks() {
    let items = [chat_item("m")];
    let plan = plan_run(
        &items,
        &CheckSelection::Custom {
            tests: vec![T::Connectivity, T::Auth, T::Models],
        },
    );
    assert_eq!(plan.keys[0].checks, [T::Connectivity, T::Auth, T::Models]);
    assert!(plan.rows[0].cells.is_empty());
    assert_eq!(plan.total_cells(), 3);
}

#[test]
fn a_custom_list_without_key_checks_has_no_key_gate_work() {
    let plan = plan_run(
        &[chat_item("m")],
        &CheckSelection::Custom {
            tests: vec![T::Chat],
        },
    );
    assert!(plan.keys[0].checks.is_empty());
}

#[test]
fn no_items_plan_nothing() {
    let plan = plan_run(&[], &set(CheckSet::Full));
    assert!(plan.keys.is_empty() && plan.rows.is_empty());
    assert_eq!(plan.total_cells(), 0);
}

#[test]
fn key_gate_table() {
    let out = |connectivity, auth| KeyOutcome { connectivity, auth };
    let cases = [
        (T::Connectivity, out(Some(S::Unreachable), None), None),
        (
            T::Auth,
            out(Some(S::Unreachable), None),
            Some(SkipReason::ConnectivityFailed),
        ),
        (
            T::Chat,
            out(Some(S::TimedOut), None),
            Some(SkipReason::ConnectivityFailed),
        ),
        (
            T::Models,
            out(Some(S::Unreachable), Some(S::Rejected)),
            Some(SkipReason::ConnectivityFailed),
        ),
        (
            T::Chat,
            out(Some(S::Ready), Some(S::Rejected)),
            Some(SkipReason::KeyRejected),
        ),
        (
            T::Models,
            out(Some(S::Ready), Some(S::Rejected)),
            Some(SkipReason::KeyRejected),
        ),
        (T::Auth, out(Some(S::Ready), Some(S::Rejected)), None),
        (
            T::Connectivity,
            out(Some(S::Ready), Some(S::Rejected)),
            None,
        ),
        (T::Chat, out(Some(S::Ready), Some(S::Ready)), None),
        (T::Chat, out(None, None), None),
        // A Connectivity answer that is merely Rejected or Unsupported is not an outage.
        (T::Chat, out(Some(S::Rejected), None), None),
        (T::Chat, out(Some(S::Ready), Some(S::TimedOut)), None),
    ];
    for (test, outcome, expected) in cases {
        assert_eq!(key_gate(test, &outcome), expected, "{test:?} {outcome:?}");
    }
}

#[test]
fn chat_gate_table() {
    for status in [S::Rejected, S::Unreachable, S::InvalidResponse, S::TimedOut] {
        for test in [
            T::Stream,
            T::Tools,
            T::JsonSchema,
            T::Vision,
            T::Reasoning,
            T::Cache,
            T::Search,
            T::Pdf,
        ] {
            assert_eq!(
                chat_gate(test, Some(status)),
                ChatGate::Skip(SkipReason::ChatFailed),
                "{test:?} {status:?}"
            );
        }
        assert_eq!(chat_gate(T::Chat, Some(status)), ChatGate::Open);
        assert_eq!(chat_gate(T::Embeddings, Some(status)), ChatGate::Open);
    }
    for status in [None, Some(S::Ready), Some(S::Unsupported), Some(S::Skipped)] {
        assert_eq!(chat_gate(T::Stream, status), ChatGate::Open, "{status:?}");
    }
}

#[test]
fn a_rate_limited_chat_leaves_the_later_cells_not_tested() {
    for test in [
        T::Stream,
        T::Tools,
        T::JsonSchema,
        T::Vision,
        T::Reasoning,
        T::Cache,
    ] {
        assert_eq!(chat_gate(test, Some(S::RateLimited)), ChatGate::NotTested);
    }
    assert_eq!(chat_gate(T::Chat, Some(S::RateLimited)), ChatGate::Open);
}

/// Every test is a column, a documented non-column or the opt-in action: the exhaustive match
/// fails to compile until a new test is classified.
#[test]
fn every_probe_test_is_accounted_for() {
    #[derive(PartialEq)]
    enum Role {
        Column,
        NonColumn,
        OptIn,
    }
    let role = |test: ProbeTest| match test {
        T::Connectivity
        | T::Auth
        | T::Chat
        | T::Stream
        | T::Tools
        | T::JsonSchema
        | T::Vision
        | T::Reasoning
        | T::Cache
        | T::Embeddings
        | T::Search
        | T::Pdf
        | T::Decision => Role::Column,
        // Models feeds the drift bar.
        T::Models => Role::NonColumn,
        T::Context => Role::OptIn,
    };
    // The check sets only use columns; Context and Models never appear in a set.
    for chosen in [CheckSet::Quick, CheckSet::Standard, CheckSet::Full] {
        let plan = plan_run(&[chat_item("m")], &set(chosen));
        let used = plan.keys[0]
            .checks
            .iter()
            .chain(plan.rows[0].cells.iter().map(|cell| &cell.test));
        for test in used {
            assert!(role(*test) == Role::Column, "{test:?}");
        }
    }
}

#[test]
fn image_and_audio_models_run_only_the_key_checks() {
    for (kind, reason) in [
        (ModelKind::Image, SkipReason::ImageModel),
        (ModelKind::Audio, SkipReason::AudioModel),
        (ModelKind::Rerank, SkipReason::RerankModel),
        (ModelKind::Moderation, SkipReason::ModerationModel),
        (ModelKind::Other, SkipReason::NotAChatModel),
    ] {
        let items = [plan_item("p", "k", "x", kind, &[Protocol::OpenaiChat])];
        for selection in [
            set(CheckSet::Standard),
            set(CheckSet::Full),
            CheckSelection::Custom {
                tests: vec![T::Chat, T::Embeddings, T::Decision],
            },
        ] {
            let plan = plan_run(&items, &selection);
            assert!(plan.rows[0].cells.iter().all(|c| c.skip == Some(reason)));
            assert!(runnable(&plan, 0).is_empty());
            // Only the key's Connectivity and Auth of a named set may run.
            let key_checks = if matches!(selection, CheckSelection::Set { .. }) {
                2
            } else {
                0
            };
            assert_eq!(plan.runnable_cells(), key_checks, "{kind:?} {selection:?}");
        }
    }
}
