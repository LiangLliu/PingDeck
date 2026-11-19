//! Rotating diagnostic log and panic hook.
//!
//! Single responsibility: append one redacted line per record to `<root>/logs/pingdeck.log`
//! (owner-only, rotated at 5 MiB, three files kept), without any logging framework. Every record
//! goes through [`crate::secrets::redact_secrets`] before it touches the disk; there is no way to
//! switch that off. Line format: `ts level target msg`, with `ts` an RFC 3339 UTC timestamp and
//! newlines in `target` and `msg` escaped, so one record is always one line. The same line goes
//! to stderr when no log file is open and in debug builds, and the newest warnings and errors
//! stay in memory for the diagnostics ([`recent_problems`]).
//!
//! Limit: only the pattern rules apply, not the stored secret values. `log_line` may be called
//! while the store lock is held, and reading the stored values takes that lock, so a log call
//! never asks the secret store; a secret that matches no pattern must not be put in a message.

use std::{
    collections::VecDeque,
    fs,
    io::{self, Write},
    panic,
    path::PathBuf,
    sync::{
        Mutex, PoisonError, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{
    error::AppError,
    platform::{DataDirs, create_private_dir_all, open_private_append},
    secrets::redact_secrets,
    time::now_ms,
};

/// Name of the active log file inside [`DataDirs::logs`]; rotated files append `.1`, `.2`.
pub const LOG_FILE_NAME: &str = "pingdeck.log";
/// Size at which the active log file is rotated.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
/// Number of log files kept: the active one plus two rotated ones.
pub const LOG_FILES: usize = 3;
/// Longest message kept in one record (after redaction); the rest is cut.
const MAX_MESSAGE_CHARS: usize = 8 * 1024;

/// Severity of a log record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Something failed.
    Error,
    /// Something unexpected that the app recovered from.
    Warn,
    /// Normal lifecycle information.
    Info,
    /// Detail for troubleshooting.
    Debug,
}

impl LogLevel {
    /// The fixed-width-free upper-case name used in the line format.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
        }
    }
}

/// An append-only file that moves itself aside when the next line would pass `max_bytes`.
struct RotatingLog {
    path: PathBuf,
    max_bytes: u64,
    files: usize,
    file: fs::File,
    size: u64,
}

impl RotatingLog {
    /// Opens (creating, owner-only) `path`; `files` counts the active file too (at least 2).
    fn open(path: PathBuf, max_bytes: u64, files: usize) -> io::Result<Self> {
        let file = open_private_append(&path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path,
            max_bytes,
            files: files.max(2),
            file,
            size,
        })
    }

    fn numbered(&self, n: usize) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    /// Appends `line` (which carries its own newline), rotating first when it would not fit. A
    /// line longer than the limit still lands, alone, in a fresh file.
    fn write_line(&mut self, line: &str) -> io::Result<()> {
        let len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if self.size > 0 && self.size.saturating_add(len) > self.max_bytes {
            self.rotate()?;
        }
        self.file.write_all(line.as_bytes())?;
        self.size = self.size.saturating_add(len);
        Ok(())
    }

    /// `log.1` → `log.2` …, dropping the oldest, then the active file becomes `log.1`.
    fn rotate(&mut self) -> io::Result<()> {
        let rename = |from: PathBuf, to: PathBuf| match fs::rename(from, to) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
        match fs::remove_file(self.numbered(self.files - 1)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        for n in (1..self.files - 1).rev() {
            rename(self.numbered(n), self.numbered(n + 1))?;
        }
        rename(self.path.clone(), self.numbered(1))?;
        self.file = open_private_append(&self.path)?;
        self.size = 0;
        Ok(())
    }
}

/// The process-wide log; `None` until [`init_logging`] and again after its guard drops.
static LOGGER: Mutex<Option<RotatingLog>> = Mutex::new(None);

/// Keeps the log open; dropping it closes the log, after which [`log_line`] discards records.
/// Hold one guard at a time: a second [`init_logging`] replaces the first log.
#[must_use = "dropping the guard closes the log immediately"]
#[derive(Debug)]
pub struct LogGuard(());

impl Drop for LogGuard {
    fn drop(&mut self) {
        *LOGGER.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Opens `<root>/logs/pingdeck.log` (creating `logs/` owner-only) as the process-wide log.
///
/// # Errors
/// [`AppError::Io`] when the directory or file cannot be created.
pub fn init_logging(dirs: &DataDirs) -> Result<LogGuard, AppError> {
    create_private_dir_all(&dirs.logs)
        .and_then(|()| RotatingLog::open(dirs.logs.join(LOG_FILE_NAME), MAX_LOG_BYTES, LOG_FILES))
        .map(|log| {
            *LOGGER.lock().unwrap_or_else(PoisonError::into_inner) = Some(log);
            LogGuard(())
        })
        .map_err(|error| AppError::io(format!("cannot open the log file: {error}")))
}

/// Appends one record. `msg` is always passed through [`redact_secrets`], cut to a bounded
/// length and flattened to one line. The same redacted line also goes to stderr when no log file
/// is open (before [`init_logging`], after its guard dropped, when the file cannot be written)
/// and in debug builds, so a terminal run sees it too. This is the one place that writes to
/// stderr: `clippy::print_stderr` is denied everywhere else outside tests
/// (docs/development.md §2.4).
pub fn log_line(level: LogLevel, target: &str, msg: &str) {
    let line = format_line(now_ms(), level, target, msg);
    let mut logger = LOGGER.lock().unwrap_or_else(PoisonError::into_inner);
    let written = logger
        .as_mut()
        .is_some_and(|log| log.write_line(&line).is_ok());
    drop(logger);
    if !written || cfg!(debug_assertions) {
        // `eprint!` panics when stderr is closed (a GUI launch); a failed write is dropped.
        let _ = io::stderr().write_all(line.as_bytes());
    }
    if matches!(level, LogLevel::Error | LogLevel::Warn) {
        remember(
            &mut RECENT.lock().unwrap_or_else(PoisonError::into_inner),
            &line,
        );
    }
}

/// How many warning and error records [`recent_problems`] keeps.
pub const RECENT_PROBLEMS: usize = 20;

/// The newest warning and error records (redacted lines without the newline), oldest first.
static RECENT: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

fn remember(recent: &mut VecDeque<String>, line: &str) {
    if recent.len() == RECENT_PROBLEMS {
        recent.pop_front();
    }
    recent.push_back(line.trim_end().to_owned());
}

/// The last [`RECENT_PROBLEMS`] warning and error records of this process, oldest first, in
/// the log's line format (already redacted). Kept in memory whether or not a log file is open,
/// so the diagnostics can name recent failures.
#[must_use]
pub fn recent_problems() -> Vec<String> {
    RECENT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .cloned()
        .collect()
}

fn append(logger: &mut Option<RotatingLog>, level: LogLevel, target: &str, msg: &str) {
    if let Some(log) = logger {
        let _ = log.write_line(&format_line(now_ms(), level, target, msg));
    }
}

/// The record `ts level target msg\n`; `target` and `msg` are flattened to one line.
fn format_line(ts_ms: i64, level: LogLevel, target: &str, msg: &str) -> String {
    let mut msg = redact_secrets(msg, &[]);
    if let Some((cut, _)) = msg.char_indices().nth(MAX_MESSAGE_CHARS) {
        msg.truncate(cut);
        msg.push_str("…[truncated]");
    }
    let one_line = |text: &str| text.replace('\r', "\\r").replace('\n', "\\n");
    format!(
        "{} {} {} {}\n",
        utc_stamp(ts_ms),
        level.as_str(),
        one_line(target),
        one_line(&msg)
    )
}

/// `ts_ms` (Unix milliseconds) as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn utc_stamp(ts_ms: i64) -> String {
    let days = ts_ms.div_euclid(86_400_000);
    let ms = ts_ms.rem_euclid(86_400_000);
    // Days → civil date (Howard Hinnant's algorithm, shifted to a 0000-03-01 epoch).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        ms / 3_600_000,
        ms % 3_600_000 / 60_000,
        ms % 60_000 / 1_000,
        ms % 1_000
    )
}

/// Chains a panic hook that logs `thread 'x' panicked at file:line:col: message` (redacted) at
/// `Error` level, then runs the previously installed hook, so the default stderr report and the
/// process behaviour stay as they were. Idempotent. Uses `try_lock` so a panic raised while the
/// log is being written cannot deadlock.
pub fn install_panic_hook() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic payload");
        let location = info.location().map_or_else(
            || "unknown location".to_owned(),
            |at| format!("{}:{}:{}", at.file(), at.line(), at.column()),
        );
        let thread = std::thread::current();
        let record = format!(
            "thread '{}' panicked at {location}: {message}",
            thread.name().unwrap_or("<unnamed>")
        );
        let mut logger = match LOGGER.try_lock() {
            Ok(logger) => logger,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                previous(info);
                return;
            }
        };
        append(&mut logger, LogLevel::Error, "panic", &record);
        drop(logger);
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "pingdeck-logging-{}-{}",
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

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    /// A 30-byte line: two digits, 27 filler bytes, newline.
    fn line(n: usize) -> String {
        format!("{n:02}{}\n", "x".repeat(27))
    }

    #[test]
    fn stamps_are_utc_rfc3339() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(utc_stamp(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
        assert_eq!(utc_stamp(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(utc_stamp(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn records_are_one_redacted_line_in_the_documented_format() {
        let text = format_line(
            0,
            LogLevel::Warn,
            "net",
            "retry https://x.test/?token=zzz9 sk-abcdefghijklmnopqrstuv\nAuthorization: Bearer abc.def",
        );
        assert!(
            text.starts_with("1970-01-01T00:00:00.000Z WARN net retry "),
            "{text}"
        );
        assert_eq!(text.matches('\n').count(), 1, "{text}");
        assert!(text.ends_with('\n'));
        for secret in ["zzz9", "sk-abcdef", "abc.def"] {
            assert!(!text.contains(secret), "{text}");
        }
        assert!(text.contains("\\n"), "{text}");
        let long = format_line(0, LogLevel::Info, "t", &"é".repeat(MAX_MESSAGE_CHARS * 2));
        assert!(long.contains("…[truncated]") && long.chars().count() < MAX_MESSAGE_CHARS + 80);
    }

    #[test]
    fn warnings_and_errors_are_remembered_newest_last_and_bounded() {
        log_line(
            LogLevel::Warn,
            "test.recent",
            "warned-7f3a x-api-key: key-1234567",
        );
        log_line(LogLevel::Info, "test.recent", "informed-7f3a");
        let recent = recent_problems();
        let warned = recent.iter().find(|line| line.contains("warned-7f3a"));
        assert!(warned.is_some_and(|line| !line.contains("key-1234567") && !line.ends_with('\n')));
        assert!(!recent.iter().any(|line| line.contains("informed-7f3a")));
        let mut ring = VecDeque::new();
        for n in 0..RECENT_PROBLEMS + 3 {
            remember(&mut ring, &format!("line {n}\n"));
        }
        assert_eq!(ring.len(), RECENT_PROBLEMS);
        assert_eq!(ring.front().map(String::as_str), Some("line 3"));
    }

    #[test]
    fn a_target_cannot_forge_a_second_record() {
        let text = format_line(0, LogLevel::Info, "net\r\n1970 ERROR forged", "msg");
        assert_eq!(text.matches('\n').count(), 1, "{text}");
        assert!(!text.contains('\r'), "{text}");
        assert!(text.contains("net\\r\\n1970 ERROR forged msg"), "{text}");
    }

    #[test]
    fn the_log_rotates_at_the_size_limit_and_keeps_three_files() {
        let scratch = Scratch::new();
        let path = scratch.0.join(LOG_FILE_NAME);
        let mut log = RotatingLog::open(path.clone(), 100, 3).unwrap();
        for n in 1..=10 {
            log.write_line(&line(n)).unwrap();
        }
        let numbered = |n: usize| read(&scratch.0.join(format!("{LOG_FILE_NAME}.{n}")));
        assert_eq!(read(&path), line(10));
        assert_eq!(numbered(1), [7, 8, 9].map(line).concat());
        assert_eq!(numbered(2), [4, 5, 6].map(line).concat());
        assert!(!scratch.0.join(format!("{LOG_FILE_NAME}.3")).exists());
        #[cfg(unix)]
        for name in [
            LOG_FILE_NAME.to_owned(),
            format!("{LOG_FILE_NAME}.1"),
            format!("{LOG_FILE_NAME}.2"),
        ] {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(scratch.0.join(&name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
    }

    #[test]
    fn an_existing_file_counts_towards_the_limit_and_a_huge_line_still_lands() {
        let scratch = Scratch::new();
        let path = scratch.0.join(LOG_FILE_NAME);
        fs::write(&path, [1, 2, 3].map(line).concat()).unwrap();
        let mut log = RotatingLog::open(path.clone(), 100, 3).unwrap();
        log.write_line(&line(4)).unwrap();
        assert_eq!(read(&path), line(4));
        assert_eq!(
            read(&scratch.0.join("pingdeck.log.1")),
            [1, 2, 3].map(line).concat()
        );
        let huge = format!("{}\n", "y".repeat(500));
        log.write_line(&huge).unwrap();
        log.write_line(&line(5)).unwrap();
        assert_eq!(read(&scratch.0.join("pingdeck.log.1")), huge);
        assert_eq!(read(&path), line(5));
    }

    #[test]
    fn the_process_log_redacts_and_panics_are_recorded_without_losing_the_default_hook() {
        let scratch = Scratch::new();
        let dirs = DataDirs::for_database(&scratch.0.join("db.sqlite3"));
        let guard = init_logging(&dirs).unwrap();
        let log_path = dirs.logs.join(LOG_FILE_NAME);
        log_line(LogLevel::Info, "test", "hello x-api-key: key-1234567 world");
        install_panic_hook();
        install_panic_hook();
        let joined = std::thread::Builder::new()
            .name("boom-thread".to_owned())
            .spawn(|| {
                let secret = "Bearer ghijkl.mnopqr";
                panic!("exploded with {secret}");
            })
            .unwrap()
            .join();
        assert!(joined.is_err());
        let text = read(&log_path);
        assert!(
            text.contains(" INFO test hello x-api-key: ••••••"),
            "{text}"
        );
        let panic_line = text
            .lines()
            .find(|line| line.contains("boom-thread"))
            .unwrap_or_default();
        assert!(
            panic_line.contains(" ERROR panic thread 'boom-thread' panicked at "),
            "{text}"
        );
        assert!(panic_line.contains("logging.rs:"), "{panic_line}");
        assert_eq!(
            text.matches("boom-thread").count(),
            1,
            "hook installed twice: {text}"
        );
        assert!(
            !text.contains("key-1234567") && !text.contains("ghijkl.mnopqr"),
            "{text}"
        );
        drop(guard);
        let size = read(&log_path).len();
        log_line(LogLevel::Info, "test", "after close");
        assert_eq!(read(&log_path).len(), size);
    }
}
