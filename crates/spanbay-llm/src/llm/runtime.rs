//! Shared runtime state of the LLM layer: database access, the routing snapshot cache, health.
//!
//! Single responsibility: give the gateway, the chat client and the probes one place to get a
//! consistent view of the configuration without touching the app's store lock. Reads go through
//! the cached [`RoutingSnapshot`], rebuilt from a *separate* SQLite connection whenever a write
//! use-case bumped the version counter ([`LlmRuntime::touch`]). Settings are pushed in by the
//! app (see [`LlmRuntime::set_settings`]), so a token change applies to the very next request.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::llm::events::{LlmEvent, LlmEventHub, new_hub};
use crate::llm::gateway::logwriter::{LOG_QUEUE, LogQueue};
use crate::llm::gateway::{GatewayManager, SeenTokens, TzOffset};
use crate::llm::health::{CircuitConfig, HealthBook};
use crate::llm::key_book::KeyBook;
use crate::llm::router::{KeyScopes, ProviderHeaders, RoutingSnapshot};
use crate::llm::services::LlmServices;
use crate::llm::store::{self, ClientHashEntry};
use crate::llm::upstream::ConnLedger;
use spanbay_base::error::AppError;
use spanbay_base::secrets::SecretStore;
use spanbay_model::events::EventSink;
use spanbay_model::settings::{
    AiGatewaySettings, Settings, SettingsEvent, SettingsHub, SettingsSection,
};
use spanbay_store::DbConnection;
pub use spanbay_store::storage::{DbConn, DbHandle};

mod http;
use self::http::SharedClients;

/// Current wall clock in Unix milliseconds.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[derive(Debug)]
struct Cached {
    version: u64,
    snapshot: Arc<RoutingSnapshot>,
}

#[derive(Debug)]
struct CachedClients {
    version: u64,
    entries: Arc<Vec<ClientHashEntry>>,
}

/// The LLM layer's shared state; one per [`spanbay_core::app::AppState`].
pub struct LlmRuntime {
    db: DbHandle,
    version: AtomicU64,
    settings: RwLock<Arc<Settings>>,
    secrets: Arc<dyn SecretStore>,
    cache: Mutex<Option<Cached>>,
    /// Circuit breakers and moving averages, shared by the gateway and the client API.
    pub health: Arc<HealthBook>,
    /// Per-key cooldowns, last errors and round-robin cursors (multi-key rotation).
    pub keys: Arc<KeyBook>,
    /// Throttles the `KeysChanged` of key run-state changes.
    pub key_events: crate::llm::events::KeysChangedThrottle,
    /// The local gateway's lifecycle.
    pub gateway: GatewayManager,
    /// What the shared clients' resolver and attempts tell each other (per-attempt DNS, reuse).
    conn_ledger: Arc<ConnLedger>,
    /// The shared upstream clients, see [`LlmRuntime::http_for`].
    http_clients: Mutex<SharedClients>,
    events: LlmEventHub,
    tz: TzOffset,
    clients: Mutex<Option<CachedClients>>,
    /// The request-log writer, started on first use (see [`LlmRuntime::log`]).
    log: Mutex<Option<Arc<LogQueue>>>,
    /// State of the use cases that outlives a call (sniff reports, balance throttle, probe run).
    pub services: LlmServices,
}

/// The runtime holds the secret store (provider keys), so it never prints its fields.
impl std::fmt::Debug for LlmRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmRuntime")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl LlmRuntime {
    /// A runtime over `db`.
    ///
    /// # Errors
    /// [`AppError::Internal`] if the HTTP clients cannot be built.
    pub fn new(
        db: DbHandle,
        settings: Arc<Settings>,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Self, AppError> {
        let conn_ledger = Arc::new(ConnLedger::default());
        Ok(Self {
            db,
            version: AtomicU64::new(1),
            settings: RwLock::new(settings),
            secrets,
            cache: Mutex::new(None),
            health: Arc::new(HealthBook::new()),
            keys: Arc::new(KeyBook::new()),
            key_events: crate::llm::events::KeysChangedThrottle::default(),
            gateway: GatewayManager::new(),
            // Built without a CA bundle; `http_for` rebuilds them once the setting names one.
            http_clients: Mutex::new(SharedClients::build(&conn_ledger, "")?),
            conn_ledger,
            events: new_hub(),
            services: LlmServices::default(),
            tz: TzOffset::new(),
            clients: Mutex::new(None),
            log: Mutex::new(None),
        })
    }

    /// Builds the runtime of an app and keeps it in step with the live settings: every settings
    /// change is pushed in, and a gateway port change restarts a running gateway (on a helper
    /// thread, because binding and stopping block).
    ///
    /// # Errors
    /// As [`LlmRuntime::new`], or [`AppError::Conflict`] when the settings hub has no free
    /// subscriber slot.
    pub fn attach(
        hub: &SettingsHub,
        db: DbHandle,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Arc<Self>, AppError> {
        let runtime = Arc::new(Self::new(db, hub.snapshot(), secrets)?);
        let weak = Arc::downgrade(&runtime);
        let sink = move |event: SettingsEvent| -> bool {
            let Some(runtime) = weak.upgrade() else {
                return false;
            };
            let SettingsEvent::Changed { section, settings } = event;
            runtime.set_settings(Arc::new(*settings));
            if section == SettingsSection::Ai {
                std::thread::spawn(move || {
                    // A failed rebind leaves the gateway stopped; its status shows that.
                    let _ = runtime.gateway.apply_settings(&runtime);
                });
            }
            true
        };
        hub.subscribe(Arc::new(sink) as Arc<dyn EventSink<SettingsEvent>>)?;
        Ok(runtime)
    }

    /// The database handle (for the gateway's own connection).
    #[must_use]
    pub fn db(&self) -> &DbHandle {
        &self.db
    }

    /// The secret store (provider keys).
    #[must_use]
    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    /// The settings in force (one cheap `Arc` clone: take one per operation).
    #[must_use]
    pub fn settings(&self) -> Arc<Settings> {
        Arc::clone(&self.settings.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Replaces the settings; the app calls this on every settings change.
    pub fn set_settings(&self, settings: Arc<Settings>) {
        *self
            .settings
            .write()
            .unwrap_or_else(PoisonError::into_inner) = settings;
    }

    /// Marks the configuration changed so the next read rebuilds the snapshot.
    pub fn touch(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    /// The configuration version [`LlmRuntime::touch`] bumps; caches key themselves on it.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// The hub of push events ([`LlmEvent`]).
    #[must_use]
    pub fn events(&self) -> &LlmEventHub {
        &self.events
    }

    /// Numbers `event` and delivers it to every subscriber; returns its sequence number.
    pub fn emit(&self, event: LlmEvent) -> u64 {
        self.events.emit(event)
    }

    /// The local UTC offset, in minutes east, the usage rollup cuts days with (see [`TzOffset`]).
    #[must_use]
    pub fn tz_offset_minutes(&self) -> i16 {
        self.tz.get()
    }

    /// Pins the local UTC offset (the shell knows the webview's); it replaces the OS reading.
    pub fn set_tz_offset_minutes(&self, minutes: i16) {
        self.tz.set(minutes);
    }

    /// The request-log writer: one per runtime, started on first use, so Chat logs its calls with
    /// the gateway stopped and the gateway queues into the same writer. It ends, after storing what
    /// is queued, when the runtime is dropped.
    ///
    /// # Errors
    /// [`AppError::Io`] when the writer thread cannot start.
    pub(crate) fn log(self: &Arc<Self>) -> Result<Arc<LogQueue>, AppError> {
        self.log_with(LOG_QUEUE)
    }

    /// [`LlmRuntime::log`] with a queue of `capacity` jobs when this call starts the writer (the
    /// gateway's limits override it in tests); an already running writer keeps its own.
    pub(crate) fn log_with(self: &Arc<Self>, capacity: usize) -> Result<Arc<LogQueue>, AppError> {
        let mut slot = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(queue) = slot.as_ref() {
            return Ok(Arc::clone(queue));
        }
        let queue = Arc::new(LogQueue::spawn(
            Arc::clone(self),
            Arc::new(SeenTokens::default()),
            capacity,
        )?);
        *slot = Some(Arc::clone(&queue));
        Ok(queue)
    }

    /// The ledger of per-attempt connection facts shared by every HTTP client of this runtime.
    #[must_use]
    pub fn conn_ledger(&self) -> &Arc<ConnLedger> {
        &self.conn_ledger
    }

    /// The current snapshot, rebuilt first if a write happened since it was built.
    ///
    /// Blocking SQLite work: call from `spawn_blocking` or a blocking context.
    ///
    /// # Errors
    /// Any error of opening the database or reading the tables.
    pub fn snapshot_blocking(&self) -> Result<Arc<RoutingSnapshot>, AppError> {
        if let Some(snapshot) = self.cached_snapshot() {
            return Ok(snapshot);
        }
        let wanted = self.version.load(Ordering::SeqCst);
        let connection = self.db.open()?;
        let snapshot = Arc::new(build_snapshot(&connection)?);
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = Some(Cached {
            version: wanted,
            snapshot: Arc::clone(&snapshot),
        });
        Ok(snapshot)
    }

    /// The cached snapshot when no write happened since it was built; never touches the database
    /// or the blocking pool.
    fn cached_snapshot(&self) -> Option<Arc<RoutingSnapshot>> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .as_ref()
            .filter(|cached| cached.version == self.version.load(Ordering::SeqCst))
            .map(|cached| Arc::clone(&cached.snapshot))
    }

    /// Async wrapper of [`LlmRuntime::snapshot_blocking`]: a cache hit answers on the calling
    /// task, only a miss hops to the blocking pool.
    ///
    /// # Errors
    /// As [`LlmRuntime::snapshot_blocking`], or [`AppError::Internal`] if the task panicked.
    pub async fn snapshot(self: &Arc<Self>) -> Result<Arc<RoutingSnapshot>, AppError> {
        if let Some(snapshot) = self.cached_snapshot() {
            return Ok(snapshot);
        }
        let this = Arc::clone(self);
        tokio::task::spawn_blocking(move || this.snapshot_blocking())
            .await
            .map_err(|error| AppError::internal(format!("snapshot task failed: {error}")))?
    }

    /// The client token hashes, rebuilt first if a write happened since they were read (so a
    /// created, rotated, revoked or deleted client applies to the very next request once its
    /// use-case called [`LlmRuntime::touch`]).
    ///
    /// Blocking SQLite work: call from `spawn_blocking` or a blocking context.
    ///
    /// # Errors
    /// Any error of opening the database or reading the clients.
    pub fn client_hashes_blocking(&self) -> Result<Arc<Vec<ClientHashEntry>>, AppError> {
        if let Some(entries) = self.cached_clients() {
            return Ok(entries);
        }
        let wanted = self.version.load(Ordering::SeqCst);
        let entries = Arc::new(store::client_hashes_snapshot(&*self.db.open()?)?);
        *self.clients.lock().unwrap_or_else(PoisonError::into_inner) = Some(CachedClients {
            version: wanted,
            entries: Arc::clone(&entries),
        });
        Ok(entries)
    }

    /// The cached client hashes when no write happened since they were read; never touches the
    /// database or the blocking pool.
    fn cached_clients(&self) -> Option<Arc<Vec<ClientHashEntry>>> {
        let cache = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .as_ref()
            .filter(|cached| cached.version == self.version.load(Ordering::SeqCst))
            .map(|cached| Arc::clone(&cached.entries))
    }

    /// Async wrapper of [`LlmRuntime::client_hashes_blocking`]: a cache hit answers on the
    /// calling task, only a miss hops to the blocking pool.
    ///
    /// # Errors
    /// As [`LlmRuntime::client_hashes_blocking`], or [`AppError::Internal`] if the task panicked.
    pub async fn client_hashes(self: &Arc<Self>) -> Result<Arc<Vec<ClientHashEntry>>, AppError> {
        if let Some(entries) = self.cached_clients() {
            return Ok(entries);
        }
        let this = Arc::clone(self);
        tokio::task::spawn_blocking(move || this.client_hashes_blocking())
            .await
            .map_err(|error| AppError::internal(format!("client task failed: {error}")))?
    }
}

/// Circuit thresholds from the settings in force.
#[must_use]
pub fn circuit_config(settings: &AiGatewaySettings) -> CircuitConfig {
    let seconds = |value: u32| Duration::from_secs(u64::from(value));
    CircuitConfig {
        failure_threshold: u32::from(settings.circuit_failure_threshold),
        window: seconds(settings.circuit_window_s),
        open_for: seconds(settings.circuit_open_s),
        open_max: seconds(settings.circuit_open_max_s),
    }
}

/// Reads the whole routing configuration with `connection`, extra provider headers resolved
/// (a provider whose header secret is missing is recorded with the error and skipped by routing,
/// so one broken header never stops the others).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn build_snapshot(connection: &DbConnection) -> Result<RoutingSnapshot, AppError> {
    let providers: std::collections::HashMap<String, store::Provider> =
        store::list_providers(connection)?
            .into_iter()
            .map(|provider| (provider.id.clone(), provider))
            .collect();
    let mut headers = ProviderHeaders::default();
    for provider in providers
        .values()
        .filter(|provider| !provider.quirks.extra_headers.is_empty())
    {
        headers.insert(
            &provider.id,
            store::resolve_provider_headers(connection, provider),
        );
    }
    let mut keys: std::collections::HashMap<String, Vec<store::ResolvedKey>> =
        std::collections::HashMap::new();
    let mut key_provider: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for key in store::list_resolved_keys(connection)? {
        // Disabled keys still say which models some key of the provider lists.
        key_provider.insert(key.key.id.clone(), key.key.provider_id.clone());
        if key.key.enabled {
            keys.entry(key.key.provider_id.clone())
                .or_default()
                .push(key);
        }
    }
    // A provider that takes no key (Ollama, LM Studio) routes through one implicit,
    // keyless target; it is never stored, so nothing persists under its id.
    for provider in providers
        .values()
        .filter(|provider| provider.auth == crate::llm::wire::AuthStyle::None)
    {
        keys.entry(provider.id.clone())
            .or_insert_with(|| vec![store::ResolvedKey::keyless(&provider.id)]);
    }
    let models = store::list_models(connection, None)?;
    let key_scopes = KeyScopes::new(store::list_key_models(connection, None)?, &key_provider)
        .with_confirmed(&models);
    Ok(RoutingSnapshot {
        providers,
        keys,
        models,
        gateway_fallbacks: store::list_fallbacks(connection)?,
        legacy_names: store::list_legacy_names(connection)?
            .into_iter()
            .map(|legacy| (legacy.name, legacy.target))
            .collect(),
        former_names: store::list_former_names(connection, 0)?
            .into_iter()
            .map(|former| {
                let name = former.name.trim().to_lowercase();
                (name, (former.provider_id, former.expires_at_ms))
            })
            .collect(),
        headers,
        key_scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use spanbay_base::secrets::SecretStore;
    use spanbay_store::secrets_sqlite::SqliteSecretStore;

    fn runtime(settings: Settings) -> LlmRuntime {
        LlmRuntime::new(
            DbHandle::Unavailable,
            Arc::new(settings),
            Arc::new(SqliteSecretStore::in_memory().unwrap()),
        )
        .unwrap()
    }

    #[test]
    fn a_bad_proxy_error_never_echoes_the_password() {
        let runtime = runtime(Settings::default());
        for provider_proxy in ["ftp://user:hunter2@proxy.test:1", "user:hunter2@proxy.test"] {
            let error = runtime
                .http_for("https://example.test", provider_proxy)
                .unwrap_err();
            assert!(matches!(error, AppError::Validation { .. }), "{error:?}");
            assert!(!error.message().contains("hunter2"), "{}", error.message());
        }
        let mut settings = Settings::default();
        settings.network.proxy_url = "ftp://user:hunter2@proxy.test:1".into();
        let error = self::runtime(settings)
            .http_for("https://example.test", "")
            .unwrap_err();
        assert!(!error.message().contains("hunter2"), "{}", error.message());
    }

    #[test]
    fn debug_output_hides_the_stored_secrets() {
        let secrets = Arc::new(SqliteSecretStore::in_memory().unwrap());
        secrets.put("llm.key.k1", "pdk_super_secret").unwrap();
        let runtime = LlmRuntime::new(
            DbHandle::Unavailable,
            Arc::new(Settings::default()),
            secrets,
        )
        .unwrap();
        let text = format!("{runtime:?}");
        assert!(!text.contains("pdk_super_secret"), "{text}");
    }

    #[test]
    fn the_snapshot_carries_each_keys_stored_secret_into_its_targets() {
        use crate::llm::store::{Provider, ProviderKey};
        use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint, Quirks};

        let c = spanbay_store::storage::migrated_memory_connection();
        store::save_provider(
            &c,
            &Provider {
                id: "p1".into(),
                name: "P1".into(),
                preset_id: None,
                category: "custom".into(),
                base_url: "https://example.test".into(),
                auth: AuthStyle::Bearer,
                endpoints: vec![ProviderEndpoint {
                    protocol: Protocol::OpenaiChat,
                    path_prefix: "/v1".into(),
                    auth: None,
                    url_mode: Default::default(),
                    enabled: true,
                }],
                models_endpoint: None,
                quirks: Quirks::default(),
                proxy_url: String::new(),
                enabled: true,
                position: 0,
                notes: String::new(),
                created_at_ms: 0,
                updated_at_ms: 0,
                ..Default::default()
            },
            1,
        )
        .unwrap();
        for (id, secret) in [("k1", "sk-first-111111"), ("k2", "sk-second-222222")] {
            let key = ProviderKey {
                id: id.into(),
                provider_id: "p1".into(),
                name: id.into(),
                secret_hint: String::new(),
                enabled: true,
                position: i64::from(id == "k2"),
                created_at_ms: 0,
                updated_at_ms: 0,
                ..Default::default()
            };
            store::save_key(&c, &key, Some(secret), 1).unwrap();
        }
        let snapshot = build_snapshot(&c).unwrap();
        let targets = snapshot.direct_targets("p1", None, "m").unwrap();
        let pairs: Vec<(&str, &str)> = targets
            .iter()
            .map(|t| (t.key_id.as_str(), t.secret.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [("k1", "sk-first-111111"), ("k2", "sk-second-222222")]
        );
    }

    #[test]
    fn circuit_config_follows_every_failover_setting() {
        let mut gateway = AiGatewaySettings::default();
        assert_eq!(circuit_config(&gateway), CircuitConfig::default());
        gateway.circuit_failure_threshold = 5;
        gateway.circuit_window_s = 12;
        gateway.circuit_open_s = 7;
        gateway.circuit_open_max_s = 90;
        assert_eq!(
            circuit_config(&gateway),
            CircuitConfig {
                failure_threshold: 5,
                window: Duration::from_secs(12),
                open_for: Duration::from_secs(7),
                open_max: Duration::from_secs(90),
            }
        );
    }

    fn provider_with_header(c: &DbConnection) {
        use crate::llm::store::Provider;
        use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint, Quirks};
        let mut quirks = Quirks::default();
        quirks
            .extra_headers
            .insert("x-org".into(), "org-secret-7".into());
        store::save_provider(
            c,
            &Provider {
                id: "p1".into(),
                name: "P1".into(),
                category: "custom".into(),
                base_url: "https://example.test".into(),
                auth: AuthStyle::Bearer,
                endpoints: vec![ProviderEndpoint {
                    protocol: Protocol::OpenaiChat,
                    path_prefix: "/v1".into(),
                    auth: None,
                    url_mode: Default::default(),
                    enabled: true,
                }],
                quirks,
                enabled: true,
                ..Default::default()
            },
            1,
        )
        .unwrap();
    }

    #[test]
    fn the_snapshot_resolves_provider_header_secrets_into_targets() {
        use crate::llm::store::ProviderKey;
        let c = spanbay_store::storage::migrated_memory_connection();
        provider_with_header(&c);
        let key = ProviderKey {
            id: "k1".into(),
            provider_id: "p1".into(),
            name: "k1".into(),
            enabled: true,
            ..Default::default()
        };
        store::save_key(&c, &key, Some("sk-111111111"), 1).unwrap();
        let snapshot = build_snapshot(&c).unwrap();
        let target = snapshot.target_for("p1", None, "m", None).unwrap();
        assert_eq!(target.quirks.extra_headers["x-org"], "org-secret-7");
        // The stored provider keeps only the placeholder.
        assert!(
            snapshot.providers["p1"].quirks.extra_headers["x-org"].starts_with("{{secret:"),
            "{:?}",
            snapshot.providers["p1"].quirks.extra_headers
        );
        let debug = format!("{snapshot:?}");
        assert!(!debug.contains("org-secret-7"), "{debug}");
    }

    #[test]
    fn a_missing_header_secret_breaks_only_that_provider_and_names_the_header() {
        use crate::llm::store::ProviderKey;
        let c = spanbay_store::storage::migrated_memory_connection();
        provider_with_header(&c);
        spanbay_store::storage::secrets::delete(&c, "llm.p1.header.x-org").unwrap();
        let key = ProviderKey {
            id: "k1".into(),
            provider_id: "p1".into(),
            name: "k1".into(),
            enabled: true,
            ..Default::default()
        };
        store::save_key(&c, &key, Some("sk-111111111"), 1).unwrap();
        let snapshot = build_snapshot(&c).unwrap();
        let error = snapshot.target_for("p1", None, "m", None).unwrap_err();
        assert!(error.message().contains("x-org"), "{}", error.message());
        assert!(!error.message().contains("org-secret-7"));
    }

    #[test]
    fn the_log_writer_is_one_per_runtime_and_does_not_keep_the_runtime_alive() {
        let runtime = Arc::new(runtime(Settings::default()));
        let first = runtime.log().unwrap();
        let again = runtime.log_with(1).unwrap();
        assert!(
            Arc::ptr_eq(&first, &again),
            "one writer, whatever the capacity asked"
        );
        let weak = Arc::downgrade(&runtime);
        drop((first, again));
        drop(runtime);
        assert!(
            weak.upgrade().is_none(),
            "the writer thread holds the runtime only weakly"
        );
    }

    /// An unavailable database makes every miss an error, so only a hit can answer. The blocking
    /// pool has one thread here and it is parked on a channel until the end, so a read that
    /// reached the pool cannot finish (it queues behind the parked job) and one that never did
    /// answers on the calling task.
    #[test]
    fn a_cache_hit_answers_without_the_blocking_pool_and_a_miss_does_not() {
        use futures_util::FutureExt;
        use std::sync::mpsc;

        let pool = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .build()
            .expect("tokio runtime");
        pool.block_on(async {
            let runtime = Arc::new(self::runtime(Settings::default()));
            let connection = spanbay_store::storage::migrated_memory_connection();
            let snapshot = Arc::new(build_snapshot(&connection).unwrap());
            let version = runtime.version();
            *runtime.cache.lock().unwrap() = Some(Cached {
                version,
                snapshot: Arc::clone(&snapshot),
            });
            let entries = Arc::new(Vec::new());
            *runtime.clients.lock().unwrap() = Some(CachedClients {
                version,
                entries: Arc::clone(&entries),
            });
            // The pool's only thread is busy until `release` is sent: nothing else that reached
            // the pool can run, so `now_or_never` answers exactly the futures that stayed on this
            // task, whatever the machine's load.
            let (release, parked) = mpsc::channel::<()>();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = parked.recv();
            });
            let hit = runtime.snapshot().now_or_never().unwrap().unwrap();
            assert!(Arc::ptr_eq(&hit, &snapshot));
            let hit = runtime.client_hashes().now_or_never().unwrap().unwrap();
            assert!(Arc::ptr_eq(&hit, &entries));
            runtime.touch();
            assert!(
                runtime.snapshot().now_or_never().is_none(),
                "a stale snapshot is rebuilt"
            );
            assert!(
                runtime.client_hashes().now_or_never().is_none(),
                "stale hashes are re-read"
            );
            release.send(()).unwrap();
            blocker.await.unwrap();
            assert!(runtime.snapshot().await.is_err());
        });
    }
}
