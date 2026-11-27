//! Run rows: insert, the columns an update never changes, the active filter and restart
//! recovery.

use super::*;

#[test]
fn runs_lifecycle_filtering_and_restart_recovery() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store.save_agent_session(&session("a", 1)).unwrap();
    store.save_agent_session(&session("b", 1)).unwrap();
    assert!(
        store
            .insert_agent_run(&run("r", "ghost", AgentRunStatus::Queued, 1))
            .is_err()
    );

    let mut parent = run("parent", "a", AgentRunStatus::Running, 100);
    store.insert_agent_run(&parent).unwrap();
    assert!(store.has_active_agent_run("a").unwrap());
    assert!(!store.has_active_agent_run("b").unwrap());

    let mut child = run("child", "a", AgentRunStatus::WaitingApproval, 200);
    child.kind = AgentRunKind::Subagent;
    child.parent_run_id = Some("parent".to_owned());
    child.parent_call_id = Some("call-9".to_owned());
    child.target = Some(AgentTarget::Named {
        name: "fast".to_owned(),
    });
    child.role = Some(spanbay_model::agent::SubagentRole::Fast);
    child.cost_currency = Some("USD".to_owned());
    store.insert_agent_run(&child).unwrap();
    assert_eq!(store.get_agent_run("child").unwrap(), Some(child.clone()));

    // Identity columns are immutable on update
    parent.status = AgentRunStatus::Completed;
    parent.finished_at_ms = Some(150);
    let mut moved = parent.clone();
    moved.session_id = "b".to_owned();
    moved.kind = AgentRunKind::Background;
    assert!(store.update_agent_run(&moved).unwrap());
    let loaded = store.get_agent_run("parent").unwrap().unwrap();
    assert_eq!(
        (loaded.session_id.as_str(), loaded.kind, loaded.status),
        ("a", AgentRunKind::Main, AgentRunStatus::Completed)
    );

    assert_eq!(
        store
            .list_agent_runs(Some("a"), true, 10)
            .unwrap()
            .into_iter()
            .map(|r| r.run_id)
            .collect::<Vec<_>>(),
        ["child"]
    );
    assert_eq!(store.interrupt_unfinished_agent_runs(500).unwrap(), 1);
    let interrupted = store.get_agent_run("child").unwrap().unwrap();
    assert_eq!(
        (interrupted.status, interrupted.stop_reason.as_deref()),
        (AgentRunStatus::Interrupted, Some("interrupted"))
    );
}

#[test]
fn restart_recovery_and_activity_queries_agree_for_all_run_statuses() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store.save_agent_session(&session("s", 1)).unwrap();
    let statuses = [
        AgentRunStatus::Queued,
        AgentRunStatus::Running,
        AgentRunStatus::WaitingApproval,
        AgentRunStatus::Completed,
        AgentRunStatus::Failed,
        AgentRunStatus::Cancelled,
        AgentRunStatus::Interrupted,
    ];
    for status in statuses {
        store
            .insert_agent_run(&run(status.as_str(), "s", status, 1))
            .unwrap();
    }
    let mut active: Vec<_> = store
        .list_agent_runs(None, true, 100)
        .unwrap()
        .into_iter()
        .map(|r| r.run_id)
        .collect();
    let mut expected: Vec<_> = statuses
        .iter()
        .filter(|s| s.is_active())
        .map(|s| s.as_str().to_owned())
        .collect();
    active.sort();
    expected.sort();
    assert_eq!(active, expected);
    assert_eq!(store.interrupt_unfinished_agent_runs(500).unwrap(), 3);
    let interrupted = store.get_agent_run("running").unwrap().unwrap();
    assert_eq!(
        (
            interrupted.status,
            interrupted.finished_at_ms,
            interrupted.error.as_deref()
        ),
        (
            AgentRunStatus::Interrupted,
            Some(500),
            Some("application restarted")
        )
    );
    assert_eq!(
        store
            .get_agent_run("completed")
            .unwrap()
            .unwrap()
            .finished_at_ms,
        None
    );
    assert_eq!(store.interrupt_unfinished_agent_runs(600).unwrap(), 0);
}
