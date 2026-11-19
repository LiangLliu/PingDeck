//! The environment a shell command starts with and its per-session temp directory.
//!
//! Single responsibility: build the child's environment from an empty one. A name enters only if
//! it is on the allow list, then the security floor removes anything that could carry a
//! credential, then the forced values are set.
//!
//! * Allow list: `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TZ`, `TMPDIR`,
//!   `TERM`, `CARGO_HOME`, `RUSTUP_HOME`, `GOPATH`, `GOROOT`, `NPM_CONFIG_CACHE`, `PNPM_HOME`,
//!   `PIP_CACHE_DIR`, `GOCACHE`, `GOMODCACHE`, `NPM_CONFIG_STORE_DIR`, `PYENV_ROOT`, `NVM_DIR`, `VOLTA_HOME`, `JAVA_HOME`, `DEVELOPER_DIR`,
//!   `SDKROOT`, the proxy variables (`http_proxy`, `https_proxy`, `no_proxy`, any case; a proxy
//!   URL loses its `user:password@` part), the names the user added in
//!   `ai.agent.shell_env_passthrough`, and on Windows the variables `cmd.exe` cannot start
//!   without.
//! * Security floor (applies to the passthrough names too): names starting `PINGDECK_`, `TAURI_`,
//!   `AWS_` or `AZURE_`, `SSH_AUTH_SOCK`, and names containing `KEY`, `TOKEN`, `SECRET`,
//!   `PASSWORD`, `PASSWD` or `CREDENTIAL` (case-insensitive; this also covers
//!   `GOOGLE_APPLICATION_CREDENTIALS`).
//! * Forced: `TERM=dumb`, `NO_COLOR=1`, `PAGER=cat`, `GIT_PAGER=cat`, `GIT_TERMINAL_PROMPT=0`,
//!   `CI=true`, and `TMPDIR` = the per-session temp directory when there is one.
//! * Outside Full access (and with a per-session temp directory) the package-manager caches are
//!   per-session folders below it ([`SESSION_CACHES`]): `CARGO_HOME`, the npm and pnpm caches,
//!   `PIP_CACHE_DIR`, the Go build and module caches; the user's own values do not pass. In Full
//!   access the user's values pass through.
//! * `PATH` is the user's `PATH` as an interactive login shell sees it, resolved once
//!   ([`resolved_path`]); the caller puts it into the variables handed to [`scrub_env`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::AppError;

/// Variables a command inherits (compared upper-cased); everything else is dropped.
const ALLOWED: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "TZ",
    "TMPDIR",
    "TERM",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "GOPATH",
    "GOROOT",
    "NPM_CONFIG_CACHE",
    "PNPM_HOME",
    "PIP_CACHE_DIR",
    "GOCACHE",
    "GOMODCACHE",
    "NPM_CONFIG_STORE_DIR",
    "PYENV_ROOT",
    "NVM_DIR",
    "VOLTA_HOME",
    "JAVA_HOME",
    "DEVELOPER_DIR",
    "SDKROOT",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
];

/// Extra variables `cmd.exe` and most Windows programs cannot start without.
#[cfg(windows)]
const WINDOWS_ENV: [&str; 6] = [
    "SYSTEMROOT",
    "PATHEXT",
    "COMSPEC",
    "USERPROFILE",
    "TEMP",
    "TMP",
];

/// Name prefixes that never reach a command.
const FLOOR_PREFIXES: [&str; 4] = ["PINGDECK_", "TAURI_", "AWS_", "AZURE_"];

/// Name fragments that never reach a command.
const FLOOR_WORDS: [&str; 6] = ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL"];

/// Values set on every command.
const FORCED: [(&str, &str); 6] = [
    ("TERM", "dumb"),
    ("NO_COLOR", "1"),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("CI", "true"),
];

/// Whether the security floor drops the (upper-cased) name.
fn below_floor(upper: &str) -> bool {
    upper == "SSH_AUTH_SOCK"
        || FLOOR_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || FLOOR_WORDS.iter().any(|word| upper.contains(word))
}

/// Removes `user:password@` from a proxy URL (`http://u:p@host:8080` → `http://host:8080`).
fn strip_userinfo(url: &str) -> String {
    let (scheme, rest) = url
        .split_once("://")
        .map_or(("", url), |(scheme, rest)| (scheme, rest));
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if scheme.is_empty() {
        format!("{host}{tail}")
    } else {
        format!("{scheme}://{host}{tail}")
    }
}

/// Package-manager caches that point below the per-session temp directory outside Full access:
/// `(variable, subfolder)`.
const SESSION_CACHES: [(&str, &str); 7] = [
    ("CARGO_HOME", "cargo"),
    ("NPM_CONFIG_CACHE", "npm"),
    ("NPM_CONFIG_STORE_DIR", "pnpm-store"),
    ("PNPM_HOME", "pnpm"),
    ("PIP_CACHE_DIR", "pip"),
    ("GOCACHE", "go-build"),
    ("GOMODCACHE", "go-mod"),
];

/// The environment a command starts with: the allow-listed (and `passthrough`) variables of `vars`
/// that survive the security floor, then the forced values, with `TMPDIR` pointed at `tmp_dir`
/// when given. Without `full_access` the caches of [`SESSION_CACHES`] point below `tmp_dir`.
pub(super) fn scrub_env(
    vars: impl Iterator<Item = (OsString, OsString)>,
    tmp_dir: Option<&Path>,
    passthrough: &[String],
    full_access: bool,
) -> Vec<(OsString, OsString)> {
    let session_caches = tmp_dir.filter(|_| !full_access);
    let mut kept: Vec<(OsString, OsString)> = vars
        .filter_map(|(name, value)| {
            let upper = name.to_string_lossy().to_ascii_uppercase();
            let allowed = ALLOWED.contains(&upper.as_str())
                || upper.starts_with("LC_")
                || passthrough
                    .iter()
                    .any(|extra| extra.eq_ignore_ascii_case(&upper));
            #[cfg(windows)]
            let allowed = allowed || WINDOWS_ENV.contains(&upper.as_str());
            if !allowed || below_floor(&upper) {
                return None;
            }
            let value = if upper.ends_with("_PROXY") && upper != "NO_PROXY" {
                OsString::from(strip_userinfo(&value.to_string_lossy()))
            } else {
                value
            };
            Some((name, value))
        })
        .collect();
    kept.retain(|(name, _)| {
        let name = name.to_string_lossy();
        !FORCED
            .iter()
            .any(|(forced, _)| name.eq_ignore_ascii_case(forced))
            && !(tmp_dir.is_some() && name.eq_ignore_ascii_case("TMPDIR"))
            && !(session_caches.is_some()
                && SESSION_CACHES
                    .iter()
                    .any(|(cache, _)| name.eq_ignore_ascii_case(cache)))
    });
    kept.extend(FORCED.map(|(name, value)| (OsString::from(name), OsString::from(value))));
    if let Some(tmp) = tmp_dir {
        kept.push((OsString::from("TMPDIR"), tmp.as_os_str().to_owned()));
    }
    if let Some(tmp) = session_caches {
        kept.extend(
            SESSION_CACHES
                .iter()
                .map(|(name, sub)| (OsString::from(name), tmp.join(sub).into_os_string())),
        );
    }
    kept
}

/// The user's login-shell `PATH` (`pingdeck_base::platform::login_path`), resolved off the async
/// threads. The app warms it at start ([`warm_login_path`]) so no command pays the shell's start.
pub(super) async fn resolved_path() -> OsString {
    tokio::task::spawn_blocking(resolve_login_path)
        .await
        .unwrap_or_else(|_| pingdeck_base::platform::fallback_path())
}

/// Resolves and caches the login `PATH` now (call at app start, off the UI thread).
pub async fn warm_login_path() {
    let _ = resolved_path().await;
}

#[cfg(not(test))]
fn resolve_login_path() -> OsString {
    pingdeck_base::platform::login_path()
}

/// Tests run with the test process's own `PATH`: no test depends on the user's shell start-up
/// files.
#[cfg(test)]
fn resolve_login_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_else(pingdeck_base::platform::fallback_path)
}

/// The canonical per-session temp directory `<system temp>/pingdeck-agent-<uid>/<session_id>`,
/// created with mode `0700` when missing. The root is per user and verified before use
/// ([`ensure_private_root`]): `/tmp` is shared, so a directory another user (or a link) put at
/// the expected name must never become a command's `TMPDIR` and cache home.
///
/// # Errors
/// [`AppError::Validation`] for a session id that is not a plain file name,
/// [`AppError::Io`]-style errors when the directory cannot be created or the root is not a
/// private directory of this user.
pub fn session_temp_dir(session_id: &str) -> Result<PathBuf, AppError> {
    let dir = session_temp_path(session_id)?;
    if let Some(root) = dir.parent() {
        ensure_private_root(root)?;
    }
    crate::platform::create_private_dir_all(&dir)
        .map_err(|error| AppError::io(format!("cannot create {}: {error}", dir.display())))?;
    std::fs::canonicalize(&dir)
        .map_err(|error| AppError::io(format!("cannot resolve {}: {error}", dir.display())))
}

/// Deletes the per-session temp directories not modified for `max_age` (app start), also those of
/// the shared pre-per-user root `<system temp>/pingdeck-agent`; returns how many were removed. A
/// directory in use is touched by every command that writes to it, so only abandoned ones age
/// out. Only directories of this user are touched: a root that is a link or belongs to someone
/// else is skipped.
pub fn sweep_session_temp_dirs(max_age: Duration) -> usize {
    let roots = [
        temp_root().ok(),
        Some(std::env::temp_dir().join(LEGACY_ROOT)),
    ];
    roots
        .into_iter()
        .flatten()
        .filter(|root| is_private_root(root))
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter(|entry| {
            entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > max_age)
        })
        .filter(|entry| std::fs::remove_dir_all(entry.path()).is_ok())
        .count()
}

/// Deletes the session's temp directory (chat delete); a missing one is fine.
///
/// # Errors
/// [`AppError::Validation`] for a bad session id, [`AppError::Io`]-style errors from the removal.
pub fn remove_session_temp_dir(session_id: &str) -> Result<(), AppError> {
    let dir = session_temp_path(session_id)?;
    if dir.parent().is_some_and(|root| !is_private_root(root)) {
        // Nothing of ours can be in a root that is missing, a link or someone else's.
        return Ok(());
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io(format!(
            "cannot remove {}: {error}",
            dir.display()
        ))),
    }
}

/// Name of the shared root earlier versions used.
const LEGACY_ROOT: &str = "pingdeck-agent";

/// `<system temp>/pingdeck-agent-<uid>` (`pingdeck-agent` where there are no user ids), not
/// created.
fn temp_root() -> Result<PathBuf, AppError> {
    #[cfg(unix)]
    let name = format!("{LEGACY_ROOT}-{}", own_uid()?);
    #[cfg(not(unix))]
    let name = LEGACY_ROOT.to_owned();
    Ok(std::env::temp_dir().join(name))
}

/// `<temp root>/<session_id>`, not created.
fn session_temp_path(session_id: &str) -> Result<PathBuf, AppError> {
    let plain = !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    if !plain {
        return Err(AppError::validation(
            "the session id cannot name a temp directory",
        ));
    }
    Ok(temp_root()?.join(session_id))
}

/// The user id this process runs as, read as the owner of a file it creates (no `unsafe`, no
/// extra crate); cached. A failed probe is not cached.
#[cfg(unix)]
fn own_uid() -> Result<u32, AppError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    static UID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    if let Some(uid) = UID.get() {
        return Ok(*uid);
    }
    let io_error =
        |error: std::io::Error| AppError::io(format!("cannot determine the current user: {error}"));
    let probe = std::env::temp_dir().join(format!(".pingdeck-uid-{}", std::process::id()));
    let _ = std::fs::remove_file(&probe);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&probe)
        .map_err(io_error)?;
    let uid = file.metadata().map_err(io_error)?.uid();
    drop(file);
    let _ = std::fs::remove_file(&probe);
    Ok(*UID.get_or_init(|| uid))
}

/// Whether `root` is a real directory (not a link) of this user that nobody else can enter.
pub(super) fn is_private_root(root: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let Ok(uid) = own_uid() else { return false };
        std::fs::symlink_metadata(root).is_ok_and(|meta| {
            meta.file_type().is_dir() && meta.uid() == uid && meta.permissions().mode() & 0o077 == 0
        })
    }
    #[cfg(not(unix))]
    {
        std::fs::symlink_metadata(root).is_ok_and(|meta| meta.file_type().is_dir())
    }
}

/// Creates `root` (mode `0700`) when missing and checks that it is a private directory of this
/// user ([`is_private_root`]): `create` succeeds on a directory or a link another user planted.
///
/// # Errors
/// [`AppError::Io`]-style errors when it cannot be created or is not private.
pub(super) fn ensure_private_root(root: &Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(AppError::io(format!(
                    "cannot create {}: {error}",
                    root.display()
                )));
            }
        }
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(root)
        .map_err(|error| AppError::io(format!("cannot create {}: {error}", root.display())))?;
    if is_private_root(root) {
        Ok(())
    } else {
        Err(AppError::io(format!(
            "{} exists but is not a private directory of this user (a link, owned by someone else or open to others); remove it",
            root.display()
        )))
    }
}
