//! The `shell` tool family: `shell`, `shell_output` and `shell_kill`.
//!
//! Single responsibility: validate the workspace, spawn the (sandbox-wrapped) command with a
//! scrubbed environment in its own process group, stream its output, enforce the timeout and the
//! run's cancellation by killing the whole group, and shape the result the model reads: at most
//! [`crate::agent::context::MAX_RESULT_BYTES`] of output (head and tail kept, the complete output spilled to
//! disk, see [`spill`]). With `background` the process is handed to the
//! [`ProcessRegistry`](crate::agent::tools::process::ProcessRegistry) and the call returns at once. Building the
//! sandbox invocation lives in `agent::sandbox`, the environment in [`env`], approval decisions in
//! `agent::permissions`.

#[cfg(test)]
mod tests;

mod ansi;
mod env;
mod spill;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::time::{sleep, sleep_until, timeout};

use crate::agent::tools::process::{BackgroundProcess, ProcessState};
pub use crate::agent::tools::shell_meta::{
    KILL_GRACE, SHELL, SHELL_KILL, SHELL_OUTPUT, TERM_GRACE, incomplete_tail_len, signal_group,
    valid_call_id,
};
use crate::agent::tools::{
    AgentTool, CallInspection, OutputSink, OutputStream, ToolApprovalPreview, ToolClass,
    ToolContext, ToolEvent, ToolEventSink, ToolOutput, ToolRegistry, ToolSource, ToolSpec,
};
use crate::cancel::CancelToken;
use crate::error::AppError;
use crate::settings::Settings;

use crate::agent::process_ledger::{LedgerHandle, LedgerKey, LedgerKind};
use crate::agent::sandbox::{
    SandboxKind, SandboxSpec, SandboxState, sandbox_apply_failed, test_sandbox, user_shell,
    wrap_command,
};
use ansi::AnsiStripper;
pub use env::{
    remove_session_temp_dir, session_temp_dir, sweep_session_temp_dirs, warm_login_path,
};
use env::{resolved_path, scrub_env};
pub use spill::read_tool_output;
use spill::{Capture, TOOL_OUTPUT_FILE_BYTES, cap_text};

/// Longest command line `shell` accepts, in characters.
const MAX_COMMAND_CHARS: usize = 8000;
/// Longest `wait_ms` of `shell_output`.
const MAX_WAIT_MS: u64 = 30_000;
/// A stream flushes to the live sink at least this often while output keeps coming.
const FLUSH_INTERVAL: Duration = Duration::from_millis(25);
/// A stream flushes at once when this many bytes are waiting.
const FLUSH_BYTES: usize = 8 * 1024;
/// Size of one pipe read.
const READ_CHUNK: usize = 8 * 1024;
/// How long output may still arrive after the process exited (a background child can hold the
/// pipe open); past it the leftover process group is killed.
const EXIT_DRAIN_GRACE: Duration = Duration::from_millis(200);
/// How long `SIGTERM` gets before `SIGKILL`.
/// How long the process group gets to disappear after `SIGKILL`.
/// What a failed command prints when the sandbox has no network (curl, git, npm, pip, cargo).
const NETWORK_FAILURES: [&str; 6] = [
    "Could not resolve host",
    "Temporary failure in name resolution",
    "Name or service not known",
    "nodename nor servname provided",
    "Network is unreachable",
    "Failed to connect",
];
/// Appended to the result of a command that failed for lack of network.
const NETWORK_HINT: &str =
    "Network is disabled in the sandbox. Ask the user to allow this command once with network.";
/// Appended to the result of a command whose sandbox denied a path outside the folder.
const SANDBOX_HINT: &str = "This command ran in the PingDeck sandbox: it may write only inside the folder and temp folders and may not read some credential folders.";
/// What the model reads when the OS sandbox could not be applied at all.
const SANDBOX_APPLY_FAILED: &str = "The command did not run: the OS sandbox could not be applied here (PingDeck itself runs inside another sandbox). Ask the user to run it without a sandbox.";
/// What the model reads when a call that was cleared to run in the sandbox finds none at run
/// time (the state changed after the permission decision): nothing ran, a retry asks first.
const SANDBOX_CHANGED: &str = "The command did not run: the OS sandbox became unavailable after it was approved. Try again; you will be asked first.";
/// Bytes of standard error kept to recognise a failed sandbox.
const STDERR_KEPT: usize = 2048;

/// Why a command with a secret in a URL query is refused (no card, in every mode). It
/// never echoes the value.
const SECRET_BLOCK: &str = "Blocked: the input contains a secret value.";

/// Whether the command line holds a URL whose query string contains a known secret value (or a
/// credential-shaped value): such a command would send the secret to a host of the model's choice.
fn secret_in_url_query(command: &str, needles: &[String]) -> bool {
    command.match_indices("://").any(|(at, _)| {
        let rest = &command[at + 3..];
        let end = rest
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '\'' | '"' | '`' | ')' | '<' | '>' | '|' | ';')
            })
            .unwrap_or(rest.len());
        rest[..end].split_once('?').is_some_and(|(_, query)| {
            let query = query.split('#').next().unwrap_or(query);
            crate::secrets::redact_secrets(query, needles) != query
        })
    })
}

/// The limits one shell call runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShellLimits {
    /// The longest a command may run.
    timeout: Duration,
    /// Bytes of output the model may see.
    output_cap: usize,
}

/// The limits the settings impose.
fn limits(settings: &Settings) -> ShellLimits {
    let agent = &settings.ai.agent;
    ShellLimits {
        timeout: Duration::from_secs(u64::from(agent.shell_timeout_s)),
        output_cap: usize::try_from(TOOL_OUTPUT_FILE_BYTES).unwrap_or(usize::MAX),
    }
}

/// The timeout a call runs with: its own request, at least a second, never above the setting.
fn effective_timeout(requested: Option<u64>, max: Duration) -> Duration {
    requested.map_or(max, |secs| Duration::from_secs(secs.max(1)).min(max))
}

/// The arguments of a `shell` call; unknown keys (such as `description`, which only the approval
/// card shows) are ignored.
#[derive(Debug, Deserialize)]
struct ShellArgs {
    /// The command line.
    command: String,
    /// Requested timeout in seconds.
    timeout_s: Option<u64>,
    /// Start the command in the background and return at once.
    #[serde(default, alias = "run_in_background")]
    background: bool,
}

impl ShellArgs {
    /// Parses and validates the model's arguments.
    fn parse(arguments: &Value) -> Result<Self, AppError> {
        let args: Self = serde_json::from_value(arguments.clone())
            .map_err(|error| AppError::validation(format!("invalid shell arguments: {error}")))?;
        if args.command.trim().is_empty() {
            return Err(AppError::validation("shell needs a non-empty `command`"));
        }
        if args.command.chars().count() > MAX_COMMAND_CHARS {
            return Err(AppError::validation(format!(
                "`command` is longer than {MAX_COMMAND_CHARS} characters; put the script in a file with write_file and run that"
            )));
        }
        Ok(args)
    }
}

/// The arguments of `shell_output`.
#[derive(Debug, Deserialize)]
struct OutputArgs {
    /// The `proc_…` id `shell` returned.
    process_id: String,
    /// Byte offset the previous call ended at.
    #[serde(default)]
    since: u64,
    /// Milliseconds to wait for new output (0 to 30,000).
    #[serde(default)]
    wait_ms: u64,
}

impl OutputArgs {
    fn parse(arguments: &Value) -> Result<Self, AppError> {
        let args: Self = serde_json::from_value(arguments.clone()).map_err(|error| {
            AppError::validation(format!("invalid shell_output arguments: {error}"))
        })?;
        if args.wait_ms > MAX_WAIT_MS {
            return Err(AppError::validation(format!(
                "`wait_ms` must be at most {MAX_WAIT_MS}"
            )));
        }
        Ok(args)
    }
}

/// The arguments of `shell_kill`.
#[derive(Debug, Deserialize)]
struct KillArgs {
    /// The `proc_…` id `shell` returned.
    process_id: String,
}

/// Canonical host locations a sandboxed command must not reach (or may use as scratch).
#[derive(Debug, Clone, Default)]
struct HostPaths {
    /// The user's home directory.
    home: Option<PathBuf>,
    /// PingDeck's data root.
    data_root: Option<PathBuf>,
    /// The session's temp directory (also the command's `TMPDIR`).
    tmp_dir: Option<PathBuf>,
}

impl HostPaths {
    /// The locations of this process and the app behind `ctx`; with `with_tmp` the session's temp
    /// directory is created and included.
    fn of(ctx: &ToolContext<'_>, with_tmp: bool) -> Result<Self, AppError> {
        let home = ["HOME", "USERPROFILE"]
            .iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .find(|path| path.is_absolute());
        Ok(Self {
            home: home.as_deref().map(canonical_or_raw),
            data_root: ctx.data_root.map(canonical_or_raw),
            tmp_dir: with_tmp
                .then(|| session_temp_dir(ctx.session_id))
                .transpose()?,
        })
    }
}

/// The canonical form of `path`, or `path` itself when it cannot be resolved (it does not exist).
fn canonical_or_raw(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The canonical workspace a command may run in.
///
/// # Errors
/// [`AppError::Validation`] when the session has no workspace, it is not an existing directory,
/// or it is the filesystem root, the home directory or one of its ancestors, PingDeck's data root
/// (or inside it or above it), or contains or sits inside a hidden credential location: the
/// sandbox lets a command write the whole workspace, so such a folder would hand out the files
/// the read-deny block hides.
fn validate_workspace(workspace: Option<&Path>, host: &HostPaths) -> Result<PathBuf, AppError> {
    let workspace = workspace.ok_or_else(|| {
        AppError::validation("shell needs a workspace folder: bind a folder to this session first")
    })?;
    let canonical = std::fs::canonicalize(workspace).map_err(|error| {
        AppError::validation(format!(
            "the workspace folder {} is not accessible: {error}",
            workspace.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(AppError::validation(format!(
            "the workspace {} is not a directory",
            canonical.display()
        )));
    }
    crate::agent::tools::fs::check_workspace(
        &canonical,
        host.home.as_deref(),
        host.data_root.as_deref(),
    )?;
    Ok(canonical)
}
/// Buffers one output stream and decides when its text goes to the live sink: at once for the
/// first bytes, then at most every [`FLUSH_INTERVAL`], or as soon as [`FLUSH_BYTES`] wait. A
/// character split across reads is held back until it is whole.
#[derive(Debug)]
struct Lane {
    /// Bytes read but not yet flushed.
    pending: Vec<u8>,
    /// When the lane last flushed.
    last_flush: Option<Instant>,
}

impl Lane {
    /// An empty lane.
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            last_flush: None,
        }
    }

    /// Bytes that form whole characters.
    fn flushable(&self) -> usize {
        self.pending.len() - incomplete_tail_len(&self.pending)
    }

    /// Adds `data` read at `now`; returns the text to emit now, if any.
    fn push(&mut self, data: &[u8], now: Instant) -> Option<String> {
        self.pending.extend_from_slice(data);
        self.tick(now)
    }

    /// When pending bytes become due, if any wait.
    fn due(&self) -> Option<Instant> {
        (self.flushable() > 0).then(|| {
            self.last_flush
                .map_or_else(Instant::now, |last| last + FLUSH_INTERVAL)
        })
    }

    /// Returns the text to emit at `now` when the lane is due.
    fn tick(&mut self, now: Instant) -> Option<String> {
        let flushable = self.flushable();
        let ready = flushable >= FLUSH_BYTES
            || (flushable > 0
                && self
                    .last_flush
                    .is_none_or(|last| last + FLUSH_INTERVAL <= now));
        ready.then(|| self.take(now, false)).flatten()
    }

    /// Returns everything left (a trailing broken character becomes U+FFFD).
    fn finish(&mut self, now: Instant) -> Option<String> {
        self.take(now, true)
    }

    /// Removes the flushable bytes (all of them when `all`) as text.
    fn take(&mut self, now: Instant, all: bool) -> Option<String> {
        let len = if all {
            self.pending.len()
        } else {
            self.flushable()
        };
        if len == 0 {
            return None;
        }
        let text = String::from_utf8_lossy(&self.pending[..len]).into_owned();
        self.pending.drain(..len);
        self.last_flush = Some(now);
        Some(text)
    }
}

/// Reads both pipes concurrently, feeding the live sink and the [`Capture`].
struct Pump<'a> {
    /// Standard output.
    out: Lane,
    /// Standard error.
    err: Lane,
    /// What the model will see.
    capture: Capture,
    /// Where live output goes.
    sink: Option<OutputSink<'a>>,
    /// The command printed something on standard output.
    stdout_seen: bool,
    /// The first [`STDERR_KEPT`] bytes of standard error, to recognise a failed sandbox.
    stderr_head: String,
    /// Removes terminal escape sequences from what the model reads, per stream.
    strip: [AnsiStripper; 2],
}

impl<'a> Pump<'a> {
    /// A pump that reports to `sink` and records into `capture`.
    fn new(capture: Capture, sink: Option<OutputSink<'a>>) -> Self {
        Self {
            out: Lane::new(),
            err: Lane::new(),
            capture,
            sink,
            stdout_seen: false,
            stderr_head: String::new(),
            strip: [AnsiStripper::default(), AnsiStripper::default()],
        }
    }

    /// Records and forwards one flushed fragment.
    fn deliver(&mut self, stream: OutputStream, text: Option<String>) {
        if let Some(text) = text {
            match stream {
                OutputStream::Stdout => self.stdout_seen = true,
                OutputStream::Stderr => {
                    let room = STDERR_KEPT.saturating_sub(self.stderr_head.len());
                    self.stderr_head.extend(text.chars().scan(0, |used, c| {
                        *used += c.len_utf8();
                        (*used <= room).then_some(c)
                    }));
                }
            }
            let lane = usize::from(stream == OutputStream::Stderr);
            let plain = self.strip[lane].push(&text);
            self.capture.push(&plain);
            if let Some(sink) = self.sink {
                sink(stream, &text);
            }
        }
    }

    /// The lane of `stream`.
    fn lane(&mut self, stream: OutputStream) -> &mut Lane {
        match stream {
            OutputStream::Stdout => &mut self.out,
            OutputStream::Stderr => &mut self.err,
        }
    }

    /// Adds bytes read from `stream`.
    fn feed(&mut self, stream: OutputStream, data: &[u8]) {
        let text = self.lane(stream).push(data, Instant::now());
        self.deliver(stream, text);
    }

    /// Flushes what is due.
    fn tick(&mut self) {
        for stream in [OutputStream::Stdout, OutputStream::Stderr] {
            let text = self.lane(stream).tick(Instant::now());
            self.deliver(stream, text);
        }
    }

    /// Flushes everything still buffered in `stream`.
    fn finish_lane(&mut self, stream: OutputStream) {
        let text = self.lane(stream).finish(Instant::now());
        self.deliver(stream, text);
    }

    /// Reads until both pipes reach end of file. Cancel-safe: bytes read but not yet flushed stay
    /// in the lanes for [`Pump::finish_lane`].
    async fn run(&mut self, stdout: ChildStdout, stderr: ChildStderr) {
        let (mut out_pipe, mut err_pipe) = (Some(stdout), Some(stderr));
        let (mut out_buf, mut err_buf) = ([0_u8; READ_CHUNK], [0_u8; READ_CHUNK]);
        while out_pipe.is_some() || err_pipe.is_some() {
            let due = [self.out.due(), self.err.due()].into_iter().flatten().min();
            tokio::select! {
                read = read_chunk(&mut out_pipe, &mut out_buf) => match read {
                    Ok(len) if len > 0 => self.feed(OutputStream::Stdout, &out_buf[..len]),
                    _ => {
                        out_pipe = None;
                        self.finish_lane(OutputStream::Stdout);
                    }
                },
                read = read_chunk(&mut err_pipe, &mut err_buf) => match read {
                    Ok(len) if len > 0 => self.feed(OutputStream::Stderr, &err_buf[..len]),
                    _ => {
                        err_pipe = None;
                        self.finish_lane(OutputStream::Stderr);
                    }
                },
                () = sleep_until(due.map_or_else(tokio::time::Instant::now, tokio::time::Instant::from_std)),
                    if due.is_some() => self.tick(),
            }
        }
    }
}

/// Reads from `pipe`; never completes once the pipe was closed (`None`).
async fn read_chunk<R: AsyncRead + Unpin>(
    pipe: &mut Option<R>,
    buf: &mut [u8],
) -> io::Result<usize> {
    match pipe {
        Some(pipe) => pipe.read(buf).await,
        None => std::future::pending().await,
    }
}

/// How a command's run ended.
enum Ending {
    /// The process exited (its status, or the error that waiting produced).
    Exited(io::Result<ExitStatus>),
    /// The timeout fired; the process group was killed.
    TimedOut,
    /// The run was cancelled; the process group was killed.
    Cancelled,
}
/// Waits up to `limit` for the child to be reaped and the pump to reach end of file.
async fn settle<P: Future<Output = ()> + Unpin>(
    child: &mut Child,
    run: &mut P,
    pump_done: &mut bool,
    limit: Duration,
) {
    let wait_child = async {
        let _ = child.wait().await;
    };
    let wait_pump = async {
        if !*pump_done {
            run.await;
            *pump_done = true;
        }
    };
    let _ = timeout(limit, async { tokio::join!(wait_child, wait_pump) }).await;
}

/// Kills the whole process group: `SIGTERM`, then `SIGKILL` after at most [`TERM_GRACE`]; returns
/// within about [`TERM_GRACE`] + [`KILL_GRACE`] even when something survives.
async fn terminate<P: Future<Output = ()> + Unpin>(
    pid: Option<u32>,
    child: &mut Child,
    run: &mut P,
    mut pump_done: bool,
) {
    signal_group(pid, false).await;
    settle(child, run, &mut pump_done, TERM_GRACE).await;
    signal_group(pid, true).await;
    let _ = child.start_kill();
    settle(child, run, &mut pump_done, KILL_GRACE).await;
}

/// Everything [`run_command`] needs besides the cancel token and the sinks.
struct RunRequest<'a> {
    /// The command line.
    command: &'a str,
    /// The validated canonical working directory.
    workspace: &'a Path,
    /// The sandbox tier to run under.
    kind: SandboxKind,
    /// The longest the command may run.
    timeout: Duration,
    /// Whether the sandbox may open the network.
    allow_network: bool,
    /// Full access: the sandbox also lets the command write the toolchain caches.
    full_access: bool,
    /// Locations the sandbox hides or offers as scratch.
    host: &'a HostPaths,
    /// The scrubbed environment.
    env: &'a [(OsString, OsString)],
    /// Bytes of output the model may see.
    output_cap: usize,
    /// Records the process group while it runs (`None` in tests that need no ledger).
    ledger: Option<LedgerHandle>,
    /// Names the call in the ledger.
    label: &'a str,
}

impl RunRequest<'_> {
    /// The command to spawn: sandbox-wrapped, scrubbed, own process group, piped output.
    fn build(&self) -> Result<(Command, String), AppError> {
        let shell = user_shell();
        let spec = SandboxSpec {
            workspace: self.workspace,
            data_root: self.host.data_root.as_deref(),
            tmp_dir: self.host.tmp_dir.as_deref(),
            home: self.host.home.as_deref(),
            allow_network: self.allow_network,
            full_access: self.full_access,
            shell: &shell,
            command: self.command,
        };
        let sandboxed = wrap_command(self.kind, &spec)?;
        let mut command = Command::new(&sandboxed.program);
        // `cmd.exe` parses its own command line: pass the command verbatim, not re-quoted.
        #[cfg(windows)]
        match sandboxed.args.as_slice() {
            [flag, line] => command.arg(flag).raw_arg(line),
            args => command.args(args),
        };
        #[cfg(not(windows))]
        command.args(&sandboxed.args);
        command
            .current_dir(self.workspace)
            .env_clear()
            .envs(self.env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Own process group: the timeout and a cancel kill the command with everything it started.
        #[cfg(unix)]
        command.process_group(0);
        Ok((command, sandboxed.program))
    }

    /// Records the spawned group in the ledger.
    async fn record(&self, pid: Option<u32>) -> Option<LedgerKey> {
        self.ledger
            .as_ref()?
            .record(pid, LedgerKind::Tool, self.label)
            .await
    }
}

/// Whether a failed command's output shows a sandbox denial that is worth explaining: it says
/// `Operation not permitted` about a path outside the folder and the session temp directory
/// (inside them the sandbox is not the cause).
fn sandbox_denied_outside(output: &str, workspace: &Path, tmp_dir: Option<&Path>) -> bool {
    output
        .lines()
        .filter(|line| {
            line.to_ascii_lowercase()
                .contains("operation not permitted")
        })
        .map(|line| {
            // Strip a leading `<argv0>: ` diagnostic prefix (such as `/bin/bash: ` or
            // `/bin/zsh:1: `) when another colon follows, so the shell/binary path itself
            // is not mistaken for a denied target outside the workspace.
            match line.split_once(": ") {
                Some((_, tail)) if tail.contains(':') => tail,
                _ => line,
            }
        })
        .flat_map(|line| line.split_whitespace())
        .map(|word| word.trim_matches(|c: char| matches!(c, '\'' | '"' | ':' | ',' | '(' | ')')))
        .filter(|word| word.starts_with('/'))
        .any(|path| {
            let path = Path::new(path);
            !path.starts_with(workspace) && !tmp_dir.is_some_and(|tmp| path.starts_with(tmp))
        })
}

/// Runs the command and shapes the result: exit code, interleaved output (truncated), and a note
/// when the timeout or a cancellation killed it.
async fn run_command(
    request: &RunRequest<'_>,
    cancel: &CancelToken,
    sink: Option<OutputSink<'_>>,
    events: Option<ToolEventSink<'_>>,
) -> ToolOutput {
    let (mut command, program) = match request.build() {
        Ok(built) => built,
        Err(error) => return ToolOutput::error(error.message()),
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ToolOutput::error(format!("failed to start `{program}`: {error}"));
        }
    };
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return ToolOutput::error("failed to capture the command's output");
    };
    let pid = child.id();
    let ledger_key = request.record(pid).await;
    let mut pump = Pump::new(Capture::new(request.output_cap), sink);
    let ending = {
        let mut run = pin!(pump.run(stdout, stderr));
        let mut pump_done = false;
        let ending = tokio::select! {
            biased;
            () = cancel.cancelled() => Ending::Cancelled,
            () = sleep(request.timeout) => Ending::TimedOut,
            status = async {
                let status = tokio::select! {
                    status = child.wait() => status,
                    () = &mut run => {
                        pump_done = true;
                        child.wait().await
                    }
                };
                if !pump_done {
                    pump_done = timeout(EXIT_DRAIN_GRACE, &mut run).await.is_ok();
                }
                status
            } => Ending::Exited(status),
        };
        // Output still open after the process exited means a leftover child holds the pipe. A
        // leftover that closed its output (a daemon with redirected stdio) would outlive the call
        // outside the process caps and the ledger, so a clean exit ends the group too: a command
        // that must keep running uses `background`.
        if !matches!(ending, Ending::Exited(_)) || !pump_done {
            terminate(pid, &mut child, &mut run, pump_done).await;
        } else {
            signal_group(pid, true).await;
        }
        ending
    };
    if let (Some(ledger), Some(key)) = (&request.ledger, ledger_key) {
        ledger.clear(key).await;
    }
    pump.finish_lane(OutputStream::Stdout);
    pump.finish_lane(OutputStream::Stderr);
    if let Ending::Exited(Ok(status)) = &ending
        && sandbox_apply_failed(
            request.kind,
            status.code(),
            !pump.stdout_seen,
            &pump.stderr_head,
        )
        // A command controls its own exit code and output: only the probe may declare the
        // sandbox unusable, and then later calls ask first instead of failing again.
        && tokio::task::spawn_blocking(test_sandbox)
            .await
            .is_ok_and(|status| status.state != SandboxState::Available)
    {
        return ToolOutput::error(SANDBOX_APPLY_FAILED);
    }
    if pump.capture.dropped()
        && let Some(events) = events
    {
        events(ToolEvent::Notice("Output truncated".to_owned()));
    }
    let output = pump.capture.render();
    let sandboxed = request.kind != SandboxKind::None;
    let denied = sandboxed
        && sandbox_denied_outside(&output, request.workspace, request.host.tmp_dir.as_deref());
    shape_output(
        &output,
        &ending,
        request.timeout,
        sandboxed && !request.allow_network,
        denied,
    )
}

/// The model-facing result for a finished run. With `network_off`, a failure whose output shows a
/// resolve or connect error ends with [`NETWORK_HINT`]; with `sandbox_denied`, a failure ends with
/// [`SANDBOX_HINT`].
fn shape_output(
    output: &str,
    ending: &Ending,
    limit: Duration,
    network_off: bool,
    sandbox_denied: bool,
) -> ToolOutput {
    let (footer, failed) = match ending {
        Ending::Exited(Ok(status)) => (exit_note(*status), !status.success()),
        Ending::Exited(Err(error)) => (format!("[failed waiting for the command: {error}]"), true),
        Ending::TimedOut => (
            format!(
                "[timed out after {}s; the process group was killed. For a long command, run it with `background: true` and read it with shell_output.]",
                limit.as_secs_f64()
            ),
            true,
        ),
        Ending::Cancelled => ("[cancelled; the process group was killed]".to_owned(), true),
    };
    let mut text = if output.is_empty() {
        "(no output)\n".to_owned()
    } else {
        output.to_owned()
    };
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&footer);
    if failed
        && network_off
        && NETWORK_FAILURES
            .iter()
            .any(|needle| output.contains(needle))
    {
        text.push('\n');
        text.push_str(NETWORK_HINT);
    }
    if failed && sandbox_denied {
        text.push('\n');
        text.push_str(SANDBOX_HINT);
    }
    // Only a process that ended with a status has a code; a signal, a timeout and a cancel do not.
    let exit_code = match ending {
        Ending::Exited(Ok(status)) => status.code(),
        _ => None,
    };
    if failed {
        ToolOutput::error(text)
    } else {
        ToolOutput::text(text)
    }
    .with_exit_code(exit_code)
}

/// `[exit code: N]` or the signal that ended the process.
fn exit_note(status: ExitStatus) -> String {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return format!("[terminated by signal {signal}]");
    }
    status.code().map_or_else(
        || "[exit code: unknown]".to_owned(),
        |code| format!("[exit code: {code}]"),
    )
}

/// Starts the command in the background; the result names the process.
fn start_background(
    registry: &crate::agent::tools::process::ProcessRegistry,
    session_id: &str,
    request: &RunRequest<'_>,
) -> ToolOutput {
    let (mut command, _) = match request.build() {
        Ok(built) => built,
        Err(error) => return ToolOutput::error(error.message()),
    };
    match registry.start(session_id, request.command, request.ledger.clone(), || {
        command.spawn()
    }) {
        Ok(process) => ToolOutput::text(format!(
            "Started background process {} (pid {}). Use shell_output to read its output and shell_kill to stop it.",
            process.id,
            process
                .pid
                .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string())
        )),
        Err(error) => ToolOutput::error(error.message()),
    }
}

/// `running`, `exited with code N`, … for a background process.
fn state_text(process: &BackgroundProcess) -> String {
    match process.state {
        ProcessState::Running => "running".to_owned(),
        ProcessState::Exited => process.exit_code.map_or_else(
            || "exited".to_owned(),
            |code| format!("exited with code {code}"),
        ),
        ProcessState::Killed => "killed".to_owned(),
        ProcessState::Expired => "stopped at its lifetime limit".to_owned(),
    }
}

/// A spec for one of the three shell tools.
fn shell_spec(
    name: &str,
    description: &str,
    input_schema: Value,
    read_only: bool,
    destructive: bool,
) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
        read_only,
        destructive,
        source: ToolSource::Builtin,
        // Reading or stopping a process the chat already started never asks (an Internal tool).
        class: if name == SHELL {
            ToolClass::Exec
        } else {
            ToolClass::Internal
        },
        parallel_safe: false,
    }
}

/// The `shell` tool.
pub struct ShellTool {
    /// Name, description and schema.
    spec: ToolSpec,
}

impl ShellTool {
    /// The tool with its model-facing description.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: shell_spec(
                SHELL,
                "Run a shell command in the session's workspace folder and return its \
                combined output and exit code. Commands run in a sandbox: they can write only \
                inside the workspace and temporary directories (and the usual package caches), \
                and the network is available only when the user's permission mode or the approval \
                allows it. stdin is empty. Long output is truncated in the middle. A non-zero \
                exit code is reported as an error with the output. With `background` the command \
                keeps running: read it with shell_output, stop it with shell_kill.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "maxLength": MAX_COMMAND_CHARS,
                            "description": "The command line, run by the platform shell in the workspace folder."
                        },
                        "timeout_s": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Seconds before the command is killed; capped by the user's setting. Ignored with `background`."
                        },
                        "background": {
                            "type": "boolean",
                            "description": "Start the command and return at once with a process id (for servers and watchers). Default false."
                        },
                        "description": {
                            "type": "string",
                            "description": "One line saying what the command is for, shown to the user."
                        }
                    },
                    "required": ["command"]
                }),
                false,
                true,
            ),
        }
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentTool for ShellTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args = ShellArgs::parse(arguments)?;
        let cwd = validate_workspace(ctx.workspace_dir, &HostPaths::of(ctx, false)?)?;
        Ok(CallInspection {
            read_only: false,
            destructive: true,
            preview: Some(ToolApprovalPreview::Command {
                cmd: args.command.clone(),
                cwd: crate::platform::normalize_display_path(&cwd),
            }),
            hard_block: secret_in_url_query(
                &args.command,
                &crate::agent_tools::app_state(ctx)
                    .secrets()
                    .active_redaction_needles(),
            )
            .then(|| SECRET_BLOCK.to_owned()),
            arg_subject: Some(args.command),
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args = match ShellArgs::parse(&arguments) {
                Ok(args) => args,
                Err(error) => return ToolOutput::error(error.message()),
            };
            let host = match HostPaths::of(ctx, true) {
                Ok(host) => host,
                Err(error) => return ToolOutput::error(error.message()),
            };
            let workspace = match validate_workspace(ctx.workspace_dir, &host) {
                Ok(workspace) => workspace,
                Err(error) => return ToolOutput::error(error.message()),
            };
            if ctx.sandbox == SandboxKind::None && !ctx.unsandboxed_approved {
                return ToolOutput::error(SANDBOX_CHANGED);
            }
            let limits = limits(ctx.settings);
            let login_path = resolved_path().await;
            let env = scrub_env(
                std::env::vars_os()
                    .filter(|(name, _)| name != "PATH")
                    .chain([(OsString::from("PATH"), login_path)]),
                host.tmp_dir.as_deref(),
                &ctx.settings.ai.agent.shell_env_passthrough,
                ctx.shell_full_access,
            );
            let request = RunRequest {
                command: &args.command,
                workspace: &workspace,
                kind: ctx.sandbox,
                timeout: effective_timeout(args.timeout_s, limits.timeout),
                allow_network: ctx.shell_network,
                full_access: ctx.shell_full_access,
                host: &host,
                env: &env,
                output_cap: limits.output_cap,
                ledger: Some(LedgerHandle::new(std::sync::Arc::clone(
                    crate::agent_tools::app_state(ctx).store_arc(),
                ))),
                label: ctx.call_id,
            };
            if args.background {
                return start_background(ctx.processes, ctx.session_id, &request);
            }
            run_command(&request, ctx.cancel, ctx.output, ctx.events).await
        })
    }
}

/// The `shell_output` tool.
pub struct ShellOutputTool {
    /// Name, description and schema.
    spec: ToolSpec,
}

impl ShellOutputTool {
    /// The tool with its model-facing description.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: shell_spec(
                SHELL_OUTPUT,
                "Read the new output of a background process started with shell and whether it \
                still runs. Pass the `next offset` of the previous call as `since`; `wait_ms` \
                blocks up to that long for new output.",
                json!({
                    "type": "object",
                    "properties": {
                        "process_id": { "type": "string", "description": "The proc_… id shell returned." },
                        "since": { "type": "integer", "minimum": 0, "description": "Byte offset the previous call ended at. Default 0." },
                        "wait_ms": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT_MS, "description": "Wait this long for new output or exit. Default 0." }
                    },
                    "required": ["process_id"]
                }),
                true,
                false,
            ),
        }
    }
}

impl Default for ShellOutputTool {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentTool for ShellOutputTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args = OutputArgs::parse(arguments)?;
        Ok(CallInspection {
            read_only: true,
            arg_subject: Some(args.process_id),
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args = match OutputArgs::parse(&arguments) {
                Ok(args) => args,
                Err(error) => return ToolOutput::error(error.message()),
            };
            let read = ctx.processes.output(
                Some(ctx.session_id),
                &args.process_id,
                args.since,
                Duration::from_millis(args.wait_ms),
            );
            let output = tokio::select! {
                () = ctx.cancel.cancelled() => return ToolOutput::error("[cancelled]"),
                output = read => match output {
                    Ok(output) => output,
                    Err(error) => return ToolOutput::error(error.message()),
                },
            };
            let mut text = cap_text(&output.text, limits(ctx.settings).output_cap);
            if output.dropped > 0 {
                text = format!(
                    "[{} bytes of older output were dropped]\n{text}",
                    output.dropped
                );
            }
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&format!(
                "[next offset: {}] [{}]",
                output.next_offset,
                state_text(&output.process)
            ));
            ctx.processes.mark_reported(&output.process.id);
            ToolOutput::text(text)
        })
    }
}

/// The `shell_kill` tool.
pub struct ShellKillTool {
    /// Name, description and schema.
    spec: ToolSpec,
}

impl ShellKillTool {
    /// The tool with its model-facing description.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: shell_spec(
                SHELL_KILL,
                "Stop a background process started with shell (its whole process group).",
                json!({
                    "type": "object",
                    "properties": {
                        "process_id": { "type": "string", "description": "The proc_… id shell returned." }
                    },
                    "required": ["process_id"]
                }),
                false,
                false,
            ),
        }
    }
}

impl Default for ShellKillTool {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentTool for ShellKillTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args: KillArgs = serde_json::from_value(arguments.clone()).map_err(|error| {
            AppError::validation(format!("invalid shell_kill arguments: {error}"))
        })?;
        Ok(CallInspection {
            arg_subject: Some(args.process_id),
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args: KillArgs = match serde_json::from_value(arguments) {
                Ok(args) => args,
                Err(error) => {
                    return ToolOutput::error(format!("invalid shell_kill arguments: {error}"));
                }
            };
            match ctx
                .processes
                .kill(Some(ctx.session_id), &args.process_id)
                .await
            {
                Ok(process) => {
                    ctx.processes.mark_reported(&process.id);
                    ToolOutput::text(format!(
                        "Process {} is {}.",
                        process.id,
                        state_text(&process)
                    ))
                }
                Err(error) => ToolOutput::error(error.message()),
            }
        })
    }
}

/// Registers `shell`, `shell_output` and `shell_kill`.
pub fn register(registry: &mut ToolRegistry) {
    registry.register(Arc::new(ShellTool::new()));
    registry.register(Arc::new(ShellOutputTool::new()));
    registry.register(Arc::new(ShellKillTool::new()));
}
