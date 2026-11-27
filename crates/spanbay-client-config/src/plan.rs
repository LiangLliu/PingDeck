//! The request checks and the preview: what each file would look like after a write.

use std::path::{Path, PathBuf};

use spanbay_base::{crypto::sha256_hex, diff::unified_diff_text, error::AppError};

use super::document::display;
use super::fsio::{git_work_tree, real_location};
use super::mask::mask_secrets;
use super::{ClientConfigPlan, ClientConfigRequest, FilePlan, ModelFacts};

/// Validated request values shared by the per-app renderers.
pub(super) struct Settings<'a> {
    /// Gateway URL without trailing slash.
    pub(super) gateway: &'a str,
    /// The client's gateway token.
    pub(super) token: &'a str,
    /// Gateway model id to write; `None` leaves the tool's model untouched.
    pub(super) model: Option<&'a str>,
    /// Alias for background tasks (Claude Code); `None` falls back to `model`.
    pub(super) fast_model: Option<&'a str>,
    /// What is known about `model` (context window, output limit, reasoning, ...).
    pub(super) facts: &'a ModelFacts,
}

/// One file's computed edit.
pub(super) struct Edit {
    /// The file as the writer names it (before symlinks are resolved).
    pub(super) path: PathBuf,
    /// Current content, `None` when the file does not exist.
    pub(super) before: Option<String>,
    /// Content after the merge.
    pub(super) after: String,
}

impl Edit {
    /// Whether the merge changes the file.
    pub(super) fn changed(&self) -> bool {
        self.before.as_deref() != Some(self.after.as_str())
    }

    /// SHA-256 (lower-case hex) of the current content, `None` for a new file.
    pub(super) fn base_sha256(&self) -> Option<String> {
        self.before
            .as_deref()
            .map(|before| sha256_hex(before.as_bytes()))
    }
}

/// Computes the merged config for `request` without writing anything.
///
/// The gateway token is masked in `before`, `after` and `diff` (`pd_cc_••••9d2f`): a preview is
/// meant for the screen, while [`super::apply`] writes the real value.
///
/// # Errors
/// [`AppError::validation`] for a bad gateway URL/token or an existing file that is not valid
/// JSON/TOML (the unparsable-file message, with its line number); [`AppError::io`] when an
/// existing file cannot be read.
pub fn preview(home: &Path, request: &ClientConfigRequest) -> Result<ClientConfigPlan, AppError> {
    let token = request.token.trim();
    let files = plan_edits(home, request)?
        .into_iter()
        .map(|edit| file_plan(home, edit, token))
        .collect();
    Ok(ClientConfigPlan {
        app: request.app,
        client_name: request.client_name.clone(),
        files,
    })
}

/// The screen form of one computed edit: `token` and the user's own credentials masked, and
/// where the write really lands when a symbolic link leads elsewhere.
fn file_plan(home: &Path, edit: Edit, token: &str) -> FilePlan {
    let changed = edit.changed();
    let path = display(&edit.path);
    let diff = if changed {
        unified_diff_text(&path, edit.before.as_deref().unwrap_or(""), &edit.after)
    } else {
        String::new()
    };
    let real = real_location(home, &edit.path);
    let git_work_tree =
        git_work_tree(real.as_deref().unwrap_or(&edit.path)).map(|dir| display(&dir));
    FilePlan {
        path,
        is_new: edit.before.is_none(),
        base_sha256: edit.base_sha256(),
        real_path: real.map(|real| display(&real)),
        git_work_tree,
        before: edit.before.map(|before| mask_secrets(&before, token)),
        after: mask_secrets(&edit.after, token),
        changed,
        diff: mask_secrets(&diff, token),
    }
}

/// A gateway URL without surrounding blanks or trailing slashes.
///
/// # Errors
/// [`AppError::validation`] unless it is an http(s) URL with a host.
pub(super) fn check_gateway_url(url: &str) -> Result<&str, AppError> {
    let gateway = url.trim().trim_end_matches('/');
    let valid = url::Url::parse(gateway)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some());
    if valid {
        Ok(gateway)
    } else {
        Err(AppError::validation(format!(
            "gateway URL `{gateway}` is not a valid http(s) URL"
        )))
    }
}

/// A token without surrounding blanks.
///
/// # Errors
/// [`AppError::validation`] when it is empty or holds whitespace, control characters or quotes.
pub(super) fn check_token(token: &str) -> Result<&str, AppError> {
    let token = token.trim();
    if token.is_empty()
        || token
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"')
    {
        return Err(AppError::validation(
            "gateway token must be non-empty and contain no whitespace, control characters or quotes",
        ));
    }
    Ok(token)
}

/// `value` trimmed, `None` when it is missing or blank.
pub(super) fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// The edits `request` makes to its tool's files, after the request values are checked.
pub(super) fn plan_edits(
    home: &Path,
    request: &ClientConfigRequest,
) -> Result<Vec<Edit>, AppError> {
    let settings = Settings {
        gateway: check_gateway_url(&request.gateway_url)?,
        token: check_token(&request.token)?,
        model: non_blank(request.alias.as_deref()),
        fast_model: non_blank(request.fast_alias.as_deref()),
        facts: &request.facts,
    };
    (request.app.writer().render)(home, &settings)
}
