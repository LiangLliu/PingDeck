//! The shell tool's names and the process-group helpers it shares with the process registry
//! and the permission layer. They live below the tool itself so a crate that must not depend on
//! the tool (or on the app state it needs) can still name them.

use std::time::Duration;

use serde::{Deserialize, Serialize};

#[cfg(unix)]
pub use nix::sys::signal::Signal;
#[cfg(windows)]
use std::process::Stdio;
#[cfg(windows)]
use tokio::process::Command;

pub const SHELL: &str = "shell";
pub const SHELL_OUTPUT: &str = "shell_output";
pub const SHELL_KILL: &str = "shell_kill";
pub const TERM_GRACE: Duration = Duration::from_millis(500);
pub const KILL_GRACE: Duration = Duration::from_millis(250);

/// Sends `signal` (`None` only probes) to process group `pgid` and reports whether it was
/// delivered. Group 0 is the caller's own and 1 holds init, so neither is ever signalled.
#[cfg(unix)]
pub fn kill_group(pgid: u32, signal: Option<Signal>) -> bool {
    match i32::try_from(pgid) {
        Ok(pgid) if pgid > 1 => {
            nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), signal).is_ok()
        }
        _ => false,
    }
}

/// Sends `SIGTERM` (`hard == false`) or `SIGKILL` to the process group led by `pid`; on Windows
/// `taskkill` takes down the whole tree.
pub async fn signal_group(pid: Option<u32>, hard: bool) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    kill_group(
        pid,
        Some(if hard {
            Signal::SIGKILL
        } else {
            Signal::SIGTERM
        }),
    );
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill");
        command.args(["/PID", &pid.to_string(), "/T"]);
        if hard {
            command.arg("/F");
        }
        let _ = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

/// Number of trailing bytes of `bytes` that start a UTF-8 sequence which is not complete yet.
pub fn incomplete_tail_len(bytes: &[u8]) -> usize {
    for back in 1..=bytes.len().min(3) {
        let byte = bytes[bytes.len() - back];
        if byte & 0xC0 == 0x80 {
            continue;
        }
        let needed = match byte {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => 1,
        };
        return if needed > back { back } else { 0 };
    }
    0
}

/// Whether `call_id` is safe as a file name: 1 to 128 characters of `A-Z a-z 0-9 . _ : -`, and
/// not `.` or `..`.
#[must_use]
pub fn valid_call_id(call_id: &str) -> bool {
    !call_id.is_empty()
        && call_id.len() <= 128
        && call_id != "."
        && call_id != ".."
        && call_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// One page of a spilled tool output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputChunk {
    /// The text of this page (invalid UTF-8 replaced).
    pub text: String,
    /// Byte offset of the first byte of `text`.
    pub offset: u64,
    /// Offset to request for the next page.
    pub next_offset: u64,
    /// Size of the whole output in bytes.
    pub total: u64,
    /// This page reaches the end of the output.
    pub eof: bool,
}

#[cfg(all(test, unix))]
mod tests {
    use super::kill_group;

    #[test]
    fn the_own_group_init_and_out_of_range_ids_are_never_signalled() {
        // Group 0 would otherwise probe successfully: it is the test process's own group.
        for pgid in [0, 1, u32::MAX] {
            assert!(!kill_group(pgid, None), "{pgid}");
        }
        assert!(kill_group(
            nix::unistd::getpgrp().as_raw().cast_unsigned(),
            None
        ));
    }
}
