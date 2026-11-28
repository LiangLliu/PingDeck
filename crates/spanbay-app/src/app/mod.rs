//! UI-framework-agnostic application core: state, use-cases and DTOs.
//!
//! Single responsibility: own the whole SpanBay application behind plain Rust
//! calls, so a UI shell is a thin adapter. `src-tauri` (Tauri 2 + React) drives
//! the [`AppState`] through the `*_impl` use-cases re-exported here; any other
//! shell (a CLI, an MCP server) could do the same.
//!
//! Nothing in this module knows about Tauri, webviews or dialogs: file dialogs
//! are resolved by the shell and passed in as plain paths. Request execution
//! uses `reqwest`, which needs a Tokio reactor, so the caller must poll the
//! async use-cases on a Tokio runtime (Tauri's async runtime is one).

use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::agent::runtime::AgentRuntime;
use crate::bodies::BodyStore;
use crate::cancel::CancellationRegistry;
use crate::error::AppError;
use crate::llm::client::LlmClient;
use crate::llm::runtime::{DbHandle, LlmRuntime};
use crate::mock::{MockHistorySink, MockHistoryWriter, MockServerManager};
use crate::platform::DataDirs;
use crate::secrets::{SecretStore, SqliteSecretStore};
use crate::settings::{Settings, SettingsHub};
use crate::workspace::{StorageError, WorkspaceId, WorkspaceStore};
pub use spanbay_store::storage::SetAsideDatabase;
use sync_runtime::SyncRuntime;

pub(crate) mod ai_run_service;
pub mod app_info_service;
pub mod client_error_service;
pub(crate) mod data_service;
pub mod dto;
pub mod environment_service;
pub mod execution_service;
pub mod factory_service;
pub mod flow_service;
pub mod history_cookies_service;
pub mod import_export_service;
pub(crate) mod import_merge;
pub(crate) mod import_service;
pub mod instance_lock;
pub(crate) mod items_service;
pub mod misc_logic_service;
pub mod mock_service;
pub(crate) mod oauth_service;
pub mod open_service;
pub(crate) mod prod_guard;
pub mod recovery;
pub mod request_model_service;
pub(crate) mod response_service;
pub mod reveal_service;
pub mod run_activity;
pub mod secret_hosts;
pub mod settings_service;
pub mod sniff_runner;
pub mod snippet_service;
pub mod startup_notices;
pub mod sync_runtime;
pub(crate) mod system_checks;
pub(crate) mod system_service;
pub mod tab_session_service;
pub mod tree_copy_service;
pub mod tree_service;
pub mod undo_store;
pub mod unresolved_scan;
pub mod workspace_service;

#[cfg(test)]
mod tests;

// Shell-facing API: DTOs and `*_impl` use-cases the `src-tauri` commands call.
pub use ai_run_service::{
    AiCompareEstimateDto, AiCompareEvent, AiCompareExportFormat, AiCompareResultDto, AiCompareRow,
    AiCompareRunInput, AiDetectInput, AiRequestRunInput, ai_compare_estimate_impl,
    ai_compare_export_impl, ai_compare_run_impl, ai_detect_impl, ai_request_cancel_impl,
    ai_request_run_impl,
};
pub use app_info_service::{
    AppInfoDto, SandboxInfo, SandboxSelftest, SecretStoreState, ShellInfo, app_diagnostics_impl,
    app_info_impl, settings_registry_get_impl, workspace_counts_impl,
};
pub use client_error_service::app_report_client_error_impl;
pub use data_service::{
    ChatRetentionPreviewDto, ClearAttachmentsOptions, DataBackupDto, DataClearReportDto,
    DataCompactDto, StorageUsageDto, chat_retention_apply_impl, chat_retention_preview_impl,
    chat_retention_run_impl, clear_attachments_impl, clear_captured_bodies_impl, data_backup_impl,
    data_compact_impl, start_chat_retention_job, storage_usage_impl,
};
pub use dto::{
    CollectionRunStatus, CollectionRunSummary, EnvironmentVariableInput, ExecuteDraftRequestInput,
    ExecuteRequestInput, GlobalsSaveInput, MockExportSaveResult, MockImportResultDto,
    MockLiveDataDto, MockRecordingRoutesDto, MockRecordingStatusDto, MockServerStatusDto,
    MockoonImportResultDto, MoveFolderInput, MoveRequestInput, PostmanExportDto,
    PostmanExportSaveResult, PostmanImportResult, ResponseBodyEncoding, ResponseSaveResult,
    RunCollectionInput, RunnerItem, RunnerResult, SaveCollectionInput, SaveEnvironmentInput,
    SaveFolderInput, SaveRequestInput,
};
pub use environment_service::{
    CollectionDto, CompareRowDto, CompareState, DefineVariableInput, DefineVariableResult,
    EnvironmentDto, EnvironmentVariableDto, GlobalsDto, ResolvedLayer, ResolvedVariable, UndoToken,
    VariableScope, environment_compare_impl, environment_delete_impl, environment_duplicate_impl,
    environment_list_impl, environment_resolved_list_impl, environment_restore_impl,
    environment_save_impl, environment_secret_copy_impl, environment_undo_commit_impl,
    environment_variable_define, environment_variable_save_impl, notify_environments_changed,
};
pub use execution_service::{
    collection_run_get_impl, execution_cancel_impl, request_body_pick_file_from_path_impl,
    request_body_validate_file_path_with_context_impl, request_execute_draft_impl,
    request_execute_draft_to_file_impl, request_execute_impl, response_body_fetch_impl,
    run_collection_impl,
};
pub use flow_service::{
    FlowListEntryDto, FlowRequestRef, flow_delete_impl, flow_list_impl, flow_ref_list_impl,
    flow_run_impl, flow_run_list_impl, flow_save_impl,
};
pub use history_cookies_service::{
    HISTORY_EXPORT_FILE_NAME, HistoryExportResult, HistoryExportScope,
    cookie_jar_delete_expired_impl, cookie_jar_delete_many_impl, cookie_jar_export_impl,
    cookie_jar_export_save_suggested_filename, cookie_jar_export_save_to_path_impl,
    cookie_jar_import_impl, cookie_jar_import_open_file_from_path_impl, cookie_jar_list_impl,
    cookie_jar_restore_many_impl, cookie_jar_update_expiry_impl, cookie_jar_update_flags_impl,
    cookie_jar_update_key_impl, cookie_jar_update_scope_impl, cookie_jar_update_value_impl,
    history_clear_filtered_impl, history_clear_impl, history_delete_many_impl,
    history_export_har_impl, history_export_save_to_path_impl, history_get_impl,
    history_restore_many_impl,
};
pub use import_export_service::{
    collection_import_text_impl, environment_export_impl, environment_import_text_impl,
    postman_export_impl, postman_export_save_to_path_impl, postman_export_suggested_filename,
    postman_import_text_impl, response_save_bytes_to_path_impl, response_save_handle_to_path_impl,
    response_save_to_path_impl, response_suggested_file_name,
};
pub use import_service::{
    ImportClashDto, ImportClashKind, ImportConflict, ImportPreviewDto, ScriptCompatIssue,
    import_preview_impl, request_curl_import_impl, script_compat_list_impl,
};
pub use instance_lock::DataDirLock;
pub use items_service::{
    TlsCertificateDto, certificate_delete_impl, certificate_list_impl, certificate_save_impl,
};
pub use mock_service::{
    mock_openapi_import_impl, mock_port_suggest_impl, mock_recording_routes_impl,
    mock_recording_set_impl, mock_route_from_log_impl, mock_server_clear_logs_impl,
    mock_server_data_get_impl, mock_server_data_reset_impl, mock_server_delete_impl,
    mock_server_duplicate_impl, mock_server_export_save_to_path_impl,
    mock_server_export_suggested_filename, mock_server_import_json_impl,
    mock_server_import_mockoon_impl, mock_server_list_impl, mock_server_logs_impl,
    mock_server_logs_subscribe_impl, mock_server_logs_unsubscribe_impl, mock_server_save_impl,
    mock_server_start_impl, mock_server_state_reset_impl, mock_server_status_list_impl,
    mock_server_stop_impl,
};
pub use oauth_service::{
    OAuthTokenInput, TokenInfoDto, oauth_token_clear_impl, oauth_token_fetch_impl,
    oauth_token_refresh_impl, oauth_token_status_impl, oauth_token_store_as_impl,
};
pub use open_service::{
    agent_attachment_open_impl, agent_image_open_impl, open_path_impl, open_path_validate,
    save_text_file_impl,
};
pub use response_service::{BodySource, response_filter_jsonpath_impl, response_format_body_impl};
pub use reveal_service::{reveal_path_impl, reveal_path_validate};
pub use secret_hosts::{DeclaredHostDto, HostSource, environment_declared_hosts_impl};
pub use settings_service::{
    SettingsSecret, settings_get_impl, settings_reset_all_impl, settings_reset_section_impl,
    settings_restore_impl, settings_secret_set_impl, settings_subscribe_impl,
    settings_undo_last_reset_impl, settings_unsubscribe_impl, settings_update_impl,
};
pub use startup_notices::app_startup_report_impl;
pub use system_checks::{
    CaBundleInfoDto, ProxyTestDto, ProxyTestOutcome, SearchTestResultDto, ai_search_test_impl,
    network_ca_test_impl, network_proxy_test_impl,
};
pub use system_service::{open_external_validate, system_open_external_impl};
pub use tree_copy_service::{CopyItemInput, CopyItemKind, CopyItemResult, tree_copy_impl};
pub use tree_service::{
    DeletedKind, RestoreFailureDto, RestoreReportDto, collection_delete_impl, collection_get_impl,
    commit_delete_undo_impl, folder_delete_impl, folder_get_impl, list_dynamic_variable_names_impl,
    request_delete_impl, request_get_impl, restore_deleted_impl, workspace_tree_impl,
};
pub use unresolved_scan::{
    UnresolvedCollectionDto, UnresolvedRequestDto, UnresolvedVariableDto,
    environment_unresolved_scan_impl,
};
pub use workspace_service::{
    collection_list_impl, collection_save_checked_impl, collection_save_impl, folder_list_impl,
    folder_move_impl, folder_revision, folder_save_checked_impl, folder_save_impl,
    globals_get_impl, globals_save_impl, request_list_impl, request_move_impl, request_revision,
    request_save_checked_impl, request_save_impl, workspace_create_impl, workspace_delete_impl,
    workspace_list_impl, workspace_touch_impl,
};

// Cross-module helpers the services share through `super::`.
pub use environment_service::environment_delete_discarding_secrets;
#[cfg(test)]
pub(crate) use execution_service::execute_request_pipeline;
pub(crate) use execution_service::{
    ExecutionContext, ExecutionMeta, Persistence, execution_context, execution_context_for,
};
pub(crate) use history_cookies_service::{
    clear_stale_history_refs, cookie_jar_timestamp, execution_history_id,
    execution_history_timestamp, new_run_id,
};
pub(crate) use import_export_service::request_belongs_to_collection;
pub(crate) use workspace_service::request_from_input;

/// The whole SpanBay application: one SQLite store, the running mock servers, the secrets and
/// the cancellable operations in flight.
///
/// A cheap handle (`Clone`, `Send + Sync`, `'static`): every use-case takes `&AppState`, while a
/// background task (an agent run, a sub-agent) clones the handle and outlives the IPC call that
/// started it. Fields live in [`AppStateInner`] and are reached through `Deref`.
#[derive(Clone)]
pub struct AppState(Arc<AppStateInner>);

impl std::ops::Deref for AppState {
    type Target = AppStateInner;

    fn deref(&self) -> &AppStateInner {
        &self.0
    }
}

/// The shared contents of an [`AppState`].
pub struct AppStateInner {
    /// The one write connection; short statements only, never held across an `.await`.
    store: Arc<Mutex<WorkspaceStore>>,
    /// Credentials at rest (see `secrets`). On disk it runs on pooled connections and never
    /// takes [`AppStateInner::store`]; the in-memory state shares that lock (see
    /// [`AppStateInner::secrets`]).
    secrets: Arc<dyn SecretStore>,
    /// Where the data lives; `None` for an in-memory state.
    dirs: Option<DataDirs>,
    /// Mock servers currently bound by this process.
    pub mocks: MockServerManager,
    /// Writer thread of the history rows the mock servers produce; flushed at shutdown.
    pub mock_history: Arc<MockHistoryWriter>,
    /// Tokens of the executions the UI may still cancel by id.
    pub cancellations: Arc<CancellationRegistry>,
    /// Full bodies of large responses, fetched by handle.
    pub bodies: BodyStore,
    /// The live global settings (see `settings`).
    pub settings: SettingsHub,
    /// The settings before the last reset-all and the settings write count (see
    /// `settings_service`).
    pub(crate) settings_undo: settings_service::ResetUndoStore,
    /// LLM providers, the chat client and the local gateway (see `llm`).
    pub llm: Arc<LlmRuntime>,
    /// MCP connections, pending tool approvals and skill folders (see `agent`).
    pub agent: AgentRuntime,
    /// Config sync: the last export and the debounced auto-exporter (see `sync_runtime`).
    pub sync: SyncRuntime,
    /// Environments deleted in the last 7 s, restorable with their secrets (see
    /// `environment_service`).
    pub(crate) env_undo: environment_service::EnvironmentUndoStore,
    /// Requests, folders and collections deleted in the last 60 s, restorable with their
    /// secrets (see `tree_service`).
    pub(crate) delete_undo: tree_service::DeleteUndoStore,
    /// History rows deleted in the last 60 s, restorable by token (see
    /// `misc_logic_service::history_remove`).
    pub(crate) history_undo: misc_logic_service::HistoryUndoStore,
    /// Flow, collection and test-bench runs in flight (see `run_activity`).
    pub run_activity: run_activity::RunActivity,
    /// Files the user chose in a save dialog and the core wrote (what `reveal_path` may show).
    pub(crate) user_files: reveal_service::UserFiles,
    /// What the post-paint start routine reported (see `startup_notices`).
    pub startup_notices: startup_notices::StartupNotices,
}

impl AppState {
    /// Opens (or creates) the store at `path`; the data directory is its parent.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        let dirs = DataDirs::for_database(path);
        let db = DbHandle::path(path.to_path_buf());
        Self::with_store(WorkspaceStore::open(path)?, db, Some(dirs))
    }

    /// [`AppState::open`] for the app start: a database another SpanBay build created is moved
    /// aside (with its `.secrets` vault, see `WorkspaceStore::open_or_set_aside`) and a fresh one
    /// is opened; the moved files come back so the start can tell the user where they are.
    pub fn open_or_set_aside(
        path: impl AsRef<Path>,
    ) -> Result<(Self, Option<SetAsideDatabase>), StorageError> {
        let path = path.as_ref();
        let dirs = DataDirs::for_database(path);
        let db = DbHandle::path(path.to_path_buf());
        let (store, aside) = WorkspaceStore::open_or_set_aside(path)?;
        Ok((Self::with_store(store, db, Some(dirs))?, aside))
    }

    /// Opens an in-memory store: headless tests, previews and CLI runs.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        Self::with_store(
            WorkspaceStore::open_in_memory()?,
            DbHandle::Unavailable,
            None,
        )
    }

    fn with_store(
        mut store: WorkspaceStore,
        db: DbHandle,
        dirs: Option<DataDirs>,
    ) -> Result<Self, StorageError> {
        let settings = settings_service::load_settings(&mut store)?;
        let skills_dir = dirs.as_ref().map(|dirs| dirs.skills.clone());
        let sync = SyncRuntime::new();
        let store = Arc::new(Mutex::new(store));
        let mock_history =
            Arc::new(MockHistoryWriter::with_store(Arc::clone(&store)).map_err(StorageError::Io)?);
        let history_sink: Arc<dyn MockHistorySink> = mock_history.clone();
        let mocks = MockServerManager::with_history(history_sink);
        settings_service::apply_to_mocks(&mocks, &settings.snapshot());
        let secrets: Arc<dyn SecretStore> =
            Arc::new(SqliteSecretStore::for_database(Arc::clone(&store), &db));
        let llm = LlmRuntime::attach(&settings, db, Arc::clone(&secrets))
            .map_err(|error| StorageError::Io(std::io::Error::other(error.to_string())))?;
        Ok(Self(Arc::new(AppStateInner {
            store,
            secrets,
            dirs,
            mocks,
            mock_history,
            cancellations: Arc::new(CancellationRegistry::new()),
            bodies: BodyStore::new(),
            settings,
            settings_undo: settings_service::ResetUndoStore::default(),
            llm,
            agent: AgentRuntime::new(skills_dir),
            sync,
            env_undo: environment_service::EnvironmentUndoStore::default(),
            delete_undo: tree_service::DeleteUndoStore::default(),
            history_undo: misc_logic_service::HistoryUndoStore::default(),
            run_activity: run_activity::RunActivity::default(),
            user_files: reveal_service::UserFiles::default(),
            startup_notices: startup_notices::StartupNotices::default(),
        })))
    }

    /// The cheap part of the start: prepares the data directory, marks agent runs a previous
    /// process left active as interrupted and drops transport secrets whose owner is gone. It does no file scan and no network, so the
    /// window can paint right after it; everything else is [`AppState::bootstrap_ai`]. Call once,
    /// right after opening, and only while this process holds the data directory's
    /// [`DataDirLock`]: the recovery treats every unfinished run as a previous process's.
    ///
    /// # Errors
    /// [`AppError::Io`] when the data directory cannot be created, [`AppError::Database`] when
    /// the recovery update fails.
    pub fn on_start(&self) -> Result<(), AppError> {
        let now = current_time_ms();
        if let Some(dirs) = &self.dirs {
            dirs.ensure()
                .map_err(|error| AppError::io(format!("{}: {error}", dirs.root.display())))?;
            let _ = std::fs::remove_dir_all(dirs.tmp.join("open"));
        }
        let store = self.store();
        store
            .interrupt_unfinished_agent_runs(now)
            .map_err(command_error)?;
        store
            .prune_orphan_transport_secrets()
            .map_err(command_error)?;
        // Full access lasts until the app restarts.
        store.end_full_access().map_err(command_error)?;
        // A queue left from the last launch waits for the user instead of sending on its own;
        // the pause lives in memory because the restart itself is the reason.
        for session_id in store.sessions_with_queue().map_err(command_error)? {
            self.agent.runs.pause_queue(&session_id);
        }
        // Counts the finished runs nobody opened and settles a file journal a crash left half
        // written.
        self.agent.seed_unseen_runs(&store);
        // Plan mode outlives a relaunch.
        self.agent.seed_plans(&store);
        recovery::recover_file_journal(&store)?;
        Ok(())
    }
}

impl AppStateInner {
    /// The current global settings. Cheap (a read lock and an `Arc` clone): take one snapshot per
    /// operation instead of reading each value separately.
    #[must_use]
    pub fn settings(&self) -> std::sync::Arc<Settings> {
        self.settings.snapshot()
    }

    /// The LLM runtime: providers' shared state, health and the local gateway.
    #[must_use]
    pub fn llm(&self) -> &Arc<LlmRuntime> {
        &self.llm
    }

    /// A handle for in-process chat calls to any configured alias or provider model.
    #[must_use]
    pub fn llm_client(&self) -> LlmClient {
        LlmClient::new(Arc::clone(&self.llm))
    }

    /// Credentials at rest. With an on-disk database every call runs on a pooled connection, so
    /// it may be called while holding [`AppStateInner::store`] (a write then waits for the busy
    /// timeout if that holder has a write transaction open). The in-memory state (tests,
    /// previews) shares the store lock: there it must not be called while holding the store.
    #[must_use]
    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    /// The data directory layout; `None` for an in-memory state.
    #[must_use]
    pub fn data_dirs(&self) -> Option<&DataDirs> {
        self.dirs.as_ref()
    }

    /// Locks the store for one query.
    ///
    /// A panic elsewhere must not brick every later command: SQLite keeps the
    /// connection consistent, so a poisoned lock is recovered, not surfaced.
    pub fn store(&self) -> MutexGuard<'_, WorkspaceStore> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Shared handle to the workspace store mutex for background cleanup and ledger installs.
    #[must_use]
    pub fn store_arc(&self) -> &Arc<Mutex<WorkspaceStore>> {
        &self.store
    }

    /// Attachment blob store under `DataDirs::attachments`, capped by agent settings.
    ///
    /// # Errors
    /// [`AppError::Unsupported`] for an in-memory state with no data directory.
    pub fn attachment_store(&self) -> Result<crate::agent::attachments::AttachmentStore, AppError> {
        let dirs = self
            .data_dirs()
            .ok_or_else(|| AppError::unsupported("attachments need an on-disk data directory"))?;
        let settings = self.settings();
        let agent = &settings.ai.agent;
        Ok(crate::agent::attachments::AttachmentStore::new(
            dirs.attachments.clone(),
            crate::agent::attachments::AttachmentLimits::from_mb(
                agent.max_image_mb,
                agent.max_file_mb,
            ),
        ))
    }

    /// Runs `work` synchronously under the app store's connection lock.
    pub fn with_llm_connection<T>(
        &self,
        work: impl FnOnce(&spanbay_store::DbConnection) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let store = self.store();
        work(store.connection())
    }

    /// Runs `work` on the blocking pool with its own pooled SQLite connection (or inline on
    /// in-memory stores).
    pub async fn with_llm_db_blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&spanbay_store::DbConnection) -> Result<T, AppError> + Send + 'static,
    ) -> Result<T, AppError> {
        let db = self.llm.db().clone();
        match db {
            DbHandle::Path { .. } => tokio::task::spawn_blocking(move || work(&*db.open()?))
                .await
                .map_err(|error| AppError::internal(format!("database task failed: {error}")))?,
            DbHandle::Unavailable => self.with_llm_connection(work),
        }
    }
}

/// Converts any domain error into the [`AppError`] the use-cases return (for `.map_err`).
pub fn command_error(error: impl Into<AppError>) -> AppError {
    error.into()
}

/// Current wall clock in Unix milliseconds.
#[must_use]
pub fn current_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Fails when `workspace_id` has no row, for use-cases that must not write into
/// a deleted workspace.
pub fn ensure_workspace_exists(store: &WorkspaceStore, workspace_id: &str) -> Result<(), AppError> {
    let workspace_id = WorkspaceId::new(workspace_id).map_err(command_error)?;
    if store
        .get_workspace(&workspace_id)
        .map_err(command_error)?
        .is_none()
    {
        return Err(
            AppError::not_found(format!("workspace {} not found", workspace_id.as_str())).coded(
                crate::error::ErrorCode::WorkspaceNotFound,
                workspace_id.as_str().to_owned(),
            ),
        );
    }

    Ok(())
}

/// Maps a `false` delete result to a "not found in workspace" error.
pub fn deleted_or_not_found(
    deleted: bool,
    kind: &str,
    id: &str,
    workspace_id: &str,
) -> Result<(), AppError> {
    if deleted {
        Ok(())
    } else {
        Err(AppError::not_found(format!(
            "{kind} {id} not found in workspace {workspace_id}"
        )))
    }
}
