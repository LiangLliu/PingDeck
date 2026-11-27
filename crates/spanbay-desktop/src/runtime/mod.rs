//! Desktop application lifecycle, shared Tokio runtime, data-directory lock, and logging.
//!
//! Completely independent of any GUI toolkit: callers open [`DesktopRuntime`] before building
//! windows, display any [`StartFailure`] or set-aside notice via their [`crate::services::HostDialog`],
//! run [`DesktopRuntime::start_background_services`] after the first paint, and invoke
//! [`DesktopRuntime::shutdown_blocking`] on exit.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use spanbay_core::{
    AppLifecycleExt as _,
    app::{AppState, DataDirLock, SetAsideDatabase, StartOptions, start_chat_retention_job},
    error::AppError,
    logging::{LogGuard, LogLevel, init_logging, install_panic_hook, log_line},
    platform::{DataDirs, default_database_path},
    storage::StorageError,
};
use tokio::runtime::{Builder, Handle, Runtime};
use tokio::task::JoinHandle;

use crate::persistence::{ChatDraftsStore, UiLocalState};
use crate::services::ImportGrants;

/// Title of the native error dialog shown when the database cannot be opened.
pub const FAILURE_DIALOG_TITLE: &str = "SpanBay could not open its database";

/// Title of the notice shown when a database of another build was moved aside.
pub const SET_ASIDE_DIALOG_TITLE: &str = "SpanBay started with a new database";

/// Title of the notice shown when another SpanBay process uses the same data directory.
pub const RUNNING_DIALOG_TITLE: &str = "SpanBay is already running";

/// Holds the data-directory lock for the life of the process.
static INSTANCE_LOCK: OnceLock<DataDirLock> = OnceLock::new();

/// Keeps the rotating diagnostic log open for the life of the process.
static LOG_GUARD: OnceLock<LogGuard> = OnceLock::new();

/// Why startup stopped: dialog title, human-readable message, and intended process exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFailure {
    pub title: &'static str,
    pub message: String,
    pub status: i32,
}

impl StartFailure {
    pub fn failed(message: String) -> Self {
        Self {
            title: FAILURE_DIALOG_TITLE,
            message,
            status: 1,
        }
    }
}

/// Pure-Rust desktop runtime shared across the UI shell and all background services.
pub struct DesktopRuntime {
    state: AppState,
    data_dirs: Option<DataDirs>,
    tokio: Arc<Runtime>,
    grants: ImportGrants,
    set_aside_notice: Option<String>,
}

impl DesktopRuntime {
    /// Opens the default SpanBay database (respecting `SPANBAY_DATA_DIR` and debug/release
    /// separation), acquires the process data-directory lock, and initializes logging.
    pub fn open_default() -> Result<Self, StartFailure> {
        let dev_build = cfg!(debug_assertions);
        let path = default_database_path(dev_build)
            .map_err(|error| StartFailure::failed(failure_message(None, &error)))?;
        let dirs = DataDirs::for_database(&path);
        if dev_build {
            log_line(
                LogLevel::Info,
                "startup",
                &format!("data directory: {}", dirs.root.display()),
            );
        }
        lock_data_dir(&dirs)?;
        start_logging(&dirs);
        let (state, aside) = AppState::open_or_set_aside(&path)
            .map_err(|error| StartFailure::failed(open_failure_message(&path, &error)))?;
        state
            .on_start()
            .map_err(|error| StartFailure::failed(failure_message(Some(&path), error.message())))?;
        Self::from_parts(
            state,
            Some(dirs),
            aside.map(|info| set_aside_message(&info)),
        )
    }

    /// Opens a database at an explicit path without taking the global process `OnceLock` lock
    /// (intended for isolated headless tests and custom embedded sessions).
    pub fn open_at(path: &Path) -> Result<Self, StartFailure> {
        let dirs = DataDirs::for_database(path);
        let (state, aside) = AppState::open_or_set_aside(path)
            .map_err(|error| StartFailure::failed(open_failure_message(path, &error)))?;
        state
            .on_start()
            .map_err(|error| StartFailure::failed(failure_message(Some(path), error.message())))?;
        Self::from_parts(
            state,
            Some(dirs),
            aside.map(|info| set_aside_message(&info)),
        )
    }

    /// Opens an in-memory `AppState` for fast unit/headless tests.
    pub fn open_in_memory() -> Result<Self, StartFailure> {
        let state = AppState::open_in_memory()
            .map_err(|error| StartFailure::failed(format!("in-memory store failed: {error}")))?;
        Self::from_parts(state, None, None)
    }

    fn from_parts(
        state: AppState,
        data_dirs: Option<DataDirs>,
        set_aside_notice: Option<String>,
    ) -> Result<Self, StartFailure> {
        let tokio = Builder::new_multi_thread()
            .thread_name("spanbay-core")
            .enable_all()
            .build()
            .map_err(|error| {
                StartFailure::failed(format!("failed to start async runtime: {error}"))
            })?;
        let tokio = Arc::new(tokio);
        let grants = ImportGrants::new(tokio.handle().clone());
        Ok(Self {
            state,
            data_dirs,
            tokio,
            grants,
            set_aside_notice,
        })
    }

    /// Core application state (cheaply cloneable handle backed by `Arc`).
    #[must_use]
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Cloned `AppState` for async tasks.
    #[must_use]
    pub fn state_cloned(&self) -> AppState {
        self.state.clone()
    }

    /// Handle to the shared multi-threaded Tokio runtime.
    #[must_use]
    pub fn tokio_handle(&self) -> Handle {
        self.tokio.handle().clone()
    }

    /// Window path-grant registry for native drag-drop and file pickers.
    #[must_use]
    pub fn grants(&self) -> &ImportGrants {
        &self.grants
    }

    /// Notice explaining where a mismatched-schema database was moved aside, if any.
    #[must_use]
    pub fn set_aside_notice(&self) -> Option<&str> {
        self.set_aside_notice.as_deref()
    }

    /// Root data directory when backed by disk (`None` for in-memory test instances).
    #[must_use]
    pub fn data_root(&self) -> Option<&Path> {
        self.data_dirs.as_ref().map(|d| d.root.as_path())
    }

    /// Path for the local UI state JSON file.
    #[must_use]
    pub fn ui_state_path(&self) -> Option<PathBuf> {
        self.data_root().map(|root| root.join("ui_state.json"))
    }

    /// Path for the local unsent chat drafts JSON file.
    #[must_use]
    pub fn chat_drafts_path(&self) -> Option<PathBuf> {
        self.data_root().map(|root| root.join("chat_drafts.json"))
    }

    /// Returns the path to the active diagnostic log file when running against a data directory.
    #[must_use]
    pub fn log_file_path(&self) -> Option<PathBuf> {
        self.data_dirs
            .as_ref()
            .map(|d| d.logs.join(spanbay_core::logging::LOG_FILE_NAME))
    }

    /// Loads persisted local UI state (falling back to defaults on missing/corrupt file).
    #[must_use]
    pub fn load_ui_state(&self) -> UiLocalState {
        self.ui_state_path()
            .map(|p| UiLocalState::load_or_default(&p))
            .unwrap_or_default()
    }

    /// Saves local UI state atomically with `0600` permissions on Unix.
    pub fn save_ui_state(&self, ui: &UiLocalState) {
        if let Some(path) = self.ui_state_path()
            && let Err(error) = ui.save(&path)
        {
            log_line(
                LogLevel::Warn,
                "persistence",
                &format!("failed to save ui_state.json: {error}"),
            );
        }
    }

    /// Loads unsent chat drafts.
    #[must_use]
    pub fn load_chat_drafts(&self) -> ChatDraftsStore {
        self.chat_drafts_path()
            .map(|p| ChatDraftsStore::load_or_default(&p))
            .unwrap_or_default()
    }

    /// Saves unsent chat drafts atomically with `0600` permissions on Unix.
    pub fn save_chat_drafts(&self, drafts: &ChatDraftsStore) {
        if let Some(path) = self.chat_drafts_path()
            && let Err(error) = drafts.save(&path)
        {
            log_line(
                LogLevel::Warn,
                "persistence",
                &format!("failed to save chat_drafts.json: {error}"),
            );
        }
    }

    /// Spawns an async task on the shared core Tokio runtime.
    pub fn spawn_async<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tokio.spawn(future)
    }

    /// Spawns a blocking task (SQLite / filesystem work) on the Tokio blocking pool.
    pub fn spawn_blocking<F, R>(&self, work: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.tokio.spawn_blocking(work)
    }

    /// Runs a synchronous core use-case against a cloned `AppState` on the blocking pool.
    pub fn run_state<T, F>(&self, work: F) -> JoinHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(&AppState) -> T + Send + 'static,
    {
        let state = self.state.clone();
        self.tokio.spawn_blocking(move || work(&state))
    }

    /// Launches the post-paint background startup routines (`bootstrap_ai` and daily chat
    /// retention). Invokes `on_notices` once startup notices are ready.
    pub fn start_background_services<F>(&self, on_notices: F)
    where
        F: FnOnce(Vec<String>) + Send + 'static,
    {
        let state = self.state.clone();
        self.tokio.spawn(async move {
            state.bootstrap_ai(StartOptions::default()).await;
            let notices = state.startup_notices.get().unwrap_or_default();
            on_notices(notices);
        });

        let retention_state = self.state.clone();
        self.tokio.spawn(async move {
            if let Err(error) = start_chat_retention_job(&retention_state) {
                log_line(
                    LogLevel::Error,
                    "startup",
                    &format!("chat retention did not start: {}", error.message()),
                );
            }
        });
    }

    /// Runs `AppState::shutdown()` to completion, stopping agent runs, MCP children, mock
    /// servers, the balance scheduler, and the local gateway cleanly.
    pub fn shutdown_blocking(&self) {
        let state = self.state.clone();
        self.tokio.block_on(async move {
            state.shutdown().await;
        });
    }
}

fn start_logging(dirs: &DataDirs) {
    match init_logging(dirs) {
        Ok(guard) => {
            let _ = LOG_GUARD.set(guard);
        }
        Err(error) => log_line(
            LogLevel::Warn,
            "startup",
            &format!("log file unavailable: {}", error.message()),
        ),
    }
    install_panic_hook();
}

fn lock_data_dir(dirs: &DataDirs) -> Result<(), StartFailure> {
    let lock = DataDirLock::acquire(dirs).map_err(|error| lock_failure(dirs, &error))?;
    let _ = INSTANCE_LOCK.set(lock);
    Ok(())
}

/// Turns a data-directory lock error into a calm exit (when held by another instance) or failure.
#[must_use]
pub fn lock_failure(dirs: &DataDirs, error: &AppError) -> StartFailure {
    match error {
        AppError::Conflict { .. } => StartFailure {
            title: RUNNING_DIALOG_TITLE,
            message: format!(
                "Another SpanBay window is already using the data folder:\n{}\n\n\
                 Switch to it from the Dock, the menu bar or the app switcher. To run a second \
                 copy, start it with SPANBAY_DATA_DIR pointing at another folder.",
                dirs.root.display()
            ),
            status: 0,
        },
        _ => StartFailure::failed(failure_message(Some(&dirs.db), error.message())),
    }
}

/// Notice text after a database of another build was moved aside.
#[must_use]
pub fn set_aside_message(aside: &SetAsideDatabase) -> String {
    let files = aside
        .moved
        .iter()
        .map(|path| format!("  {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "The database was created by a different SpanBay build (schema {}) that this build \
         cannot open, so SpanBay moved it aside and started with an empty database.\n\n\
         The old files and folders are kept at:\n{files}\n\n\
         The .secrets file holds your secrets in plaintext: keep it private. To use the old data \
         again, move all of them back (without the .bak suffix) and start the SpanBay build \
         that created them.",
        aside.version
    )
}

/// Builds a human-readable database failure dialog message.
#[must_use]
pub fn failure_message(path: Option<&Path>, error: &str) -> String {
    match path {
        Some(path) => format!(
            "SpanBay could not open its database at:\n{}\n\n{error}\n\n\
             Make sure the file is writable and not in use by another SpanBay instance, \
             then start SpanBay again.",
            path.display()
        ),
        None => format!("SpanBay could not locate its data directory.\n\n{error}"),
    }
}

fn open_failure_message(path: &Path, error: &StorageError) -> String {
    match error {
        StorageError::UnsupportedSchemaVersion { .. } => {
            format!(
                "SpanBay could not open its database at:\n{}\n\n{error}",
                path.display()
            )
        }
        _ => failure_message(Some(path), &error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_runtime_opens_and_shuts_down_cleanly() {
        let runtime = DesktopRuntime::open_in_memory().expect("in-memory runtime opens");
        assert!(runtime.data_root().is_none());
        let workspaces =
            spanbay_core::app::workspace_list_impl(runtime.state()).expect("list workspaces");
        assert!(workspaces.is_empty());
        runtime.shutdown_blocking();
    }

    #[test]
    fn held_data_folder_produces_calm_zero_exit_and_other_errors_fail() {
        let dirs = DataDirs::for_database(Path::new("/data/spanbay.sqlite3"));
        let running = lock_failure(&dirs, &AppError::conflict("held"));
        assert_eq!((running.title, running.status), (RUNNING_DIALOG_TITLE, 0));
        assert!(running.message.contains("/data"));

        let broken = lock_failure(&dirs, &AppError::io("read-only file system"));
        assert_eq!((broken.title, broken.status), (FAILURE_DIALOG_TITLE, 1));
        assert!(broken.message.contains("read-only file system"));
    }

    #[test]
    fn set_aside_notice_names_schema_and_moved_files() {
        let notice = set_aside_message(&SetAsideDatabase {
            version: 42,
            moved: vec![
                "/data/spanbay.sqlite3.bak-20260101-000000".into(),
                "/data/spanbay.sqlite3.secrets.bak-20260101-000000".into(),
            ],
        });
        assert!(notice.contains("schema 42"));
        assert!(notice.contains("spanbay.sqlite3.bak-20260101-000000"));
        assert!(notice.contains("spanbay.sqlite3.secrets.bak-20260101-000000"));
    }
}
