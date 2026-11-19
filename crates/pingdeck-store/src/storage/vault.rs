//! Where the `secrets` table lives: the owner-only `<db>.secrets` vault for file databases,
//! `main` for in-memory ones.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::StorageError;
use super::schema::SECRETS_COLUMNS;

/// Schema name the vault file is attached under.
const VAULT: &str = "vault";

/// The vault file next to `database`: its file name plus `.secrets`.
#[must_use]
pub(super) fn vault_path(database: &Path) -> PathBuf {
    let name = database
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    database.with_file_name(format!("{name}.secrets"))
}

fn vault_attached(connection: &Connection) -> Result<bool, StorageError> {
    Ok(connection.query_row(
        "SELECT count(*) FROM pragma_database_list WHERE name = ?1",
        [VAULT],
        |row| row.get::<_, i64>(0),
    )? > 0)
}

/// Attaches and initializes the owner-only vault; safe to call again on an attached connection.
///
/// The file is created (or tightened) to `0600` without truncation, so two connections opening
/// the same new database at once cannot empty a vault the other has just initialized. `ATTACH`
/// binds the path through `to_string_lossy()`, so a non-UTF-8 path would attach a different
/// file. The schema name is spelled out in the SQL rather than taken from [`VAULT`] so the
/// statement text stays fixed.
///
/// # Errors
/// [`StorageError`] if the file cannot be created, restricted or attached.
pub(super) fn attach_vault(connection: &Connection, database: &Path) -> Result<(), StorageError> {
    if vault_attached(connection)? {
        return Ok(());
    }
    let path = vault_path(database);
    pingdeck_base::platform::open_private_append(&path).map(drop)?;
    connection.execute(
        "ATTACH DATABASE ?1 AS vault",
        [path.to_string_lossy().as_ref()],
    )?;
    connection.execute_batch(&format!(
        "PRAGMA vault.secure_delete = ON; CREATE TABLE IF NOT EXISTS vault.secrets ({SECRETS_COLUMNS});"
    ))?;
    Ok(())
}

/// Creates the in-memory secrets table only when no vault is attached, avoiding vault shadowing.
pub(super) fn create_memory_secrets(connection: &Connection) -> Result<(), StorageError> {
    if !vault_attached(connection)? {
        connection.execute_batch(&format!("CREATE TABLE secrets ({SECRETS_COLUMNS});"))?;
    }
    Ok(())
}
