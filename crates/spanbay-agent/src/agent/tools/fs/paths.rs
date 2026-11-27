//! Path confinement for the file tools.
//!
//! Single responsibility: turn the model's path string into an absolute path that is provably
//! inside the session workspace and outside the places no agent may touch, and say which inside
//! paths need an explicit approval even under `auto_all`.
//!
//! Resolution walks the candidate component by component and canonicalizes every existing prefix,
//! so `..` and symlinks (also on the way to a file that does not exist yet) cannot lead out of the
//! workspace. Paths that do not exist yet are resolved lexically below their deepest existing
//! ancestor; a dangling symlink is refused because creating a file "through" it would escape.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::agent::model::{ToolErrorCode, coded_error_text};
use crate::agent::sandbox::denied_locations;
use crate::agent::sandbox::protect::{CARGO_CONFIGS, PROTECTED_DIRS, STARTUP_FILES};
use spanbay_base::error::AppError;

/// The directories a path may never be inside, whatever the workspace is.
#[derive(Debug, Clone, Default)]
pub struct PathGuard {
    denied: Vec<PathBuf>,
}

impl PathGuard {
    /// A guard that denies `data_root` (SpanBay's data directory: database, secrets, attachments)
    /// and every credential location below `home` the shell sandbox hides (`~/.ssh`, `~/.aws`,
    /// `~/.kube`, …: the one list of [`denied_locations`]), under their canonical spellings.
    #[must_use]
    pub fn new(data_root: Option<&Path>, home: Option<&Path>) -> Self {
        Self {
            denied: denied_locations(data_root, home)
                .into_iter()
                .map(|(path, _)| path)
                .collect(),
        }
    }

    /// A guard for this process: `data_root` plus the `$HOME` (`%USERPROFILE%`) of the process.
    #[must_use]
    pub fn for_process(data_root: Option<&Path>) -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        Self::new(data_root, home.as_deref())
    }

    /// Whether `path` (already canonical) is a denied location or inside one.
    #[must_use]
    pub fn denies(&self, path: &Path) -> bool {
        self.denied.iter().any(|denied| path.starts_with(denied))
    }

    /// Resolves `candidate` against the workspace like [`resolve_workspace_path`], additionally
    /// denying this guard's directories. `workspace` must already be canonical (see
    /// [`canonical_workspace`]).
    ///
    /// # Errors
    /// [`AppError::Validation`] for a NUL byte, an escape from the workspace, a denied location, a
    /// dangling symlink or (reads only) a path that does not exist.
    pub fn resolve(
        &self,
        workspace: &Path,
        candidate: &str,
        for_write: bool,
    ) -> Result<PathBuf, AppError> {
        match self.confine_coded(workspace, candidate, for_write)? {
            Ok(path) => Ok(path),
            Err((true, reason)) => Err(AppError::validation(coded_error_text(
                ToolErrorCode::OutsideFolder,
                &reason,
            ))),
            Err((false, reason)) => Err(AppError::validation(reason)),
        }
    }

    /// As [`PathGuard::resolve`] with the plain refusal message and no `outside_folder` code line:
    /// for user-driven callers (undo, the change diff) whose error is shown as text, not parsed out
    /// of a stored tool result.
    ///
    /// # Errors
    /// The same as [`PathGuard::resolve`].
    pub fn resolve_plain(
        &self,
        workspace: &Path,
        candidate: &str,
        for_write: bool,
    ) -> Result<PathBuf, AppError> {
        self.confine(workspace, candidate, for_write)?
            .map_err(AppError::validation)
    }

    /// As [`PathGuard::resolve`], but a path outside the workspace or in a denied location is
    /// `Ok(Err(reason))`: a call on it is refused outright (a hard block), whereas the outer error
    /// is a mistake in the call itself.
    ///
    /// # Errors
    /// [`AppError::Validation`] for a NUL byte, a dangling symlink or (reads only) a path that
    /// does not exist.
    pub fn confine(
        &self,
        workspace: &Path,
        candidate: &str,
        for_write: bool,
    ) -> Result<Result<PathBuf, String>, AppError> {
        Ok(self
            .confine_coded(workspace, candidate, for_write)?
            .map_err(|(_, reason)| reason))
    }

    /// As [`PathGuard::confine`]; a refusal also says whether the path lies outside the folder
    /// (as opposed to in a protected location), which only [`PathGuard::resolve`] needs for its
    /// `outside_folder` code line.
    fn confine_coded(
        &self,
        workspace: &Path,
        candidate: &str,
        for_write: bool,
    ) -> Result<Result<PathBuf, (bool, String)>, AppError> {
        if candidate.contains('\0') {
            return Err(AppError::validation("The path contains a NUL byte"));
        }
        let candidate = if candidate.is_empty() { "." } else { candidate };
        let (resolved, exists) = resolve_lenient(workspace, Path::new(candidate))?;
        if !resolved.starts_with(workspace) {
            return Ok(Err((
                true,
                format!(
                    "'{candidate}' is outside the workspace ({})",
                    workspace.display()
                ),
            )));
        }
        if self.denies(&resolved) {
            return Ok(Err((
                false,
                format!(
                    "'{candidate}' is a protected location (SpanBay data or credentials) and cannot be accessed"
                ),
            )));
        }
        if !exists && !for_write {
            return Err(AppError::validation(format!(
                "'{candidate}' does not exist"
            )));
        }
        Ok(Ok(resolved))
    }
}

/// The canonical form of the session workspace directory.
///
/// # Errors
/// [`AppError::Validation`] when it does not exist or is not a directory.
pub fn canonical_workspace(workspace_dir: &Path) -> Result<PathBuf, AppError> {
    let canonical = fs::canonicalize(workspace_dir).map_err(|error| {
        AppError::validation(format!(
            "The workspace folder {} is not accessible: {error}",
            workspace_dir.display()
        ))
    })?;
    if canonical.is_dir() {
        Ok(canonical)
    } else {
        Err(AppError::validation(format!(
            "The workspace {} is not a folder",
            workspace_dir.display()
        )))
    }
}

/// Checks that the canonical folder `canonical` may be a session workspace: not the filesystem
/// root, not the home directory or a folder above it, not SpanBay's data directory (or inside
/// or above it), and neither inside nor above a hidden credential location. The shell sandbox lets
/// a command write the whole workspace and the file tools read it, so such a folder would hand out
/// what the read-deny list hides. The one check of the session folder, the file tools and `shell`.
///
/// # Errors
/// [`AppError::Validation`] naming the reason.
pub fn check_workspace(
    canonical: &Path,
    home: Option<&Path>,
    data_root: Option<&Path>,
) -> Result<(), AppError> {
    if canonical.parent().is_none() {
        return Err(AppError::validation(
            "the workspace cannot be the filesystem root; pick a project folder",
        ));
    }
    if home.is_some_and(|home| home.starts_with(canonical)) {
        return Err(AppError::validation(
            "the workspace cannot be the home directory or a folder above it; pick a project folder",
        ));
    }
    let overlaps = |path: &Path| path.starts_with(canonical) || canonical.starts_with(path);
    if data_root.is_some_and(overlaps) {
        return Err(AppError::validation(
            "the workspace cannot be SpanBay's own data directory or a folder above it",
        ));
    }
    if denied_locations(None, home)
        .iter()
        .any(|(path, _)| overlaps(path))
    {
        return Err(AppError::validation(
            "the workspace cannot contain or be inside a credential folder (such as ~/.ssh, ~/.aws or ~/Library/Keychains); pick a project folder",
        ));
    }
    Ok(())
}

/// Resolves `candidate` to an absolute path inside `workspace_dir`: NUL bytes are
/// rejected, relative paths start at the canonical workspace, every existing ancestor is
/// canonicalized (so `..` and symlinks cannot escape, also for files about to be created) and
/// the credential locations of the home directory are always denied. With `for_write` the final component may
/// not exist yet; reads need an existing path. The tools use a [`PathGuard`] that also denies
/// SpanBay's data directory; this function has no data root to learn it from.
///
/// # Errors
/// [`AppError::Validation`] naming the offending path.
pub fn resolve_workspace_path(
    workspace_dir: &Path,
    candidate: &str,
    for_write: bool,
) -> Result<PathBuf, AppError> {
    PathGuard::for_process(None).resolve_plain(
        &canonical_workspace(workspace_dir)?,
        candidate,
        for_write,
    )
}

/// Walks `candidate` (resolved against `base` when relative) component by component, canonicalizing
/// each existing prefix. Returns the resolved path and whether all of it exists.
fn resolve_lenient(base: &Path, candidate: &Path) -> Result<(PathBuf, bool), AppError> {
    let mut resolved = PathBuf::new();
    let mut exists = true;
    if candidate.is_relative() {
        resolved.push(base);
    }
    for component in candidate.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                if !exists {
                    continue;
                }
                match fs::canonicalize(&resolved) {
                    Ok(real) => resolved = real,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        if fs::symlink_metadata(&resolved).is_ok() {
                            return Err(AppError::validation(format!(
                                "{} is a dangling symlink",
                                resolved.display()
                            )));
                        }
                        exists = false;
                    }
                    Err(error) => {
                        return Err(AppError::validation(format!(
                            "{} is not accessible: {error}",
                            resolved.display()
                        )));
                    }
                }
            }
        }
    }
    Ok((resolved, exists))
}

/// The workspace-relative spelling of `path` with `/` separators (`.` for the workspace itself).
#[must_use]
pub fn relative_display(workspace: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(workspace).unwrap_or(path);
    let text = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/");
    if text.is_empty() {
        ".".to_owned()
    } else {
        text
    }
}

/// Credential files that ask on a read: well-known names (`.git-credentials`, `.npmrc`, `.pypirc`,
/// `.netrc`).
const CREDENTIAL_FILES: [&str; 4] = [".git-credentials", ".npmrc", ".pypirc", ".netrc"];

/// Extensions of key and certificate bundles that ask on a read.
const CREDENTIAL_EXTENSIONS: [&str; 3] = [".pem", ".p12", ".pfx"];

/// Why a call on `path` (canonical, inside `workspace`) needs an explicit approval even under
/// `auto_all`, if it does: `.env*` files and credential files (any access: `.git/config`, whose
/// remote URLs often hold tokens, `.git-credentials`, `.npmrc`, `.pypirc`, `.netrc`,
/// `.docker/config.json`, `id_*` keys, `*.pem`, `*.p12`, `*.pfx`) and, for writes, git hooks and
/// config, `.claude/`, `.agents/`, `.cursor/`, `.husky/`, `.vscode/`, `.idea/`,
/// `.cargo/config.toml` and shell or direnv startup files (the sandbox's protected list). Matching is ASCII case-insensitive
/// because macOS and Windows file systems are.
#[must_use]
pub fn approval_reason(workspace: &Path, path: &Path, for_write: bool) -> Option<String> {
    let relative = path.strip_prefix(workspace).unwrap_or(path);
    let names: Vec<&OsStr> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    let is = |name: &OsStr, expected: &str| name.eq_ignore_ascii_case(expected);
    let last = names.last().and_then(|name| name.to_str()).unwrap_or("");
    if last
        .as_bytes()
        .get(..4)
        .is_some_and(|head| head.eq_ignore_ascii_case(b".env"))
    {
        return Some(format!(
            "'{}' is an environment file that may hold secrets",
            relative_display(workspace, path)
        ));
    }
    if !for_write {
        let name = last.to_ascii_lowercase();
        let credential = CREDENTIAL_FILES.contains(&name.as_str())
            || (name.starts_with("id_") && !name.ends_with(".pub"))
            || CREDENTIAL_EXTENSIONS.iter().any(|ext| name.ends_with(ext))
            || names.windows(2).any(|pair| {
                (is(pair[0], ".git") && is(pair[1], "config"))
                    || (is(pair[0], ".docker") && is(pair[1], "config.json"))
            });
        return credential.then(|| {
            format!(
                "'{}' may hold credentials",
                relative_display(workspace, path)
            )
        });
    }
    let protected = names.iter().enumerate().any(|(index, name)| {
        let next = names.get(index + 1);
        (is(name, ".git") && next.is_some_and(|next| is(next, "hooks") || is(next, "config")))
            || (is(name, ".cargo")
                && next.is_some_and(|next| CARGO_CONFIGS.iter().any(|config| is(next, config))))
            || PROTECTED_DIRS.iter().any(|dir| is(name, dir))
    }) || STARTUP_FILES
        .iter()
        .any(|startup| names.last().is_some_and(|name| is(name, startup)));
    protected.then(|| {
        format!(
            "'{}' can change what runs on this machine (git hooks, agent settings or shell startup)",
            relative_display(workspace, path)
        )
    })
}
