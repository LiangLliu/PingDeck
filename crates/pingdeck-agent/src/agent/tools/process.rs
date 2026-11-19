//! Background shell processes: the registry behind `shell` with `background`, `shell_output` and
//! `shell_kill`, and the `agent_process_list` rows.
//!
//! Single responsibility: own every background process group the agent started: cap their number
//! (4 per session, 8 app-wide), keep a bounded ring of each one's output, end them (process-group
//! `SIGTERM`, then `SIGKILL`) on request, at the 8-hour lifetime and for a deleted session, and
//! forget finished ones after a while. Spawning the command and building its sandbox invocation is
//! the shell tool's job; the registry receives the spawned [`Child`].

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

use super::super::process_ledger::{LedgerHandle, LedgerKind};
use super::shell_meta::{KILL_GRACE, TERM_GRACE, incomplete_tail_len, signal_group};
use pingdeck_base::error::AppError;
use pingdeck_llm::llm::runtime::now_ms;

/// Background processes per session, in the app, and the longest life of one (run limits table).
pub use crate::agent::limits::{
    PROCESS_LIFETIME as MAX_LIFETIME, PROCESSES_PER_CHAT as MAX_PER_SESSION,
    PROCESSES_TOTAL as MAX_TOTAL,
};
/// How long a finished process stays listed (its output stays readable).
pub const RETAIN_FINISHED: Duration = Duration::from_secs(30 * 60);
/// Output bytes kept per process; older bytes are dropped.
pub const RING_BYTES: usize = 256 * 1024;
/// Size of one pipe read.
const READ_CHUNK: usize = 8 * 1024;

/// What a background process is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    /// Still running.
    Running,
    /// Ended by itself (see `exit_code`).
    Exited,
    /// Ended by `shell_kill`, a session delete or app quit.
    Killed,
    /// Ended because it reached its lifetime.
    Expired,
}

/// One row of `agent_process_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundProcess {
    /// `proc_<hex>`.
    pub id: String,
    /// The session that started it.
    pub session_id: String,
    /// The command line.
    pub command: String,
    /// The OS process id (also the process group id).
    pub pid: Option<u32>,
    /// When it started, Unix milliseconds.
    pub started_ms: i64,
    /// When it ended, Unix milliseconds.
    pub ended_ms: Option<i64>,
    /// Running or how it ended.
    pub state: ProcessState,
    /// The exit code, when it exited by itself with one.
    pub exit_code: Option<i32>,
}

/// New output of a process since an offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    /// The new text (invalid UTF-8 replaced).
    pub text: String,
    /// The offset to pass next time.
    pub next_offset: u64,
    /// Bytes between the requested offset and the oldest byte still kept (dropped by the ring).
    pub dropped: u64,
    /// The process now.
    pub process: BackgroundProcess,
}

/// The limits of a [`ProcessRegistry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessLimits {
    /// Running processes per session.
    pub per_session: usize,
    /// Running processes in the app.
    pub total: usize,
    /// Longest life of one process.
    pub lifetime: Duration,
    /// How long a finished process stays listed.
    pub retain_finished: Duration,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            per_session: MAX_PER_SESSION,
            total: MAX_TOTAL,
            lifetime: MAX_LIFETIME,
            retain_finished: RETAIN_FINISHED,
        }
    }
}

/// The newest [`RING_BYTES`] bytes of a process's output and the offset of the oldest.
#[derive(Debug, Default)]
struct Ring {
    bytes: VecDeque<u8>,
    /// Offset (since the process started) of `bytes[0]`.
    base: u64,
}

impl Ring {
    fn push(&mut self, data: &[u8]) {
        self.bytes.extend(data);
        if self.bytes.len() > RING_BYTES {
            let excess = self.bytes.len() - RING_BYTES;
            self.bytes.drain(..excess);
            self.base += excess as u64;
        }
    }

    /// The bytes from `since` (clamped to what is kept) and how many bytes were dropped.
    fn read(&self, since: u64) -> (Vec<u8>, u64) {
        let dropped = self.base.saturating_sub(since);
        let start = usize::try_from(since.max(self.base) - self.base).unwrap_or(usize::MAX);
        (self.bytes.iter().skip(start).copied().collect(), dropped)
    }
}

/// One tracked process.
struct Entry {
    info: BackgroundProcess,
    ring: Ring,
    /// Woken on new output and on exit.
    changed: Arc<Notify>,
    /// `shell_kill` asks the supervising task to end the process group.
    kill: Arc<Notify>,
    /// When the process ended, for reaping.
    ended: Option<Instant>,
    /// [`ProcessRegistry::take_finished_unreported`] already handed this ended process out.
    reported: bool,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
}

/// Every background process of the app. Clones share the same processes.
#[derive(Clone, Default)]
pub struct ProcessRegistry {
    inner: Arc<Mutex<Inner>>,
    limits: ProcessLimits,
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A fresh `proc_<hex>` id.
fn new_id() -> String {
    pingdeck_base::id::prefixed_hex::<6>("proc")
}

impl ProcessRegistry {
    /// A registry with the default limits.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with custom limits.
    #[must_use]
    pub fn with_limits(limits: ProcessLimits) -> Self {
        Self {
            inner: Arc::default(),
            limits,
        }
    }

    /// Starts tracking the process `spawn` creates, unless a limit is reached. With a `ledger`
    /// the process group is recorded while it runs, so a crash cannot leave it behind unnoticed.
    ///
    /// The limit check, the spawn and the registration happen under one lock, so racing starts
    /// cannot exceed a cap. `spawn` must not block.
    ///
    /// # Errors
    /// [`AppError::Conflict`] when the session or the app has too many running processes,
    /// [`AppError::Io`]-style errors from `spawn` and a missing output pipe.
    pub fn start(
        &self,
        session_id: &str,
        command: &str,
        ledger: Option<LedgerHandle>,
        spawn: impl FnOnce() -> io::Result<Child>,
    ) -> Result<BackgroundProcess, AppError> {
        let mut inner = lock(&self.inner);
        self.reap_locked(&mut inner);
        let running = inner
            .entries
            .values()
            .filter(|entry| entry.info.state == ProcessState::Running);
        let (mut mine, mut all) = (0, 0);
        for entry in running {
            all += 1;
            mine += usize::from(entry.info.session_id == session_id);
        }
        if mine >= self.limits.per_session {
            return Err(AppError::conflict(format!(
                "This chat already has {} background processes running; stop one with shell_kill first",
                self.limits.per_session
            )));
        }
        if all >= self.limits.total {
            return Err(AppError::conflict(format!(
                "{} background processes are already running in PingDeck; stop one with shell_kill first",
                self.limits.total
            )));
        }
        let mut child = spawn()
            .map_err(|error| AppError::io(format!("failed to start the command: {error}")))?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Err(AppError::internal("failed to capture the command's output"));
        };
        let info = BackgroundProcess {
            id: new_id(),
            session_id: session_id.to_owned(),
            command: command.to_owned(),
            pid: child.id(),
            started_ms: now_ms(),
            ended_ms: None,
            state: ProcessState::Running,
            exit_code: None,
        };
        let changed = Arc::new(Notify::new());
        let kill = Arc::new(Notify::new());
        inner.entries.insert(
            info.id.clone(),
            Entry {
                info: info.clone(),
                ring: Ring::default(),
                changed: Arc::clone(&changed),
                kill: Arc::clone(&kill),
                ended: None,
                reported: false,
            },
        );
        drop(inner);
        tokio::spawn(supervise(
            Arc::clone(&self.inner),
            info.id.clone(),
            child,
            stdout,
            stderr,
            changed,
            kill,
            self.limits.lifetime,
            ledger,
        ));
        Ok(info)
    }

    /// Forgets finished processes older than the retention time.
    fn reap_locked(&self, inner: &mut Inner) {
        let keep = self.limits.retain_finished;
        inner
            .entries
            .retain(|_, entry| entry.ended.is_none_or(|ended| ended.elapsed() < keep));
    }

    /// Forgets finished processes older than the retention time; returns how many were removed.
    pub fn reap(&self) -> usize {
        let mut inner = lock(&self.inner);
        let before = inner.entries.len();
        self.reap_locked(&mut inner);
        before - inner.entries.len()
    }

    /// The processes of `session_id` that ended since the last call, each returned once, oldest
    /// first (a background exit is told to the model at the start of its next run).
    pub fn take_finished_unreported(&self, session_id: &str) -> Vec<BackgroundProcess> {
        let mut inner = lock(&self.inner);
        let mut rows: Vec<BackgroundProcess> = inner
            .entries
            .values_mut()
            .filter(|entry| {
                entry.info.session_id == session_id
                    && entry.info.state != ProcessState::Running
                    && !entry.reported
            })
            .map(|entry| {
                entry.reported = true;
                entry.info.clone()
            })
            .collect();
        rows.sort_by(|a, b| (a.started_ms, &a.id).cmp(&(b.started_ms, &b.id)));
        rows
    }

    /// Notes that the model already saw process `id` end (`shell_output` or `shell_kill` said
    /// so), so [`Self::take_finished_unreported`] does not tell it again (AI-loop-11).
    pub fn mark_reported(&self, id: &str) {
        if let Some(entry) = lock(&self.inner).entries.get_mut(id)
            && entry.info.state != ProcessState::Running
        {
            entry.reported = true;
        }
    }

    /// The processes of `session_id` (`None`: every session), oldest first.
    #[must_use]
    pub fn list(&self, session_id: Option<&str>) -> Vec<BackgroundProcess> {
        let mut inner = lock(&self.inner);
        self.reap_locked(&mut inner);
        let mut rows: Vec<_> = inner
            .entries
            .values()
            .filter(|entry| session_id.is_none_or(|session| entry.info.session_id == session))
            .map(|entry| entry.info.clone())
            .collect();
        rows.sort_by(|a, b| (a.started_ms, &a.id).cmp(&(b.started_ms, &b.id)));
        rows
    }

    /// The process `id`, when it belongs to `session_id` (`None` skips the check).
    fn find<'a>(
        inner: &'a Inner,
        session_id: Option<&str>,
        id: &str,
    ) -> Result<&'a Entry, AppError> {
        inner
            .entries
            .get(id)
            .filter(|entry| session_id.is_none_or(|session| entry.info.session_id == session))
            .ok_or_else(|| AppError::not_found("No such process"))
    }

    /// New output of process `id` since byte offset `since`; with `wait` non-zero, blocks up to
    /// that long while nothing new has arrived and the process still runs.
    ///
    /// # Errors
    /// [`AppError::NotFound`] (`No such process`) for an unknown id or another session's process.
    pub async fn output(
        &self,
        session_id: Option<&str>,
        id: &str,
        since: u64,
        wait: Duration,
    ) -> Result<ProcessOutput, AppError> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notify = {
                let inner = lock(&self.inner);
                Arc::clone(&Self::find(&inner, session_id, id)?.changed)
            };
            // Registered before the check, so a change between check and await is not lost.
            let mut notified = std::pin::pin!(notify.notified());
            notified.as_mut().enable();
            {
                let inner = lock(&self.inner);
                let entry = Self::find(&inner, session_id, id)?;
                let (bytes, dropped) = entry.ring.read(since);
                let running = entry.info.state == ProcessState::Running;
                // Hold back a character the next read completes.
                let keep = if running {
                    incomplete_tail_len(&bytes)
                } else {
                    0
                };
                let used = &bytes[..bytes.len() - keep];
                if !used.is_empty() || !running || tokio::time::Instant::now() >= deadline {
                    return Ok(ProcessOutput {
                        text: String::from_utf8_lossy(used).into_owned(),
                        next_offset: since.max(entry.ring.base) + used.len() as u64,
                        dropped,
                        process: entry.info.clone(),
                    });
                }
            }
            let _ = timeout(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                notified,
            )
            .await;
        }
    }

    /// Ends process `id` (`SIGTERM` to its group, `SIGKILL` after 500 ms) and waits until it is
    /// gone. Killing a finished process is a no-op that returns its row.
    ///
    /// # Errors
    /// [`AppError::NotFound`] (`No such process`) for an unknown id or another session's process.
    pub async fn kill(
        &self,
        session_id: Option<&str>,
        id: &str,
    ) -> Result<BackgroundProcess, AppError> {
        let (kill, changed) = {
            let inner = lock(&self.inner);
            let entry = Self::find(&inner, session_id, id)?;
            (Arc::clone(&entry.kill), Arc::clone(&entry.changed))
        };
        loop {
            let mut notified = std::pin::pin!(changed.notified());
            notified.as_mut().enable();
            {
                let inner = lock(&self.inner);
                let entry = Self::find(&inner, session_id, id)?;
                if entry.info.state != ProcessState::Running {
                    return Ok(entry.info.clone());
                }
            }
            kill.notify_one();
            // The supervisor needs at most TERM_GRACE + KILL_GRACE; the margin covers scheduling.
            let _ = timeout(TERM_GRACE + KILL_GRACE + Duration::from_secs(1), notified).await;
        }
    }

    /// Ends every running process of `session_id` and forgets all its processes (chat delete).
    pub async fn kill_session(&self, session_id: &str) {
        self.kill_matching(Some(session_id)).await;
    }

    /// Ends every running process of the app (quit).
    pub async fn kill_all(&self) {
        self.kill_matching(None).await;
    }

    async fn kill_matching(&self, session_id: Option<&str>) {
        let ids: Vec<String> = self
            .list(session_id)
            .into_iter()
            .map(|process| process.id)
            .collect();
        let kills = ids.iter().map(|id| self.kill(None, id));
        futures_util::future::join_all(kills).await;
        let mut inner = lock(&self.inner);
        inner
            .entries
            .retain(|_, entry| session_id.is_some_and(|session| entry.info.session_id != session));
    }
}

/// Reads one pipe into the ring until it closes.
async fn pump<R: AsyncRead + Unpin>(inner: &Mutex<Inner>, id: &str, changed: &Notify, mut pipe: R) {
    let mut buf = [0_u8; READ_CHUNK];
    while let Ok(len) = pipe.read(&mut buf).await {
        if len == 0 {
            break;
        }
        if let Some(entry) = lock(inner).entries.get_mut(id) {
            entry.ring.push(&buf[..len]);
        }
        changed.notify_waiters();
    }
}

/// How the supervised process ended.
enum End {
    Exited(io::Result<std::process::ExitStatus>),
    Killed,
    Expired,
}

/// Waits for the process to end (or be killed, or expire), then records the outcome.
#[allow(clippy::too_many_arguments)]
async fn supervise(
    inner: Arc<Mutex<Inner>>,
    id: String,
    mut child: Child,
    stdout: impl AsyncRead + Unpin,
    stderr: impl AsyncRead + Unpin,
    changed: Arc<Notify>,
    kill: Arc<Notify>,
    lifetime: Duration,
    ledger: Option<LedgerHandle>,
) {
    let pid = child.id();
    let ledger_key = match &ledger {
        Some(ledger) => ledger.record(pid, LedgerKind::Tool, &id).await,
        None => None,
    };
    let pumps = async {
        tokio::join!(
            pump(&inner, &id, &changed, stdout),
            pump(&inner, &id, &changed, stderr)
        );
    };
    let mut pumps = std::pin::pin!(pumps);
    let mut pumps_done = false;
    let end = {
        let wait = async {
            let status = child.wait().await;
            End::Exited(status)
        };
        tokio::select! {
            end = wait => end,
            () = kill.notified() => End::Killed,
            () = sleep(lifetime) => End::Expired,
            () = &mut pumps, if !pumps_done => {
                // Both pipes closed: the process (or its last holder) is gone.
                pumps_done = true;
                End::Exited(child.wait().await)
            }
        }
    };
    // A leftover child may hold the pipes open; give the output a moment, then end the group.
    if !matches!(end, End::Exited(_))
        || (!pumps_done
            && timeout(Duration::from_millis(200), &mut pumps)
                .await
                .is_err())
    {
        end_group(pid, &mut child, &mut pumps, &mut pumps_done).await;
    } else {
        // The leader exited and the output is closed: a member that closed its own output (a
        // daemon with redirected stdio) would outlive the process outside the caps and the ledger.
        signal_group(pid, true).await;
    }
    if let (Some(ledger), Some(key)) = (&ledger, ledger_key) {
        ledger.clear(key).await;
    }
    let ended = Instant::now();
    if let Some(entry) = lock(&inner).entries.get_mut(&id) {
        entry.info.ended_ms = Some(now_ms());
        entry.ended = Some(ended);
        match end {
            End::Exited(Ok(status)) => {
                entry.info.state = ProcessState::Exited;
                entry.info.exit_code = status.code();
            }
            End::Exited(Err(_)) => entry.info.state = ProcessState::Exited,
            End::Killed => entry.info.state = ProcessState::Killed,
            End::Expired => entry.info.state = ProcessState::Expired,
        }
    }
    changed.notify_waiters();
}

/// `SIGTERM`, then `SIGKILL` after [`TERM_GRACE`], to the process group of `pid`.
async fn end_group<P: Future<Output = ()> + Unpin>(
    pid: Option<u32>,
    child: &mut Child,
    pumps: &mut P,
    pumps_done: &mut bool,
) {
    signal_group(pid, false).await;
    settle(child, pumps, pumps_done, TERM_GRACE).await;
    signal_group(pid, true).await;
    let _ = child.start_kill();
    settle(child, pumps, pumps_done, KILL_GRACE).await;
}

/// Waits up to `limit` for the child to be reaped and the output pipes to close.
async fn settle<P: Future<Output = ()> + Unpin>(
    child: &mut Child,
    pumps: &mut P,
    pumps_done: &mut bool,
    limit: Duration,
) {
    let wait_child = async {
        let _ = child.wait().await;
    };
    let wait_pumps = async {
        if !*pumps_done {
            pumps.await;
            *pumps_done = true;
        }
    };
    let _ = timeout(limit, async { tokio::join!(wait_child, wait_pumps) }).await;
}

#[cfg(all(test, unix))]
mod tests;
