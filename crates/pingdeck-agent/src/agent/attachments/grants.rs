//! Which file paths the webview may ask the core to read: only those the operating system handed
//! to this window as a drag-drop or a file-picker answer in the last minute.
//!
//! Remembers `(window, canonical path, time)` triples and hands each back once. The shell records
//! grants from the native drop event and the native picker, never from a command the webview can
//! call, so the webview cannot name a path it was not given. Time is passed in, so tests need no
//! sleeping.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pingdeck_base::error::AppError;

/// How long a dropped or picked path stays readable.
pub const GRANT_TTL: Duration = Duration::from_secs(60);
/// Most grants kept at once; the oldest go first.
const MAX_GRANTS: usize = 256;

struct Grant {
    window: String,
    /// The path as the operating system reported it.
    raw: PathBuf,
    /// Its canonical form, resolved on first use; `None` until then or when it does not resolve.
    canonical: Option<PathBuf>,
    at: Instant,
}

/// The paths each window may read, for [`GRANT_TTL`].
#[derive(Default)]
pub struct PathGrants {
    grants: Mutex<Vec<Grant>>,
}

impl PathGrants {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Grant>> {
        self.grants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Lets `window` read `paths` until [`GRANT_TTL`] passes. Cheap and free of file-system
    /// access, so the native event thread can call it: the paths are resolved by
    /// [`PathGrants::resolve_pending`], and the canonical form is what is compared, so a symlink
    /// and its target are one grant.
    pub fn grant(&self, window: &str, paths: &[PathBuf], now: Instant) {
        let mut grants = self.lock();
        grants.retain(|grant| now.saturating_duration_since(grant.at) < GRANT_TTL);
        for path in paths {
            grants.retain(|grant| !(grant.window == window && grant.raw == *path));
            grants.push(Grant {
                window: window.to_owned(),
                raw: path.clone(),
                canonical: None,
                at: now,
            });
        }
        let excess = grants.len().saturating_sub(MAX_GRANTS);
        grants.drain(..excess);
    }

    /// The canonical path of `path` when `window` holds a live grant for it; the grant stays.
    ///
    /// # Errors
    /// [`AppError::Validation`] when the path never came from a drop or a pick of this window in
    /// the last minute, or no longer resolves to the file that was granted (a symlink retargeted
    /// since).
    pub fn check(&self, window: &str, path: &Path, now: Instant) -> Result<PathBuf, AppError> {
        self.find(window, path, now, false)
    }

    /// Like [`PathGrants::check`], and the grant is used up: a path is read once per drop.
    ///
    /// # Errors
    /// As [`PathGrants::check`].
    pub fn take(&self, window: &str, path: &Path, now: Instant) -> Result<PathBuf, AppError> {
        self.find(window, path, now, true)
    }

    fn find(
        &self,
        window: &str,
        path: &Path,
        now: Instant,
        consume: bool,
    ) -> Result<PathBuf, AppError> {
        let refused = || {
            AppError::validation(
                "this file was not dropped or picked in this window in the last minute",
            )
        };
        let canonical = std::fs::canonicalize(path).map_err(|_| refused())?;
        self.resolve_pending();
        let mut grants = self.lock();
        grants.retain(|grant| now.saturating_duration_since(grant.at) < GRANT_TTL);
        let position = grants
            .iter()
            .position(|grant| {
                grant.window == window && grant.canonical.as_ref() == Some(&canonical)
            })
            .ok_or_else(refused)?;
        if consume {
            grants.remove(position);
        }
        Ok(canonical)
    }

    /// Resolves the grants recorded since the last call. The shell calls this off its event
    /// thread right after [`PathGrants::grant`], so a symlink retargeted afterwards no longer
    /// matches; the lookups call it too, so a lookup that wins that race still sees the grant.
    /// The canonicalizing runs outside the lock: a slow volume must not stall `grant`.
    pub fn resolve_pending(&self) {
        let unresolved: Vec<PathBuf> = self
            .lock()
            .iter()
            .filter(|grant| grant.canonical.is_none())
            .map(|grant| grant.raw.clone())
            .collect();
        if unresolved.is_empty() {
            return;
        }
        let resolved: Vec<(PathBuf, PathBuf)> = unresolved
            .into_iter()
            .filter_map(|raw| std::fs::canonicalize(&raw).ok().map(|real| (raw, real)))
            .collect();
        for grant in self
            .lock()
            .iter_mut()
            .filter(|grant| grant.canonical.is_none())
        {
            grant.canonical = resolved
                .iter()
                .find(|(raw, _)| *raw == grant.raw)
                .map(|(_, real)| real.clone());
        }
    }
}
