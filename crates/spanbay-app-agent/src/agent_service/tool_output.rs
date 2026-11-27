//! Reading the kept full output of a tool call back in pages ("Show full
//! output", `read_output`), and clearing it.
//!
//! Single responsibility: check that the call belongs to the chat and that the core kept its
//! output (`agent_tool_calls.spill_ref`), then page through the file the runner wrote
//! (`<data_dir>/tool-output/<session_id>/<call_id>`); the file name, offset and size limits are
//! `crate::agent_tools::shell::read_tool_output`'s. Purge removes the files of one chat or all.

use spanbay_model::agent::PurgeResult;

use crate::agent::secret_guard::expand_needles;
use crate::agent::tool_output_files::{CLEARED, purge, session_dir, touch_session};
use crate::agent::tools::shell_meta::ToolOutputChunk;
use crate::agent::tools::shell_meta::valid_call_id;
use crate::agent_tools::shell::read_tool_output;
use crate::error::AppError;
use crate::secrets::redact_secrets;

use crate::app::{AppState, command_error};

/// One page of the full output of call `call_id` of chat `session_id`, from byte `offset`, at
/// most `limit` bytes (capped at 64 KiB; a page never ends inside a character).
///
/// # Errors
/// [`AppError::Validation`] for a call id that is not a safe file name, [`AppError::NotFound`]
/// when the chat made no such call, no full output was kept for it, or it was cleared since,
/// [`AppError::Unsupported`] without a data directory.
pub fn agent_tool_output_read_impl(
    state: &AppState,
    session_id: &str,
    call_id: &str,
    offset: u64,
    limit: u32,
) -> Result<ToolOutputChunk, AppError> {
    if !valid_call_id(call_id) {
        return Err(AppError::validation(
            "the call id is not a valid output name",
        ));
    }
    let record = state
        .store()
        .get_tool_call(session_id, call_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found("this chat made no such tool call"))?;
    let root = tool_output_root(state)?;
    if record.spill_ref.is_none() {
        return Err(AppError::not_found("no full output was kept for this call"));
    }
    let dir = session_dir(&root, session_id)
        .ok_or_else(|| AppError::validation("the chat id is not a valid output name"))?;
    let mut chunk =
        read_tool_output(&dir, call_id, offset, limit).map_err(|error| match error {
            AppError::NotFound { .. } => AppError::not_found(CLEARED),
            other => other,
        })?;
    touch_session(&root, session_id);
    // Kept files are redacted when written. This covers any secret saved since; a secret cut by
    // a page boundary is not matched here, which the write side handles.
    let needles = expand_needles(state.secrets().active_redaction_needles());
    chunk.text = redact_secrets(&chunk.text, &needles);
    Ok(chunk)
}

/// Reads the entire kept spill file for `(session_id, call_id)` in one pass (verifying ownership,
/// `spill_ref`, and redacting secrets once instead of once per 64 KiB page). Used by `read_output`
/// when `grep` is supplied.
///
/// # Errors
/// Same error cases as [`agent_tool_output_read_impl`].
pub(crate) fn agent_tool_output_read_all_impl(
    state: &AppState,
    session_id: &str,
    call_id: &str,
) -> Result<String, AppError> {
    if !valid_call_id(call_id) {
        return Err(AppError::validation(
            "the call id is not a valid output name",
        ));
    }
    let record = state
        .store()
        .get_tool_call(session_id, call_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found("this chat made no such tool call"))?;
    let root = tool_output_root(state)?;
    if record.spill_ref.is_none() {
        return Err(AppError::not_found("no full output was kept for this call"));
    }
    let dir = session_dir(&root, session_id)
        .ok_or_else(|| AppError::validation("the chat id is not a valid output name"))?;
    let path = dir.join(call_id);
    let bytes = std::fs::read(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => AppError::not_found(CLEARED),
        _ => AppError::internal(format!("reading kept output failed: {error}")),
    })?;
    touch_session(&root, session_id);
    let raw = String::from_utf8_lossy(&bytes);
    let needles = expand_needles(state.secrets().active_redaction_needles());
    Ok(redact_secrets(&raw, &needles))
}

/// Removes the kept full output of chat `session_id`, or of every chat with `None`, and reports
/// how many files and bytes went. A state without a data directory has nothing to remove.
///
/// # Errors
/// [`AppError::Validation`] for a chat id that is not a safe file name.
pub fn agent_tool_output_delete_impl(
    state: &AppState,
    session_id: Option<&str>,
) -> Result<PurgeResult, AppError> {
    if session_id.is_some_and(|id| !valid_call_id(id)) {
        return Err(AppError::validation(
            "the chat id is not a valid output name",
        ));
    }
    let Ok(root) = tool_output_root(state) else {
        return Ok(PurgeResult::default());
    };
    let purged = purge(&root, session_id);
    Ok(PurgeResult {
        files: purged.files,
        bytes: purged.bytes,
    })
}

/// `<data_dir>/tool-output`.
fn tool_output_root(state: &AppState) -> Result<std::path::PathBuf, AppError> {
    state
        .data_dirs()
        .map(|dirs| dirs.root.join("tool-output"))
        .ok_or_else(|| AppError::unsupported("this state has no data folder to read output from"))
}
