//! The process-group ledger: every process group the core spawns for a tool call, a background
//! process or an MCP stdio server is recorded when it starts and removed when it ends, so a
//! group a crash left behind is stopped at the next start.
//!
//! Single responsibility: identify a process ([`process_start_time`]: its start time, which tells
//! a recycled process id from the original), keep the table in step with the live groups
//! ([`LedgerHandle`]) and reap leftovers ([`reap_leftover_processes`]). A recorded group is killed
//! when its leader's process id still exists with the same start time, or when the leader is gone
//! but members of the group remain; a recycled id (another start time) is cleared without a kill.
//! The table is `process_ledger` (see `workspace::storage`); nothing is resumed. Unix only: where
//! there are no process groups the ledger records nothing.

use std::process::{Command, Stdio};
use std::time::Duration;

pub use pingdeck_model::agent::ledger::{LedgerEntry, LedgerKind};

use serde::{Deserialize, Serialize};

use pingdeck_store::storage::WorkspaceStore;

#[cfg(unix)]
use crate::agent::tools::shell_meta::{Signal, kill_group};

/// What [`reap_leftover_processes`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReapReport {
    /// Groups that were still alive and were stopped.
    pub killed: usize,
    /// Of those, groups that had been running a tool call (the start notice names these).
    pub tool_calls: usize,
    /// Records whose process was gone or whose id belongs to another program now (cleared, not
    /// killed).
    pub cleared: usize,
}

/// The start time of process `pid` as the system reports it (`ps -o lstart=`, one-second
/// resolution, locale-independent), or `None` when no such process exists or the platform cannot
/// say. Two processes that share an id never share a start time, which is what makes a recorded
/// group safe to kill.
#[must_use]
pub fn process_start_time(pid: u32) -> Option<String> {
    if !cfg!(unix) {
        return None;
    }
    let output = Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    (output.status.success() && !text.is_empty())
        .then(|| text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The entry for a group led by `pid` that was just spawned with its own process group.
#[must_use]
pub fn entry_for(pid: u32, kind: LedgerKind, label: &str) -> Option<LedgerEntry> {
    Some(LedgerEntry {
        pid,
        pgid: pid,
        started: process_start_time(pid)?,
        kind,
        label: label.to_owned(),
    })
}

/// Whether any process of group `pgid` exists (`kill -0` on the group). A process id cannot be
/// handed out again while a group of that number exists, so a group whose leader is gone consists
/// of the original members.
fn group_alive(pgid: u32) -> bool {
    #[cfg(unix)]
    return kill_group(pgid, None);
    #[cfg(not(unix))]
    return false;
}

/// Stops the group of `entry`: `SIGTERM`, a short grace, then `SIGKILL`. Blocking.
fn stop_group(pgid: u32) {
    #[cfg(unix)]
    {
        kill_group(pgid, Some(Signal::SIGTERM));
        std::thread::sleep(Duration::from_millis(300));
        kill_group(pgid, Some(Signal::SIGKILL));
    }
}

/// Stops every group of `entries` that is still ours and reports what happened: its leader exists
/// with the recorded start time, or the leader is gone while members of the group remain (a
/// shell killed first leaves its children behind). A leader id now held by another process (a
/// different start time) is a recycled id and is left alone. Blocking: a few hundred
/// milliseconds per live group.
pub fn reap_entries(entries: Vec<LedgerEntry>) -> ReapReport {
    let mut report = ReapReport::default();
    for entry in entries {
        let leader = process_start_time(entry.pid);
        let ours = match leader.as_deref() {
            Some(started) => started == entry.started,
            None => group_alive(entry.pgid),
        };
        if ours {
            stop_group(entry.pgid);
            report.killed += 1;
            report.tool_calls += usize::from(entry.kind == LedgerKind::Tool);
        } else {
            report.cleared += 1;
        }
    }
    report
}

/// At app start: reads and clears the ledger, then stops every recorded group that is still alive
/// (same process id and start time) and reports what happened. Runs on a blocking thread; the
/// store lock is held only while the records are read. Call once, before any new group is
/// recorded; nothing is resumed.
pub async fn reap_leftover_processes(
    store: std::sync::Arc<std::sync::Mutex<WorkspaceStore>>,
) -> ReapReport {
    tokio::task::spawn_blocking(move || {
        // A failed read leaves the table for the next start; nothing is killed blindly.
        let entries = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ledger_take_all()
            .unwrap_or_default();
        reap_entries(entries)
    })
    .await
    .unwrap_or_default()
}

/// Records and clears groups in the ledger of one app. Cheap to clone.
#[derive(Clone)]
pub struct LedgerHandle {
    store: std::sync::Arc<std::sync::Mutex<WorkspaceStore>>,
}

/// Identifies a recorded group, to clear it when the group ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerKey {
    pid: u32,
    started: String,
}

impl LedgerHandle {
    /// A ledger over `state`'s store.
    #[must_use]
    pub fn new(store: std::sync::Arc<std::sync::Mutex<WorkspaceStore>>) -> Self {
        Self { store }
    }

    /// Records the group led by `pid`; `None` when it already ended, the platform cannot read
    /// start times or the table cannot be written (the group then runs unrecorded).
    pub async fn record(
        &self,
        pid: Option<u32>,
        kind: LedgerKind,
        label: &str,
    ) -> Option<LedgerKey> {
        let (store, label) = (std::sync::Arc::clone(&self.store), label.to_owned());
        let pid = pid?;
        tokio::task::spawn_blocking(move || {
            let entry = entry_for(pid, kind, &label)?;
            store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ledger_record(&entry)
                .ok()?;
            Some(LedgerKey {
                pid,
                started: entry.started,
            })
        })
        .await
        .ok()
        .flatten()
    }

    /// Removes the record of a group that ended (best effort: a stale record is cleared at the
    /// next start).
    pub async fn clear(&self, key: LedgerKey) {
        let store = std::sync::Arc::clone(&self.store);
        let _ = tokio::task::spawn_blocking(move || {
            let _ = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ledger_remove(key.pid, &key.started);
        })
        .await;
    }
}

/// The ledger installed for this process (see [`install`]).
static INSTALLED: std::sync::Mutex<Option<LedgerHandle>> = std::sync::Mutex::new(None);

/// Installs `state`'s ledger for code that has no `AppState` at hand (the MCP stdio transport,
/// spawned on helper threads): call once at app start, before [`reap_leftover_processes`].
pub fn install(store: std::sync::Arc<std::sync::Mutex<WorkspaceStore>>) {
    *INSTALLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(LedgerHandle::new(store));
}

/// Records the group led by `pid` in the installed ledger (blocking: a short `ps` and one row);
/// `None` when no ledger is installed or the process cannot be identified.
#[must_use]
pub fn record_installed(pid: u32, kind: LedgerKind, label: &str) -> Option<LedgerKey> {
    let handle = INSTALLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()?;
    let entry = entry_for(pid, kind, label)?;
    handle
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ledger_record(&entry)
        .ok()?;
    Some(LedgerKey {
        pid,
        started: entry.started,
    })
}

/// Removes a record made by [`record_installed`] (blocking, best effort).
pub fn clear_installed(key: &LedgerKey) {
    let handle = INSTALLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(handle) = handle {
        let _ = handle
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ledger_remove(key.pid, &key.started);
    }
}

#[cfg(all(test, unix))]
mod tests;
