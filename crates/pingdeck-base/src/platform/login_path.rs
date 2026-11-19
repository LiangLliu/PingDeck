//! The user's login-shell `PATH`, for the processes the app starts on the user's behalf (the
//! agent's shell tool, stdio MCP servers). An app opened from Finder or the Dock inherits launchd's
//! short `PATH`, where `npx`, `uvx` and version managers are missing.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How long the login shell gets to print its `PATH`.
pub const LOGIN_PATH_TIMEOUT: Duration = Duration::from_secs(3);
/// Markers around the `PATH` in the login shell's output (rc files may print other text).
const PATH_MARKS: (&str, &str) = ("__PD_PATH_BEGIN__", "__PD_PATH_END__");

/// The user's shell: an absolute, existing `$SHELL`, else `/bin/zsh` on macOS and `/bin/sh`
/// elsewhere.
#[must_use]
pub fn user_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| Path::new(shell).is_absolute() && Path::new(shell).is_file())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh".to_owned()
            } else {
                "/bin/sh".to_owned()
            }
        })
}

/// The `PATH` an interactive login shell sees (where nvm, pyenv, asdf and volta add themselves):
/// `-ilc`, then `-lc`, then [`fallback_path`]. Resolved once per process and cached; the first
/// call blocks up to [`LOGIN_PATH_TIMEOUT`] twice, so call it off async threads, and early.
pub fn login_path() -> OsString {
    static PATH: OnceLock<OsString> = OnceLock::new();
    PATH.get_or_init(|| {
        let shell = user_shell();
        path_from_shell(&shell, "-ilc")
            .or_else(|| path_from_shell(&shell, "-lc"))
            .unwrap_or_else(fallback_path)
    })
    .clone()
}

/// The fixed list used when no shell can say: `/usr/bin:/bin:/usr/sbin:/sbin`, `~/.cargo/bin`,
/// `/opt/homebrew/bin`, `/usr/local/bin`.
#[must_use]
pub fn fallback_path() -> OsString {
    let mut dirs: Vec<PathBuf> = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        .map(PathBuf::from)
        .into();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".cargo/bin"));
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    std::env::join_paths(dirs).unwrap_or_else(|_| OsString::from("/usr/bin:/bin"))
}

/// The text between the path marks in a shell's output.
#[must_use]
pub fn path_between_marks(output: &str) -> Option<&str> {
    let (begin, end) = PATH_MARKS;
    let rest = &output[output.find(begin)? + begin.len()..];
    let path = &rest[..rest.find(end)?];
    (!path.is_empty()).then_some(path)
}

/// Runs `shell` with `flags` and a script printing `$PATH` between marks, stdin from `/dev/null`,
/// `TERM=dumb`, killed after [`LOGIN_PATH_TIMEOUT`].
#[must_use]
pub fn path_from_shell(shell: &str, flags: &str) -> Option<OsString> {
    let script = format!(
        "printf '%s%s%s' '{}' \"$PATH\" '{}'",
        PATH_MARKS.0, PATH_MARKS.1
    );
    let mut child = Command::new(shell)
        .args([flags, &script])
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a thread so a chatty rc file cannot fill the pipe and stall the shell.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = sender.send(text);
    });
    let deadline = Instant::now() + LOGIN_PATH_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let text = receiver.recv_timeout(Duration::from_millis(200)).ok()?;
    path_between_marks(&text).map(OsString::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_is_read_between_the_marks_with_a_fixed_fallback() {
        assert_eq!(
            path_between_marks("motd\n__PD_PATH_BEGIN__/a:/b__PD_PATH_END__\nbye"),
            Some("/a:/b")
        );
        assert_eq!(path_between_marks("__PD_PATH_BEGIN____PD_PATH_END__"), None);
        assert!(fallback_path().to_string_lossy().contains("/usr/bin"));
        #[cfg(unix)]
        assert!(
            !path_from_shell("/bin/sh", "-c")
                .unwrap_or_default()
                .is_empty()
        );
    }
}
