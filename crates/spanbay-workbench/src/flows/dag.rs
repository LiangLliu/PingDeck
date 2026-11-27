//! DAG cycle checking, conditional branch routing, and bounded-concurrency wave scheduler
//! for executable API flows.
//!
//! Invoked by [`super::run_flow`] whenever [`has_dag_topology`] returns `true`. Reuses the exact
//! same per-step executor, condition evaluator, secret masking, cancellation token, and
//! deterministic variable merging rules as linear flows.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::time::Instant;

use futures_util::future::join_all;

use crate::cancel::CancelToken;
use crate::error::AppError;
use crate::workspace::items::{Flow, FlowRunInput, FlowRunResult, FlowStep, FlowStepKind};
use crate::workspace::{EnvironmentVariable, KeyValue, Request, VariableScopes};

use super::condition::evaluate_condition;
use super::eval::{apply_local, render_condition, set_variable};
use super::{
    FlowRun, SingleStepOutcome, StepOutput, blank_result, elapsed_ms, execute_single_request_step,
    record, step_name,
};

/// Upper bound on concurrent HTTP steps dispatched in one parallel DAG wave.
pub const MAX_FLOW_CONCURRENCY: usize = 8;

/// Returns `true` when `flow` uses explicit DAG features (`depends_on`, `Condition` routers,
/// `Join` barriers, or conditional `branches`). Purely linear flows return `false` and run on
/// the original sequential path.
#[must_use]
pub fn has_dag_topology(flow: &Flow) -> bool {
    flow.steps.iter().any(|s| {
        s.step_kind != FlowStepKind::Request || !s.depends_on.is_empty() || !s.branches.is_empty()
    })
}

/// Validates that the control edges (`depends_on` + `branches[].target_step_ids`) form an acyclic
/// DAG using Kahn's algorithm. Returns `Err(cyclic_step_labels)` when a cycle is detected.
pub fn check_flow_dag_cycles(flow: &Flow) -> Result<(), Vec<String>> {
    let n = flow.steps.len();
    if n == 0 {
        return Ok(());
    }
    let id_to_idx: HashMap<&str, usize> = flow
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    let mut indeg = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut seen_edges: HashSet<(usize, usize)> = HashSet::new();

    for (idx, step) in flow.steps.iter().enumerate() {
        for dep in &step.depends_on {
            if let Some(&u) = id_to_idx.get(dep.as_str()) {
                if u == idx {
                    return Err(vec![step_name(step).to_owned()]);
                }
                if seen_edges.insert((u, idx)) {
                    adj[u].push(idx);
                    indeg[idx] += 1;
                }
            }
        }
        for branch in &step.branches {
            for target in &branch.target_step_ids {
                if let Some(&v) = id_to_idx.get(target.as_str()) {
                    if v == idx {
                        return Err(vec![step_name(step).to_owned()]);
                    }
                    if seen_edges.insert((idx, v)) {
                        adj[idx].push(v);
                        indeg[v] += 1;
                    }
                }
            }
        }
    }

    let mut q: VecDeque<usize> = indeg
        .iter()
        .enumerate()
        .filter_map(|(i, &d)| (d == 0).then_some(i))
        .collect();
    let mut visited = 0usize;
    while let Some(u) = q.pop_front() {
        visited += 1;
        for &v in &adj[u] {
            indeg[v] = indeg[v].saturating_sub(1);
            if indeg[v] == 0 {
                q.push_back(v);
            }
        }
    }
    if visited == n {
        Ok(())
    } else {
        let cyclic: Vec<String> = indeg
            .iter()
            .enumerate()
            .filter(|&(_, &d)| d > 0)
            .map(|(i, _)| step_name(&flow.steps[i]).to_owned())
            .collect();
        Err(cyclic)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeExecState {
    Pending,
    Skipped,
    Done { passed: bool },
}

/// Executes a DAG flow with parallel waves (bounded by [`MAX_FLOW_CONCURRENCY`]), conditional
/// multi-exit routing, automatic skip propagation for unmatched branches, and deterministic
/// variable merging in `position` order.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_flow_dag<Step, StepFuture, Sent, Scopes, ScopesFuture>(
    flow: &Flow,
    requests: &HashMap<String, Request>,
    input: &FlowRunInput,
    run: &FlowRun<'_>,
    started: Instant,
    mut flow_variables: Vec<EnvironmentVariable>,
    condition_scopes: Scopes,
    execute_step: Step,
) -> FlowRunResult
where
    Step: Fn(Request, Vec<EnvironmentVariable>) -> StepFuture,
    StepFuture: Future<Output = Result<Sent, AppError>>,
    Sent: Into<StepOutput>,
    Scopes: Fn(Option<Request>, Vec<EnvironmentVariable>) -> ScopesFuture,
    ScopesFuture: Future<Output = VariableScopes>,
{
    let cancel = run.cancel;
    let mut steps: Vec<&FlowStep> = flow.steps.iter().collect();
    steps.sort_by_key(|step| step.position);
    let n = steps.len();
    let id_to_idx: HashMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();

    // Build unified predecessor list: explicit `depends_on` + incoming conditional branch edges.
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut cond_edges: HashSet<(usize, usize)> = HashSet::new();
    for (idx, step) in steps.iter().enumerate() {
        for dep in &step.depends_on {
            if let Some(&u) = id_to_idx.get(dep.as_str())
                && u != idx
                && !preds[idx].contains(&u)
            {
                preds[idx].push(u);
            }
        }
    }
    for (u, step) in steps.iter().enumerate() {
        if step.step_kind == FlowStepKind::Condition {
            for branch in &step.branches {
                for target in &branch.target_step_ids {
                    if let Some(&v) = id_to_idx.get(target.as_str())
                        && u != v
                    {
                        cond_edges.insert((u, v));
                        if !preds[v].contains(&u) {
                            preds[v].push(u);
                        }
                    }
                }
            }
        }
    }

    let mut states = vec![NodeExecState::Pending; n];
    let mut branch_active: HashMap<(usize, usize), bool> = HashMap::new();
    let mut results = Vec::with_capacity(n);
    let mut passed = true;
    let mut cancelled = false;
    let mut halted = false;

    loop {
        if cancel.is_some_and(CancelToken::is_cancelled) {
            cancelled = true;
            break;
        }
        if halted {
            break;
        }

        // Collect every Pending step whose predecessors have all reached a terminal state.
        let mut ready: Vec<usize> = Vec::new();
        let mut progressed_skip = false;

        for idx in 0..n {
            if states[idx] != NodeExecState::Pending {
                continue;
            }
            if !preds[idx]
                .iter()
                .all(|&u| !matches!(states[u], NodeExecState::Pending))
            {
                continue;
            }

            // A node with predecessors is skipped if NO predecessor delivered an active control edge.
            if !preds[idx].is_empty() {
                let any_active_pred = preds[idx].iter().any(|&u| match states[u] {
                    NodeExecState::Done { .. } => {
                        if cond_edges.contains(&(u, idx)) {
                            branch_active.get(&(u, idx)).copied().unwrap_or(false)
                        } else {
                            true
                        }
                    }
                    NodeExecState::Skipped | NodeExecState::Pending => false,
                });
                if !any_active_pred {
                    states[idx] = NodeExecState::Skipped;
                    let mut sk = blank_result(steps[idx], 0, Some(elapsed_ms(started)));
                    sk.skipped = true;
                    sk.condition = Some("upstream branch skipped".to_owned());
                    record(&mut results, run, idx, sk);
                    progressed_skip = true;
                    continue;
                }
            }
            ready.push(idx);
        }

        if ready.is_empty() {
            if progressed_skip {
                continue;
            }
            break;
        }

        // Execute control routers (`Condition` / `Join`) immediately in deterministic order.
        let mut request_wave: Vec<usize> = Vec::new();
        for idx in ready {
            let step = steps[idx];
            match step.step_kind {
                FlowStepKind::Request => request_wave.push(idx),
                FlowStepKind::Join => {
                    let offset = elapsed_ms(started);
                    let any_failed = preds[idx]
                        .iter()
                        .any(|&u| matches!(states[u], NodeExecState::Done { passed: false }));
                    let mut res = blank_result(step, 0, Some(offset));
                    if any_failed && !step.continue_on_error {
                        res.passed = false;
                        res.error = Some("an upstream parallel branch failed".to_owned());
                        passed = false;
                        halted = true;
                    }
                    states[idx] = NodeExecState::Done { passed: res.passed };
                    record(&mut results, run, idx, res);
                }
                FlowStepKind::Condition => {
                    let step_started = Instant::now();
                    let offset = elapsed_ms(started);
                    let scopes = condition_scopes(None, flow_variables.clone()).await;
                    let mut matched_id: Option<String> = None;
                    let mut matched_targets: HashSet<usize> = HashSet::new();
                    let mut eval_error: Option<String> = None;
                    let mut matched_expr: Option<String> = None;

                    for branch in &step.branches {
                        let expr = branch.expression.trim();
                        let matched = if expr.is_empty() {
                            Ok(true)
                        } else {
                            evaluate_condition(expr, &scopes)
                        };
                        match matched {
                            Ok(true) => {
                                matched_id = Some(branch.id.clone());
                                matched_expr =
                                    (!expr.is_empty()).then(|| render_condition(expr, &scopes));
                                for target in &branch.target_step_ids {
                                    if let Some(&v) = id_to_idx.get(target.as_str()) {
                                        matched_targets.insert(v);
                                    }
                                }
                                break;
                            }
                            Ok(false) => {}
                            Err(reason) => {
                                eval_error = Some(format!(
                                    "branch \"{}\" condition: {reason}",
                                    branch.label
                                ));
                                break;
                            }
                        }
                    }

                    // Mark every outgoing conditional edge active or inactive.
                    for branch in &step.branches {
                        for target in &branch.target_step_ids {
                            if let Some(&v) = id_to_idx.get(target.as_str()) {
                                branch_active.insert((idx, v), matched_targets.contains(&v));
                            }
                        }
                    }

                    let mut res = blank_result(step, elapsed_ms(step_started), Some(offset));
                    res.matched_branch_id = matched_id;
                    res.condition = matched_expr;
                    if let Some(err) = eval_error {
                        res.passed = false;
                        res.error = Some(err);
                        passed = false;
                        if !step.continue_on_error {
                            halted = true;
                        }
                    }
                    states[idx] = NodeExecState::Done { passed: res.passed };
                    record(&mut results, run, idx, res);
                }
            }
        }

        if halted || request_wave.is_empty() {
            continue;
        }

        // Dispatch ready HTTP/AI request steps in bounded parallel chunks.
        for chunk in request_wave.chunks(MAX_FLOW_CONCURRENCY) {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                cancelled = true;
                break;
            }
            let vars_snapshot = flow_variables.clone();
            let futs = chunk.iter().map(|&idx| {
                let vars = vars_snapshot.clone();
                let step = steps[idx];
                let cond_ref = &condition_scopes;
                let exec_ref = &execute_step;
                async move {
                    let outcome = execute_single_request_step(
                        flow, step, idx, requests, vars, started, cancel, cond_ref, exec_ref,
                    )
                    .await;
                    (idx, outcome)
                }
            });

            let outcomes = join_all(futs).await;

            // Merge results & variables strictly in deterministic step-index order.
            for (idx, outcome) in outcomes {
                match outcome {
                    SingleStepOutcome::Cancelled => {
                        cancelled = true;
                    }
                    SingleStepOutcome::Finished {
                        result,
                        local_updates,
                        extracted,
                    } => {
                        if let Some(local) = local_updates {
                            apply_local(&mut flow_variables, local);
                        }
                        for (k, v, is_secret) in extracted {
                            set_variable(&mut flow_variables, &k, &v, is_secret);
                        }
                        if result.skipped {
                            states[idx] = NodeExecState::Skipped;
                        } else {
                            let ok = result.passed;
                            states[idx] = NodeExecState::Done { passed: ok };
                            passed &= ok;
                            if !ok && !steps[idx].continue_on_error {
                                halted = true;
                            }
                        }
                        record(&mut results, run, idx, *result);
                    }
                }
            }
            if cancelled || halted {
                break;
            }
        }
    }

    FlowRunResult {
        run_id: run.run_id.clone(),
        flow_id: flow.id.clone(),
        started_at_ms: run.started_at_ms,
        passed: passed && !cancelled,
        cancelled,
        elapsed_ms: elapsed_ms(started),
        steps_total: flow.steps.len(),
        variables: flow_variables
            .iter()
            .map(|variable| KeyValue {
                key: variable.key.clone(),
                value: variable.value.clone().unwrap_or_default(),
                enabled: variable.enabled,
                description: None,
            })
            .collect(),
        secret_variables: flow_variables
            .iter()
            .filter(|variable| variable.is_secret)
            .map(|variable| variable.key.clone())
            .collect(),
        steps: results,
        scope: input.scope(),
        environment_id: None,
    }
}
