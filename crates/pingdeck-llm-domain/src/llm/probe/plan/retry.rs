//! "Retry failed" and "Run remaining" plans: a new run built from chosen cells of a previous one.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::{KeyPlan, PlanItem, PlannedCell, ProbePlan, RowPlan, SkipReason, key_plans};
use crate::llm::probe::{ProbeStatus, ProbeTest};

/// A cell of the run being retried or resumed, as the visible run shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviousCell {
    /// Index into the previous run's items. A key-scoped cell (Connectivity, Auth, Models) may
    /// name any item of its key.
    pub item: usize,
    /// The check.
    pub test: ProbeTest,
    /// Its status.
    pub status: ProbeStatus,
    /// Why the cell was skipped, for a Skipped cell.
    pub skip: Option<SkipReason>,
}

/// The plan of a retry or resume run and the rows it refers to: [`ProbePlan`] indexes `items`,
/// which is the subset of the previous run's items that have something to run, in their old
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPlan {
    /// Rows of the new run.
    pub items: Vec<PlanItem>,
    /// What to run on them.
    pub plan: ProbePlan,
}

/// Chosen cells `(item, test)` of the previous run.
type Chosen = BTreeSet<(usize, ProbeTest)>;
/// Tests per row index, in column order.
type TestsByRow = BTreeMap<usize, BTreeSet<ProbeTest>>;
/// Tests per key `(provider_id, key_id)`, in column order.
type TestsByKey<'a> = BTreeMap<(&'a str, &'a str), BTreeSet<ProbeTest>>;
/// Failed key-scoped cells `(provider_id, key_id, test)`.
type KeyReruns<'a> = HashSet<(&'a str, &'a str, ProbeTest)>;

/// Whether the previous run has a cell for (`item`, `test`) of the same key, key-scoped cells
/// matching by key.
fn previous_of<'a>(
    items: &[PlanItem],
    previous: &'a [PreviousCell],
    item: usize,
    test: ProbeTest,
) -> Option<&'a PreviousCell> {
    let key = |index: usize| items.get(index).map(PlanItem::key);
    previous.iter().find(|cell| {
        cell.test == test
            && if test.is_key_scoped() {
                key(cell.item) == key(item)
            } else {
                cell.item == item
            }
    })
}

/// The chosen tests per kept row (every item with a model-scoped cell, and the item that carries
/// each key-scoped one) and per key.
fn group_chosen<'a>(items: &'a [PlanItem], chosen: &Chosen) -> (TestsByRow, TestsByKey<'a>) {
    let mut row_tests = TestsByRow::new();
    let mut key_tests = TestsByKey::new();
    for &(item, test) in chosen {
        let row = row_tests.entry(item).or_default();
        if test.is_key_scoped() {
            key_tests.entry(items[item].key()).or_default().insert(test);
        } else {
            row.insert(test);
        }
    }
    (row_tests, key_tests)
}

/// The rows of the new run, in kept order; the derived order of `ProbeTest` is the column order.
fn row_plans(row_tests: &TestsByRow) -> Vec<RowPlan> {
    row_tests
        .values()
        .enumerate()
        .map(|(index, tests)| RowPlan {
            item: index,
            cells: tests
                .iter()
                .map(|&test| PlannedCell { test, skip: None })
                .collect(),
        })
        .collect()
}

/// The distinct keys of the `rate_limited` items, in first-seen (ascending item) order.
fn rate_limited_keys(items: &[PlanItem], rate_limited: &BTreeSet<usize>) -> Vec<(String, String)> {
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    rate_limited
        .iter()
        .map(|&item| items[item].key())
        .filter(|key| seen.insert(*key))
        .map(|(provider, key)| (provider.to_owned(), key.to_owned()))
        .collect()
}

/// Builds the plan of a new run from chosen cells `(item, test)` of the previous run. Every
/// index in `chosen` and `rate_limited` is `< items.len()`; both callers filter first.
fn assemble(items: &[PlanItem], chosen: &Chosen, rate_limited: &BTreeSet<usize>) -> RetryPlan {
    let (row_tests, key_tests) = group_chosen(items, chosen);
    let new_items: Vec<PlanItem> = row_tests.keys().map(|old| items[*old].clone()).collect();
    let keys: Vec<KeyPlan> = key_plans(&new_items, |item| {
        key_tests
            .get(&item.key())
            .map(|tests| tests.iter().copied().collect())
            .unwrap_or_default()
    });
    RetryPlan {
        plan: ProbePlan {
            keys,
            rows: row_plans(&row_tests),
            rate_limited_keys: rate_limited_keys(items, rate_limited),
        },
        items: new_items,
    }
}

/// The failed key-scoped cells `(provider, key, test)` and the rows whose Chat failed: the
/// causes of the cells a retry reruns.
fn reruns<'a>(items: &'a [PlanItem], known: &[&PreviousCell]) -> (KeyReruns<'a>, HashSet<usize>) {
    let key_reruns = known
        .iter()
        .filter(|cell| cell.status.is_failure() && cell.test.is_key_scoped())
        .map(|cell| {
            let (provider, key) = items[cell.item].key();
            (provider, key, cell.test)
        })
        .collect();
    let chat_reruns = known
        .iter()
        .filter(|cell| cell.status.is_failure() && cell.test == ProbeTest::Chat)
        .map(|cell| cell.item)
        .collect();
    (key_reruns, chat_reruns)
}

/// The failed cells and the Skipped cells whose cause is rerun, plus the rows of the
/// rate-limited cells.
fn failed_and_gated(
    items: &[PlanItem],
    known: &[&PreviousCell],
    key_reruns: &KeyReruns<'_>,
    chat_reruns: &HashSet<usize>,
) -> (Chosen, BTreeSet<usize>) {
    let mut chosen = Chosen::new();
    let mut rate_limited: BTreeSet<usize> = BTreeSet::new();
    for cell in known {
        let (provider, key) = items[cell.item].key();
        let selected = match (cell.status, cell.skip) {
            (ProbeStatus::Skipped, Some(SkipReason::ConnectivityFailed)) => {
                key_reruns.contains(&(provider, key, ProbeTest::Connectivity))
            }
            (ProbeStatus::Skipped, Some(SkipReason::KeyRejected)) => {
                key_reruns.contains(&(provider, key, ProbeTest::Auth))
            }
            (ProbeStatus::Skipped, Some(SkipReason::ChatFailed)) => {
                chat_reruns.contains(&cell.item)
            }
            // A failed or Not observed cell is worth running again; Ready and Unsupported are
            // final.
            (status, _) => status.is_retryable(),
        };
        if selected {
            chosen.insert((cell.item, cell.test));
        }
        if cell.status == ProbeStatus::RateLimited {
            rate_limited.insert(cell.item);
        }
    }
    (chosen, rate_limited)
}

/// Adds the cells of `original` that never ran (no cell in `previous`) behind a Chat that is run
/// again.
fn not_tested_behind_rerun_chat(
    items: &[PlanItem],
    original: &ProbePlan,
    previous: &[PreviousCell],
    chat_reruns: &HashSet<usize>,
    chosen: &mut Chosen,
) {
    for row in &original.rows {
        if row.item >= items.len() || !chat_reruns.contains(&row.item) {
            continue;
        }
        for planned in row.cells.iter().filter(|cell| cell.skip.is_none()) {
            if planned.test != ProbeTest::Chat
                && planned.test.is_chat_family()
                && previous_of(items, previous, row.item, planned.test).is_none()
            {
                chosen.insert((row.item, planned.test));
            }
        }
    }
}

/// Plans "Retry failed" from the previous run's `original` plan and its cells: the Rejected,
/// Rate limited, Unreachable, Invalid response, Timed out and Not observed cells plus the cells
/// they gated,
/// which are
/// - a Skipped cell whose cause is rerun ("Connectivity failed" after a Connectivity rerun,
///   "Key rejected" after an Auth rerun, "Chat failed" after that row's Chat rerun), and
/// - a cell left Not tested behind a rate-limited Chat of a row whose Chat is rerun.
///
/// Same checks, same row order, key-scoped checks once per key. The keys of the rate-limited
/// cells are listed in [`ProbePlan::rate_limited_keys`] so they run one check at a time.
/// "Embedding model", the `"<kind> model"` skips and "Stopped" are not dependents.
#[must_use]
pub fn plan_retry(
    items: &[PlanItem],
    original: &ProbePlan,
    previous: &[PreviousCell],
) -> RetryPlan {
    let known: Vec<&PreviousCell> = previous
        .iter()
        .filter(|cell| cell.item < items.len())
        .collect();
    let (key_reruns, chat_reruns) = reruns(items, &known);
    let (mut chosen, rate_limited) = failed_and_gated(items, &known, &key_reruns, &chat_reruns);
    not_tested_behind_rerun_chat(items, original, previous, &chat_reruns, &mut chosen);
    assemble(items, &chosen, &rate_limited)
}

/// Plans "Run remaining N" after Stop or an interrupted run: the cells of `original` that never
/// started (no cell in `previous`) or were Stopped, as a new run. Planned skips and every cell
/// that finished or was skipped for another reason stay as they are.
#[must_use]
pub fn plan_remaining(
    items: &[PlanItem],
    original: &ProbePlan,
    previous: &[PreviousCell],
) -> RetryPlan {
    let open = |item: usize, test: ProbeTest| {
        previous_of(items, previous, item, test).is_none_or(|cell| {
            cell.status == ProbeStatus::Skipped && cell.skip == Some(SkipReason::Stopped)
        })
    };
    let mut chosen = Chosen::new();
    for key in &original.keys {
        for test in &key.checks {
            if open(key.first_item, *test) {
                chosen.insert((key.first_item, *test));
            }
        }
    }
    for row in &original.rows {
        for planned in row.cells.iter().filter(|cell| cell.skip.is_none()) {
            if open(row.item, planned.test) {
                chosen.insert((row.item, planned.test));
            }
        }
    }
    chosen.retain(|(item, _)| *item < items.len());
    assemble(items, &chosen, &BTreeSet::new())
}

#[cfg(test)]
mod tests;
