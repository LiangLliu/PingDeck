//! The shared registry of live MCP connections, their cached tool lists, status and logs.
//!
//! Single responsibility: one [`McpConnection`] per (server id, working directory), reconnecting
//! when it died or its resolved config changed; a failed connect is remembered for a minute so a
//! broken server is not respawned on every call; `tools/list` answers are cached per server for
//! five minutes, or until the server announces `notifications/tools/list_changed`; every status
//! change of a server is published on a hub ([`McpStatusEvent`]) and
//! the last 200 stderr lines of each stdio server are kept for the settings UI (secrets redacted).
//! A stdio server runs in its configured `cwd`, else the session's workspace, else
//! `<mcp root>/<server id>`, never the home directory. Time comes from an injectable [`Clock`] so
//! expiry is testable without sleeping.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::model::AgentMcpServer;
use super::http::is_auth_rejection;
use super::{
    LogSink, McpConnection, McpServerStatus, McpStatusEvent, McpStreamEvent, McpTool,
    McpTransportConfig, TEST_ID_PREFIX, lock, scrub, secret_values, timed_out,
};
use pingdeck_base::error::AppError;
use pingdeck_base::secrets::redact_secrets;
use pingdeck_model::events::{EventHub, EventSink, Sequenced};

/// How long a failed connect is answered with the same error instead of retrying.
const CONNECT_BACKOFF: Duration = Duration::from_secs(60);
/// How long a `tools/list` answer is served from the cache.
const TOOLS_TTL: Duration = Duration::from_secs(5 * 60);
/// Log lines kept per server.
pub const LOG_LINES: usize = 200;
/// Clients that may watch the status hub at once.
const MAX_WATCHERS: usize = 16;
/// Status events remembered for a late subscriber.
const STATUS_RING: usize = 256;

/// A source of the current instant.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// A connection is shared by sessions with the same server and working directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    server_id: String,
    /// Only stdio servers run in a directory of their own.
    cwd: Option<PathBuf>,
}

/// The last failed connect of a server.
struct Failed {
    config: McpTransportConfig,
    at: Instant,
    error: AppError,
}

/// A cached `tools/list` answer.
struct CachedTools {
    /// Hash of the resolved config the answer belongs to.
    config_hash: u64,
    at: Instant,
    tools: Vec<McpTool>,
}

/// One live connection per server and working directory; see the module documentation.
pub struct McpManager {
    clock: Clock,
    connections: Mutex<HashMap<Key, (McpTransportConfig, Arc<McpConnection>)>>,
    /// The last failed connect per server id.
    failures: Mutex<HashMap<String, Failed>>,
    tools: Mutex<HashMap<String, CachedTools>>,
    /// The status last published per server id.
    statuses: Mutex<HashMap<String, McpServerStatus>>,
    /// Servers the user trusts: their own read-only hints count (see [`Self::tool_counts`]).
    trusted: Mutex<HashSet<String>>,
    hub: EventHub<McpStreamEvent>,
    /// The newest log lines per server id.
    logs: Arc<Mutex<HashMap<String, VecDeque<String>>>>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::with_clock(Arc::new(Instant::now))
    }
}

/// A stable hash of a resolved config (secrets included, so a changed secret changes it).
fn config_hash(config: &McpTransportConfig) -> u64 {
    let mut hasher = DefaultHasher::new();
    config.hash(&mut hasher);
    hasher.finish()
}

/// The status a failed connect shows: `NeedsAuth` for a `401`/`403`, else `Error`.
fn failure_status(error: &AppError) -> McpServerStatus {
    if is_auth_rejection(error) {
        McpServerStatus::NeedsAuth
    } else {
        McpServerStatus::Error {
            message: error.message().to_owned(),
        }
    }
}

impl McpManager {
    /// An empty manager on the system clock.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty manager that reads time from `clock`.
    #[must_use]
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            clock,
            connections: Mutex::default(),
            failures: Mutex::default(),
            tools: Mutex::default(),
            statuses: Mutex::default(),
            trusted: Mutex::default(),
            hub: EventHub::new(MAX_WATCHERS, STATUS_RING),
            logs: Arc::default(),
        }
    }

    /// The directory a stdio server of `server_id` runs in: the config's `cwd` when the user
    /// declared one, else `<system temp>/pingdeck-mcp/<server id>` (created, owner-only): never
    /// the data folder, the home directory or a chat's folder. `None` for an
    /// http server.
    ///
    /// # Errors
    /// [`AppError::Io`] when the default directory cannot be created.
    pub fn working_dir(
        server_id: &str,
        config: &McpTransportConfig,
    ) -> Result<Option<PathBuf>, AppError> {
        let McpTransportConfig::Stdio { cwd, .. } = config else {
            return Ok(None);
        };
        if let Some(cwd) = cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()) {
            return Ok(Some(PathBuf::from(cwd)));
        }
        let name: String = server_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let dir = std::env::temp_dir().join("pingdeck-mcp").join(name);
        pingdeck_base::platform::create_private_dir_all(&dir)
            .map_err(|error| AppError::io(format!("cannot create {}: {error}", dir.display())))?;
        Ok(Some(dir))
    }

    /// The live connection of `server_id`, connecting (outside the lock) when there is none, it
    /// died, or the resolved `config` differs from the cached one. A stdio server runs in the
    /// directory of [`Self::working_dir`] and gets the proxy variables when `proxy` (the app's
    /// network proxy URL) is set. A
    /// racing double connect keeps the first. After a failed connect the same error is returned
    /// without retrying for 60 s, unless the config changed or [`Self::disconnect`] ran. A fresh
    /// connection invalidates the cached tool list. Status changes are published.
    pub async fn connection(
        &self,
        server_id: &str,
        config: &McpTransportConfig,
        proxy: Option<&str>,
        timeout: Duration,
    ) -> Result<Arc<McpConnection>, AppError> {
        let key = Key {
            server_id: server_id.to_owned(),
            cwd: Self::working_dir(server_id, config)?,
        };
        if let Some(live) = self.cached(&key, config) {
            return Ok(live);
        }
        if let Some(error) = self.recent_failure(server_id, config) {
            return Err(error);
        }
        self.publish(server_id, McpServerStatus::Starting);
        let log = self.log_sink(server_id, config);
        let fresh =
            match McpConnection::connect_logged(config, key.cwd.as_deref(), proxy, timeout, log)
                .await
            {
                Ok(connection) => Arc::new(connection),
                Err(error) => {
                    self.record_failure(server_id, config, error.clone());
                    return Err(error);
                }
            };
        lock(&self.failures).remove(server_id);
        lock(&self.tools).remove(server_id);
        let (kept, displaced) = {
            let mut connections = lock(&self.connections);
            match connections.get(&key) {
                Some((cached, live)) if cached == config && live.is_alive() => {
                    (Arc::clone(live), Some(fresh))
                }
                _ => {
                    let old = connections
                        .insert(key, (config.clone(), Arc::clone(&fresh)))
                        .map(|(_, old)| old);
                    (fresh, old)
                }
            }
        };
        if let Some(displaced) = displaced {
            displaced.shutdown();
        }
        self.publish(server_id, McpServerStatus::Running);
        Ok(kept)
    }

    /// The tool counts of `server`: `(tool_count, read_only_count)` of its cached `tools/list`,
    /// `(None, None)` until something is cached. A read-only hint counts only when the user trusts
    /// the server. Also records that trust, so a status event published later counts the same way.
    #[must_use]
    pub fn tool_counts(&self, server: &AgentMcpServer) -> (Option<usize>, Option<usize>) {
        {
            let mut trusted = lock(&self.trusted);
            if server.trusted {
                trusted.insert(server.id.clone());
            } else {
                trusted.remove(&server.id);
            }
        }
        self.counts_of(&server.id)
    }

    /// The counts of what is cached for `server_id` (see [`Self::tool_counts`]).
    fn counts_of(&self, server_id: &str) -> (Option<usize>, Option<usize>) {
        let trusted = lock(&self.trusted).contains(server_id);
        self.tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(server_id)
            .map_or((None, None), |cached| {
                (
                    Some(cached.tools.len()),
                    Some(
                        cached
                            .tools
                            .iter()
                            .filter(|tool| trusted && tool.read_only)
                            .count(),
                    ),
                )
            })
    }

    /// The sequence number of the latest status event, or 0 before the first: a caller subscribes
    /// from it.
    #[must_use]
    pub fn last_seq(&self) -> u64 {
        self.hub.last_seq()
    }

    /// The cached `tools/list` of `server_id` when it is younger than five minutes, or older while
    /// its connection is still alive (stale-while-revalidate: an expired list is not a cold
    /// start; [`Self::tools_stale`] says to refresh it in the background). Never blocks and never
    /// connects.
    #[must_use]
    pub fn cached_tools(&self, server_id: &str) -> Option<Vec<McpTool>> {
        let tools = {
            let tools = lock(&self.tools);
            let cached = tools.get(server_id)?;
            if (self.clock)().saturating_duration_since(cached.at) < TOOLS_TTL {
                return Some(cached.tools.clone());
            }
            cached.tools.clone()
        };
        self.alive(server_id).then_some(tools)
    }

    /// Whether the cached list of `server_id` is older than five minutes, or the server said
    /// its tools changed since it was read.
    #[must_use]
    pub fn tools_stale(&self, server_id: &str) -> bool {
        let old = lock(&self.tools)
            .get(server_id)
            .is_some_and(|cached| (self.clock)().saturating_duration_since(cached.at) >= TOOLS_TTL);
        old || self.list_changed(server_id)
    }

    /// Whether a live connection of `server_id` announced `notifications/tools/list_changed`
    /// since its last `tools/list`.
    fn list_changed(&self, server_id: &str) -> bool {
        lock(&self.connections)
            .iter()
            .any(|(key, (_, live))| key.server_id == server_id && live.tools_changed())
    }

    /// Whether a live connection of `server_id` declared the `resources` capability: the run
    /// offers the server's resource tool.
    #[must_use]
    pub fn offers_resources(&self, server_id: &str) -> bool {
        lock(&self.connections).iter().any(|(key, (_, live))| {
            key.server_id == server_id && live.is_alive() && live.server_info().resources
        })
    }

    /// Whether a live connection of `server_id` exists (in any working directory).
    fn alive(&self, server_id: &str) -> bool {
        lock(&self.connections)
            .iter()
            .any(|(key, (_, live))| key.server_id == server_id && live.is_alive())
    }

    /// The tools of `server_id`: the cached list when it is fresh, belongs to this resolved
    /// `config` and the server announced no change since, else a `tools/list` over the (possibly
    /// new) connection, which refills the cache.
    /// The whole call is bounded by `timeout`; any failure drops the cached list. A caller that
    /// must not wait connects cold servers in a background task and reads
    /// [`Self::cached_tools`] meanwhile.
    ///
    /// # Errors
    /// The connect or `tools/list` error, secrets scrubbed.
    pub async fn ensure_tools(
        &self,
        server_id: &str,
        config: &McpTransportConfig,
        proxy: Option<&str>,
        timeout: Duration,
    ) -> Result<Vec<McpTool>, AppError> {
        let hash = config_hash(config);
        let now = (self.clock)();
        let changed = self.list_changed(server_id);
        {
            let tools = lock(&self.tools);
            if let Some(cached) = tools.get(server_id)
                && cached.config_hash == hash
                && now.saturating_duration_since(cached.at) < TOOLS_TTL
                && !changed
            {
                return Ok(cached.tools.clone());
            }
        }
        let fetch = async {
            let connection = self.connection(server_id, config, proxy, timeout).await?;
            let (tools, skipped) = connection.list_tools_reporting(timeout).await?;
            for why in skipped {
                self.note(server_id, &format!("[pingdeck] tool skipped: {why}"));
            }
            Ok::<_, AppError>(tools)
        };
        let outcome = match tokio::time::timeout(timeout, fetch).await {
            Ok(outcome) => outcome,
            Err(_) => {
                let error = scrub(timed_out(timeout), &secret_values(config));
                self.record_failure(server_id, config, error.clone());
                Err(error)
            }
        };
        match outcome {
            Ok(tools) => {
                lock(&self.tools).insert(
                    server_id.to_owned(),
                    CachedTools {
                        config_hash: hash,
                        at: (self.clock)(),
                        tools: tools.clone(),
                    },
                );
                Ok(tools)
            }
            Err(error) => {
                lock(&self.tools).remove(server_id);
                Err(error)
            }
        }
    }

    /// What the settings UI shows for an enabled server: [`McpServerStatus::Running`] while a
    /// connection lives, the last connect failure otherwise (`NeedsAuth` for `401`/`403`), and
    /// `Starting` while there is neither (a connect under way or nobody needed the server yet).
    #[must_use]
    pub fn status(&self, server_id: &str) -> McpServerStatus {
        let running = lock(&self.connections)
            .iter()
            .any(|(key, (_, live))| key.server_id == server_id && live.is_alive());
        if running {
            return McpServerStatus::Running;
        }
        match lock(&self.failures).get(server_id) {
            Some(failed) => failure_status(&failed.error),
            None => McpServerStatus::Starting,
        }
    }

    /// The status of every server the manager has seen, by server id. Re-checks each one first,
    /// so a server process that died since is published as changed.
    #[must_use]
    pub fn status_list(&self) -> Vec<McpStatusEvent> {
        let ids: Vec<String> = lock(&self.statuses)
            .iter()
            .filter(|(_, status)| **status != McpServerStatus::Disabled)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.publish(&id, self.status(&id));
        }
        let mut list: Vec<McpStatusEvent> = lock(&self.statuses)
            .iter()
            .map(|(server_id, status)| {
                let (tool_count, read_only_count) = self.counts_of(server_id);
                McpStatusEvent {
                    server_id: server_id.clone(),
                    status: status.clone(),
                    tool_count,
                    read_only_count,
                }
            })
            .collect();
        list.sort_by(|a, b| a.server_id.cmp(&b.server_id));
        list
    }

    /// Watches status changes: replays the remembered events after `from_seq` (all when `None`),
    /// then delivers new ones. A `from_seq` whose events were evicted is announced with
    /// [`McpStreamEvent::Gap`] first, so the subscriber reloads the list instead of guessing.
    ///
    /// # Errors
    /// [`AppError::Conflict`] when there are too many watchers (or `from_seq` is `None` and the
    /// ring no longer starts at 1).
    pub fn subscribe(
        &self,
        from_seq: Option<u64>,
        sink: Arc<dyn EventSink<Sequenced<McpStreamEvent>>>,
    ) -> Result<u64, AppError> {
        self.hub
            .subscribe_announcing_gap(from_seq, sink, |first_seq| McpStreamEvent::Gap {
                first_seq,
            })
    }

    /// Ends a [`Self::subscribe`] subscription.
    pub fn unsubscribe(&self, subscription_id: u64) -> bool {
        self.hub.unsubscribe(subscription_id)
    }

    /// Records that `server_id` is switched off and publishes it.
    pub fn mark_disabled(&self, server_id: &str) {
        self.disconnect(server_id);
        self.publish(server_id, McpServerStatus::Disabled);
    }

    /// Stops `server_id` and forgets its failure and cached tools; the next use reconnects.
    /// Publishes `Starting`. The caller reconnects (it owns the resolved config).
    pub fn restart(&self, server_id: &str) {
        self.disconnect(server_id);
        self.publish(server_id, McpServerStatus::Starting);
    }

    /// Forgets everything about a deleted server: connection, state and log.
    pub fn forget(&self, server_id: &str) {
        self.disconnect(server_id);
        lock(&self.statuses).remove(server_id);
        lock(&self.logs).remove(server_id);
    }

    /// The last [`LOG_LINES`] lines `server_id` wrote to stderr (secrets redacted), oldest first.
    #[must_use]
    pub fn log(&self, server_id: &str) -> Vec<String> {
        lock(&self.logs)
            .get(server_id)
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Appends one line of PingDeck's own to the log of `server_id` (test connections keep none).
    fn note(&self, server_id: &str, line: &str) {
        if server_id.starts_with(TEST_ID_PREFIX) {
            return;
        }
        let mut logs = lock(&self.logs);
        let ring = logs.entry(server_id.to_owned()).or_default();
        if ring.len() == LOG_LINES {
            ring.pop_front();
        }
        ring.push_back(line.to_owned());
    }

    /// Closes the connections of `server_id` and forgets its failure and cached tools.
    pub fn disconnect(&self, server_id: &str) {
        lock(&self.failures).remove(server_id);
        lock(&self.tools).remove(server_id);
        let removed: Vec<_> = {
            let mut connections = lock(&self.connections);
            let keys: Vec<Key> = connections
                .keys()
                .filter(|key| key.server_id == server_id)
                .cloned()
                .collect();
            keys.iter()
                .filter_map(|key| connections.remove(key))
                .collect()
        };
        for (_, connection) in removed {
            connection.shutdown();
        }
    }

    /// Closes every connection and forgets every failure and cached tool list.
    pub fn disconnect_all(&self) {
        lock(&self.failures).clear();
        lock(&self.tools).clear();
        let removed: Vec<_> = lock(&self.connections).drain().collect();
        for (_, (_, connection)) in removed {
            connection.shutdown();
        }
    }

    /// Stores `status` for `server_id` and publishes it when it differs from the last one.
    /// Throwaway test connections (`test:` ids) are not published.
    fn publish(&self, server_id: &str, status: McpServerStatus) {
        if server_id.starts_with(TEST_ID_PREFIX) {
            return;
        }
        {
            let mut statuses = lock(&self.statuses);
            if statuses.get(server_id) == Some(&status) {
                return;
            }
            statuses.insert(server_id.to_owned(), status.clone());
        }
        let (tool_count, read_only_count) = self.counts_of(server_id);
        self.hub.emit(McpStreamEvent::Status(McpStatusEvent {
            server_id: server_id.to_owned(),
            status,
            tool_count,
            read_only_count,
        }));
    }

    /// A sink that appends redacted stderr lines of `server_id` to its ring; `None` for test
    /// connections.
    fn log_sink(&self, server_id: &str, config: &McpTransportConfig) -> Option<LogSink> {
        if server_id.starts_with(TEST_ID_PREFIX) {
            return None;
        }
        let logs = Arc::clone(&self.logs);
        let id = server_id.to_owned();
        let needles = secret_values(config);
        Some(Arc::new(move |line: String| {
            let line = redact_secrets(&line, &needles);
            let mut logs = lock(&logs);
            let ring = logs.entry(id.clone()).or_default();
            if ring.len() == LOG_LINES {
                ring.pop_front();
            }
            ring.push_back(line);
        }))
    }

    fn record_failure(&self, server_id: &str, config: &McpTransportConfig, error: AppError) {
        self.publish(server_id, failure_status(&error));
        lock(&self.failures).insert(
            server_id.to_owned(),
            Failed {
                config: config.clone(),
                at: (self.clock)(),
                error,
            },
        );
    }

    /// The error of a failed connect with this exact `config` less than 60 s ago.
    fn recent_failure(&self, server_id: &str, config: &McpTransportConfig) -> Option<AppError> {
        let mut failures = lock(&self.failures);
        let failed = failures.get(server_id)?;
        if failed.config == *config
            && (self.clock)().saturating_duration_since(failed.at) < CONNECT_BACKOFF
        {
            return Some(failed.error.clone());
        }
        failures.remove(server_id);
        None
    }

    fn cached(&self, key: &Key, config: &McpTransportConfig) -> Option<Arc<McpConnection>> {
        let connections = lock(&self.connections);
        let (cached, live) = connections.get(key)?;
        (cached == config && live.is_alive()).then(|| Arc::clone(live))
    }
}
