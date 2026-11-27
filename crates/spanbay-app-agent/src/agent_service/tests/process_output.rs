//! Background processes (`agent_process_list` / `agent_process_kill`) and the paged read of a
//! call's full output (`agent_tool_output_read`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use super::*;
use crate::agent::model::AgentToolCallRecord;
use crate::agent::tools::ProcessState;

use super::super::{agent_process_kill_impl, agent_process_list_impl, agent_tool_output_read_impl};

fn sleeping() -> impl FnOnce() -> std::io::Result<tokio::process::Child> + use<> {
    || {
        Command::new("sh")
            .args(["-c", "sleep 60"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0)
            .spawn()
    }
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached in 5 s");
}

#[tokio::test(flavor = "multi_thread")]
async fn processes_are_listed_per_chat_and_killed_by_id() {
    let state = state();
    let (one, two) = (session(&state, "one"), session(&state, "two"));
    let mine = state
        .agent
        .processes
        .start(&one.id, "sleep 60", None, sleeping())
        .unwrap();
    let other = state
        .agent
        .processes
        .start(&two.id, "sleep 60", None, sleeping())
        .unwrap();
    let pid = mine.pid.unwrap();

    let all = agent_process_list_impl(&state, None).unwrap();
    assert_eq!(all.len(), 2);
    let only_one = agent_process_list_impl(&state, Some(&one.id)).unwrap();
    assert_eq!(only_one.len(), 1);
    assert_eq!(only_one[0].id, mine.id);
    assert_eq!(only_one[0].state, ProcessState::Running);
    assert!(alive(pid));

    assert!(agent_process_kill_impl(&state, &mine.id).unwrap());
    eventually(|| !alive(pid)).await;
    eventually(|| {
        agent_process_list_impl(&state, Some(&one.id)).unwrap()[0].state == ProcessState::Killed
    })
    .await;
    // The other chat's process is untouched, and a finished one has nothing left to kill.
    assert!(alive(other.pid.unwrap()));
    assert!(!agent_process_kill_impl(&state, &mine.id).unwrap());
    let error = agent_process_kill_impl(&state, "proc_missing").unwrap_err();
    assert!(matches!(error, AppError::NotFound { .. }), "{error:?}");
    assert!(agent_process_kill_impl(&state, &other.id).unwrap());
    eventually(|| !alive(other.pid.unwrap())).await;
}

// ----------------------------------------------------------------------------- tool output

fn call_record(session_id: &str, call_id: &str) -> AgentToolCallRecord {
    AgentToolCallRecord {
        session_id: session_id.to_owned(),
        call_id: call_id.to_owned(),
        run_id: "r".to_owned(),
        message_id: None,
        name: "shell".to_owned(),
        class: "exec".to_owned(),
        state: "output-available".to_owned(),
        args_json: "{}".to_owned(),
        risk: "medium".to_owned(),
        decided_by: None,
        decision: None,
        decided_ms: None,
        started_ms: None,
        ended_ms: None,
        exit_code: None,
        sandbox: None,
        truncated: false,
        output_bytes: 0,
        spill_ref: None,
        requested_ms: None,
    }
}

/// The audit row of a call whose full output the core kept.
fn kept_record(session_id: &str, call_id: &str) -> AgentToolCallRecord {
    AgentToolCallRecord {
        truncated: true,
        spill_ref: Some(call_id.to_owned()),
        ..call_record(session_id, call_id)
    }
}

/// A state with a data folder holding a spilled output for `call_id`.
fn spilled(content: &str, call_id: &str) -> (TempDir, AppState, AgentSession) {
    let root = TempDir::new("spill");
    let state = AppState::open(root.path().join("spanbay.sqlite3")).unwrap();
    state.on_start().unwrap();
    let session = session(&state, "chat");
    let dir = state
        .data_dirs()
        .unwrap()
        .root
        .join("tool-output")
        .join(&session.id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(call_id), content).unwrap();
    state
        .store()
        .upsert_tool_call(&kept_record(&session.id, call_id))
        .unwrap();
    (root, state, session)
}

#[test]
fn a_chat_reads_only_output_of_its_own_calls_and_only_safe_names() {
    let (_root, state, owner) = spilled("secret output", "call-1");
    assert_eq!(
        agent_tool_output_read_impl(&state, &owner.id, "call-1", 0, 100)
            .unwrap()
            .text,
        "secret output"
    );
    let stranger = session(&state, "stranger");

    // Another chat has no such call, whatever the file holds.
    let error = agent_tool_output_read_impl(&state, &stranger.id, "call-1", 0, 100).unwrap_err();
    assert!(matches!(error, AppError::NotFound { .. }), "{error:?}");
    assert!(agent_tool_output_read_impl(&state, "no-such-chat", "call-1", 0, 100).is_err());
    // A call that never spilled has no output to show.
    state
        .store()
        .upsert_tool_call(&call_record(&owner.id, "call-2"))
        .unwrap();
    let error = agent_tool_output_read_impl(&state, &owner.id, "call-2", 0, 100).unwrap_err();
    assert!(matches!(error, AppError::NotFound { .. }), "{error:?}");

    // Names that could leave the folder are refused before anything is looked up.
    for call_id in ["../spanbay.sqlite3", "a/b", "..", ".", "", "x\0y"] {
        let error = agent_tool_output_read_impl(&state, &owner.id, call_id, 0, 100).unwrap_err();
        assert!(
            matches!(error, AppError::Validation { .. }),
            "{call_id:?}: {error:?}"
        );
    }
    // A planted symlink is not followed.
    let root = state.data_dirs().unwrap().root.clone();
    let dir = root.join("tool-output").join(&owner.id);
    let target = root.join("spanbay-note.txt");
    fs::write(&target, "not an output").unwrap();
    std::os::unix::fs::symlink(&target, dir.join("call-3")).unwrap();
    state
        .store()
        .upsert_tool_call(&kept_record(&owner.id, "call-3"))
        .unwrap();
    let error = agent_tool_output_read_impl(&state, &owner.id, "call-3", 0, 100).unwrap_err();
    assert!(matches!(error, AppError::NotFound { .. }), "{error:?}");
}

#[test]
fn a_state_without_a_data_folder_has_no_output_to_read() {
    let state = state();
    let chat = session(&state, "chat");
    state
        .store()
        .upsert_tool_call(&call_record(&chat.id, "call-1"))
        .unwrap();
    let error = agent_tool_output_read_impl(&state, &chat.id, "call-1", 0, 100).unwrap_err();
    assert!(matches!(error, AppError::Unsupported { .. }), "{error:?}");
}
