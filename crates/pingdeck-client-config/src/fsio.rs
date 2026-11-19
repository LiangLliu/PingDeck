//! Guarded reads and owner-only, link-safe writes of the user's config files.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use super::display;
use pingdeck_base::{crypto::sha256_hex, error::AppError};

/// Mode of a file PingDeck creates: owner read/write only.
#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;
/// The group and other permission bits, cleared on a file that gains a token.
pub(super) const GROUP_OTHER_BITS: u32 = 0o077;
/// All permission bits of a mode (including setuid, setgid and sticky).
#[cfg(unix)]
const PERMISSION_BITS: u32 = 0o7777;
/// Suffix of the staging file beside a target: `.<name>.pingdeck-tmp`.
const TMP_SUFFIX: &str = ".pingdeck-tmp";

/// Largest config file PingDeck reads; a real client config is a few KiB.
pub(super) const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

/// Reads `path` (a symbolic link is followed, as dotfile setups need), mapping "not found" to
/// `None`. Anything that is not a regular file (a FIFO would block the reader forever) or is
/// larger than [`MAX_CONFIG_BYTES`] is refused unread; a file that grows past the limit after
/// that check is refused too, never read whole.
pub(super) fn read_optional(path: &Path) -> Result<Option<String>, AppError> {
    let failed = |e: &io::Error| AppError::io(format!("failed to read {}: {e}", display(path)));
    let refused = || {
        AppError::validation(format!(
            "{} is not a regular config file of a sane size. PingDeck did not read it.",
            display(path)
        ))
    };
    // The check goes by path, before any open: opening a FIFO for reading blocks.
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(failed(&e)),
    };
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return Err(refused());
    }
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(failed(&e)),
    };
    let mut text = String::new();
    if let Err(e) = (&mut file)
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
    {
        // A file that grew past the limit after the check can be cut inside a UTF-8 character,
        // so the capped read fails before the length check below; re-stat the open handle (which
        // also covers a file swapped by rename) to report the size refusal instead.
        if file.metadata().is_ok_and(|m| m.len() > MAX_CONFIG_BYTES) {
            return Err(refused());
        }
        return Err(failed(&e));
    }
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err(refused());
    }
    Ok(Some(text))
}

pub(super) fn write_error(path: &str, e: &io::Error) -> AppError {
    if e.kind() == io::ErrorKind::PermissionDenied {
        AppError::io(format!(
            "PingDeck cannot write `{path}`. Check the file's permissions."
        ))
    } else {
        AppError::io(format!("failed to write {path}: {e}"))
    }
}

pub(super) fn file_name(path: &Path) -> io::Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))
}

/// The permission bits of an existing file (Unix); `None` elsewhere or when it does not exist.
pub(super) fn existing_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .ok()
            .map(|meta| meta.permissions().mode() & PERMISSION_BITS)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn is_hard_linked(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

fn set_mode(file: &fs::File, mode: Option<u32>) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(
            mode.unwrap_or(PRIVATE_FILE_MODE),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = (file, mode);
        Ok(())
    }
}

/// Writes `bytes` to a new file `path` flushed to disk, with `mode` (default owner-only `0600`)
/// on Unix. An existing file, or a symbolic link of any kind, at `path` is an
/// [`io::ErrorKind::AlreadyExists`] error (`O_EXCL` never follows a link); a failed write leaves
/// nothing behind.
pub(super) fn write_synced(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The process umask narrows the mode given to `open`; `set_mode` below sets it exactly.
        options.mode(mode.unwrap_or(PRIVATE_FILE_MODE));
    }
    let mut file = options.open(path)?;
    let result = set_mode(&file, mode)
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| file.sync_all());
    if result.is_err() {
        drop(file);
        let _ = fs::remove_file(path);
    }
    result
}

pub(super) fn tmp_path(target: &Path) -> io::Result<PathBuf> {
    Ok(target.with_file_name(format!(".{}{TMP_SUFFIX}", file_name(target)?)))
}

/// Stages `bytes` as the new file `tmp`: a stale file or link left at that name by a crash (or
/// planted there) is removed first, never written through.
pub(super) fn write_tmp(tmp: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let _ = fs::remove_file(tmp);
    write_synced(tmp, bytes, mode)
}

/// Writes `bytes` over `target` through a sibling temp file and one rename, with `mode` (see
/// [`write_synced`]); pass [`existing_mode`] to keep the mode the file has.
pub(super) fn replace_file(target: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let tmp = tmp_path(target)?;
    let result = write_tmp(&tmp, bytes, mode).and_then(|()| fs::rename(&tmp, target));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// SHA-256 of the file, `None` when it does not exist.
pub(super) fn current_sha(path: &Path) -> io::Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(sha256_hex(&bytes))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The file a write to `path` must replace: symlinks and symlinked directories resolved. A
/// dangling symlink, a hard-linked file and anything that is not a regular file are refused,
/// because replacing them would break the link or the layout the user set up.
pub(super) fn check_target(shown: &str, path: &Path) -> Result<PathBuf, AppError> {
    let refuse = |why: &str| {
        AppError::validation(format!(
            "`{shown}` {why}. PingDeck refuses to write it and changed nothing."
        ))
    };
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let dir = path.parent().and_then(|dir| fs::canonicalize(dir).ok());
            return Ok(match (dir, path.file_name()) {
                (Some(dir), Some(name)) => dir.join(name),
                _ => path.to_owned(),
            });
        }
        Err(e) => return Err(write_error(shown, &e)),
    };
    let real = match fs::canonicalize(path) {
        Ok(real) => real,
        Err(_) if meta.file_type().is_symlink() => {
            return Err(refuse("is a symbolic link that points nowhere"));
        }
        Err(e) => return Err(write_error(shown, &e)),
    };
    let real_meta = fs::metadata(&real).map_err(|e| write_error(shown, &e))?;
    if !real_meta.is_file() {
        return Err(refuse("is not a regular file"));
    }
    if is_hard_linked(&real_meta) {
        return Err(refuse(
            "is a hard link: another name points at the same data and would be cut off",
        ));
    }
    Ok(real)
}

/// Where a write to `path` really lands when a symbolic link (the file itself, or a folder on the
/// way under `home`) leads elsewhere, such as into a dotfiles repository; `None` when it lands at
/// `path`. A file that does not exist yet resolves through its folder.
pub(super) fn real_location(home: &Path, path: &Path) -> Option<PathBuf> {
    let resolve = |path: &Path| {
        fs::canonicalize(path).ok().or_else(|| {
            let dir = fs::canonicalize(path.parent()?).ok()?;
            Some(dir.join(path.file_name()?))
        })
    };
    let real = resolve(path)?;
    // What `path` resolves to without links below `home`: the home itself may sit behind a
    // system link (`/var` -> `/private/var`), which is no redirection the user set up.
    let plain = match path.strip_prefix(home) {
        Ok(rel) => fs::canonicalize(home).ok()?.join(rel),
        Err(_) if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) => {
            return Some(real);
        }
        Err(_) => return None,
    };
    (real != plain).then_some(real)
}

/// Most folders walked up when looking for a Git work tree.
const MAX_GIT_DEPTH: usize = 64;

/// The root of the Git work tree that holds `path` (a folder on the way up has a `.git` entry);
/// `None` when there is none. A gateway token written there could be committed by accident.
pub(super) fn git_work_tree(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .take(MAX_GIT_DEPTH)
        .find(|dir| fs::symlink_metadata(dir.join(".git")).is_ok())
        .map(Path::to_path_buf)
}

/// The read guard of the writer (L-16).
#[cfg(all(test, unix))]
mod tests {
    use std::fs;

    use super::super::test_util::{TempHome, request};
    use super::super::{ClientApp, preview};

    #[test]
    fn a_fifo_at_the_config_path_is_refused_without_reading_it() {
        let home = TempHome::new();
        fs::create_dir_all(home.0.join(".claude")).unwrap();
        let fifo = home.0.join(".claude/settings.json");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.is_ok_and(|status| status.success()), "mkfifo");

        let err = preview(&home.0, &request(ClientApp::ClaudeCode, None)).unwrap_err();

        assert!(
            err.message().contains("not a regular config file"),
            "{err:?}"
        );
    }

    #[test]
    fn a_linked_config_previews_its_real_location_and_its_git_work_tree() {
        let home = TempHome::new();
        home.write("dotfiles/.git/HEAD", "ref: refs/heads/main\n");
        home.write("dotfiles/claude/settings.json", "{}\n");
        fs::create_dir_all(home.0.join(".claude")).unwrap();
        std::os::unix::fs::symlink(
            home.0.join("dotfiles/claude/settings.json"),
            home.0.join(".claude/settings.json"),
        )
        .unwrap();

        let plan = preview(&home.0, &request(ClientApp::ClaudeCode, None)).unwrap();

        let file = &plan.files[0];
        let real = file.real_path.as_deref().unwrap();
        assert!(real.ends_with("dotfiles/claude/settings.json"), "{real}");
        assert!(file.git_work_tree.as_deref().unwrap().ends_with("dotfiles"));
        // A plain file of the same home has neither.
        let plain = preview(&home.0, &request(ClientApp::Codex, None)).unwrap();
        assert_eq!(plain.files[0].real_path, None);
        assert_eq!(plain.files[0].git_work_tree, None);
    }

    #[test]
    fn an_oversized_config_is_refused_unread() {
        let home = TempHome::new();
        home.write(".claude/settings.json", "");
        let file = fs::OpenOptions::new()
            .write(true)
            .open(home.0.join(".claude/settings.json"))
            .unwrap();
        file.set_len(super::MAX_CONFIG_BYTES + 1).unwrap();

        let err = preview(&home.0, &request(ClientApp::ClaudeCode, None)).unwrap_err();

        assert!(
            err.message().contains("not a regular config file"),
            "{err:?}"
        );
    }
}
