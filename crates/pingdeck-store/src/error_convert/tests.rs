use pingdeck_model::validation::WorkspaceError;

use super::*;

#[test]
fn every_storage_error_maps_to_its_app_error_class() {
    let database = AppError::database("");
    let validation = AppError::validation("");
    let conflict = AppError::conflict("");
    let not_found = AppError::not_found("");
    let folder = |fault| StorageError::InvalidFolderMove {
        reason: "r".into(),
        fault,
    };
    let request = |fault| StorageError::InvalidRequestPlacement {
        reason: "r".into(),
        fault,
    };
    let cases = [
        (
            StorageError::Sqlite(rusqlite::Error::QueryReturnedNoRows),
            database.clone(),
        ),
        (
            StorageError::Json(serde_json::from_str::<i32>("x").unwrap_err()),
            database.clone(),
        ),
        (
            StorageError::Io(std::io::Error::other("x")),
            AppError::io(""),
        ),
        // The start-up set-aside of a database from another build keys on this class.
        (
            StorageError::UnsupportedSchemaVersion { version: 7 },
            AppError::unsupported(""),
        ),
        (
            StorageError::TabSessionTooLarge {
                workspace_id: "w".into(),
                size_bytes: 2,
                limit_bytes: 1,
            },
            validation.clone(),
        ),
        (
            StorageError::ItemWorkspaceMismatch {
                kind: "flow",
                id: "f".into(),
            },
            conflict.clone(),
        ),
        (
            StorageError::InvalidAgentData { reason: "r".into() },
            validation.clone(),
        ),
        (
            StorageError::Workspace(WorkspaceError::BlankId { entity: "request" }),
            validation.clone(),
        ),
        (
            StorageError::InvalidStoredVariable {
                environment_id: "e".into(),
                key: "k".into(),
                reason: "r",
            },
            database.clone(),
        ),
        (
            StorageError::InvalidProtocol {
                request_id: "r".into(),
                protocol: "p".into(),
            },
            database.clone(),
        ),
        (
            StorageError::InvalidHttpMethod {
                request_id: "r".into(),
                method: "m".into(),
            },
            database.clone(),
        ),
        (
            StorageError::InvalidExecutionHistoryResultKind {
                history_entry_id: "h".into(),
                result_kind: "k".into(),
            },
            database.clone(),
        ),
        (
            StorageError::InvalidStoredValue {
                table: "t",
                column: "c",
                value: "v".into(),
            },
            database.clone(),
        ),
        (folder(PlacementFault::Invalid), validation.clone()),
        (folder(PlacementFault::NotFound), not_found.clone()),
        (request(PlacementFault::NotFound), not_found.clone()),
        (request(PlacementFault::Conflict), conflict.clone()),
    ];
    for (error, expected) in cases {
        let label = format!("{error:?}");
        assert_eq!(
            std::mem::discriminant(&AppError::from(error)),
            std::mem::discriminant(&expected),
            "{label}"
        );
    }
}

#[test]
fn a_busy_writer_a_full_disk_and_a_refused_file_carry_their_code() {
    let sqlite = |code| {
        StorageError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            None,
        ))
    };
    let cases = [
        (
            sqlite(rusqlite::ffi::SQLITE_BUSY),
            Some(ErrorCode::DatabaseBusy),
        ),
        (
            sqlite(rusqlite::ffi::SQLITE_LOCKED),
            Some(ErrorCode::DatabaseBusy),
        ),
        (
            sqlite(rusqlite::ffi::SQLITE_FULL),
            Some(ErrorCode::DiskFull),
        ),
        (
            sqlite(rusqlite::ffi::SQLITE_READONLY),
            Some(ErrorCode::PermissionDenied),
        ),
        (sqlite(rusqlite::ffi::SQLITE_CORRUPT), None),
        (
            StorageError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            Some(ErrorCode::PermissionDenied),
        ),
    ];
    for (error, code) in cases {
        let label = format!("{error:?}");
        assert_eq!(AppError::from(error).code(), code, "{label}");
    }
}
