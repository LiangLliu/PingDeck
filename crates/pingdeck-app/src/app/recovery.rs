//! Startup recovery for incomplete agent file-change journals left behind by a crash.

use std::path::Path;

use crate::crypto::sha256_hex;
use crate::error::AppError;
use crate::workspace::WorkspaceStore;

/// Completes or drops the journal rows a crash left `Applying`: a row whose file now holds the
/// expected post-edit hash (for a delete: whose file is gone) is finished (undoable); any other
/// row belongs to an incomplete change and is deleted.
///
/// # Errors
/// [`AppError::Database`] when the journal cannot be read or updated.
pub fn recover_file_journal(store: &WorkspaceStore) -> Result<(), AppError> {
    let rows = store.list_applying_file_changes()?;
    for row in rows {
        let workspace = match row.folder.clone() {
            Some(folder) => Some(folder),
            None => store
                .get_agent_session(&row.session_id)?
                .and_then(|session| session.workspace_dir),
        };
        let written = workspace
            .and_then(|dir| std::fs::read(Path::new(&dir).join(&row.path)).ok())
            .map(|bytes| sha256_hex(&bytes));
        // A delete (an edit row without an after hash) is complete once the file is gone.
        let deleted = row.after_sha256.is_none() && row.before_sha256.is_some();
        if written == row.after_sha256 && (written.is_some() || deleted) {
            store.finish_file_change(&row.id)?;
        } else {
            store.delete_file_change(&row.id)?;
        }
    }
    Ok(())
}
