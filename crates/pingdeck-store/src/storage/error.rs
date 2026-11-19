//! The error type of the SQLite-backed store and the placement fault classes it carries.

use thiserror::Error;

use pingdeck_model::validation::WorkspaceError;

/// Why a folder move or request placement was rejected, so callers can pick an error class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementFault {
    /// The request is malformed (cycle, duplicate id, wrong shape).
    Invalid,
    /// A referenced collection, folder or parent does not exist.
    NotFound,
    /// The target belongs to a different workspace or collection.
    Conflict,
}

/// Failure raised by the SQLite-backed workspace store.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite storage error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("workspace data error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("json storage error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("storage file error: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "the database was created by a different PingDeck build (schema {version}) and this build has no migration for it; keep a copy of the database and its .secrets file, move both out of the data folder (or point PINGDECK_DATA_DIR at an empty folder) and start again"
    )]
    UnsupportedSchemaVersion { version: i64 },
    #[error("invalid stored variable {key} in environment {environment_id}: {reason}")]
    InvalidStoredVariable {
        environment_id: String,
        key: String,
        reason: &'static str,
    },
    #[error("invalid request protocol stored for request {request_id}: {protocol}")]
    InvalidProtocol {
        request_id: String,
        protocol: String,
    },
    #[error("invalid HTTP method stored for request {request_id}: {method}")]
    InvalidHttpMethod { request_id: String, method: String },
    #[error(
        "invalid execution history result kind stored for entry {history_entry_id}: {result_kind}"
    )]
    InvalidExecutionHistoryResultKind {
        history_entry_id: String,
        result_kind: String,
    },
    #[error("invalid folder move: {reason}")]
    InvalidFolderMove {
        reason: String,
        fault: PlacementFault,
    },
    #[error("invalid request placement: {reason}")]
    InvalidRequestPlacement {
        reason: String,
        fault: PlacementFault,
    },
    #[error(
        "tab session for workspace {workspace_id} is {size_bytes} bytes, over the {limit_bytes} byte limit"
    )]
    TabSessionTooLarge {
        workspace_id: String,
        size_bytes: usize,
        limit_bytes: usize,
    },
    #[error("invalid stored value for {table}.{column}: {value}")]
    InvalidStoredValue {
        table: &'static str,
        column: &'static str,
        value: String,
    },
    #[error("{kind} {id} belongs to another workspace")]
    ItemWorkspaceMismatch { kind: &'static str, id: String },
    /// An agent-store call whose arguments cannot be stored (a malformed rule, a tool turn
    /// without tool calls, a message id of another session).
    #[error("invalid agent data: {reason}")]
    InvalidAgentData { reason: String },
}

impl StorageError {
    /// Whether SQLite rejected the write with a constraint violation (unique, foreign key, check,
    /// not null), so a caller can report a racing duplicate as its own error.
    #[must_use]
    pub fn is_constraint_violation(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(failure, _))
                if failure.code == rusqlite::ErrorCode::ConstraintViolation
        )
    }
}
