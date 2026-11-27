//! Data and sync use-cases against an on-disk state in a temp folder.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::agent::model::{
    AgentAttachment, AgentMessage, AgentMessageMeta, AgentRole, AgentSession, AttachmentKind,
    FileChange, FileChangeKind, FileChangeStatus,
};
use crate::error::AppError;
use crate::llm::capture_record::CapturedExchange;
use crate::llm::hub::HubUsage;
use crate::llm::store::{LogFilter, LogWrite, RequestLogEntry};
use crate::llm::wire::Protocol;
use crate::settings::ToolApproval;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "spanbay-data-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn disk_state(dir: &TempDir) -> AppState {
    let state = AppState::open(dir.path().join("spanbay.db")).expect("state opens");
    state.on_start().expect("start");
    state
}

fn session(state: &AppState, id: &str, updated_ms: i64, pinned: bool) {
    state
        .store()
        .save_agent_session(&AgentSession {
            id: id.to_owned(),
            title: "t".to_owned(),
            target: None,
            workspace_dir: None,
            permission_mode: ToolApproval::Ask,
            reasoning_effort: None,
            disabled_tools: Vec::new(),
            disabled_skills: Vec::new(),
            web_enabled: false,
            title_locked: false,
            head_message_id: None,
            pinned_at_ms: pinned.then_some(1_i64),
            archived_at_ms: None,
            deleted_at_ms: None,
            forked_from: None,
            forked_from_message_id: None,
            parent_session_id: None,
            parent_run_id: None,
            todos: Vec::new(),
            total_usage: HubUsage::default(),
            total_cost_micro_usd: 0,
            last_context_tokens: 0,
            created_at_ms: updated_ms,
            updated_at_ms: updated_ms,
            spanbay_workspace_id: None,
            spanbay_environment_id: None,
            spanbay_workspace_name: None,
            notice: None,
            instructions: None,
        })
        .expect("session inserts");
}

fn message(state: &AppState, id: &str, session: &str) {
    state
        .store()
        .append_agent_message(&AgentMessage {
            id: id.to_owned(),
            session_id: session.to_owned(),
            seq: 1,
            role: AgentRole::User,
            parts: Vec::new(),
            usage: None,
            model: None,
            error: None,
            run_id: None,
            parent_id: None,
            branch_index: 0,
            partial: false,
            cost_micro_usd: 0,
            cost_currency: None,
            meta: AgentMessageMeta::default(),
            branch_siblings: Vec::new(),
            attachments: Vec::new(),
            created_at_ms: 1,
        })
        .expect("message inserts");
}

fn attach(state: &AppState, name: &str, bytes: &[u8]) -> AgentAttachment {
    let sha256 = state
        .attachment_store()
        .expect("blob store")
        .put_blob(bytes)
        .expect("blob stores");
    let row = AgentAttachment {
        id: crate::app::factory_service::new_entity_id("att"),
        sha256,
        name: name.to_owned(),
        media_type: "text/plain".to_owned(),
        size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        kind: AttachmentKind::Text,
        width: None,
        height: None,
        thumb_data_url: None,
        lines: Some(1),
        created_at_ms: crate::app::current_time_ms(),
        removed: false,
    };
    state
        .store()
        .insert_attachment(&row)
        .expect("attachment inserts")
}

fn blob_exists(state: &AppState, sha: &str) -> bool {
    state.attachment_store().expect("blob store").exists(sha)
}

#[test]
fn storage_usage_reports_each_part_of_the_data_folder() {
    let dir = TempDir::new();
    let state = disk_state(&dir);
    session(&state, "s", 1, false);
    message(&state, "m", "s");
    let file = attach(&state, "a.txt", b"twelve bytes");
    fs::write(
        state.data_dirs().expect("dirs").logs.join("app.log"),
        [0_u8; 100],
    )
    .expect("log");

    let usage = storage_usage_impl(&state).expect("usage");
    assert_eq!(usage.attachments_count, 1);
    assert_eq!(usage.attachments_bytes, file.size_bytes);
    assert_eq!(usage.chats_count, 1);
    assert_eq!(usage.chats_bytes, 2, "the message holds `[]`");
    assert_eq!(usage.logs_bytes, 100);
    assert!(usage.database_bytes > 0);
    assert_eq!(usage.captured_bodies_count, 0);
}

#[test]
fn storage_usage_of_an_in_memory_state_has_no_files() {
    let state = AppState::open_in_memory().expect("state opens");
    let usage = storage_usage_impl(&state).expect("usage");
    assert!(usage.database_bytes > 0);
    assert_eq!(
        (usage.attachments_bytes, usage.logs_bytes, usage.chats_bytes),
        (0, 0, 0)
    );
}

#[test]
fn clearing_attachments_removes_blobs_and_keeps_the_message_links() {
    let dir = TempDir::new();
    let state = disk_state(&dir);
    session(&state, "s", 1, false);
    message(&state, "m", "s");
    let file = attach(&state, "a.txt", b"some text");
    state
        .store()
        .link_message_attachments("m", std::slice::from_ref(&file.id))
        .expect("link");
    state
        .store()
        .set_attachment_thumb(&file.id, "data:image/png;base64,AA")
        .expect("thumb");

    let all = ClearAttachmentsOptions::default();
    let dry = clear_attachments_impl(
        &state,
        ClearAttachmentsOptions {
            dry_run: true,
            ..all
        },
    )
    .expect("dry run");
    assert_eq!(
        dry,
        DataClearReportDto {
            removed: 1,
            freed_bytes: file.size_bytes,
            chats: 1
        }
    );
    assert!(
        blob_exists(&state, &file.sha256),
        "a dry run deletes nothing"
    );

    assert_eq!(clear_attachments_impl(&state, all).expect("clear"), dry);
    assert!(!blob_exists(&state, &file.sha256));
    let row = state
        .store()
        .get_attachment(&file.id)
        .expect("row")
        .expect("kept");
    assert_eq!(row.thumb_data_url, None);
    assert_eq!(
        clear_attachments_impl(&state, all).expect("again"),
        DataClearReportDto::default(),
        "a cleared attachment counts for nothing"
    );
}

#[test]
fn clearing_older_attachments_keeps_newer_ones_and_drops_unlinked_rows() {
    let dir = TempDir::new();
    let state = disk_state(&dir);
    let old = attach(&state, "old.txt", b"old");
    let new = attach(&state, "new.txt", b"new");
    state
        .store()
        .set_attachment_created_at_ms(&old.id, current_time_ms() - 100 * DAY_MS)
        .expect("age");

    let report = clear_attachments_impl(
        &state,
        ClearAttachmentsOptions {
            older_than_days: Some(90),
            dry_run: false,
        },
    )
    .expect("clear");
    assert_eq!(report.removed, 1);
    assert!(!blob_exists(&state, &old.sha256));
    assert!(blob_exists(&state, &new.sha256));
    assert!(
        state
            .store()
            .get_attachment(&old.id)
            .expect("row")
            .is_none()
    );
    assert!(
        state
            .store()
            .get_attachment(&new.id)
            .expect("row")
            .is_some()
    );
}

#[test]
fn clearing_attachments_leaves_blobs_the_change_journal_uses() {
    let dir = TempDir::new();
    let state = disk_state(&dir);
    let file = attach(&state, "a.txt", b"shared bytes");
    session(&state, "s", 1, false);
    state
        .store()
        .insert_file_change(&FileChange {
            id: "c".into(),
            session_id: "s".into(),
            run_id: "r".into(),
            call_id: "call".into(),
            path: "/x".into(),
            folder: None,
            kind: FileChangeKind::Edit,
            before_sha256: None,
            after_sha256: Some(file.sha256.clone()),
            added: 0,
            removed: 0,
            status: FileChangeStatus::Pending,
            created_at_ms: 1,
        })
        .expect("journal");
    let report = clear_attachments_impl(&state, ClearAttachmentsOptions::default()).expect("clear");
    assert_eq!(report, DataClearReportDto::default());
    assert!(blob_exists(&state, &file.sha256));
}

#[test]
fn clearing_attachments_validates_the_age_and_needs_a_data_folder() {
    let state = AppState::open_in_memory().expect("state opens");
    let all = ClearAttachmentsOptions::default();
    assert!(matches!(
        clear_attachments_impl(&state, all),
        Err(AppError::Unsupported { .. })
    ));
    assert!(matches!(
        clear_attachments_impl(
            &state,
            ClearAttachmentsOptions {
                older_than_days: Some(4000),
                ..all
            }
        ),
        Err(AppError::Validation { .. })
    ));
}

#[test]
fn retention_deletes_old_chats_but_not_pinned_or_recent_ones() {
    let state = AppState::open_in_memory().expect("state opens");
    let now = 1_000 * DAY_MS;
    session(&state, "old", now - 100 * DAY_MS, false);
    session(&state, "pinned", now - 100 * DAY_MS, true);
    session(&state, "recent", now - 10 * DAY_MS, false);

    assert_eq!(chat_retention_apply_impl(&state, 0, now).expect("off"), 0);
    assert_eq!(chat_retention_apply_impl(&state, 90, now).expect("90"), 1);

    let deleted = |id: &str| {
        state
            .store()
            .get_agent_session(id)
            .expect("read")
            .expect("row")
            .deleted_at_ms
            .is_some()
    };
    assert!(deleted("old"));
    assert!(!deleted("pinned"));
    assert!(!deleted("recent"));
    assert_eq!(
        chat_retention_apply_impl(&state, 90, now).expect("again"),
        0
    );
}

#[test]
fn the_retention_preview_counts_what_the_job_would_delete() {
    let state = AppState::open_in_memory().expect("state opens");
    let now = current_time_ms();
    session(&state, "old", now - 100 * DAY_MS, false);
    session(&state, "pinned", now - 100 * DAY_MS, true);
    session(&state, "mid", now - 40 * DAY_MS, false);

    let chats = |days| {
        chat_retention_preview_impl(&state, days)
            .expect("preview")
            .chats
    };
    assert_eq!((chats(0), chats(30), chats(90)), (0, 2, 1));
    assert!(matches!(
        chat_retention_preview_impl(&state, 4000),
        Err(AppError::Validation { .. })
    ));
    assert_eq!(chat_retention_run_impl(&state).expect("off by default"), 0);
}

#[tokio::test]
async fn clearing_captured_bodies_keeps_the_log_rows() {
    let state = AppState::open_in_memory().expect("state opens");
    {
        let store = state.store();
        let entry = RequestLogEntry {
            ts_ms: 1,
            source: "gateway".to_owned(),
            protocol_in: "openai_chat".to_owned(),
            status: 200,
            outcome: "ok".to_owned(),
            ..RequestLogEntry::default()
        };
        let capture = CapturedExchange {
            request_log_id: 0,
            ts_ms: 1,
            client_protocol: Protocol::OpenaiChat,
            client_req_headers: Vec::new(),
            client_req_body: None,
            hub_request: None,
            upstream_url: "https://x".to_owned(),
            upstream_req_headers: Vec::new(),
            upstream_req_body: None,
            upstream_status: Some(200),
            upstream_resp_headers: Vec::new(),
            upstream_resp_body: None,
            client_resp_headers: Vec::new(),
            client_resp_body: None,
            upstream_frames: None,
            client_frames: None,
            translation_notes: Vec::new(),
            timing: Default::default(),
            bytes_total: 700,
            redacted_count: 0,
        };
        crate::llm::store::insert_log(
            store.connection(),
            &LogWrite {
                entry: &entry,
                capture: Some(&capture),
                rollup: None,
            },
        )
        .expect("log row");
    }
    assert_eq!(
        clear_captured_bodies_impl(&state).await.expect("clear"),
        DataClearReportDto {
            removed: 1,
            freed_bytes: 700,
            chats: 0
        }
    );
    let rows = crate::llm::store::count_log(state.store().connection(), &LogFilter::default())
        .expect("count");
    assert_eq!(rows, 1);
    assert_eq!(
        clear_captured_bodies_impl(&state).await.expect("again"),
        DataClearReportDto::default()
    );
}

#[test]
fn a_backup_never_replaces_the_live_database_and_reports_both_copies() {
    let dir = TempDir::new();
    let state = disk_state(&dir);
    let live = dir.path().join("spanbay.db");
    let refused = data_backup_impl(&state, &live).unwrap_err();
    assert!(
        matches!(refused, AppError::Validation { .. }),
        "{refused:?}"
    );
    // Any name inside the data folder: the final rename would replace a live companion file.
    for name in [
        "spanbay.db.secrets",
        "spanbay.db-wal",
        "attachments/copy.sqlite3",
    ] {
        let refused = data_backup_impl(&state, &dir.path().join(name)).unwrap_err();
        assert!(
            matches!(refused, AppError::Validation { .. }),
            "{name}: {refused:?}"
        );
    }

    let elsewhere = TempDir::new();
    let copy = data_backup_impl(&state, &elsewhere.path().join("backup.sqlite3")).unwrap();
    assert!(copy.path.ends_with("backup.sqlite3"));
    assert!(
        copy.secrets_path
            .as_deref()
            .is_some_and(|path| path.ends_with("backup.sqlite3.secrets"))
    );
    assert!(copy.bytes > 0);
    let compacted = data_compact_impl(&state).unwrap();
    assert!(compacted.after_bytes > 0);
    assert_eq!(storage_usage_impl(&state).unwrap().database_free_bytes, 0);
}
