//! `StorageError` to the shared [`AppError`]: the orphan rule requires this impl to live in the
//! crate that defines `StorageError`.

use spanbay_base::error::{AppError, ErrorCode, io_code};

use crate::storage::{PlacementFault, StorageError};

/// The [`ErrorCode`] of an SQLite failure a UI explains on its own (a busy writer, a full disk,
/// a refused file); the rest stay generic database errors.
fn sqlite_code(error: &rusqlite::Error) -> Option<ErrorCode> {
    use rusqlite::ErrorCode as Sqlite;
    match error.sqlite_error_code()? {
        Sqlite::DatabaseBusy | Sqlite::DatabaseLocked => Some(ErrorCode::DatabaseBusy),
        Sqlite::DiskFull => Some(ErrorCode::DiskFull),
        Sqlite::ReadOnly | Sqlite::PermissionDenied => Some(ErrorCode::PermissionDenied),
        _ => None,
    }
}

impl From<StorageError> for AppError {
    fn from(error: StorageError) -> Self {
        let message = error.to_string();
        // Every variant is listed so a new one has to pick its error class.
        match error {
            StorageError::Workspace(inner) => inner.into(),
            StorageError::Io(inner) => match io_code(inner.kind()) {
                Some(code) => Self::io(message).coded(code, None),
                None => Self::io(message),
            },
            StorageError::Sqlite(inner) => match sqlite_code(&inner) {
                Some(code) => Self::database(message).coded(code, None),
                None => Self::database(message),
            },
            StorageError::UnsupportedSchemaVersion { .. } => Self::unsupported(message),
            StorageError::InvalidFolderMove { fault, .. }
            | StorageError::InvalidRequestPlacement { fault, .. } => match fault {
                PlacementFault::Invalid => Self::validation(message),
                PlacementFault::NotFound => Self::not_found(message),
                PlacementFault::Conflict => Self::conflict(message),
            },
            StorageError::TabSessionTooLarge { .. } => Self::validation(message),
            StorageError::ItemWorkspaceMismatch { .. } => Self::conflict(message),
            StorageError::InvalidAgentData { .. } => Self::validation(message),
            StorageError::Json(_)
            | StorageError::InvalidStoredVariable { .. }
            | StorageError::InvalidProtocol { .. }
            | StorageError::InvalidHttpMethod { .. }
            | StorageError::InvalidExecutionHistoryResultKind { .. }
            | StorageError::InvalidStoredValue { .. } => Self::database(message),
        }
    }
}

#[cfg(test)]
mod tests;
