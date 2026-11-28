//! Safe pure-Rust OS file opener confined to a session's bound workspace folder.
//!
//! Single responsibility: resolve a model- or UI-supplied path inside a canonicalized workspace
//! directory (rejecting traversal and symlink escapes) and hand it to the OS default handler
//! (`open` on macOS, `xdg-open` on Linux) without blocking the UI thread.

use std::path::{Path, PathBuf};

use spanbay_core::error::AppError;

/// Resolves `raw_path` relative to `workspace_dir`, canonicalizing both and rejecting any path
/// that does not exist or resolves outside `workspace_dir` (including via symlinks).
///
/// # Errors
/// Returns [`AppError::Validation`] if `raw_path` is empty or escapes `workspace_dir`, or
/// [`AppError::NotFound`] / [`AppError::Io`] if the workspace or target does not exist.
pub fn resolve_workspace_open_target(
    workspace_dir: &Path,
    raw_path: &str,
) -> Result<PathBuf, AppError> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err(AppError::validation("file path is empty"));
    }
    let canonical_root = std::fs::canonicalize(workspace_dir)
        .map_err(|err| AppError::not_found(format!("workspace folder unavailable: {err}")))?;
    let candidate = Path::new(trimmed);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        canonical_root.join(candidate)
    };
    let canonical_target = std::fs::canonicalize(&joined)
        .map_err(|err| AppError::not_found(format!("cannot locate '{trimmed}': {err}")))?;
    if !canonical_target.starts_with(&canonical_root) {
        return Err(AppError::validation(
            "refused to open a path outside the bound workspace folder",
        ));
    }
    Ok(canonical_target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "spanbay-open-path-{label}-{}-{n}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolves_relative_and_absolute_paths_inside_workspace() {
        let dir = TestDir::new("inside");
        let nested = dir.path().join("src");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let rel = resolve_workspace_open_target(dir.path(), "src/main.rs").unwrap();
        let abs = resolve_workspace_open_target(dir.path(), file.to_str().unwrap()).unwrap();
        assert_eq!(rel, std::fs::canonicalize(&file).unwrap());
        assert_eq!(abs, rel);
    }

    #[test]
    fn rejects_traversal_outside_workspace_and_external_symlinks() {
        let outer = TestDir::new("outer");
        let ws = outer.path().join("project");
        std::fs::create_dir_all(&ws).unwrap();
        let secret = outer.path().join("secret.txt");
        std::fs::write(&secret, "top-secret").unwrap();

        let err = resolve_workspace_open_target(&ws, "../secret.txt").unwrap_err();
        assert!(matches!(err, AppError::Validation { .. }), "{err:?}");

        let abs_err = resolve_workspace_open_target(&ws, secret.to_str().unwrap()).unwrap_err();
        assert!(
            matches!(abs_err, AppError::Validation { .. }),
            "{abs_err:?}"
        );

        #[cfg(unix)]
        {
            let link = ws.join("escape.txt");
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            let sym_err = resolve_workspace_open_target(&ws, "escape.txt").unwrap_err();
            assert!(
                matches!(sym_err, AppError::Validation { .. }),
                "{sym_err:?}"
            );
        }

        let missing = resolve_workspace_open_target(&ws, "does-not-exist.rs").unwrap_err();
        assert!(matches!(missing, AppError::NotFound { .. }), "{missing:?}");
    }
}
