//! The Agent board and the "opened" acknowledgement of finished runs.
//!
//! Single responsibility: answer "what is every run doing and what needs me" in one call
//! ([`agent_board_impl`]) and keep the count of finished runs nobody has opened ([`session_seen`],
//! [`agent_session_seen_impl`]). Lanes: Running = queued ("Waiting for a
//! free slot") and running; Needs you = runs waiting for an approval (their own or a sub-agent's) or
//! at the cost cap, runs whose approval timed out, and every run that ended in any status and
//! has not been opened ("Review"); Done = the 20 most recent opened runs that ended in the last 24
//! hours. Sub-agent runs have no card of their own. A run counts as opened when its chat is
//! subscribed to, when the user sends into it, or on an explicit acknowledgement.

use std::collections::{HashMap, HashSet};

use crate::agent::attention::PendingApprovalSummary;
use crate::agent::dto::{
    AgentBoard, AgentBoardRow, AgentSubscribeTarget, BoardChanges, BoardLimits,
    BoardSubagentCounts, BoardWait, TokensToday,
};
use crate::agent::limits::QUEUE_MESSAGES;
use crate::agent::model::{AgentRunKind, AgentRunStatus, AgentRunSummary, AgentTodoStatus};
use crate::error::AppError;
use crate::llm::store as llm_store;
use crate::llm::usage::{UsageQuery, UsageSource};

use super::host::blocking;
use crate::app::{AppState, command_error, current_time_ms};

/// Opened runs that ended within this long stay in the Done lane.
const DONE_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
/// Runs the Done lane shows.
const DONE_LANE: u32 = 20;
/// Characters of the last-answer line on a card.
const LAST_ANSWER_CHARS: usize = 160;
/// `last_step` of a run paused at the cost cap.
pub(super) const COST_LIMIT_STEP: &str = "cost limit";

/// Reads the finished runs nobody has opened (from earlier sessions of the app) into the
/// registry, once, so the first attention snapshot already counts them.
pub(super) fn ensure_seeded(state: &AppState) {
    if !state.agent.claim_unseen_seed() {
        return;
    }
    let unseen = state.store().list_unseen_agent_runs();
    if let Ok(runs) = unseen {
        state
            .agent
            .runs
            .seed_unseen(runs.into_iter().map(|run| (run.run_id, run.session_id)));
        state.agent.publish_attention();
    }
}

/// Marks the ended runs of `session_id` as opened; returns how many were unopened. Blocking.
fn mark_seen(state: &AppState, session_id: &str) -> u32 {
    let remembered = state.agent.runs.see_session(session_id);
    let stored = state
        .store()
        .mark_agent_runs_seen(session_id, current_time_ms())
        .unwrap_or(0);
    let changed = u32::try_from(remembered.max(stored)).unwrap_or(u32::MAX);
    if changed > 0 {
        state.agent.publish_attention();
    }
    changed
}

/// The user opened the chat a subscription follows (or the chat of the run it follows).
pub(super) fn target_seen(state: &AppState, target: &AgentSubscribeTarget) {
    let session_id = match target {
        AgentSubscribeTarget::Session { session_id } => Some(session_id.clone()),
        AgentSubscribeTarget::Run { run_id } => state
            .agent
            .runs
            .get(run_id)
            .map(|run| run.session_id.clone()),
    };
    if let Some(session_id) = session_id {
        mark_seen(state, &session_id);
    }
}

/// The user is in `session_id` (sending into it): its finished runs are no longer news.
pub(super) async fn session_seen(state: &AppState, session_id: &str) {
    let id = session_id.to_owned();
    let _ = blocking(state, move |state| mark_seen(state, &id)).await;
}

/// The UI shows `session_id`: its finished runs count as opened and the attention feed drops
/// them. Returns how many runs were unopened.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown chat.
pub fn agent_session_seen_impl(state: &AppState, session_id: &str) -> Result<u32, AppError> {
    super::sessions::load_session(state, session_id)?;
    Ok(mark_seen(state, session_id))
}

/// The top-level run `run_id` belongs to: itself, or the parent of a sub-agent run.
fn top_level_run(state: &AppState, run_id: &str) -> String {
    state
        .agent
        .runs
        .get(run_id)
        .and_then(|run| run.parent_run_id.clone())
        .unwrap_or_else(|| run_id.to_owned())
}

/// Start of the local day containing `now_ms`, for a zone `offset_minutes` east of UTC.
fn local_midnight_ms(now_ms: i64, offset_minutes: i16) -> i64 {
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    let local = now_ms + i64::from(offset_minutes) * 60_000;
    now_ms - local.rem_euclid(DAY_MS)
}

/// One card.
fn row(
    state: &AppState,
    run_id: &str,
    approvals: &HashMap<String, Vec<PendingApprovalSummary>>,
    extras: &HashMap<String, crate::workspace::storage::RunExtras>,
    subagents: &HashMap<String, BoardSubagentCounts>,
) -> Option<AgentBoardRow> {
    let live = state.agent.runs.get(run_id);
    let store = state.store();
    let summary: AgentRunSummary = match &live {
        Some(run) => run.summary(),
        None => store.get_agent_run(run_id).ok().flatten()?,
    };
    let session = store.get_agent_session(&summary.session_id).ok().flatten();
    let extra = extras.get(run_id).cloned().unwrap_or_default();
    let pending_approval = approvals
        .get(run_id)
        .and_then(|list| {
            list.iter()
                .min_by_key(|a| (a.requested_ms, a.call_id.clone()))
        })
        .cloned();
    let active = summary.status.is_active();
    let wait = if pending_approval.is_some() {
        Some(BoardWait::Approval)
    } else if summary.status == AgentRunStatus::WaitingApproval
        && summary.last_step.as_deref() == Some(COST_LIMIT_STEP)
    {
        Some(BoardWait::CostLimit)
    } else {
        None
    };
    let (plan_done, plan_total) = session.as_ref().map_or((0, 0), |session| {
        let counted = session
            .todos
            .iter()
            .filter(|todo| todo.status != AgentTodoStatus::Cancelled);
        let done = counted
            .clone()
            .filter(|todo| todo.status == AgentTodoStatus::Completed)
            .count();
        (
            u32::try_from(done).unwrap_or(u32::MAX),
            u32::try_from(counted.count()).unwrap_or(u32::MAX),
        )
    });
    let changes = store.agent_pending_changes(run_id).unwrap_or_default();
    let seen = active || extra.seen_at_ms.is_some();
    let subagent_counts = subagents.get(run_id).copied().unwrap_or_default();
    Some(AgentBoardRow {
        inline_allow: pending_approval
            .as_ref()
            .is_some_and(|a| a.risk == "low" && a.full_text.is_some()),
        waiting_for_slot: summary.status == AgentRunStatus::Queued,
        wait,
        pending_approval,
        title: session
            .as_ref()
            .map_or_else(String::new, |s| s.title.clone()),
        folder: session.as_ref().and_then(|s| s.workspace_dir.clone()),
        awake_ms: live.as_ref().map_or(extra.awake_ms, |run| run.awake_ms()),
        plan_done,
        plan_total,
        cost_currency: extra.cost_currency,
        subagents: subagent_counts.total,
        subagent_counts,
        pending_changes: BoardChanges {
            files: changes.files,
            added: changes.added,
            removed: changes.removed,
        },
        last_answer: store
            .agent_run_last_answer(run_id, LAST_ANSWER_CHARS)
            .ok()
            .flatten(),
        approval_timed_out: extra.approval_timed_out
            || live.as_ref().is_some_and(|run| run.approval_timed_out()),
        review: !active && !seen,
        seen,
        run: summary,
    })
}

/// Tokens of the `agent` source in the usage rollup for the local day of `now_ms`.
fn agent_tokens_since_midnight(
    connection: &spanbay_store::DbConnection,
    now_ms: i64,
    tz_offset_minutes: i16,
) -> Result<TokensToday, AppError> {
    let query = UsageQuery {
        provider_id: None,
        key_id: None,
        client_id: None,
        from_ms: local_midnight_ms(now_ms, tz_offset_minutes),
        to_ms: now_ms.saturating_add(1),
        group_by: Vec::new(),
        sources: vec![UsageSource::Agent],
        tz_offset_minutes,
        bucket_minutes: None,
    };
    Ok(llm_store::usage_rows(connection, &query)?.iter().fold(
        TokensToday::default(),
        |total, row| TokensToday {
            input: total.input.saturating_add(row.input_tokens),
            output: total.output.saturating_add(row.output_tokens),
        },
    ))
}

/// The board: three lanes, the limits and today's cost per currency.
///
/// # Errors
/// [`AppError::Database`] when the store cannot be read.
pub fn agent_board_impl(state: &AppState) -> Result<AgentBoard, AppError> {
    let now = current_time_ms();
    let settings = state.settings();
    let tz = state.llm().tz_offset_minutes();
    let (ids, cost_today, tokens_today) = {
        let store = state.store();
        (
            store
                .list_board_run_ids(now - DONE_WINDOW_MS, DONE_LANE)
                .map_err(command_error)?,
            store
                .agent_cost_since(local_midnight_ms(now, tz))
                .map_err(command_error)?,
            agent_tokens_since_midnight(store.connection(), now, tz)?,
        )
    };
    // Approvals by the top-level run that answers for them (a sub-agent's belongs to its parent).
    let mut approvals: HashMap<String, Vec<PendingApprovalSummary>> = HashMap::new();
    for approval in state.agent.approvals.list() {
        approvals
            .entry(top_level_run(state, &approval.run_id))
            .or_default()
            .push(approval);
    }
    let mut waiting = ids.waiting.clone();
    for run_id in approvals.keys() {
        if !waiting.contains(run_id) && !ids.running.contains(run_id) {
            waiting.push(run_id.clone());
        }
    }
    let every: Vec<String> = [&ids.running, &waiting, &ids.unseen, &ids.done]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let (extras, stored_subagents) = {
        let store = state.store();
        (
            store.agent_run_extras(&every).map_err(command_error)?,
            store
                .agent_subagent_status_counts(&every)
                .map_err(command_error)?,
        )
    };
    let subagents: HashMap<String, BoardSubagentCounts> = stored_subagents
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                BoardSubagentCounts {
                    total: v.total,
                    active: v.active,
                    waiting: v.waiting,
                    completed: v.completed,
                    failed: v.failed,
                    cancelled: v.cancelled,
                },
            )
        })
        .collect();
    let active_subagents = u32::try_from(
        state
            .agent
            .runs
            .active_runs()
            .into_iter()
            .filter(|run| run.kind == AgentRunKind::Subagent && run.status().is_active())
            .count(),
    )
    .unwrap_or(u32::MAX);
    let cards = |run_ids: &[String]| -> Vec<AgentBoardRow> {
        run_ids
            .iter()
            .filter_map(|id| row(state, id, &approvals, &extras, &subagents))
            .collect()
    };
    let mut needs_you = cards(&waiting);
    let mut running = Vec::new();
    for card in cards(&ids.running) {
        // An unattended run whose approval timed out waits for the user to come back.
        if card.approval_timed_out && card.wait.is_none() {
            needs_you.push(card);
        } else if card.wait.is_some() {
            if !needs_you
                .iter()
                .any(|known| known.run.run_id == card.run.run_id)
            {
                needs_you.push(card);
            }
        } else {
            running.push(card);
        }
    }
    needs_you.extend(cards(&ids.unseen));
    let agent = &settings.ai.agent;
    Ok(AgentBoard {
        running,
        needs_you,
        done: cards(&ids.done),
        limits: BoardLimits {
            max_concurrent_runs: agent.max_concurrent_runs,
            max_subagents: agent.max_subagents,
            max_steps: agent.max_tool_iterations,
            approval_timeout_s: agent.approval_timeout_s,
            queue_messages: u32::try_from(QUEUE_MESSAGES).unwrap_or(5),
            auto_compact_pct: agent.auto_compact.then_some(agent.compact_threshold_pct),
        },
        cost_today,
        tokens_today,
        running_processes: super::work::running_process_count(state),
        active_subagents,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::llm::store::upsert_rollup;
    use crate::llm::usage::{RollupDelta, add_days, local_day};

    fn rollup(day: &str, source: &str, input: u64, output: u64) -> RollupDelta {
        RollupDelta {
            day: day.to_owned(),
            source: source.to_owned(),
            provider_id: "p1".to_owned(),
            key_id: String::new(),
            model: "m".to_owned(),
            client_id: String::new(),
            alias: String::new(),
            currency: "USD".to_owned(),
            requests: 1,
            ok_requests: 1,
            input_tokens: input,
            output_tokens: output,
            cached_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cost_micro: 0,
            failure_kind: None,
        }
    }

    #[test]
    fn tokens_today_count_only_the_agent_source_of_the_local_day() {
        let state = AppState::open_in_memory().unwrap();
        let now = current_time_ms();
        let today = local_day(now, 0);
        let yesterday = add_days(&today, -1).unwrap();
        let store = state.store();
        for delta in [
            rollup(&today, "agent", 500, 20),
            rollup(&today, "gateway", 9_000, 900),
            rollup(&yesterday, "agent", 70, 7),
        ] {
            upsert_rollup(store.connection(), &delta).unwrap();
        }
        let tokens = agent_tokens_since_midnight(store.connection(), now, 0).unwrap();
        assert_eq!(
            tokens,
            TokensToday {
                input: 500,
                output: 20
            }
        );
    }

    #[test]
    fn the_day_starts_at_local_midnight() {
        const HOUR: i64 = 3_600_000;
        const DAY: i64 = 24 * HOUR;
        let now = 20_000 * DAY + 23 * HOUR + HOUR / 2;
        // 23:30 UTC is 07:30 of the next day in UTC+8 and 18:30 of the same day in UTC-5.
        assert_eq!(local_midnight_ms(now, 480), 20_000 * DAY + 16 * HOUR);
        assert_eq!(local_midnight_ms(now, 0), 20_000 * DAY);
        assert_eq!(local_midnight_ms(now, -300), 20_000 * DAY + 5 * HOUR);
    }
}
