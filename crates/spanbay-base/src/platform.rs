//! Operating-system path conventions and owner-only file helpers.
//!
//! Single responsibility: isolate OS differences (data directory resolution,
//! Windows verbatim paths, Unix file modes) behind small, testable functions.

use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

mod login_path;
pub use login_path::{
    LOGIN_PATH_TIMEOUT, fallback_path, login_path, path_between_marks, path_from_shell, user_shell,
};

/// Reverse-DNS application identifier shared across macOS, Linux, and Windows.
pub const APP_IDENTIFIER: &str = "com.spanbay.desktop";

/// Default SQLite database filename inside the application data directory.
pub const DATABASE_FILENAME: &str = "spanbay.sqlite3";

/// Longest Windows path (in characters) that legacy, non-verbatim APIs accept.
const WINDOWS_MAX_PATH: usize = 260;

/// Host operating system family used to pick path conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    /// Apple macOS (`target_os = "macos"`).
    MacOs,
    /// Linux and every other XDG/POSIX target.
    Linux,
    /// Microsoft Windows (`target_os = "windows"`).
    Windows,
}

impl HostOs {
    /// Returns the [`HostOs`] compiled for the current target platform.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// Data directory name of debug builds (`tauri dev`, `cargo run`), kept apart from the release
/// directory so a development build never opens or migrates the everyday database.
pub const DEV_DATA_DIR_NAME: &str = "com.spanbay.desktop.dev";

/// Environment variable naming an explicit data directory; honoured only when absolute.
pub const DATA_DIR_ENV: &str = "SPANBAY_DATA_DIR";

/// Resolves the application data directory for the current process environment.
///
/// An absolute `SPANBAY_DATA_DIR` wins in every build. Otherwise the directory name is
/// `com.spanbay.desktop` (`com.spanbay.desktop.dev` when `dev_build`) under:
///
/// - **macOS**: `$HOME/Library/Application Support`
/// - **Linux**: `$XDG_DATA_HOME` (when absolute), otherwise `$HOME/.local/share`
/// - **Windows**: `%APPDATA%`, falling back to `%LOCALAPPDATA%`, then
///   `%USERPROFILE%\AppData\Roaming`
#[must_use]
pub fn resolve_app_data_dir(dev_build: bool) -> Option<PathBuf> {
    resolve_app_data_dir_with_env(HostOs::current(), dev_build, |key| std::env::var_os(key))
}

/// Testable implementation of [`resolve_app_data_dir`] accepting an explicit
/// [`HostOs`], build flavour and environment variable lookup closure.
pub fn resolve_app_data_dir_with_env<F>(
    os: HostOs,
    dev_build: bool,
    env_lookup: F,
) -> Option<PathBuf>
where
    F: Fn(&str) -> Option<OsString>,
{
    if let Some(explicit) = explicit_data_dir(os, &env_lookup) {
        return Some(explicit);
    }
    let name = if dev_build {
        DEV_DATA_DIR_NAME
    } else {
        APP_IDENTIFIER
    };
    match os {
        HostOs::MacOs => {
            let home = non_empty_env(&env_lookup, "HOME")?;
            Some(home.join("Library").join("Application Support").join(name))
        }
        HostOs::Linux => {
            if let Some(xdg) = non_empty_env(&env_lookup, "XDG_DATA_HOME")
                && is_absolute_for_os(os, &xdg)
            {
                return Some(xdg.join(name));
            }
            let home = non_empty_env(&env_lookup, "HOME")?;
            Some(home.join(".local").join("share").join(name))
        }
        HostOs::Windows => {
            if let Some(appdata) = non_empty_env(&env_lookup, "APPDATA") {
                return Some(join_windows_segments(&appdata, &[name]));
            }
            if let Some(local_appdata) = non_empty_env(&env_lookup, "LOCALAPPDATA") {
                return Some(join_windows_segments(&local_appdata, &[name]));
            }
            let profile = non_empty_env(&env_lookup, "USERPROFILE")?;
            Some(join_windows_segments(
                &profile,
                &["AppData", "Roaming", name],
            ))
        }
    }
}

/// Resolves and creates (owner-only) the application data directory, returning the full path
/// to `spanbay.sqlite3`. An existing `SPANBAY_DATA_DIR` keeps its permissions: it may be a
/// shared directory.
pub fn default_database_path(dev_build: bool) -> Result<PathBuf, String> {
    database_path_with_env(HostOs::current(), dev_build, |key| std::env::var_os(key))
}

fn database_path_with_env<F>(os: HostOs, dev_build: bool, env_lookup: F) -> Result<PathBuf, String>
where
    F: Fn(&str) -> Option<OsString>,
{
    let dir = resolve_app_data_dir_with_env(os, dev_build, &env_lookup)
        .ok_or_else(|| "unable to determine SpanBay application data directory".to_owned())?;
    if !(explicit_data_dir(os, &env_lookup).is_some() && dir.is_dir()) {
        create_private_dir_all(&dir)
            .map_err(|error| format!("failed to create application data directory: {error}"))?;
    }
    Ok(dir.join(DATABASE_FILENAME))
}

/// Writes `contents` to `path`, truncating it; on Unix the file is created with
/// mode `0600` and an existing file is restricted to `0600` before any bytes are
/// written. Other platforms perform a plain write.
pub fn write_private_file(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::{
            io::Write,
            os::unix::fs::{OpenOptionsExt, PermissionsExt},
        };

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(contents.as_ref())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)
    }
}

/// Opens `path` for appending (creating it when missing); on Unix the file is created with mode
/// `0600` and an existing file is restricted to `0600`. Other platforms perform a plain open.
pub fn open_private_append(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    restrict_file_to_owner(path)?;
    Ok(file)
}

/// Restricts an existing file to owner read/write (`0600`) on Unix; a no-op
/// elsewhere.
pub fn restrict_file_to_owner(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Creates `path` and any missing parents; on Unix the created directories get
/// mode `0700` and the leaf is chmodded to `0700`. Other platforms use
/// [`fs::create_dir_all`].
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Keeps the staging file names of concurrent atomic writes apart.
static ATOMIC_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `contents` to `path` atomically: the bytes go to a uniquely named sibling temporary
/// file, are flushed to disk, and replace `path` with one rename, so a crash or a concurrent
/// reader never sees a half-written file and two writers never share a temporary file. With
/// `private` the temporary file (hence the result) is created `0600` on Unix. The parent
/// directory must exist; the temporary file is removed when anything fails.
pub fn write_atomic(path: &Path, contents: impl AsRef<[u8]>, private: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let staged = parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        ATOMIC_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        use std::io::Write;

        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut file = options.open(&staged)?;
        file.write_all(contents.as_ref())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&staged, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}

/// The directory layout under the application data root (`0700` on Unix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDirs {
    /// The data root: everything SpanBay persists lives below it.
    pub root: PathBuf,
    /// The SQLite database file.
    pub db: PathBuf,
    /// Imported agent skills (`<root>/skills`).
    pub skills: PathBuf,
    /// Content-addressed attachments (`<root>/attachments/<sha256[0..2]>/<sha256>`).
    pub attachments: PathBuf,
    /// Scratch space for staged writes and sandboxed shell commands (`<root>/tmp`).
    pub tmp: PathBuf,
    /// Rotating diagnostic log files (`<root>/logs`).
    pub logs: PathBuf,
}

impl DataDirs {
    /// The layout for a database file: the root is the file's directory.
    #[must_use]
    pub fn for_database(db: &Path) -> Self {
        let root = db
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Self {
            skills: root.join("skills"),
            attachments: root.join("attachments"),
            tmp: root.join("tmp"),
            logs: root.join("logs"),
            db: db.to_path_buf(),
            root,
        }
    }

    /// Creates the root (when missing; an existing root keeps its permissions, it may be a shared
    /// directory) and every subdirectory (owner-only on Unix); idempotent.
    pub fn ensure(&self) -> io::Result<()> {
        if !self.root.exists() {
            create_private_dir_all(&self.root)?;
        }
        [&self.skills, &self.attachments, &self.tmp, &self.logs]
            .into_iter()
            .try_for_each(|dir| create_private_dir_all(dir))
    }
}

/// Extracts the trailing file name from `path`, honoring both POSIX (`/`) and
/// Windows (`\`) path separators regardless of the host OS.
///
/// This ensures imported collections or workspaces created on Windows produce
/// clean multipart filenames when executed on macOS or Linux (and vice versa).
#[must_use]
pub fn portable_file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return path;
    }
    if let Some(index) = trimmed.rfind(['/', '\\']) {
        let candidate = &trimmed[index + 1..];
        if !candidate.is_empty() {
            return candidate;
        }
    }
    Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

/// Formats a [`Path`] for **display** and persistence in UI state, stripping the
/// Windows extended-length verbatim prefix (`\\?\` and `\\?\UNC\`) when present.
///
/// The result is meant for humans, not for IO: when the stripped path would be
/// 260 characters or longer the verbatim prefix is kept, because legacy Windows
/// APIs reject such paths without it and the string must stay usable.
#[must_use]
pub fn normalize_display_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let stripped = match raw.strip_prefix(r"\\?\UNC\") {
        Some(unc) => format!(r"\\{unc}"),
        None => match raw.strip_prefix(r"\\?\") {
            Some(rest) => rest.to_owned(),
            None => return raw.into_owned(),
        },
    };
    if stripped.chars().count() < WINDOWS_MAX_PATH {
        stripped
    } else {
        raw.into_owned()
    }
}

/// Reads `key` through `env_lookup`, treating unset, empty and blank values as absent.
fn non_empty_env(env_lookup: &impl Fn(&str) -> Option<OsString>, key: &str) -> Option<PathBuf> {
    let value = env_lookup(key)?;
    if value.to_string_lossy().trim().is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// The `SPANBAY_DATA_DIR` override, when it is set to an absolute path.
fn explicit_data_dir(
    os: HostOs,
    env_lookup: &impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    non_empty_env(env_lookup, DATA_DIR_ENV).filter(|dir| is_absolute_for_os(os, dir))
}

fn is_absolute_for_os(os: HostOs, path: &Path) -> bool {
    let text = path.to_string_lossy();
    match os {
        HostOs::Windows => {
            path.is_absolute()
                || text.starts_with(r"\\")
                || (text.len() >= 3
                    && text.as_bytes()[0].is_ascii_alphabetic()
                    && text.as_bytes()[1] == b':'
                    && matches!(text.as_bytes()[2], b'\\' | b'/'))
        }
        HostOs::MacOs | HostOs::Linux => text.starts_with('/'),
    }
}

fn join_windows_segments(base: &Path, segments: &[&str]) -> PathBuf {
    if cfg!(target_os = "windows") {
        let mut out = base.to_path_buf();
        for segment in segments {
            out.push(segment);
        }
        return out;
    }
    let base_text = base.to_string_lossy();
    if base_text.contains('\\') && !base_text.contains('/') {
        let mut out = base_text.trim_end_matches('\\').to_owned();
        for segment in segments {
            out.push('\\');
            out.push_str(segment);
        }
        PathBuf::from(out)
    } else {
        let mut out = base.to_path_buf();
        for segment in segments {
            out.push(segment);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::{
        APP_IDENTIFIER, DATABASE_FILENAME, DataDirs, HostOs, create_private_dir_all,
        database_path_with_env, normalize_display_path, portable_file_name,
        resolve_app_data_dir_with_env, restrict_file_to_owner, write_atomic, write_private_file,
    };

    /// Unique scratch directory removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "spanbay-platform-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(*v)))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn resolves_macos_data_dir_under_application_support() {
        let lookup = env_map(&[("HOME", "/Users/ada")]);
        assert_eq!(
            resolve_app_data_dir_with_env(HostOs::MacOs, false, &lookup),
            Some(PathBuf::from(
                "/Users/ada/Library/Application Support/com.spanbay.desktop"
            ))
        );
    }

    #[test]
    fn resolves_linux_xdg_dir_and_ignores_relative_xdg_paths() {
        let custom = env_map(&[
            ("HOME", "/home/ada"),
            ("XDG_DATA_HOME", "/var/lib/ada-data"),
        ]);
        assert_eq!(
            resolve_app_data_dir_with_env(HostOs::Linux, false, &custom),
            Some(PathBuf::from("/var/lib/ada-data/com.spanbay.desktop"))
        );

        // XDG Base Directory Specification requires relative paths to be ignored.
        let relative = env_map(&[("HOME", "/home/ada"), ("XDG_DATA_HOME", "relative/data")]);
        assert_eq!(
            resolve_app_data_dir_with_env(HostOs::Linux, false, &relative),
            Some(PathBuf::from("/home/ada/.local/share/com.spanbay.desktop"))
        );
    }

    #[test]
    fn blank_environment_values_count_as_unset() {
        let blank = env_map(&[("HOME", "  "), ("XDG_DATA_HOME", "")]);
        assert_eq!(
            resolve_app_data_dir_with_env(HostOs::Linux, false, &blank),
            None
        );
    }

    #[test]
    fn resolves_windows_appdata_with_localappdata_and_userprofile_fallbacks() {
        let primary = env_map(&[
            ("APPDATA", r"C:\Users\Ada\AppData\Roaming"),
            ("LOCALAPPDATA", r"C:\Users\Ada\AppData\Local"),
        ]);
        assert_eq!(
            resolve_app_data_dir_with_env(HostOs::Windows, false, &primary),
            Some(PathBuf::from(format!(
                r"C:\Users\Ada\AppData\Roaming\{APP_IDENTIFIER}"
            )))
        );

        let local_fallback = env_map(&[("LOCALAPPDATA", r"C:\Users\Ada\AppData\Local")]);
        assert_eq!(
            resolve_app_data_dir_with_env(HostOs::Windows, false, &local_fallback),
            Some(PathBuf::from(format!(
                r"C:\Users\Ada\AppData\Local\{APP_IDENTIFIER}"
            )))
        );

        let profile_fallback = env_map(&[("USERPROFILE", r"C:\Users\Ada")]);
        assert_eq!(
            resolve_app_data_dir_with_env(HostOs::Windows, false, &profile_fallback),
            Some(PathBuf::from(format!(
                r"C:\Users\Ada\AppData\Roaming\{APP_IDENTIFIER}"
            )))
        );
    }

    #[test]
    fn dev_builds_resolve_to_a_separate_directory_on_macos_and_linux() {
        let home = env_map(&[("HOME", "/h")]);
        assert_eq!(
            resolve_app_data_dir_with_env(HostOs::MacOs, true, &home),
            Some(PathBuf::from(
                "/h/Library/Application Support/com.spanbay.desktop.dev"
            ))
        );
        assert_eq!(
            resolve_app_data_dir_with_env(HostOs::Linux, true, &home),
            Some(PathBuf::from("/h/.local/share/com.spanbay.desktop.dev"))
        );
        let xdg = env_map(&[("HOME", "/h"), ("XDG_DATA_HOME", "/x")]);
        assert_eq!(
            resolve_app_data_dir_with_env(HostOs::Linux, true, &xdg),
            Some(PathBuf::from("/x/com.spanbay.desktop.dev"))
        );
    }

    #[test]
    fn an_absolute_data_dir_override_wins_in_every_build() {
        let lookup = env_map(&[("HOME", "/h"), ("SPANBAY_DATA_DIR", "/data/pd")]);
        for (os, dev) in [(HostOs::MacOs, true), (HostOs::Linux, false)] {
            assert_eq!(
                resolve_app_data_dir_with_env(os, dev, &lookup),
                Some(PathBuf::from("/data/pd"))
            );
        }
        // The override does not need HOME at all.
        let bare = env_map(&[("SPANBAY_DATA_DIR", "/data/pd")]);
        assert_eq!(
            resolve_app_data_dir_with_env(HostOs::MacOs, false, &bare),
            Some(PathBuf::from("/data/pd"))
        );
    }

    #[test]
    fn relative_or_blank_data_dir_overrides_are_ignored() {
        for value in ["relative/pd", "", "   "] {
            let lookup = env_map(&[("HOME", "/h"), ("SPANBAY_DATA_DIR", value)]);
            assert_eq!(
                resolve_app_data_dir_with_env(HostOs::MacOs, false, &lookup),
                Some(PathBuf::from(
                    "/h/Library/Application Support/com.spanbay.desktop"
                )),
                "override {value:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_override_directory_keeps_its_mode_and_a_missing_one_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let scratch = Scratch::new();
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o755)).unwrap();
        let shared = scratch.0.to_string_lossy().into_owned();
        let db = database_path_with_env(
            HostOs::current(),
            true,
            env_map(&[("SPANBAY_DATA_DIR", shared.as_str())]),
        )
        .unwrap();
        assert_eq!(db, scratch.0.join(DATABASE_FILENAME));
        assert_eq!(mode(&scratch.0), 0o755);

        let fresh = scratch.0.join("fresh");
        let fresh_text = fresh.to_string_lossy().into_owned();
        database_path_with_env(
            HostOs::current(),
            false,
            env_map(&[("SPANBAY_DATA_DIR", fresh_text.as_str())]),
        )
        .unwrap();
        assert_eq!(mode(&fresh), 0o700);
    }

    #[test]
    fn extracts_portable_file_names_across_posix_and_windows_separators() {
        assert_eq!(portable_file_name("/tmp/receipt.pdf"), "receipt.pdf");
        assert_eq!(
            portable_file_name(r"C:\Users\Ada\Documents\invoice.json"),
            "invoice.json"
        );
        assert_eq!(
            portable_file_name("nested/folder\\archive.tar.gz"),
            "archive.tar.gz"
        );
        assert_eq!(portable_file_name("plain-file.txt"), "plain-file.txt");
    }

    #[test]
    fn normalizes_windows_verbatim_display_paths() {
        assert_eq!(
            normalize_display_path(Path::new(r"\\?\C:\Users\Ada\collection.json")),
            r"C:\Users\Ada\collection.json"
        );
        assert_eq!(
            normalize_display_path(Path::new(r"\\?\UNC\fileserver\share\export.json")),
            r"\\fileserver\share\export.json"
        );
        assert_eq!(
            normalize_display_path(Path::new("/Users/ada/export.json")),
            "/Users/ada/export.json"
        );
    }

    #[test]
    fn keeps_verbatim_prefix_when_stripped_path_reaches_max_path() {
        let long_tail = "a".repeat(260 - r"C:\".len());
        let at_limit = format!(r"\\?\C:\{long_tail}");
        assert_eq!(normalize_display_path(Path::new(&at_limit)), at_limit);

        let short_tail = "a".repeat(260 - r"C:\".len() - 1);
        assert_eq!(
            normalize_display_path(Path::new(&format!(r"\\?\C:\{short_tail}"))),
            format!(r"C:\{short_tail}")
        );

        let unc = format!(r"\\?\UNC\server\share\{}", "b".repeat(260));
        assert_eq!(normalize_display_path(Path::new(&unc)), unc);
    }

    #[test]
    fn write_private_file_writes_and_truncates() {
        let scratch = Scratch::new();
        let path = scratch.0.join("secret.txt");
        write_private_file(&path, "first contents").unwrap();
        write_private_file(&path, b"second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    }

    #[cfg(unix)]
    #[test]
    fn private_helpers_set_owner_only_unix_modes() {
        use std::os::unix::fs::PermissionsExt;

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let scratch = Scratch::new();

        let new_file = scratch.0.join("new.txt");
        write_private_file(&new_file, "x").unwrap();
        assert_eq!(mode(&new_file), 0o600);

        let loose_file = scratch.0.join("loose.txt");
        fs::write(&loose_file, "old data").unwrap();
        fs::set_permissions(&loose_file, fs::Permissions::from_mode(0o644)).unwrap();
        write_private_file(&loose_file, "new").unwrap();
        assert_eq!(mode(&loose_file), 0o600);

        let restricted = scratch.0.join("restricted.txt");
        fs::write(&restricted, "x").unwrap();
        fs::set_permissions(&restricted, fs::Permissions::from_mode(0o666)).unwrap();
        restrict_file_to_owner(&restricted).unwrap();
        assert_eq!(mode(&restricted), 0o600);

        let nested = scratch.0.join("a").join("b");
        create_private_dir_all(&nested).unwrap();
        assert_eq!(mode(&nested), 0o700);
        assert_eq!(mode(&scratch.0.join("a")), 0o700);
    }

    #[test]
    fn restrict_file_to_owner_reports_missing_files() {
        let scratch = Scratch::new();
        let missing = scratch.0.join("missing.txt");
        if cfg!(unix) {
            assert!(restrict_file_to_owner(&missing).is_err());
        } else {
            assert!(restrict_file_to_owner(&missing).is_ok());
        }
    }

    #[test]
    fn create_private_dir_all_is_idempotent() {
        let scratch = Scratch::new();
        let dir = scratch.0.join("data");
        create_private_dir_all(&dir).unwrap();
        create_private_dir_all(&dir).unwrap();
        assert!(dir.is_dir());
    }
    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_staging_file() {
        let scratch = Scratch::new();
        let target = scratch.0.join("out.json");
        write_atomic(&target, b"one", true).unwrap();
        write_atomic(&target, b"two", true).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(&scratch.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("out.json")]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn write_atomic_fails_cleanly_when_the_directory_is_missing() {
        let scratch = Scratch::new();
        assert!(write_atomic(&scratch.0.join("no/such/dir.txt"), b"x", false).is_err());
    }

    #[test]
    fn data_dirs_hang_off_the_database_directory() {
        let scratch = Scratch::new();
        let dirs = DataDirs::for_database(&scratch.0.join(DATABASE_FILENAME));
        assert_eq!(dirs.root, scratch.0);
        assert_eq!(dirs.attachments, scratch.0.join("attachments"));
        dirs.ensure().unwrap();
        dirs.ensure().unwrap();
        assert!(dirs.skills.is_dir() && dirs.attachments.is_dir() && dirs.tmp.is_dir());
        assert_eq!(dirs.logs, scratch.0.join("logs"));
        assert!(dirs.logs.is_dir());
        let bare = DataDirs::for_database(Path::new("x.sqlite3"));
        assert_eq!(bare.root, PathBuf::from("."));
    }

    #[test]
    fn open_private_append_appends_and_restricts_the_file_to_the_owner() {
        use std::io::Write;

        let scratch = Scratch::new();
        let path = scratch.0.join("a.log");
        fs::write(&path, "old\n").unwrap();
        super::open_private_append(&path)
            .unwrap()
            .write_all(b"new\n")
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "old\nnew\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
