use super::*;
use crate::llm::probe::listing::ModelKind;
use crate::llm::probe::plan::{CheckSelection, CheckSet, plan_run};
use crate::llm::probe::test_support::{chat_item, embedding_item, plan_item, set};
use crate::llm::wire::Protocol;
use ProbeStatus as S;
use ProbeTest as T;

fn cell(item: usize, test: ProbeTest, status: ProbeStatus) -> PreviousCell {
    PreviousCell {
        item,
        test,
        status,
        skip: None,
    }
}

fn skipped(item: usize, test: ProbeTest, reason: SkipReason) -> PreviousCell {
    PreviousCell {
        item,
        test,
        status: S::Skipped,
        skip: Some(reason),
    }
}

fn rerun(plan: &RetryPlan, row: usize) -> Vec<ProbeTest> {
    plan.plan.rows[row].cells.iter().map(|c| c.test).collect()
}

fn std_plan(items: &[PlanItem]) -> ProbePlan {
    plan_run(items, &set(CheckSet::Standard))
}

/// `bad` fails Chat with 403 and skips its five dependents, `slow` times out on Chat and
/// skips them too, `fine` passes everything it ran.
fn sample_run() -> (Vec<PlanItem>, Vec<PreviousCell>) {
    let items = vec![chat_item("bad"), chat_item("slow"), chat_item("fine")];
    let mut cells = vec![
        cell(0, T::Connectivity, S::Ready),
        cell(0, T::Auth, S::Ready),
    ];
    cells.push(cell(0, T::Chat, S::Rejected));
    cells.push(cell(1, T::Chat, S::TimedOut));
    cells.push(cell(2, T::Chat, S::Ready));
    cells.push(cell(2, T::Stream, S::Ready));
    for row in [0, 1] {
        for test in [T::Stream, T::Tools, T::JsonSchema, T::Vision, T::Reasoning] {
            cells.push(skipped(row, test, SkipReason::ChatFailed));
        }
    }
    (items, cells)
}

#[test]
fn two_failed_models_retry_their_chat_and_dependents_only() {
    let (items, cells) = sample_run();
    let retry = plan_retry(&items, &std_plan(&items), &cells);
    let models: Vec<&str> = retry.items.iter().map(|i| i.model_id.as_str()).collect();
    assert_eq!(models, ["bad", "slow"]);
    for row in 0..2 {
        assert_eq!(
            rerun(&retry, row),
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
    // The key's Connectivity and Auth passed: nothing to rerun there.
    assert_eq!(retry.plan.keys.len(), 1);
    assert!(retry.plan.keys[0].checks.is_empty());
    assert_eq!(retry.plan.total_cells(), 12);
    assert!(retry.plan.rate_limited_keys.is_empty());
    assert!(
        retry
            .plan
            .rows
            .iter()
            .flat_map(|r| &r.cells)
            .all(|c| c.skip.is_none())
    );
}

#[test]
fn unsupported_and_ready_cells_are_not_rerun() {
    let items = vec![chat_item("a"), chat_item("b")];
    let cells = [
        cell(0, T::Vision, S::Unsupported),
        cell(1, T::Chat, S::Ready),
        skipped(1, T::Stream, SkipReason::Stopped),
    ];
    let retry = plan_retry(&items, &std_plan(&items), &cells);
    assert!(retry.items.is_empty() && retry.plan.rows.is_empty() && retry.plan.keys.is_empty());
}

#[test]
fn every_retryable_status_is_rerun() {
    for status in [
        S::Rejected,
        S::RateLimited,
        S::Unreachable,
        S::InvalidResponse,
        S::TimedOut,
    ] {
        let items = [chat_item("m")];
        let retry = plan_retry(&items, &std_plan(&items), &[cell(0, T::Tools, status)]);
        assert_eq!(rerun(&retry, 0), [T::Tools], "{status:?}");
    }
}

#[test]
fn rate_limited_cells_run_one_at_a_time_per_key_and_free_the_not_tested_cells_behind_them() {
    let items = vec![
        plan_item("p", "k", "a", ModelKind::Chat, &[Protocol::OpenaiChat]),
        plan_item("p", "k2", "b", ModelKind::Chat, &[Protocol::OpenaiChat]),
    ];
    // `a`'s Chat was rate limited: its later cells never started (no cells). `b` timed out.
    let cells = [
        cell(0, T::Chat, S::RateLimited),
        cell(1, T::Vision, S::TimedOut),
    ];
    let retry = plan_retry(&items, &std_plan(&items), &cells);
    assert_eq!(
        rerun(&retry, 0),
        [
            T::Chat,
            T::Stream,
            T::Tools,
            T::JsonSchema,
            T::Vision,
            T::Reasoning
        ]
    );
    assert_eq!(rerun(&retry, 1), [T::Vision]);
    assert_eq!(
        retry.plan.rate_limited_keys,
        [("p".to_owned(), "k".to_owned())]
    );
}

#[test]
fn a_failed_connectivity_check_reruns_everything_it_gated() {
    let items = vec![chat_item("a"), chat_item("b")];
    let mut cells = vec![cell(0, T::Connectivity, S::Unreachable)];
    cells.push(skipped(0, T::Auth, SkipReason::ConnectivityFailed));
    for row in [0, 1] {
        for test in [T::Chat, T::Stream] {
            cells.push(skipped(row, test, SkipReason::ConnectivityFailed));
        }
    }
    let retry = plan_retry(&items, &plan_run(&items, &set(CheckSet::Quick)), &cells);
    assert_eq!(retry.items.len(), 2);
    assert_eq!(retry.plan.keys.len(), 1);
    assert_eq!(retry.plan.keys[0].checks, [T::Connectivity, T::Auth]);
    assert_eq!(rerun(&retry, 0), [T::Chat, T::Stream]);
    assert_eq!(rerun(&retry, 1), [T::Chat, T::Stream]);
}

#[test]
fn a_rejected_key_reruns_auth_and_its_dependents_but_not_connectivity() {
    let items = vec![chat_item("a")];
    let cells = [
        cell(0, T::Connectivity, S::Ready),
        cell(0, T::Auth, S::Rejected),
        skipped(0, T::Chat, SkipReason::KeyRejected),
        skipped(0, T::Models, SkipReason::KeyRejected),
    ];
    let original = plan_run(
        &items,
        &CheckSelection::Custom {
            tests: vec![T::Connectivity, T::Auth, T::Models, T::Chat],
        },
    );
    let retry = plan_retry(&items, &original, &cells);
    assert_eq!(retry.plan.keys[0].checks, [T::Auth, T::Models]);
    assert_eq!(rerun(&retry, 0), [T::Chat]);
}

#[test]
fn key_checks_of_several_rows_are_planned_once() {
    let items = vec![
        plan_item("p", "k", "a", ModelKind::Chat, &[Protocol::OpenaiChat]),
        plan_item("p", "k", "b", ModelKind::Chat, &[Protocol::OpenaiChat]),
        plan_item("p", "k2", "c", ModelKind::Chat, &[Protocol::OpenaiChat]),
    ];
    let cells = [
        cell(0, T::Auth, S::Rejected),
        cell(1, T::Auth, S::Rejected),
        cell(2, T::Chat, S::TimedOut),
    ];
    let retry = plan_retry(&items, &std_plan(&items), &cells);
    assert_eq!(retry.plan.keys.len(), 2);
    assert_eq!(retry.plan.keys[0].checks, [T::Auth]);
    assert_eq!(retry.plan.keys[0].first_item, 0);
    assert!(retry.plan.keys[1].checks.is_empty());
    assert_eq!(retry.plan.keys[1].first_item, 2);
    assert_eq!(retry.plan.rows.len(), 3);
}

#[test]
fn skips_that_do_not_depend_on_a_failure_stay_as_they_were() {
    let items = vec![embedding_item("e")];
    let cells = [
        skipped(0, T::Chat, SkipReason::EmbeddingModel),
        skipped(0, T::Stream, SkipReason::Stopped),
        skipped(0, T::Tools, SkipReason::NotAChatModel),
        cell(0, T::Embeddings, S::TimedOut),
    ];
    let retry = plan_retry(&items, &std_plan(&items), &cells);
    assert_eq!(rerun(&retry, 0), [T::Embeddings]);
}

#[test]
fn unknown_items_are_ignored_and_nothing_is_planned_for_an_empty_run() {
    let items = [chat_item("m")];
    let retry = plan_retry(&items, &std_plan(&items), &[cell(9, T::Chat, S::TimedOut)]);
    assert!(retry.items.is_empty());
    let retry = plan_retry(&[], &std_plan(&[]), &[]);
    assert_eq!(retry.plan.total_cells(), 0);
}

/// Stop at 21 of 38, then "Run remaining 17" runs 17 models.
#[test]
fn run_remaining_runs_the_models_that_never_started() {
    let items: Vec<PlanItem> = (0..38).map(|n| chat_item(&format!("m{n}"))).collect();
    let original = plan_run(&items, &set(CheckSet::Quick));
    let mut cells = vec![
        cell(0, T::Connectivity, S::Ready),
        cell(0, T::Auth, S::Ready),
    ];
    for row in 0..21 {
        cells.push(cell(row, T::Chat, S::Ready));
        cells.push(cell(row, T::Stream, S::Ready));
    }
    let remaining = plan_remaining(&items, &original, &cells);
    assert_eq!(remaining.items.len(), 17);
    assert_eq!(remaining.items[0].model_id, "m21");
    assert!(remaining.plan.keys[0].checks.is_empty());
    assert_eq!(remaining.plan.total_cells(), 34);
}

#[test]
fn run_remaining_takes_stopped_cells_and_never_started_key_checks() {
    let items = vec![chat_item("a"), chat_item("b")];
    let original = plan_run(&items, &set(CheckSet::Quick));
    // Stopped while Connectivity was in flight: nothing else started.
    let cells = [skipped(0, T::Connectivity, SkipReason::Stopped)];
    let remaining = plan_remaining(&items, &original, &cells);
    assert_eq!(remaining.plan.keys[0].checks, [T::Connectivity, T::Auth]);
    assert_eq!(remaining.items.len(), 2);
    assert_eq!(rerun(&remaining, 0), [T::Chat, T::Stream]);
    // Finished and otherwise skipped cells are not remaining.
    let cells = [
        cell(0, T::Connectivity, S::Ready),
        cell(0, T::Auth, S::Ready),
        cell(0, T::Chat, S::Rejected),
        skipped(0, T::Stream, SkipReason::ChatFailed),
        skipped(1, T::Chat, SkipReason::Stopped),
    ];
    let remaining = plan_remaining(&items, &original, &cells);
    assert_eq!(remaining.items.len(), 1);
    assert_eq!(remaining.items[0].model_id, "b");
    assert_eq!(rerun(&remaining, 0), [T::Chat, T::Stream]);
}

#[test]
fn run_remaining_of_a_finished_run_is_empty_and_ignores_planned_skips() {
    let items = vec![embedding_item("e")];
    let original = std_plan(&items);
    let cells = [
        cell(0, T::Connectivity, S::Ready),
        cell(0, T::Auth, S::Ready),
        cell(0, T::Embeddings, S::Ready),
    ];
    let remaining = plan_remaining(&items, &original, &cells);
    assert!(remaining.items.is_empty() && remaining.plan.total_cells() == 0);
}
