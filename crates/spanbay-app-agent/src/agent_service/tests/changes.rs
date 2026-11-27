//! The file-change journal: rows written by real runs, keep, undo (clean, conflict, force, a
//! created file, a chain of edits), undo in the recorded folder and its confinement there, and
//! the run guard of undo and of changing the folder.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;

use super::run::{
    Seen, chat, events, input, wait_terminal, wait_until, watch, workspace_with, world,
};
use super::upstream::scripted;
use super::*;
use crate::agent::model::{AgentEvent, FileChange, FileChangeKind, FileChangeStatus};
use crate::crypto::sha256_hex;
use crate::settings::ToolApproval;

use super::super::{
    agent_blob_put_impl, agent_blob_read_impl, agent_change_keep_impl, agent_change_undo_impl,
    agent_changes_list_impl, agent_run_start_impl, agent_run_stop_impl,
};

/// A pending edit of `path` in `folder` from `before` to the bytes hashed as `after_sha256`.
fn edit_row(
    state: &AppState,
    session: &AgentSession,
    id: &str,
    folder: Option<String>,
    path: &str,
    before: &[u8],
    after_sha256: String,
) -> FileChange {
    let row = FileChange {
        id: id.to_owned(),
        session_id: session.id.clone(),
        run_id: "r".to_owned(),
        call_id: format!("c-{id}"),
        path: path.to_owned(),
        folder,
        kind: FileChangeKind::Edit,
        before_sha256: Some(agent_blob_put_impl(state, before).unwrap()),
        after_sha256: Some(after_sha256),
        added: 1,
        removed: 1,
        status: FileChangeStatus::Pending,
        created_at_ms: i64::MAX,
    };
    state.store().insert_file_change(&row).unwrap();
    row
}

fn move_chat(
    state: &AppState,
    session: &AgentSession,
    to: &Path,
) -> Result<AgentSession, AppError> {
    agent_session_update_impl(
        state,
        &session.id,
        AgentSessionPatch {
            workspace_dir: Some(to.display().to_string()),
            ..AgentSessionPatch::default()
        },
    )
}

fn read(path: &str) -> (&'static str, String) {
    ("read_file", json!({ "path": path }).to_string())
}

fn edit(path: &str, old: &str, new: &str) -> (&'static str, String) {
    (
        "edit_file",
        json!({"path": path, "old_string": old, "new_string": new}).to_string(),
    )
}

fn write(path: &str, content: &str) -> (&'static str, String) {
    (
        "write_file",
        json!({"path": path, "content": content}).to_string(),
    )
}

async fn play(
    steps: Vec<(&'static str, String)>,
    workspace: &TempDir,
) -> (super::run::World, AgentSession, Seen) {
    let world = world(scripted(steps));
    let session = chat(&world, ToolApproval::AutoAll, Some(workspace.path()));
    let seen = watch(&world, &session);
    agent_run_start_impl(&world.state, input(&session, "work"))
        .await
        .unwrap();
    wait_terminal(&seen, 1).await;
    (world, session, seen)
}

#[tokio::test(flavor = "multi_thread")]
async fn run_journals_changes_and_supports_keep_conflict_and_chain_undo() {
    let workspace = workspace_with("a.txt", "one\ntwo\n");
    let (world, session, seen) = play(
        vec![
            read("a.txt"),
            edit("a.txt", "two", "three"),
            edit("a.txt", "three", "four"),
            write("new.txt", "fresh\n"),
        ],
        &workspace,
    )
    .await;

    let rows = agent_changes_list_impl(&world.state, &session.id).unwrap();
    assert_eq!(rows.len(), 3);
    let (created, edit2, edit1) = (&rows[0], &rows[1], &rows[2]);
    assert_eq!(
        (created.path.as_str(), created.kind),
        ("new.txt", FileChangeKind::Create)
    );
    assert_eq!(
        (edit1.path.as_str(), edit1.kind),
        ("a.txt", FileChangeKind::Edit)
    );
    assert_eq!(
        agent_blob_read_impl(&world.state, edit1.before_sha256.as_deref().unwrap()).unwrap(),
        b"one\ntwo\n"
    );
    assert!(
        events(&seen)
            .iter()
            .any(|e| matches!(e, AgentEvent::FileChanged { path, .. } if path == "a.txt"))
    );

    // Keep marks `new.txt` kept; undo on kept is a no-op.
    assert_eq!(
        agent_change_keep_impl(&world.state, std::slice::from_ref(&created.id)).unwrap(),
        1
    );
    assert!(
        agent_change_undo_impl(&world.state, std::slice::from_ref(&created.id), false)
            .unwrap()
            .is_empty()
    );

    // External modification causes conflict until forced; chain undo on edit1 undoes edit2 then edit1.
    fs::write(workspace.path().join("a.txt"), "external\n").unwrap();
    let blocked =
        agent_change_undo_impl(&world.state, std::slice::from_ref(&edit1.id), false).unwrap();
    assert_eq!(
        (
            blocked.len(),
            blocked[0].call_id.as_str(),
            blocked[0].status
        ),
        (1, edit2.call_id.as_str(), FileChangeStatus::Conflict)
    );

    let forced =
        agent_change_undo_impl(&world.state, std::slice::from_ref(&edit1.id), true).unwrap();
    assert_eq!(forced.len(), 2);
    assert!(forced.iter().all(|r| r.status == FileChangeStatus::Undone));
    assert_eq!(
        fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
        "one\ntwo\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_never_leaves_the_chats_folder() {
    let workspace = workspace_with("a.txt", "one\n");
    let (world, session, _) = play(vec![read("a.txt")], &workspace).await;
    let outside = workspace
        .path()
        .parent()
        .unwrap()
        .join(format!("spanbay-escape-{}.txt", std::process::id()));
    fs::write(&outside, "outside").unwrap();
    let hash = sha256_hex(b"outside");
    let blob = agent_blob_put_impl(&world.state, b"planted").unwrap();
    for path in [
        format!("../{}", outside.file_name().unwrap().to_string_lossy()),
        outside.display().to_string(),
    ] {
        let row = FileChange {
            id: format!("fc-escape-{}", path.len()),
            session_id: session.id.clone(),
            run_id: "r".to_owned(),
            call_id: "c".to_owned(),
            path: path.clone(),
            folder: session.workspace_dir.clone(),
            kind: FileChangeKind::Edit,
            before_sha256: Some(blob.clone()),
            after_sha256: Some(hash.clone()),
            added: 1,
            removed: 1,
            status: FileChangeStatus::Pending,
            created_at_ms: 1,
        };
        world.state.store().insert_file_change(&row).unwrap();
        let error =
            agent_change_undo_impl(&world.state, std::slice::from_ref(&row.id), true).unwrap_err();
        assert!(matches!(error, AppError::Validation { .. }));
        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside");
    }
    let _ = fs::remove_file(&outside);
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_is_refused_while_the_chat_has_an_active_run() {
    let workspace = workspace_with("a.txt", "one\n");
    let world = world(scripted(vec![write("held.txt", "x")]));
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    let seen = watch(&world, &session);
    let row = FileChange {
        id: "fc-held".to_owned(),
        session_id: session.id.clone(),
        run_id: "r".to_owned(),
        call_id: "c".to_owned(),
        path: "a.txt".to_owned(),
        folder: session.workspace_dir.clone(),
        kind: FileChangeKind::Create,
        before_sha256: None,
        after_sha256: Some(sha256_hex(b"one\n")),
        added: 1,
        removed: 0,
        status: FileChangeStatus::Pending,
        created_at_ms: 1,
    };
    world.state.store().insert_file_change(&row).unwrap();
    let started = agent_run_start_impl(&world.state, input(&session, "write"))
        .await
        .unwrap();
    wait_until("the approval request", || {
        events(&seen)
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolApprovalRequired { .. }))
    })
    .await;

    assert!(matches!(
        agent_change_undo_impl(&world.state, std::slice::from_ref(&row.id), true),
        Err(AppError::Conflict { .. })
    ));
    assert!(agent_run_stop_impl(&world.state, &started.run_id));
    wait_terminal(&seen, 1).await;
    let undone = agent_change_undo_impl(&world.state, &[row.id], false).unwrap();
    assert_eq!(undone[0].status, FileChangeStatus::Undone);
    assert!(!workspace.path().join("a.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_writes_back_in_the_folder_the_change_was_made_in_after_the_chat_moves() {
    let first = workspace_with("a.txt", "one\ntwo\n");
    let (world, session, _) =
        play(vec![read("a.txt"), edit("a.txt", "two", "three")], &first).await;
    let made = agent_changes_list_impl(&world.state, &session.id).unwrap()[0].clone();
    assert_eq!(made.folder, session.workspace_dir);

    // The second folder holds the same bytes and a later change of the same path of its own.
    let second = workspace_with("a.txt", "one\nthree\n");
    let moved = move_chat(&world.state, &session, second.path()).unwrap();
    let theirs = edit_row(
        &world.state,
        &session,
        "fc-b",
        moved.workspace_dir.clone(),
        "a.txt",
        b"b before\n",
        sha256_hex(b"one\nthree\n"),
    );

    let touched =
        agent_change_undo_impl(&world.state, std::slice::from_ref(&made.id), false).unwrap();
    assert_eq!(
        touched
            .iter()
            .map(|row| (row.id.as_str(), row.status))
            .collect::<Vec<_>>(),
        [(made.id.as_str(), FileChangeStatus::Undone)]
    );
    assert_eq!(
        fs::read_to_string(first.path().join("a.txt")).unwrap(),
        "one\ntwo\n"
    );
    assert_eq!(
        fs::read_to_string(second.path().join("a.txt")).unwrap(),
        "one\nthree\n"
    );
    let rows = agent_changes_list_impl(&world.state, &session.id).unwrap();
    let still = rows.iter().find(|row| row.id == theirs.id).unwrap();
    assert_eq!(still.status, FileChangeStatus::Pending);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_without_a_recorded_folder_is_refused_before_anything_is_written() {
    let workspace = workspace_with("a.txt", "agent\n");
    let world = world(scripted(Vec::new()));
    let session = chat(&world, ToolApproval::AutoAll, Some(workspace.path()));
    let located = edit_row(
        &world.state,
        &session,
        "fc-known",
        session.workspace_dir.clone(),
        "a.txt",
        b"before\n",
        sha256_hex(b"agent\n"),
    );
    let unknown = edit_row(
        &world.state,
        &session,
        "fc-unknown",
        None,
        "b.txt",
        b"before\n",
        sha256_hex(b"agent\n"),
    );

    let error = agent_change_undo_impl(&world.state, &[located.id, unknown.id], true).unwrap_err();
    assert!(matches!(error, AppError::Validation { .. }));
    assert_eq!(
        fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
        "agent\n"
    );
    let rows = agent_changes_list_impl(&world.state, &session.id).unwrap();
    assert!(
        rows.iter()
            .all(|row| row.status == FileChangeStatus::Pending)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrongly_backfilled_folder_whose_file_differs_is_left_untouched() {
    // A v52 backfill used the chat's folder at upgrade time; the file there is a different one.
    let workspace = workspace_with("a.txt", "the user's own file\n");
    let world = world(scripted(Vec::new()));
    let session = chat(&world, ToolApproval::AutoAll, Some(workspace.path()));
    let row = edit_row(
        &world.state,
        &session,
        "fc-backfilled",
        session.workspace_dir.clone(),
        "a.txt",
        b"agent before\n",
        sha256_hex(b"agent after\n"),
    );

    let touched = agent_change_undo_impl(&world.state, &[row.id], false).unwrap();
    assert_eq!(touched[0].status, FileChangeStatus::Conflict);
    assert_eq!(
        fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
        "the user's own file\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_the_folder_is_refused_while_the_chat_has_an_active_run() {
    let first = workspace_with("a.txt", "one\n");
    let second = workspace_with("b.txt", "two\n");
    let world = world(scripted(vec![write("held.txt", "x")]));
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(first.path()));
    let seen = watch(&world, &session);
    let started = agent_run_start_impl(&world.state, input(&session, "write"))
        .await
        .unwrap();
    wait_until("the approval request", || {
        events(&seen)
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolApprovalRequired { .. }))
    })
    .await;

    assert!(matches!(
        move_chat(&world.state, &session, second.path()),
        Err(AppError::Conflict { .. })
    ));
    // Naming the folder it already has changes nothing and is allowed.
    move_chat(&world.state, &session, first.path()).unwrap();
    assert!(agent_run_stop_impl(&world.state, &started.run_id));
    wait_terminal(&seen, 1).await;
    let moved = move_chat(&world.state, &session, second.path()).unwrap();
    assert_ne!(moved.workspace_dir, session.workspace_dir);
}
