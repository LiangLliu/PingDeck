//! Writing and reading files without trusting the path check.
//!
//! Single responsibility: close the gap between "the path was checked" and "the file was written
//! or opened" (a process that shares the folder could swap the file, or any parent directory, for
//! a symlink in between). The file tools read [`identity`] at their path check and
//! write through [`write_in`]: it walks from the workspace root through `cap_std` directory
//! handles (no lookup can leave the root), refuses a parent that is a symlink at all, confirms the
//! target is still the file seen at the check, and replaces it by renaming a staged file inside
//! the held parent handle, so a later swap of any path component cannot redirect the bytes.
//! Reads open by name and are confirmed with [`confirm_open`].
//!
//! `shortcut: (device, inode) pairs exist on Unix only; elsewhere the pair is unavailable and the
//! check passes, upgrade when std exposes a file index on Windows.`

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions, Permissions};
use spanbay_base::error::AppError;

/// The (device, inode) pair of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    /// Device id.
    device: u64,
    /// Inode number.
    inode: u64,
}

/// The pair of an already open file; `None` where the platform has none.
#[cfg(unix)]
fn identity_of(meta: &std::fs::Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        device: meta.dev(),
        inode: meta.ino(),
    })
}

#[cfg(not(unix))]
fn identity_of(_meta: &std::fs::Metadata) -> Option<FileIdentity> {
    None
}

/// As [`identity_of`], for metadata read through a directory handle.
#[cfg(unix)]
fn cap_identity_of(meta: &cap_std::fs::Metadata) -> Option<FileIdentity> {
    use cap_std::fs::MetadataExt;
    Some(FileIdentity {
        device: meta.dev(),
        inode: meta.ino(),
    })
}

#[cfg(not(unix))]
fn cap_identity_of(_meta: &cap_std::fs::Metadata) -> Option<FileIdentity> {
    None
}

/// The identity of the file `path` names now (symlinks followed, as an open would), or `None` when
/// it does not exist or the platform has no identity. Read this at the path check.
///
/// # Errors
/// [`AppError::Io`] for any failure other than "not found".
pub fn identity(path: &Path) -> Result<Option<FileIdentity>, AppError> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(identity_of(&meta)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::io(format!(
            "cannot inspect {}: {error}",
            path.display()
        ))),
    }
}

fn changed(path: &Path) -> AppError {
    AppError::conflict(format!(
        "{} changed while it was being written, so nothing was written",
        path.display()
    ))
}

/// Keeps the staged file names of concurrent writes apart.
static STAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Replaces `rel` (relative to the canonical workspace `root`, as the path check resolved it, so
/// every parent component is a real directory) with `content`. `seen` is [`identity`]'s answer
/// at the check: `Some` means the file existed then and must still be that file (its permissions
/// are kept); `None` means it did not and must still not exist (missing parent directories are
/// created). The bytes go to a staged sibling in the held parent handle, are flushed, and replace
/// the target with one rename, so a crash never leaves a half-written file.
///
/// # Errors
/// [`AppError::Conflict`] when a parent became a symlink or left the root, or the target changed,
/// nothing having been written; [`AppError::Io`] for any other failure.
pub fn write_in(
    root: &Path,
    rel: &Path,
    seen: Option<FileIdentity>,
    content: &[u8],
) -> Result<(), AppError> {
    write_staged(root, rel, seen, content, None)
}

/// As [`write_in`] for a file that must not exist yet, created with `permissions` (the mode of
/// the file a move takes its bytes from) instead of the default mode.
///
/// # Errors
/// As [`write_in`].
pub fn create_in(
    root: &Path,
    rel: &Path,
    content: &[u8],
    permissions: std::fs::Permissions,
) -> Result<(), AppError> {
    write_staged(
        root,
        rel,
        None,
        content,
        Some(Permissions::from_std(permissions)),
    )
}

/// The staged write of [`write_in`] and [`create_in`]: an existing target keeps its own
/// permissions, a new one gets `mode` when given.
fn write_staged(
    root: &Path,
    rel: &Path,
    seen: Option<FileIdentity>,
    content: &[u8],
    mode: Option<Permissions>,
) -> Result<(), AppError> {
    let io_error =
        |error: io::Error| AppError::io(format!("cannot write {}: {error}", rel.display()));
    let name = rel
        .file_name()
        .ok_or_else(|| AppError::validation(format!("{} has no file name", rel.display())))?;
    let mut dir = Dir::open_ambient_dir(root, ambient_authority()).map_err(io_error)?;
    for component in rel.parent().map(Path::components).into_iter().flatten() {
        let Component::Normal(part) = component else {
            return Err(changed(rel));
        };
        dir = enter(&dir, part, seen.is_none(), rel)?;
    }
    let kept = check_target(&dir, name, seen, rel)?.or(mode);
    let staged = format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        STAGE_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let result = (|| {
        let mut file = dir.open_with(&staged, OpenOptions::new().write(true).create_new(true))?;
        file.write_all(content)?;
        if let Some(permissions) = kept {
            // Best effort, as a chmod after a rename would be.
            let _ = file.set_permissions(permissions);
        }
        file.sync_all()?;
        drop(file);
        dir.rename(&staged, &dir, name)
    })();
    if result.is_err() {
        let _ = dir.remove_file(&staged);
    }
    result.map_err(io_error)
}

/// Deletes `rel` (relative to the canonical workspace `root`, as the path check resolved it) when
/// it is still the file `seen` at the check, walking the parents through held handles as
/// [`write_in`] does; a file already gone is fine.
///
/// # Errors
/// [`AppError::Conflict`] when a parent became a symlink or left the root, or the target is
/// another file than the one seen, nothing having been deleted; [`AppError::Io`] otherwise.
pub fn remove_in(root: &Path, rel: &Path, seen: Option<FileIdentity>) -> Result<(), AppError> {
    let io_error =
        |error: io::Error| AppError::io(format!("cannot delete {}: {error}", rel.display()));
    let name = rel
        .file_name()
        .ok_or_else(|| AppError::validation(format!("{} has no file name", rel.display())))?;
    let mut dir = Dir::open_ambient_dir(root, ambient_authority()).map_err(io_error)?;
    for component in rel.parent().map(Path::components).into_iter().flatten() {
        let Component::Normal(part) = component else {
            return Err(changed(rel));
        };
        dir = enter(&dir, part, false, rel)?;
    }
    if check_target(&dir, name, seen, rel)?.is_none() {
        return Ok(());
    }
    match dir.remove_file(name) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io_error(error)),
        _ => Ok(()),
    }
}

/// The directory `name` inside `dir` (created first when `create`). `cap_std` keeps the lookup
/// inside the root; comparing the handle with the entry's own unfollowed metadata also refuses a
/// symlink that points elsewhere inside it.
fn enter(dir: &Dir, name: &OsStr, create: bool, rel: &Path) -> Result<Dir, AppError> {
    let io_error =
        |error: io::Error| AppError::io(format!("cannot write {}: {error}", rel.display()));
    if create {
        match dir.create_dir(name) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    let opened = dir.open_dir(name);
    let entry = match dir.symlink_metadata(name) {
        Ok(entry) if !entry.file_type().is_symlink() => entry,
        Ok(_) => return Err(changed(rel)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(changed(rel)),
        Err(error) => return Err(io_error(error)),
    };
    let sub = opened.map_err(io_error)?;
    let handle = sub.dir_metadata().map_err(io_error)?;
    match (cap_identity_of(&handle), cap_identity_of(&entry)) {
        (Some(handle), Some(entry)) if handle != entry => Err(changed(rel)),
        _ => Ok(sub),
    }
}

/// Confirms the entry `name` in `dir` is still what the check saw (and not a symlink); the
/// permissions to keep when it existed.
fn check_target(
    dir: &Dir,
    name: &OsStr,
    seen: Option<FileIdentity>,
    rel: &Path,
) -> Result<Option<Permissions>, AppError> {
    match dir.symlink_metadata(name) {
        Ok(meta) => {
            let same = seen.is_some_and(|expected| {
                !meta.file_type().is_symlink()
                    && cap_identity_of(&meta).is_none_or(|actual| actual == expected)
            });
            if same {
                Ok(Some(meta.permissions()))
            } else {
                Err(changed(rel))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound && seen.is_none() => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(changed(rel)),
        Err(error) => Err(AppError::io(format!(
            "cannot inspect {}: {error}",
            rel.display()
        ))),
    }
}

/// Confirms the already opened `file` is still what `path` names: `path` is canonicalized *after*
/// the open, `allowed` must accept that canonical path (inside the workspace, not protected), and
/// the open handle must be the file at it. A symlink swapped in around the open
/// therefore either lands outside and is refused, or no longer matches the handle. Reading
/// through the handle afterwards cannot be redirected.
///
/// # Errors
/// What `allowed` returns; [`AppError::Conflict`] when the handle is another file than the path
/// names now; [`AppError::Io`] when either cannot be inspected.
pub fn confirm_open(
    file: &File,
    path: &Path,
    allowed: impl FnOnce(&Path) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let inspect =
        |error: io::Error| AppError::io(format!("cannot inspect {}: {error}", path.display()));
    let now = std::fs::canonicalize(path).map_err(inspect)?;
    allowed(&now)?;
    let opened = file.metadata().map_err(inspect)?;
    let current = std::fs::metadata(&now).map_err(inspect)?;
    match (identity_of(&opened), identity_of(&current)) {
        (Some(opened), Some(current)) if opened != current => Err(AppError::conflict(format!(
            "{} changed while it was being opened, so nothing was read",
            path.display()
        ))),
        _ => Ok(()),
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spanbay-fs-safe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ws")).unwrap();
        dir
    }

    fn refused(result: Result<(), AppError>) -> bool {
        matches!(result, Err(AppError::Conflict { .. }))
    }

    #[test]
    fn a_file_swapped_for_a_symlink_after_the_check_is_refused_and_the_target_untouched() {
        let dir = scratch("swap");
        let (ws, secret) = (dir.join("ws"), dir.join("secret"));
        std::fs::write(&secret, "precious").unwrap();
        std::fs::write(ws.join("a.txt"), "mine").unwrap();
        let seen = identity(&ws.join("a.txt")).unwrap();
        // The race: another process replaces the file by a link to the secret.
        std::fs::remove_file(ws.join("a.txt")).unwrap();
        std::os::unix::fs::symlink(&secret, ws.join("a.txt")).unwrap();
        assert!(refused(write_in(&ws, Path::new("a.txt"), seen, b"evil")));
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "precious");
        // A file deleted after the check is not recreated either.
        std::fs::remove_file(ws.join("a.txt")).unwrap();
        assert!(refused(write_in(&ws, Path::new("a.txt"), seen, b"x")));
        assert!(!ws.join("a.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_unswapped_file_is_replaced_keeping_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("same");
        let file = dir.join("ws/a.txt");
        std::fs::write(&file, "old contents").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o750)).unwrap();
        let seen = identity(&file).unwrap();
        write_in(&dir.join("ws"), Path::new("a.txt"), seen, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o750);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_file_is_never_created_through_a_planted_link_and_missing_parents_are_made() {
        let dir = scratch("create");
        let (ws, outside) = (dir.join("ws"), dir.join("outside"));
        let rel = Path::new("new.txt");
        let seen = identity(&ws.join(rel)).unwrap();
        assert_eq!(seen, None);
        // A link planted after the check, pointing at a file that does not exist yet.
        std::os::unix::fs::symlink(&outside, ws.join(rel)).unwrap();
        assert!(refused(write_in(&ws, rel, seen, b"x")));
        assert!(!outside.exists(), "nothing was created through the link");
        write_in(&ws, Path::new("a/b/c.txt"), None, b"deep").unwrap();
        assert_eq!(
            std::fs::read_to_string(ws.join("a/b/c.txt")).unwrap(),
            "deep"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_parent_directory_swapped_for_a_symlink_after_the_check_is_refused() {
        let dir = scratch("parent");
        let (ws, outside) = (dir.join("ws"), dir.join("outside"));
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::create_dir_all(ws.join("other")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let rel = Path::new("src/a.txt");
        let seen = identity(&ws.join(rel)).unwrap();
        // The race: the checked parent becomes a link out of the workspace...
        std::fs::remove_dir(ws.join("src")).unwrap();
        std::os::unix::fs::symlink(&outside, ws.join("src")).unwrap();
        assert!(refused(write_in(&ws, rel, seen, b"evil")));
        assert!(
            !outside.join("a.txt").exists(),
            "nothing was written outside"
        );
        // ...or to another directory inside it.
        std::fs::remove_file(ws.join("src")).unwrap();
        std::os::unix::fs::symlink(ws.join("other"), ws.join("src")).unwrap();
        assert!(refused(write_in(&ws, rel, seen, b"evil")));
        assert!(!ws.join("other/a.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_delete_through_a_parent_swapped_for_a_symlink_is_refused() {
        let dir = scratch("remove");
        let (ws, outside) = (dir.join("ws"), dir.join("outside"));
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(ws.join("src/a.txt"), "mine").unwrap();
        std::fs::write(outside.join("a.txt"), "precious").unwrap();
        let rel = Path::new("src/a.txt");
        let seen = identity(&ws.join(rel)).unwrap();
        std::fs::rename(ws.join("src"), ws.join("moved")).unwrap();
        std::os::unix::fs::symlink(&outside, ws.join("src")).unwrap();
        assert!(refused(remove_in(&ws, rel, seen)));
        assert_eq!(
            std::fs::read_to_string(outside.join("a.txt")).unwrap(),
            "precious"
        );
        std::fs::remove_file(ws.join("src")).unwrap();
        std::fs::rename(ws.join("moved"), ws.join("src")).unwrap();
        remove_in(&ws, rel, seen).unwrap();
        assert!(!ws.join(rel).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_opened_handle_must_be_the_file_the_path_names_inside_the_allowed_area() {
        let dir = scratch("confirm");
        let (secret, mine) = (dir.join("secret"), dir.join("mine.txt"));
        std::fs::write(&secret, "precious").unwrap();
        std::fs::write(&mine, "mine").unwrap();
        let anywhere = |_: &Path| Ok(());
        // The handle was opened while the name pointed at the secret; now it names `mine`.
        let handle = File::open(&secret).unwrap();
        assert!(matches!(
            confirm_open(&handle, &mine, anywhere),
            Err(AppError::Conflict { .. })
        ));
        let handle = File::open(&mine).unwrap();
        assert!(confirm_open(&handle, &mine, anywhere).is_ok());
        // A name that now resolves outside the allowed area is refused even when it matches.
        let link = dir.join("link");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        let handle = File::open(&link).unwrap();
        let only_mine = |canonical: &Path| {
            if canonical.ends_with("mine.txt") {
                Ok(())
            } else {
                Err(AppError::validation("outside"))
            }
        };
        assert!(matches!(
            confirm_open(&handle, &link, only_mine),
            Err(AppError::Validation { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
