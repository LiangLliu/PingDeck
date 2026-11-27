//! MCP stdio transport: a child process speaking newline-delimited JSON-RPC.
//!
//! A reader thread routes responses to the waiting request by id, hands notifications to the
//! connection's [`Signals`] and answers the server's own requests; a stderr thread keeps the last few KiB for error messages; a writer thread owns
//! stdin and is fed through a bounded queue, so a server that stops reading never blocks an async
//! task or a lock, and [`StdioTransport::shutdown`] never has to touch stdin. The child runs in
//! its own process group, which shutdown kills as a whole.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::lock;
use super::signals::Signals;
use spanbay_base::error::AppError;

/// Receives every line a stdio server writes to stderr (the manager's per-server log).
pub type LogSink = Arc<dyn Fn(String) + Send + Sync>;

/// Variables of the parent process every stdio server gets; the rest of the parent's environment
/// stays out unless the server's `env_allow` names it.
pub const BASE_ENV: [&str; 6] = ["PATH", "HOME", "USER", "LANG", "TERM", "TMPDIR"];
/// Variables a stdio server gets when their name starts with this (`LC_ALL`, `LC_CTYPE`, …).
const LOCALE_PREFIX: &str = "LC_";
/// The proxy variables set from the app's proxy setting, never inherited from the parent.
const PROXY_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];
/// Extra variables most Windows programs cannot start without.
#[cfg(windows)]
const WINDOWS_ENV: [&str; 6] = [
    "SYSTEMROOT",
    "PATHEXT",
    "COMSPEC",
    "USERPROFILE",
    "TEMP",
    "TMP",
];
/// Longest stderr line passed to the log, in bytes (longer ones are cut).
const MAX_LOG_LINE_BYTES: usize = 4096;
/// How much of the server's stderr is kept for error messages.
const STDERR_TAIL_BYTES: usize = 4096;
/// Longest single stdout line accepted from a server.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// Messages waiting for the writer thread.
const WRITE_QUEUE: usize = 32;

/// One message for the writer thread; `ack` receives the write result.
struct Outgoing {
    line: Vec<u8>,
    ack: Option<oneshot::Sender<std::io::Result<()>>>,
}

/// State shared with the helper threads.
struct Shared {
    /// `None` once the transport shut down; dropping it stops the writer thread.
    writer: Mutex<Option<mpsc::Sender<Outgoing>>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    /// The reader thread is still running.
    alive: AtomicBool,
    /// Why the reader gave up on a still-open stream (an over-long line).
    failure: Mutex<Option<AppError>>,
    stderr_tail: Mutex<Vec<u8>>,
    /// Where the server's notifications go.
    signals: Arc<Signals>,
}

impl Shared {
    fn writer(&self) -> Option<mpsc::Sender<Outgoing>> {
        lock(&self.writer).clone()
    }

    /// The error for a request that lost its server.
    fn closed(&self) -> AppError {
        lock(&self.failure).clone().unwrap_or_else(closed)
    }
}

/// Removes the pending entry of a request when its future finishes or is dropped.
struct PendingGuard<'a> {
    shared: &'a Shared,
    id: u64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        lock(&self.shared.pending).remove(&self.id);
    }
}

/// A running MCP server child process.
pub(super) struct StdioTransport {
    shared: Arc<Shared>,
    /// `None` once [`Self::shutdown`] handed it to a reaper thread.
    child: Mutex<Option<Child>>,
    /// The process-group ledger record of the child, cleared when it is shut down.
    ledger: Mutex<Option<crate::agent::process_ledger::LedgerKey>>,
}

fn closed() -> AppError {
    AppError::network("MCP server closed the connection")
}

fn encode_line(message: &Value) -> Result<Vec<u8>, AppError> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    Ok(line)
}

impl StdioTransport {
    /// Spawns `command` (in `cwd` when given) with the cleared-then-rebuilt environment of
    /// [`server_env`] and starts the helper threads; each stderr line also goes to `log`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        env_allow: &[String],
        cwd: Option<&Path>,
        proxy: Option<&str>,
        login_path: Option<&OsStr>,
        log: Option<LogSink>,
        signals: Arc<Signals>,
    ) -> Result<Self, AppError> {
        if command.trim().is_empty() {
            return Err(AppError::validation("MCP server command is empty"));
        }
        let vars = server_env(std::env::vars_os(), env_allow, env, proxy, login_path);
        let mut process = Command::new(command);
        process
            .args(args)
            .env_clear()
            .envs(vars.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            process.current_dir(cwd);
        }
        // Own group: shutdown can kill the server together with whatever it spawned.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut process, 0);
        let mut child = process
            .spawn()
            .map_err(|error| spawn_error(command, &vars, &error))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            kill_tree(&mut child);
            let _ = child.wait();
            return Err(AppError::internal("MCP server pipes were not created"));
        };
        let (writer, queue) = mpsc::channel(WRITE_QUEUE);
        let shared = Arc::new(Shared {
            writer: Mutex::new(Some(writer)),
            pending: Mutex::default(),
            alive: AtomicBool::new(true),
            failure: Mutex::default(),
            stderr_tail: Mutex::default(),
            signals,
        });
        // Recorded so a crash cannot leave the server behind unnoticed (cleared in `shutdown`).
        let ledger = crate::agent::process_ledger::record_installed(
            child.id(),
            crate::agent::process_ledger::LedgerKind::Mcp,
            command,
        );
        // Built before the threads so an error below drops it and kills the child.
        let transport = Self {
            shared: Arc::clone(&shared),
            child: Mutex::new(Some(child)),
            ledger: Mutex::new(ledger),
        };
        spawn_thread("mcp-stdin", move || write_loop(stdin, queue))?;
        spawn_thread("mcp-stdout", {
            let shared = Arc::clone(&shared);
            move || read_loop(&shared, stdout)
        })?;
        spawn_thread("mcp-stderr", move || drain_stderr(&shared, stderr, log))?;
        Ok(transport)
    }

    /// Sends `message` and waits for the response with `id`.
    pub(super) async fn send_request(&self, id: u64, message: &Value) -> Result<Value, AppError> {
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = lock(&self.shared.pending);
            // Checked under the lock: the reader sets `alive` before it drains `pending`.
            if !self.shared.alive.load(Ordering::SeqCst) {
                return Err(self.shared.closed());
            }
            pending.insert(id, sender);
        }
        let _guard = PendingGuard {
            shared: &self.shared,
            id,
        };
        self.send_notification(message).await?;
        receiver.await.map_err(|_| self.shared.closed())
    }

    /// Queues `message` for the writer thread and waits until it is written. A server that does
    /// not read stdin keeps this pending (callers bound it with a timeout) but never blocks a thread
    /// of the async runtime.
    pub(super) async fn send_notification(&self, message: &Value) -> Result<(), AppError> {
        let line = encode_line(message)?;
        let writer = self.shared.writer().ok_or_else(|| self.shared.closed())?;
        let (ack, written) = oneshot::channel();
        let outgoing = Outgoing {
            line,
            ack: Some(ack),
        };
        writer
            .send(outgoing)
            .await
            .map_err(|_| self.shared.closed())?;
        written
            .await
            .map_err(|_| self.shared.closed())?
            .map_err(|error| AppError::network(format!("Failed to write to MCP server: {error}")))
    }

    /// The child is running and its stdout is still being read.
    pub(super) fn is_alive(&self) -> bool {
        // `try_wait` first: it also reaps an exited child.
        let running = lock(&self.child)
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
        running && self.shared.alive.load(Ordering::SeqCst)
    }

    /// The last bytes the server wrote to stderr, trimmed.
    pub(super) fn stderr_tail(&self) -> String {
        let tail = lock(&self.shared.stderr_tail);
        String::from_utf8_lossy(&tail).trim().to_owned()
    }

    /// Stops the server the MCP way, on a helper thread so the caller never waits: closes stdin
    /// (the writer thread ends with its queue), gives the server [`STDIN_GRACE`] to exit, then
    /// SIGTERMs its process group, and SIGKILLs the group after [`TERM_GRACE`]. The ledger record
    /// stays until the child is reaped, so a crash meanwhile still cleans the group up at the next
    /// start. Safe to call repeatedly.
    pub(super) fn shutdown(&self) {
        *lock(&self.shared.writer) = None;
        let key = lock(&self.ledger).take();
        match lock(&self.child).take() {
            Some(child) => stop(child, key),
            None => {
                if let Some(key) = key {
                    crate::agent::process_ledger::clear_installed(&key);
                }
            }
        }
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// SIGKILLs the child's process group (a grandchild that left the group survives) and the child
/// itself.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    crate::agent::tools::shell_meta::kill_group(
        child.id(),
        Some(crate::agent::tools::shell_meta::Signal::SIGKILL),
    );
    let _ = child.kill();
}

/// How long a server may take to exit once its stdin is closed.
const STDIN_GRACE: Duration = Duration::from_secs(1);
/// How long a server may take to exit after SIGTERM before its group is SIGKILLed.
const TERM_GRACE: Duration = Duration::from_secs(2);

/// Whether `child` exits within `grace` (polled).
fn exits_within(child: &mut Child, grace: Duration) -> bool {
    let deadline = std::time::Instant::now() + grace;
    loop {
        if !matches!(child.try_wait(), Ok(None)) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Closed stdin → [`STDIN_GRACE`] → SIGTERM to the group → [`TERM_GRACE`] → SIGKILL, then reaps
/// the child and clears its ledger record. Falls back to killing at once when no thread starts.
fn stop(child: Child, key: Option<crate::agent::process_ledger::LedgerKey>) {
    let cell = Arc::new(Mutex::new(Some((child, key))));
    let worker = Arc::clone(&cell);
    let started = spawn_thread("mcp-stop", move || {
        let Some((mut child, key)) = lock(&worker).take() else {
            return;
        };
        if !exits_within(&mut child, STDIN_GRACE) {
            #[cfg(unix)]
            crate::agent::tools::shell_meta::kill_group(
                child.id(),
                Some(crate::agent::tools::shell_meta::Signal::SIGTERM),
            );
            if !exits_within(&mut child, TERM_GRACE) {
                kill_tree(&mut child);
            }
        }
        let _ = child.wait();
        // The leader is gone; a member that ignored SIGTERM must not outlive the server.
        #[cfg(unix)]
        crate::agent::tools::shell_meta::kill_group(
            child.id(),
            Some(crate::agent::tools::shell_meta::Signal::SIGKILL),
        );
        if let Some(key) = key {
            crate::agent::process_ledger::clear_installed(&key);
        }
    });
    if started.is_err()
        && let Some((mut child, key)) = lock(&cell).take()
    {
        kill_tree(&mut child);
        let _ = child.wait();
        if let Some(key) = key {
            crate::agent::process_ledger::clear_installed(&key);
        }
    }
}

/// Owns stdin: writes queued messages until the queue closes or a write fails.
fn write_loop(mut stdin: ChildStdin, mut queue: mpsc::Receiver<Outgoing>) {
    while let Some(Outgoing { line, ack }) = queue.blocking_recv() {
        let result = stdin.write_all(&line).and_then(|()| stdin.flush());
        let failed = result.is_err();
        if let Some(ack) = ack {
            let _ = ack.send(result);
        }
        if failed {
            break;
        }
    }
}

fn spawn_thread(name: &str, work: impl FnOnce() + Send + 'static) -> Result<(), AppError> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
        .map(drop)
        .map_err(|error| AppError::io(format!("Failed to start MCP helper thread: {error}")))
}

fn drain_stderr(shared: &Shared, mut stderr: impl Read, log: Option<LogSink>) {
    let mut buffer = [0u8; 1024];
    let mut line = Vec::new();
    while let Ok(read) = stderr.read(&mut buffer) {
        if read == 0 {
            break;
        }
        {
            let mut tail = lock(&shared.stderr_tail);
            tail.extend_from_slice(&buffer[..read]);
            if tail.len() > STDERR_TAIL_BYTES {
                let excess = tail.len() - STDERR_TAIL_BYTES;
                tail.drain(..excess);
            }
        }
        if let Some(log) = &log {
            for byte in &buffer[..read] {
                if *byte == b'\n' {
                    send_log_line(log, &mut line);
                } else if line.len() < MAX_LOG_LINE_BYTES {
                    line.push(*byte);
                }
            }
        }
    }
    if let Some(log) = &log {
        send_log_line(log, &mut line);
    }
}

/// Passes the collected `line` (if not blank) to `log` and clears it.
fn send_log_line(log: &LogSink, line: &mut Vec<u8>) {
    let text = String::from_utf8_lossy(line).trim_end().to_owned();
    line.clear();
    if !text.is_empty() {
        log(text);
    }
}

/// Why a server did not start; a command not found names the `PATH` searched and the fix.
fn spawn_error(command: &str, vars: &[(OsString, OsString)], error: &std::io::Error) -> AppError {
    if error.kind() != std::io::ErrorKind::NotFound || Path::new(command).is_absolute() {
        return AppError::io(format!("Failed to start MCP server `{command}`: {error}"));
    }
    let searched = vars
        .iter()
        .rfind(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    AppError::io(format!(
        "Failed to start MCP server: `{command}` was not found in PATH `{searched}`. Set `env.PATH` \
         in the server settings or use an absolute path to the command."
    ))
}

/// The environment a stdio server starts with: the [`BASE_ENV`] variables of `parent` with `PATH`
/// replaced by the user's `login_path` when known, then the ones `env_allow` names (never
/// `SPANBAY_*`), then the server's own (resolved) `env`, which wins (an explicit `env.PATH`
/// included). Nothing else of the parent leaks in.
pub(super) fn server_env(
    parent: impl Iterator<Item = (OsString, OsString)>,
    env_allow: &[String],
    env: &BTreeMap<String, String>,
    proxy: Option<&str>,
    login_path: Option<&OsStr>,
) -> Vec<(OsString, OsString)> {
    let mut vars: Vec<(OsString, OsString)> = parent
        .filter(|(name, _)| {
            let upper = name.to_string_lossy().to_ascii_uppercase();
            #[cfg(windows)]
            let base = BASE_ENV.contains(&upper.as_str())
                || upper.starts_with(LOCALE_PREFIX)
                || WINDOWS_ENV.contains(&upper.as_str());
            #[cfg(not(windows))]
            let base = BASE_ENV.contains(&upper.as_str()) || upper.starts_with(LOCALE_PREFIX);
            let allowed = env_allow
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(&upper));
            (base || allowed) && !upper.starts_with("SPANBAY_")
        })
        .collect();
    if let Some(path) = login_path {
        vars.retain(|(name, _)| !name.eq_ignore_ascii_case("PATH"));
        vars.push((OsString::from("PATH"), path.to_owned()));
    }
    if let Some(proxy) = proxy.and_then(proxy_without_userinfo) {
        vars.extend(
            PROXY_VARS
                .iter()
                .map(|name| (OsString::from(name), OsString::from(&proxy))),
        );
    }
    vars.retain(|(name, _)| !env.keys().any(|own| name.to_string_lossy() == own.as_str()));
    vars.extend(
        env.iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value))),
    );
    vars
}

fn read_loop(shared: &Shared, stdout: impl Read) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        // One extra byte tells "exactly the cap, then newline" from "no newline within the cap".
        let read = reader
            .by_ref()
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut line);
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) if line.len() > MAX_LINE_BYTES => {
                *lock(&shared.failure) =
                    Some(AppError::network("MCP server sent a line over 16 MiB"));
                break;
            }
            Ok(_) => {
                // Lines that are not JSON (stray logging) are ignored.
                if let Ok(message) = serde_json::from_slice::<Value>(&line) {
                    route(shared, message);
                }
            }
        }
    }
    shared.alive.store(false, Ordering::SeqCst);
    // Dropping the senders wakes every waiter with the failure or "closed".
    lock(&shared.pending).clear();
}

/// Delivers a response to its request, hands a notification to the signals, or answers a
/// server-to-client request.
fn route(shared: &Shared, message: Value) {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id");
    match (method, id) {
        (Some(method), Some(id)) => {
            let reply = if method == "ping" {
                json!({"jsonrpc": "2.0", "id": id, "result": {}})
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": format!("Method not found: {method}")},
                })
            };
            if let Ok(line) = encode_line(&reply)
                && let Some(writer) = shared.writer()
            {
                // Never wait here: a full queue means the server is not reading, so the reply is dropped.
                let _ = writer.try_send(Outgoing { line, ack: None });
            }
        }
        (None, Some(id)) => {
            if let Some(id) = id.as_u64()
                && let Some(sender) = lock(&shared.pending).remove(&id)
            {
                let _ = sender.send(message);
            }
        }
        (Some(_), None) => shared.signals.notice(&message),
        (None, None) => {}
    }
}

/// The app's proxy URL without user name and password (a server must not receive the credentials);
/// `None` for a blank or unparsable value.
fn proxy_without_userinfo(proxy: &str) -> Option<String> {
    let proxy = proxy.trim();
    if proxy.is_empty() {
        return None;
    }
    let mut url = url::Url::parse(proxy).ok()?;
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    Some(url.to_string().trim_end_matches('/').to_owned())
}
