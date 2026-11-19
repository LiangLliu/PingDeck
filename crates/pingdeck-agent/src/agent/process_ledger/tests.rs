//! Tests of the process-group ledger: start times, reaping and the table behind it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

/// A private in-memory store with the current schema, for the ledger tests.
fn store_handle() -> std::sync::Arc<std::sync::Mutex<pingdeck_store::storage::WorkspaceStore>> {
    std::sync::Arc::new(std::sync::Mutex::new(
        pingdeck_store::storage::WorkspaceStore::open_in_memory().expect("in-memory store"),
    ))
}

use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use super::*;

/// A fake child that outlives a simulated crash: `sleep 300` leading its own process group.
fn fake_child() -> Child {
    Command::new("sleep")
        .arg("300")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn alive(pid: u32) -> bool {
    process_start_time(pid).is_some()
}

#[test]
fn a_process_has_a_stable_start_time_and_a_missing_one_has_none() {
    let me = std::process::id();
    let first = process_start_time(me).unwrap();
    assert_eq!(process_start_time(me).as_deref(), Some(first.as_str()));
    assert_eq!(process_start_time(u32::MAX - 1), None);
}

#[test]
fn a_leftover_group_is_stopped_and_a_recycled_id_is_left_alone() {
    let mut leftover = fake_child();
    let mut innocent = fake_child();
    let recorded = entry_for(leftover.id(), LedgerKind::Tool, "call-1").unwrap();
    // The record of `innocent` carries a start time that is not its own: its id was recycled.
    let mut recycled = entry_for(innocent.id(), LedgerKind::Tool, "call-2").unwrap();
    recycled.started = "Thu Jan  1 00:00:00 1970".to_owned();
    let report = reap_entries(vec![recorded, recycled]);
    assert_eq!(
        report,
        ReapReport {
            killed: 1,
            tool_calls: 1,
            cleared: 1
        }
    );
    // The leftover died (killed by a signal); the other program is untouched.
    let status = leftover.wait().unwrap();
    assert!(!status.success());
    assert!(alive(innocent.id()), "a recycled id is never touched");
    innocent.kill().unwrap();
    let _ = innocent.wait();
}

#[test]
fn members_of_a_group_whose_leader_is_gone_are_stopped() {
    use std::io::BufRead as _;
    let mut leader = Command::new("sh")
        .args(["-c", "sleep 300 & echo $!; wait"])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // `$!` arrives only after the shell forked `sleep`: the kill below cannot beat the fork.
    let mut member = String::new();
    std::io::BufReader::new(leader.stdout.take().unwrap())
        .read_line(&mut member)
        .unwrap();
    assert!(
        member.trim().parse::<u32>().is_ok(),
        "the shell named its child: {member:?}"
    );
    let entry = entry_for(leader.id(), LedgerKind::Tool, "call-orphans").unwrap();
    let pgid = entry.pgid;
    // The shell dies first; its `sleep` child keeps the group alive.
    leader.kill().unwrap();
    leader.wait().unwrap();
    assert!(process_start_time(pgid).is_none(), "the leader is gone");
    assert!(group_alive(pgid), "a member of the group remains");
    let report = reap_entries(vec![entry]);
    assert_eq!(
        report,
        ReapReport {
            killed: 1,
            tool_calls: 1,
            cleared: 0
        }
    );
    // The member dies by `SIGKILL`; it is reaped by init, so poll until the group is gone.
    let gone = (0..200).any(|_| {
        let gone = !group_alive(pgid);
        if !gone {
            std::thread::sleep(Duration::from_millis(50));
        }
        gone
    });
    assert!(gone, "the orphaned member survived the reap");
    // A group that never existed (or ended) is cleared, not killed.
    let ended = LedgerEntry {
        pid: pgid,
        pgid,
        started: "Thu Jan  1 00:00:00 1970".to_owned(),
        kind: LedgerKind::Tool,
        label: "ended".to_owned(),
    };
    assert_eq!(reap_entries(vec![ended]).cleared, 1);
}

#[test]
fn only_tool_groups_count_for_the_notice() {
    let mut mcp = fake_child();
    let entry = entry_for(mcp.id(), LedgerKind::Mcp, "server").unwrap();
    assert_eq!(
        reap_entries(vec![entry]),
        ReapReport {
            killed: 1,
            tool_calls: 0,
            cleared: 0
        }
    );
    let _ = mcp.wait();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ledger_survives_a_simulated_crash_and_the_next_start_reaps() {
    let state = store_handle();
    let mut child = fake_child();
    let ledger = LedgerHandle::new(std::sync::Arc::clone(&state));
    let key = ledger
        .record(Some(child.id()), LedgerKind::Tool, "call-1")
        .await
        .unwrap();
    // A group that ended cleanly leaves no record behind.
    let mut finished = fake_child();
    let done = ledger
        .record(Some(finished.id()), LedgerKind::Tool, "call-2")
        .await
        .unwrap();
    ledger.clear(done).await;
    finished.kill().unwrap();
    let _ = finished.wait();
    // "Crash": nobody clears `key`. The next start reads the table and stops the group.
    let report = reap_leftover_processes(std::sync::Arc::clone(&state)).await;
    assert_eq!(
        report,
        ReapReport {
            killed: 1,
            tool_calls: 1,
            cleared: 0
        }
    );
    assert!(!child.wait().unwrap().success());
    // The table is empty afterwards.
    assert!(
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ledger_take_all()
            .unwrap()
            .is_empty()
    );
    let _ = key;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_whose_start_time_was_edited_is_cleared_without_a_kill() {
    let state = store_handle();
    let mut child = fake_child();
    let mut entry = entry_for(child.id(), LedgerKind::Tool, "call-1").unwrap();
    entry.started = "Thu Jan  1 00:00:00 1970".to_owned();
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ledger_record(&entry)
        .unwrap();
    let report = reap_leftover_processes(std::sync::Arc::clone(&state)).await;
    assert_eq!(
        report,
        ReapReport {
            killed: 0,
            tool_calls: 0,
            cleared: 1
        }
    );
    assert!(alive(child.id()));
    child.kill().unwrap();
    let _ = child.wait();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_process_that_already_ended_is_not_recorded() {
    let state = store_handle();
    let mut child = fake_child();
    let pid = child.id();
    child.kill().unwrap();
    let _ = child.wait();
    assert!(
        LedgerHandle::new(std::sync::Arc::clone(&state))
            .record(Some(pid), LedgerKind::Tool, "gone")
            .await
            .is_none()
    );
    assert!(
        LedgerHandle::new(std::sync::Arc::clone(&state))
            .record(None, LedgerKind::Tool, "none")
            .await
            .is_none()
    );
}

#[test]
fn a_thread_without_state_records_and_clears_through_the_installed_ledger() {
    let state = store_handle();
    let mut child = fake_child();
    let pid = child.id();
    // `INSTALLED` is process-wide: MCP stdio tests running in parallel may add their own rows
    // to this store, so only the row of `pid` is examined.
    let own_rows = || {
        let mut rows = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ledger_take_all()
            .unwrap();
        rows.retain(|row| row.pid == pid);
        rows
    };
    install(std::sync::Arc::clone(&state));
    let key = record_installed(pid, LedgerKind::Mcp, "server-1").unwrap();
    let rows = own_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].kind, rows[0].label.as_str()),
        (LedgerKind::Mcp, "server-1")
    );
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ledger_record(&rows[0])
        .unwrap();
    clear_installed(&key);
    assert!(own_rows().is_empty());
    child.kill().unwrap();
    let _ = child.wait();
}
