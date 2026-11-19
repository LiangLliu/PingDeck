//! Baseline schema initialization. There are no migrations: the stored `user_version` is a
//! fingerprint of the DDL, and a database built from any other DDL is refused.

use rusqlite::{Connection, TransactionBehavior};

use super::{StorageError, WorkspaceStore};

/// The `user_version` of a database built by this binary: the [`fingerprint`] of the main and
/// vault DDL.
///
/// `cargo xtask schema-fingerprint` (run by `scripts/smoke.sh`, guarded by `xtask` tests)
/// recomputes this value from the top-level `storage/*.rs` files. Each of `SCHEMA_DDL` (an
/// `r#"..."#` raw string) and `SECRETS_COLUMNS` (a plain `"..."` literal without escape sequences)
/// must be declared exactly once, at the start of a line, in one of those files. Keep the two
/// fingerprint algorithms in step.
pub(super) const CURRENT_SCHEMA_VERSION: i64 = fingerprint(&[SCHEMA_DDL, SECRETS_COLUMNS]);

/// FNV-1a of `parts`, kept to 31 bits so it is a positive, non-zero `user_version`. Outside
/// quotes, `--` comments are skipped and whitespace only counts as one space between two
/// identifier characters, so layout edits keep the fingerprint. Written with `while` loops
/// because it must be a `const fn`.
pub(super) const fn fingerprint(parts: &[&str]) -> i64 {
    const fn mix(hash: u32, byte: u8) -> u32 {
        (hash ^ byte as u32).wrapping_mul(0x0100_0193)
    }
    const fn is_word(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'_'
    }
    let mut hash: u32 = 0x811c_9dc5;
    let (mut in_quote, mut space, mut prev) = (false, false, 0u8);
    let mut part = 0;
    while part < parts.len() {
        let bytes = parts[part].as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            index += 1;
            if !in_quote && byte == b'-' && index < bytes.len() && bytes[index] == b'-' {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                space = true;
                continue;
            }
            if !in_quote && byte.is_ascii_whitespace() {
                space = true;
                continue;
            }
            if space && is_word(prev) && is_word(byte) {
                hash = mix(hash, b' ');
            }
            space = false;
            prev = byte;
            if byte == b'\'' {
                in_quote = !in_quote;
            }
            hash = mix(hash, byte);
        }
        part += 1;
    }
    let value = (hash & 0x7fff_ffff) as i64;
    if value == 0 { 1 } else { value }
}

/// Columns of the secrets table, in the vault file or (in-memory stores) in main.
pub(super) const SECRETS_COLUMNS: &str = "secret_ref TEXT PRIMARY KEY,
    value_text TEXT NOT NULL,
    hint TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL";

/// Pragmas every main-database connection sets on open; `busy_timeout` is in milliseconds.
///
/// `secure_delete = FAST` zeroes deleted cell content on pages SQLite rewrites anyway, so a
/// deleted history row or request stops lingering in the file at no extra I/O; pages that move to
/// the freelist keep their bytes until reused or until a compaction (`VACUUM`). The vault, which
/// is attached after these run, turns it fully `ON` for itself.
pub(super) const CONNECTION_PRAGMAS: &str = "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; \
     PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000; PRAGMA secure_delete = FAST;";

/// Prepared statements each connection keeps cached (`prepare_cached`).
pub(super) const STATEMENT_CACHE_CAPACITY: usize = 64;

impl WorkspaceStore {
    pub(super) fn apply_pragmas(&self) -> Result<(), StorageError> {
        self.connection.execute_batch(CONNECTION_PRAGMAS)?;
        Ok(())
    }

    /// The stored `user_version`: [`CURRENT_SCHEMA_VERSION`] once this build created the schema,
    /// 0 for an empty database.
    pub fn schema_version(&self) -> Result<i64, StorageError> {
        Ok(self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }
}

/// Refuses a database built from another DDL and creates the baseline schema in a fresh one.
pub(super) fn ensure_schema(
    connection: &Connection,
    stored: i64,
    file_backed: bool,
) -> Result<(), StorageError> {
    if stored != 0 && stored != CURRENT_SCHEMA_VERSION {
        return Err(StorageError::UnsupportedSchemaVersion { version: stored });
    }
    // A file database whose `secrets` table is still in main predates the vault: refuse it, even
    // when `stored` (reported as the version) equals the current fingerprint.
    if file_backed && table_exists(connection, "secrets")? {
        return Err(StorageError::UnsupportedSchemaVersion { version: stored });
    }
    if stored == 0 {
        // Take the write lock first so a concurrent first open waits, then sees the schema.
        let transaction =
            rusqlite::Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
        let now: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if now == CURRENT_SCHEMA_VERSION {
            return Ok(());
        }
        if now != 0 {
            return Err(StorageError::UnsupportedSchemaVersion { version: now });
        }
        transaction.execute_batch(SCHEMA_DDL)?;
        super::vault::create_memory_secrets(&transaction)?;
        transaction.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
        transaction.commit()?;
    }
    Ok(())
}

/// Whether `main` has a table named `table_name`.
pub(super) fn table_exists(
    connection: &Connection,
    table_name: &str,
) -> Result<bool, StorageError> {
    Ok(connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table_name],
        |row| row.get::<_, i64>(0),
    )? > 0)
}

const SCHEMA_DDL: &str = r#"
-- ==== workspace ====
CREATE TABLE workspaces (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_opened_at TEXT
);

CREATE TABLE collections (
    id                 TEXT PRIMARY KEY,
    workspace_id       TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    description        TEXT NOT NULL DEFAULT '',
    auth_json          TEXT NOT NULL DEFAULT '"Inherit"',
    variables_json     TEXT NOT NULL DEFAULT '[]',
    pre_request_script TEXT NOT NULL DEFAULT '',
    test_script        TEXT NOT NULL DEFAULT '',
    position           INTEGER NOT NULL DEFAULT 0,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_collections_workspace_id ON collections(workspace_id);

CREATE TABLE folders (
    id                 TEXT PRIMARY KEY,
    collection_id      TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    parent_folder_id   TEXT REFERENCES folders(id) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    description        TEXT NOT NULL DEFAULT '',
    auth_json          TEXT NOT NULL DEFAULT '"Inherit"',
    pre_request_script TEXT NOT NULL DEFAULT '',
    test_script        TEXT NOT NULL DEFAULT '',
    position           INTEGER NOT NULL DEFAULT 0,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_folders_collection_id ON folders(collection_id);
CREATE INDEX idx_folders_parent_folder_id ON folders(parent_folder_id);

CREATE TABLE requests (
    id                   TEXT PRIMARY KEY,
    workspace_id         TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    collection_id        TEXT REFERENCES collections(id) ON DELETE CASCADE,
    folder_id            TEXT REFERENCES folders(id) ON DELETE SET NULL,
    name                 TEXT NOT NULL,
    protocol             TEXT NOT NULL,
    method               TEXT NOT NULL,
    url                  TEXT NOT NULL,
    query_params_json    TEXT NOT NULL DEFAULT '[]',
    path_variables_json  TEXT NOT NULL DEFAULT '[]',
    headers_json         TEXT NOT NULL DEFAULT '[]',
    auth_json            TEXT NOT NULL DEFAULT '"None"',
    body_json            TEXT NOT NULL DEFAULT '"None"',
    network_options_json TEXT NOT NULL DEFAULT '{}',
    assertions_json      TEXT NOT NULL DEFAULT '[]',
    description          TEXT NOT NULL DEFAULT '',
    pre_request_script   TEXT NOT NULL DEFAULT '',
    test_script          TEXT NOT NULL DEFAULT '',
    position             INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    mode                 TEXT NOT NULL DEFAULT 'http',
    ai_json              TEXT,
    agent_touched_ms     INTEGER
);

CREATE INDEX idx_requests_collection_id ON requests(collection_id);
CREATE INDEX idx_requests_folder_id ON requests(folder_id);
CREATE INDEX idx_requests_workspace_id ON requests(workspace_id);

CREATE TABLE environments (
    id                   TEXT PRIMARY KEY,
    workspace_id         TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name                 TEXT NOT NULL,
    confirm_before_send  INTEGER NOT NULL DEFAULT 0 CHECK (confirm_before_send IN (0, 1)),
    confirm_safe_methods INTEGER NOT NULL DEFAULT 0 CHECK (confirm_safe_methods IN (0, 1)),
    declared_hosts_json  TEXT NOT NULL DEFAULT '[]',
    guard_changed_at_ms  INTEGER,
    updated_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE UNIQUE INDEX uq_environments_workspace_name ON environments(workspace_id, name COLLATE NOCASE);

CREATE TABLE environment_variables (
    environment_id TEXT NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    position       INTEGER NOT NULL,
    key            TEXT NOT NULL,
    value          TEXT,
    is_secret      INTEGER NOT NULL CHECK (is_secret IN (0, 1)),
    enabled        INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    note           TEXT NOT NULL DEFAULT '',
    variable_id    TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (environment_id, key)
);

CREATE INDEX idx_environment_variables_environment_id
    ON environment_variables(environment_id, position);

CREATE TABLE global_variables (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    position     INTEGER NOT NULL,
    key          TEXT NOT NULL,
    value        TEXT,
    is_secret    INTEGER NOT NULL CHECK (is_secret IN (0, 1)),
    enabled      INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    note         TEXT NOT NULL DEFAULT '',
    variable_id  TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (workspace_id, key)
);

CREATE TABLE cookie_jar (
    id                TEXT PRIMARY KEY,
    workspace_id      TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    source_request_id TEXT REFERENCES requests(id) ON DELETE SET NULL,
    name              TEXT NOT NULL,
    value             TEXT NOT NULL,
    domain            TEXT NOT NULL,
    path              TEXT NOT NULL,
    host_only         INTEGER NOT NULL CHECK (host_only IN (0, 1)),
    expires           TEXT,
    max_age           TEXT,
    secure            INTEGER NOT NULL CHECK (secure IN (0, 1)),
    http_only         INTEGER NOT NULL CHECK (http_only IN (0, 1)),
    same_site         TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    UNIQUE(workspace_id, domain, path, name)
);

CREATE INDEX idx_cookie_jar_source_request_id ON cookie_jar(source_request_id);

CREATE TABLE tls_certificates (
    id             TEXT PRIMARY KEY,
    workspace_id   TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    kind           TEXT NOT NULL CHECK (kind IN ('ca', 'client')),
    host_pattern   TEXT NOT NULL DEFAULT '',
    cert_path      TEXT NOT NULL,
    key_path       TEXT,
    passphrase_ref TEXT,
    enabled        INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1))
);

CREATE INDEX idx_tls_certificates_workspace ON tls_certificates(workspace_id, kind);

CREATE TABLE execution_history (
    id                         TEXT PRIMARY KEY,
    workspace_id               TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    request_id                 TEXT REFERENCES requests(id) ON DELETE SET NULL,
    environment_id             TEXT REFERENCES environments(id) ON DELETE SET NULL,
    result_kind                TEXT NOT NULL DEFAULT 'Success' CHECK (result_kind IN ('Success', 'Failure')),
    request_name               TEXT NOT NULL,
    method                     TEXT NOT NULL,
    request_url                TEXT NOT NULL,
    status_code                INTEGER NOT NULL,
    status_text                TEXT NOT NULL,
    final_url                  TEXT NOT NULL,
    elapsed_ms                 INTEGER NOT NULL,
    body_size_bytes            INTEGER NOT NULL,
    attempt_count              INTEGER NOT NULL DEFAULT 1,
    response_body_preview      TEXT NOT NULL,
    response_body_truncated    INTEGER NOT NULL CHECK (response_body_truncated IN (0, 1)),
    error_kind                 TEXT,
    error_message              TEXT,
    executed_at                TEXT NOT NULL,
    request_snapshot_json      TEXT,
    source                     TEXT NOT NULL DEFAULT 'request' CHECK (source IN ('request', 'runner', 'flow', 'mock', 'agent')),
    source_name                TEXT NOT NULL DEFAULT '',
    source_id                  TEXT NOT NULL DEFAULT '',
    run_id                     TEXT,
    protocol                   TEXT NOT NULL DEFAULT '',
    timing_json                TEXT,
    environment_name           TEXT NOT NULL DEFAULT '',
    response_headers_json      TEXT,
    response_headers_truncated INTEGER NOT NULL DEFAULT 0 CHECK (response_headers_truncated IN (0, 1)),
    sent_request_json          TEXT,
    call_chain_json            TEXT,
    connect_phase              INTEGER,
    check_passed               INTEGER,
    check_failed               INTEGER,
    origin_ref                 TEXT
);

CREATE INDEX idx_execution_history_environment_id ON execution_history(environment_id);
CREATE INDEX idx_execution_history_request_id ON execution_history(request_id);
CREATE INDEX idx_execution_history_workspace_executed
    ON execution_history(workspace_id, executed_at DESC, id DESC);
CREATE INDEX idx_execution_history_workspace_run ON execution_history(workspace_id, run_id);
CREATE INDEX idx_execution_history_workspace_source
    ON execution_history(workspace_id, source, executed_at DESC, id DESC);

CREATE TABLE collection_runs (
    id            TEXT PRIMARY KEY,
    workspace_id  TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    started_at_ms INTEGER NOT NULL,
    summary_json  TEXT NOT NULL
);

CREATE INDEX idx_collection_runs_collection ON collection_runs(collection_id, started_at_ms DESC);

CREATE TABLE flows (
    id            TEXT PRIMARY KEY,
    workspace_id  TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',
    steps_json    TEXT NOT NULL DEFAULT '[]',
    updated_at_ms INTEGER NOT NULL,
    options_json  TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX idx_flows_workspace ON flows(workspace_id, name);

CREATE TABLE flow_runs (
    id            TEXT PRIMARY KEY,
    flow_id       TEXT NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
    started_at_ms INTEGER NOT NULL,
    summary_json  TEXT NOT NULL
);

CREATE INDEX idx_flow_runs_flow ON flow_runs(flow_id, started_at_ms DESC);

CREATE TABLE mock_servers (
    id                     TEXT PRIMARY KEY,
    workspace_id           TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name                   TEXT NOT NULL,
    port                   INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    endpoint_prefix        TEXT NOT NULL DEFAULT '',
    latency_ms             INTEGER NOT NULL DEFAULT 0 CHECK (latency_ms BETWEEN 0 AND 60000),
    cors_enabled           INTEGER NOT NULL DEFAULT 1 CHECK (cors_enabled IN (0, 1)),
    headers_json           TEXT NOT NULL DEFAULT '[]',
    routes_json            TEXT NOT NULL DEFAULT '[]',
    data_buckets_json      TEXT NOT NULL DEFAULT '[]',
    callbacks_json         TEXT NOT NULL DEFAULT '[]',
    proxy_enabled          INTEGER NOT NULL DEFAULT 0 CHECK (proxy_enabled IN (0, 1)),
    proxy_base_url         TEXT NOT NULL DEFAULT '',
    proxy_remove_prefix    INTEGER NOT NULL DEFAULT 0 CHECK (proxy_remove_prefix IN (0, 1)),
    proxy_req_headers_json TEXT NOT NULL DEFAULT '[]',
    proxy_res_headers_json TEXT NOT NULL DEFAULT '[]',
    updated_at_ms          INTEGER NOT NULL,
    environment_id         TEXT REFERENCES environments(id) ON DELETE SET NULL
);

CREATE INDEX idx_mock_servers_workspace ON mock_servers(workspace_id, name);

CREATE TABLE tab_sessions (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    payload      TEXT NOT NULL
);

CREATE TABLE sync_export_files (
    directory TEXT NOT NULL,
    path      TEXT NOT NULL,
    sha256    TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (directory, path)
);

-- ==== llm providers ====
CREATE TABLE llm_providers (
    id                      TEXT PRIMARY KEY,
    name                    TEXT NOT NULL,
    preset_id               TEXT,
    category                TEXT NOT NULL DEFAULT 'official',
    base_url                TEXT NOT NULL,
    auth                    TEXT NOT NULL DEFAULT 'bearer',
    endpoints_json          TEXT NOT NULL DEFAULT '[]',
    models_endpoint         TEXT,
    quirks_json             TEXT NOT NULL DEFAULT '{}',
    proxy_url               TEXT NOT NULL DEFAULT '',
    enabled                 INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    position                INTEGER NOT NULL DEFAULT 0,
    notes                   TEXT NOT NULL DEFAULT '',
    gateway_family          TEXT NOT NULL DEFAULT 'unknown_compatible',
    primary_protocol        TEXT,
    quota_per_unit          REAL,
    display_in_currency     INTEGER CHECK (display_in_currency IN (0, 1)),
    custom_balance_json     TEXT,
    balance_off             INTEGER NOT NULL DEFAULT 0 CHECK (balance_off IN (0, 1)),
    key_strategy            TEXT NOT NULL DEFAULT 'primary' CHECK (key_strategy IN ('primary', 'round_robin', 'random')),
    key_cooldown            INTEGER NOT NULL DEFAULT 1 CHECK (key_cooldown IN (0, 1)),
    sniff_evidence_json     TEXT NOT NULL DEFAULT '[]',
    sniff_summary_json      TEXT,
    model_drift_json        TEXT,
    needs_resniff           INTEGER NOT NULL DEFAULT 0 CHECK (needs_resniff IN (0, 1)),
    review_pending_since_ms INTEGER,
    created_at_ms           INTEGER NOT NULL,
    updated_at_ms           INTEGER NOT NULL
);

CREATE TABLE llm_provider_former_names (
    name          TEXT PRIMARY KEY COLLATE NOCASE,
    provider_id   TEXT NOT NULL REFERENCES llm_providers(id) ON DELETE CASCADE,
    expires_at_ms INTEGER NOT NULL
);

CREATE TABLE llm_provider_tombstones (
    id TEXT PRIMARY KEY COLLATE NOCASE
) WITHOUT ROWID;

CREATE TABLE llm_keys (
    id                  TEXT PRIMARY KEY,
    provider_id         TEXT NOT NULL REFERENCES llm_providers(id) ON DELETE CASCADE,
    name                TEXT NOT NULL DEFAULT '',
    secret_hint         TEXT NOT NULL DEFAULT '',
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    position            INTEGER NOT NULL DEFAULT 0,
    notes               TEXT NOT NULL DEFAULT '',
    group_name          TEXT NOT NULL DEFAULT '',
    api_user_id         TEXT,
    protocols_json      TEXT,
    preferred_protocol  TEXT,
    models_listed_at_ms INTEGER,
    balance_json        TEXT,
    balance_error_json  TEXT,
    balance_attempt_ms  INTEGER,
    last_test_json      TEXT,
    created_at_ms       INTEGER NOT NULL,
    updated_at_ms       INTEGER NOT NULL
);

CREATE INDEX idx_llm_keys_provider ON llm_keys(provider_id, position);

CREATE TABLE llm_provider_models (
    provider_id            TEXT NOT NULL REFERENCES llm_providers(id) ON DELETE CASCADE,
    model_id               TEXT NOT NULL,
    source                 TEXT NOT NULL CHECK (source IN ('preset', 'live', 'manual')),
    role                   TEXT,
    notes                  TEXT,
    enabled                INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    capabilities_json      TEXT NOT NULL DEFAULT '{}',
    context                INTEGER,
    max_output             INTEGER,
    context_declared       INTEGER NOT NULL DEFAULT 0 CHECK (context_declared IN (0, 1)),
    max_output_declared    INTEGER NOT NULL DEFAULT 0 CHECK (max_output_declared IN (0, 1)),
    listed_context         INTEGER,
    listed_max_output      INTEGER,
    measured_context       INTEGER,
    presence               TEXT NOT NULL DEFAULT 'listed' CHECK (presence IN ('listed', 'not_in_provider')),
    first_seen_ms          INTEGER,
    new_cleared_at_ms      INTEGER,
    listed_confirmed_at_ms INTEGER,
    PRIMARY KEY (provider_id, model_id)
);

CREATE TABLE llm_key_models (
    key_id         TEXT NOT NULL REFERENCES llm_keys(id) ON DELETE CASCADE,
    model_id       TEXT NOT NULL,
    protocols_json TEXT,
    acked_at_ms    INTEGER,
    PRIMARY KEY (key_id, model_id)
);

CREATE TABLE llm_model_fallbacks (
    provider_id        TEXT NOT NULL REFERENCES llm_providers(id) ON DELETE CASCADE,
    model_id           TEXT NOT NULL,
    position           INTEGER NOT NULL,
    target_provider_id TEXT NOT NULL,
    target_model_id    TEXT NOT NULL,
    PRIMARY KEY (provider_id, model_id, position)
);

CREATE TABLE llm_model_legacy_names (
    name        TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL REFERENCES llm_providers(id) ON DELETE CASCADE,
    model_id    TEXT NOT NULL
);

CREATE TABLE llm_routing_notices (
    id            INTEGER PRIMARY KEY,
    message       TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE llm_gateway_clients (
    id                       TEXT PRIMARY KEY,
    name                     TEXT NOT NULL,
    token_sha256             TEXT NOT NULL UNIQUE,
    token_prefix             TEXT NOT NULL,
    token_last4              TEXT NOT NULL DEFAULT '',
    allow_all                INTEGER NOT NULL DEFAULT 1 CHECK (allow_all IN (0, 1)),
    allowed_models_json      TEXT NOT NULL DEFAULT '[]',
    allowed_aliases_json     TEXT NOT NULL DEFAULT '[]',
    default_model            TEXT,
    unknown_model_to_default INTEGER NOT NULL DEFAULT 0 CHECK (unknown_model_to_default IN (0, 1)),
    created_at_ms            INTEGER NOT NULL,
    last_used_at_ms          INTEGER,
    verified_at_ms           INTEGER,
    revoked_at_ms            INTEGER
);

CREATE TABLE llm_client_applied (
    id            TEXT PRIMARY KEY,
    client_id     TEXT NOT NULL,
    app           TEXT NOT NULL,
    path          TEXT NOT NULL,
    backup_path   TEXT NOT NULL,
    sha256_after  TEXT NOT NULL,
    applied_at_ms INTEGER NOT NULL
);

-- ==== llm log usage probe ====
CREATE TABLE llm_request_log (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms               INTEGER NOT NULL,
    protocol_in         TEXT NOT NULL,
    requested_model     TEXT NOT NULL DEFAULT '',
    provider_id         TEXT,
    provider_name       TEXT,
    key_id              TEXT,
    upstream_model      TEXT,
    protocol_out        TEXT,
    status              INTEGER NOT NULL,
    outcome             TEXT NOT NULL,
    stream              INTEGER NOT NULL DEFAULT 0 CHECK (stream IN (0, 1)),
    latency_ms          INTEGER NOT NULL DEFAULT 0,
    ttft_ms             INTEGER,
    input_tokens        INTEGER,
    output_tokens       INTEGER,
    cached_tokens       INTEGER,
    attempts_json       TEXT NOT NULL DEFAULT '[]',
    error               TEXT,
    source              TEXT NOT NULL DEFAULT 'gateway' CHECK (source IN ('gateway', 'agent', 'api', 'probe')),
    client_id           TEXT,
    session_id          TEXT,
    run_id              TEXT,
    reasoning_tokens    INTEGER,
    cache_write_tokens  INTEGER,
    cost_micro_usd      INTEGER,
    failure_kind        TEXT,
    source_ref          TEXT,
    request_id          TEXT,
    client_name         TEXT,
    peer                TEXT,
    alias               TEXT,
    translated          INTEGER NOT NULL DEFAULT 0 CHECK (translated IN (0, 1)),
    tokens_estimated    INTEGER NOT NULL DEFAULT 0 CHECK (tokens_estimated IN (0, 1)),
    cost_currency       TEXT NOT NULL DEFAULT 'USD',
    price_source        TEXT,
    upstream_request_id TEXT,
    req_bytes           INTEGER,
    resp_bytes          INTEGER,
    timing_json         TEXT,
    has_capture         INTEGER NOT NULL DEFAULT 0 CHECK (has_capture IN (0, 1)),
    replay_of           INTEGER,
    message_id          TEXT,
    purpose             TEXT,
    finish_reason       TEXT,
    price_basis         TEXT
);

CREATE INDEX idx_llm_request_log_key ON llm_request_log(key_id, ts_ms DESC);
CREATE INDEX idx_llm_request_log_provider ON llm_request_log(provider_id, ts_ms DESC);
CREATE UNIQUE INDEX uq_llm_request_log_request_id
    ON llm_request_log(request_id)
    WHERE request_id IS NOT NULL;
CREATE INDEX idx_llm_request_log_ts ON llm_request_log(ts_ms DESC);
CREATE INDEX idx_llm_request_log_run ON llm_request_log(run_id) WHERE run_id IS NOT NULL;

CREATE TABLE llm_debug_captures (
    request_log_id             INTEGER PRIMARY KEY REFERENCES llm_request_log(id) ON DELETE CASCADE,
    ts_ms                      INTEGER NOT NULL,
    client_protocol            TEXT NOT NULL,
    client_headers_json        TEXT NOT NULL,
    client_body_text           TEXT,
    hub_request_json           TEXT,
    upstream_url               TEXT NOT NULL,
    upstream_headers_json      TEXT NOT NULL,
    upstream_req_body_text     TEXT,
    upstream_status            INTEGER,
    upstream_resp_headers_json TEXT,
    upstream_resp_body_text    TEXT,
    phase_timings_json         TEXT NOT NULL DEFAULT '{}',
    bytes_total                INTEGER NOT NULL DEFAULT 0,
    client_resp_headers_json   TEXT,
    client_resp_body_text      TEXT,
    upstream_frames_json       TEXT,
    client_frames_json         TEXT,
    translation_notes_json     TEXT,
    redacted_count             INTEGER NOT NULL DEFAULT 0,
    body_meta_json             TEXT
);

CREATE INDEX idx_llm_debug_captures_bytes ON llm_debug_captures(bytes_total);

CREATE TABLE llm_usage_daily (
    day                TEXT NOT NULL,
    source             TEXT NOT NULL,
    provider_id        TEXT NOT NULL,
    key_id             TEXT NOT NULL,
    model              TEXT NOT NULL,
    client_id          TEXT NOT NULL DEFAULT '',
    alias              TEXT NOT NULL DEFAULT '',
    currency           TEXT NOT NULL DEFAULT '',
    requests           INTEGER NOT NULL DEFAULT 0,
    ok_requests        INTEGER NOT NULL DEFAULT 0,
    input_tokens       INTEGER NOT NULL DEFAULT 0,
    output_tokens      INTEGER NOT NULL DEFAULT 0,
    cached_tokens      INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens   INTEGER NOT NULL DEFAULT 0,
    cost_micro_usd     INTEGER NOT NULL DEFAULT 0,
    latency_ms         INTEGER NOT NULL DEFAULT 0,
    timed_requests     INTEGER NOT NULL DEFAULT 0,
    canceled_requests  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, source, provider_id, key_id, model, client_id, alias, currency)
);

CREATE TABLE llm_usage_minute (
    minute_ms          INTEGER NOT NULL,
    source             TEXT NOT NULL,
    provider_id        TEXT NOT NULL,
    key_id             TEXT NOT NULL,
    model              TEXT NOT NULL,
    client_id          TEXT NOT NULL DEFAULT '',
    alias              TEXT NOT NULL DEFAULT '',
    currency           TEXT NOT NULL DEFAULT '',
    failure_kind       TEXT NOT NULL DEFAULT '',
    requests           INTEGER NOT NULL DEFAULT 0,
    ok_requests        INTEGER NOT NULL DEFAULT 0,
    input_tokens       INTEGER NOT NULL DEFAULT 0,
    output_tokens      INTEGER NOT NULL DEFAULT 0,
    cached_tokens      INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens   INTEGER NOT NULL DEFAULT 0,
    cost_micro_usd     INTEGER NOT NULL DEFAULT 0,
    latency_ms         INTEGER NOT NULL DEFAULT 0,
    timed_requests     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (minute_ms, source, provider_id, key_id, model, client_id, alias,
        currency, failure_kind)
) WITHOUT ROWID;

CREATE TABLE llm_usage_failures (
    day         TEXT NOT NULL,
    source      TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    model       TEXT NOT NULL,
    kind        TEXT NOT NULL,
    count       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, source, provider_id, model, kind)
);

CREATE TABLE llm_probe_runs (
    run_id          TEXT PRIMARY KEY,
    provider_id     TEXT REFERENCES llm_providers(id) ON DELETE CASCADE,
    check_set       TEXT NOT NULL,
    started_at_ms   INTEGER NOT NULL,
    finished_at_ms  INTEGER,
    passed          INTEGER NOT NULL DEFAULT 0,
    total           INTEGER NOT NULL DEFAULT 0,
    summary_json    TEXT NOT NULL DEFAULT '{}',
    previous_run_id TEXT
);

CREATE TABLE llm_probe_history (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id        TEXT NOT NULL,
    key_id        TEXT NOT NULL REFERENCES llm_keys(id) ON DELETE CASCADE,
    model_id      TEXT NOT NULL,
    endpoint      TEXT NOT NULL,
    test          TEXT NOT NULL,
    ok            INTEGER NOT NULL CHECK (ok IN (0, 1)),
    status        INTEGER,
    latency_ms    INTEGER NOT NULL,
    ttft_ms       INTEGER,
    tps           REAL,
    input_tokens  INTEGER,
    output_tokens INTEGER,
    failure_kind  TEXT,
    message       TEXT NOT NULL,
    detail_json   TEXT NOT NULL DEFAULT 'null',
    checked_at_ms INTEGER NOT NULL,
    status_word   TEXT NOT NULL DEFAULT 'ready',
    skip_reason   TEXT
);

CREATE INDEX idx_llm_probe_history_key_model ON llm_probe_history(key_id, model_id);
CREATE INDEX idx_llm_probe_history_run ON llm_probe_history(run_id);

CREATE TABLE llm_probe_results (
    key_id        TEXT NOT NULL REFERENCES llm_keys(id) ON DELETE CASCADE,
    model_id      TEXT NOT NULL,
    endpoint      TEXT NOT NULL,
    test          TEXT NOT NULL,
    ok            INTEGER NOT NULL CHECK (ok IN (0, 1)),
    status        INTEGER,
    latency_ms    INTEGER,
    ttft_ms       INTEGER,
    tps           REAL,
    input_tokens  INTEGER,
    output_tokens INTEGER,
    failure_kind  TEXT,
    message       TEXT NOT NULL DEFAULT '',
    detail_json   TEXT NOT NULL DEFAULT 'null',
    checked_at_ms INTEGER NOT NULL,
    run_id        TEXT,
    status_word   TEXT NOT NULL DEFAULT 'ready',
    skip_reason   TEXT,
    PRIMARY KEY (key_id, model_id, endpoint, test)
);

CREATE TABLE llm_probe_apply_log (
    apply_id      TEXT PRIMARY KEY,
    run_id        TEXT NOT NULL,
    applied_at_ms INTEGER NOT NULL,
    previous_json TEXT NOT NULL
);

-- ==== model catalog ====
CREATE TABLE model_catalog (
    family               TEXT NOT NULL,
    model_id             TEXT NOT NULL,
    name                 TEXT NOT NULL DEFAULT '',
    family_name          TEXT NOT NULL DEFAULT '',
    publisher            TEXT NOT NULL DEFAULT '',
    publisher_name       TEXT NOT NULL DEFAULT '',
    model_key            TEXT NOT NULL DEFAULT '',
    canonical_id         TEXT NOT NULL DEFAULT '',
    kind                 TEXT NOT NULL DEFAULT 'chat' CHECK (kind IN ('chat', 'embedding')),
    release_date         TEXT,
    context_tokens       INTEGER,
    max_output_tokens    INTEGER,
    input_modalities     TEXT,
    reasoning            INTEGER NOT NULL DEFAULT 0 CHECK (reasoning IN (0, 1)),
    vision               INTEGER NOT NULL DEFAULT 0 CHECK (vision IN (0, 1)),
    tools                INTEGER NOT NULL DEFAULT 0 CHECK (tools IN (0, 1)),
    prompt_caching       INTEGER NOT NULL DEFAULT 0 CHECK (prompt_caching IN (0, 1)),
    input_per_mtok       REAL,
    output_per_mtok      REAL,
    cache_read_per_mtok  REAL,
    cache_write_per_mtok REAL,
    reasoning_per_mtok   REAL,
    currency             TEXT NOT NULL DEFAULT 'USD',
    source               TEXT NOT NULL CHECK (source IN ('models.dev', 'user')),
    synced_at_ms         INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (family, model_id)
);

CREATE INDEX idx_model_catalog_key ON model_catalog (model_key);
CREATE INDEX idx_model_catalog_model ON model_catalog (model_id);
CREATE INDEX idx_model_catalog_model_lower ON model_catalog (lower(model_id));

CREATE TABLE model_catalog_sync (
    id                 INTEGER PRIMARY KEY CHECK (id = 1),
    origin             TEXT NOT NULL DEFAULT 'builtin' CHECK (origin IN ('builtin', 'models.dev')),
    snapshot_at_ms     INTEGER NOT NULL DEFAULT 0,
    last_success_ms    INTEGER,
    last_attempt_ms    INTEGER,
    last_error         TEXT,
    last_added         INTEGER NOT NULL DEFAULT 0,
    last_updated       INTEGER NOT NULL DEFAULT 0,
    last_unchanged     INTEGER NOT NULL DEFAULT 0,
    last_total_remote  INTEGER NOT NULL DEFAULT 0,
    last_duration_ms   INTEGER NOT NULL DEFAULT 0,
    auto_interval_days INTEGER NOT NULL DEFAULT 7
);

CREATE TABLE model_alias (
    provider_id   TEXT NOT NULL,
    model_id      TEXT NOT NULL,
    model_key     TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (provider_id, model_id)
);

CREATE TABLE model_pricing (
    provider_id          TEXT NOT NULL DEFAULT '',
    model_id             TEXT NOT NULL,
    model_key            TEXT NOT NULL DEFAULT '',
    catalog_family       TEXT,
    mode                 TEXT NOT NULL CHECK (mode IN ('dynamic', 'fixed', 'custom')),
    per_call             REAL,
    input_per_mtok       REAL,
    output_per_mtok      REAL,
    cache_read_per_mtok  REAL,
    cache_write_per_mtok REAL,
    reasoning_per_mtok   REAL,
    currency             TEXT NOT NULL DEFAULT 'USD',
    updated_at_ms        INTEGER NOT NULL,
    PRIMARY KEY (provider_id, model_id)
);

CREATE INDEX idx_model_pricing_key ON model_pricing (model_key, provider_id);

CREATE TABLE provider_pricing (
    provider_id    TEXT PRIMARY KEY,
    catalog_family TEXT NOT NULL,
    updated_at_ms  INTEGER NOT NULL
);

-- ==== agent ====
CREATE TABLE agent_sessions (
    id                      TEXT PRIMARY KEY,
    title                   TEXT NOT NULL,
    title_locked            INTEGER NOT NULL DEFAULT 0 CHECK (title_locked IN (0, 1)),
    notice                  TEXT,
    target_json             TEXT,
    workspace_dir           TEXT,
    pingdeck_workspace_id   TEXT,
    pingdeck_environment_id TEXT,
    permission_mode         TEXT NOT NULL DEFAULT 'auto_read_only',
    reasoning_effort        TEXT,
    instructions            TEXT,
    plan_mode               INTEGER NOT NULL DEFAULT 0 CHECK (plan_mode IN (0, 1)),
    web_enabled             INTEGER NOT NULL DEFAULT 0 CHECK (web_enabled IN (0, 1)),
    disabled_tools_json     TEXT NOT NULL DEFAULT '[]',
    disabled_skills_json    TEXT NOT NULL DEFAULT '[]',
    forked_from             TEXT,
    forked_from_message_id  TEXT,
    parent_session_id       TEXT REFERENCES agent_sessions(id) ON DELETE SET NULL,
    parent_run_id           TEXT,
    head_message_id         TEXT,
    todos_json              TEXT NOT NULL DEFAULT '[]',
    total_usage_json        TEXT NOT NULL DEFAULT '{}',
    total_cost_micro_usd    INTEGER NOT NULL DEFAULT 0,
    last_context_tokens     INTEGER NOT NULL DEFAULT 0,
    created_at_ms           INTEGER NOT NULL,
    updated_at_ms           INTEGER NOT NULL,
    pinned_at_ms            INTEGER,
    archived_at_ms          INTEGER,
    deleted_at_ms           INTEGER
);

CREATE INDEX idx_agent_sessions_parent ON agent_sessions(parent_session_id);

CREATE TABLE agent_attachments (
    id             TEXT PRIMARY KEY,
    sha256         TEXT NOT NULL UNIQUE,
    name           TEXT NOT NULL,
    media_type     TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('image', 'pdf', 'text')),
    size_bytes     INTEGER NOT NULL,
    width          INTEGER,
    height         INTEGER,
    line_count     INTEGER,
    thumb_data_url TEXT,
    extracted_of   TEXT,
    created_at_ms  INTEGER NOT NULL
);

CREATE TABLE agent_messages (
    id                    TEXT PRIMARY KEY,
    session_id            TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
    seq                   INTEGER NOT NULL,
    role                  TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'tool', 'summary')),
    parts_json            TEXT NOT NULL,
    usage_json            TEXT,
    model                 TEXT,
    error                 TEXT,
    search_text           TEXT NOT NULL DEFAULT '',
    created_at_ms         INTEGER NOT NULL,
    run_id                TEXT,
    cost_micro_usd        INTEGER NOT NULL DEFAULT 0,
    compacted_through_seq INTEGER,
    parent_id             TEXT,
    branch_index          INTEGER NOT NULL DEFAULT 0,
    partial               INTEGER NOT NULL DEFAULT 0 CHECK (partial IN (0, 1)),
    meta_json             TEXT NOT NULL DEFAULT '{}',
    cost_currency         TEXT,
    fts_id                INTEGER,
    UNIQUE (session_id, seq)
);

CREATE UNIQUE INDEX uq_agent_messages_fts_id ON agent_messages(fts_id);
CREATE INDEX idx_agent_messages_parent ON agent_messages(session_id, parent_id);
CREATE INDEX idx_agent_messages_run ON agent_messages(run_id, seq);
CREATE UNIQUE INDEX uq_agent_messages_sibling
    ON agent_messages(session_id, COALESCE(parent_id, ''), branch_index);

CREATE VIRTUAL TABLE agent_messages_fts USING fts5(
    search_text,
    tokenize = 'trigram',
    detail = none,
    content = '',
    contentless_delete = 1
);

CREATE TRIGGER agent_messages_fts_delete AFTER DELETE ON agent_messages
BEGIN
    DELETE FROM agent_messages_fts WHERE rowid = old.fts_id;
END;

CREATE TRIGGER agent_messages_fts_insert AFTER INSERT ON agent_messages
BEGIN
    UPDATE agent_messages
        SET fts_id = (SELECT COALESCE(MAX(fts_id), 0) + 1 FROM agent_messages)
        WHERE rowid = new.rowid;
    INSERT INTO agent_messages_fts (rowid, search_text)
        SELECT fts_id, search_text FROM agent_messages
        WHERE rowid = new.rowid AND search_text <> '';
END;

CREATE TRIGGER agent_messages_fts_update AFTER UPDATE OF search_text ON agent_messages
BEGIN
    DELETE FROM agent_messages_fts WHERE rowid = old.fts_id;
    INSERT INTO agent_messages_fts (rowid, search_text)
        SELECT new.fts_id, new.search_text WHERE new.search_text <> '';
END;

CREATE TABLE agent_message_attachments (
    message_id    TEXT NOT NULL REFERENCES agent_messages(id) ON DELETE CASCADE,
    attachment_id TEXT NOT NULL REFERENCES agent_attachments(id) ON DELETE RESTRICT,
    ord           INTEGER NOT NULL,
    PRIMARY KEY (message_id, attachment_id)
);

CREATE INDEX idx_agent_message_attachments_attachment ON agent_message_attachments(attachment_id);

CREATE TABLE agent_branch_plans (
    message_id TEXT PRIMARY KEY REFERENCES agent_messages(id) ON DELETE CASCADE,
    todos_json TEXT NOT NULL
);

CREATE TABLE agent_runs (
    id                 TEXT PRIMARY KEY,
    session_id         TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
    parent_run_id      TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
    parent_call_id     TEXT,
    kind               TEXT NOT NULL DEFAULT 'main' CHECK (kind IN ('main', 'subagent', 'background')),
    role               TEXT,
    status             TEXT NOT NULL CHECK (status IN ('queued', 'running', 'waiting_approval', 'completed', 'failed', 'cancelled', 'interrupted')),
    model_label        TEXT NOT NULL,
    target_json        TEXT,
    prompt_version     INTEGER NOT NULL DEFAULT 1,
    iterations         INTEGER NOT NULL DEFAULT 0,
    last_step          TEXT,
    usage_json         TEXT NOT NULL DEFAULT '{}',
    cost_micro_usd     INTEGER NOT NULL DEFAULT 0,
    cost_currency      TEXT,
    stop_reason        TEXT,
    error              TEXT,
    error_kind         TEXT,
    failure_json       TEXT,
    approval_timed_out INTEGER NOT NULL DEFAULT 0 CHECK (approval_timed_out IN (0, 1)),
    awake_ms           INTEGER NOT NULL DEFAULT 0,
    started_at_ms      INTEGER NOT NULL,
    last_event_ms      INTEGER,
    finished_at_ms     INTEGER,
    seen_at_ms         INTEGER
);

CREATE INDEX idx_agent_runs_parent ON agent_runs(parent_run_id);
CREATE INDEX idx_agent_runs_session ON agent_runs(session_id, started_at_ms DESC);
CREATE INDEX idx_agent_runs_status ON agent_runs(status);

CREATE TABLE agent_tool_calls (
    session_id   TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
    call_id      TEXT NOT NULL,
    run_id       TEXT NOT NULL,
    message_id   TEXT,
    name         TEXT NOT NULL,
    class        TEXT NOT NULL,
    state        TEXT NOT NULL,
    args_json    TEXT NOT NULL,
    risk         TEXT NOT NULL DEFAULT 'low',
    decided_by   TEXT,
    decision     TEXT,
    decided_ms   INTEGER,
    started_ms   INTEGER,
    ended_ms     INTEGER,
    exit_code    INTEGER,
    sandbox      TEXT,
    truncated    INTEGER NOT NULL DEFAULT 0 CHECK (truncated IN (0, 1)),
    output_bytes INTEGER NOT NULL DEFAULT 0,
    spill_ref    TEXT,
    requested_ms INTEGER,
    PRIMARY KEY (session_id, call_id)
);

CREATE TABLE agent_tool_policy (
    tool_key TEXT PRIMARY KEY,
    policy   TEXT NOT NULL CHECK (policy IN ('ask', 'allow', 'block'))
);

CREATE TABLE agent_permission_rules (
    id            TEXT PRIMARY KEY,
    session_id    TEXT REFERENCES agent_sessions(id) ON DELETE CASCADE,
    workspace_dir TEXT,
    tool_pattern  TEXT NOT NULL,
    arg_pattern   TEXT,
    action        TEXT NOT NULL CHECK (action IN ('allow', 'ask', 'block')),
    created_at_ms INTEGER NOT NULL
);

CREATE INDEX idx_agent_permission_rules_session ON agent_permission_rules(session_id);

CREATE TABLE agent_file_changes (
    id            TEXT PRIMARY KEY,
    session_id    TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
    run_id        TEXT NOT NULL,
    call_id       TEXT NOT NULL,
    path          TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('create', 'edit')),
    before_sha256 TEXT,
    after_sha256  TEXT,
    added         INTEGER NOT NULL DEFAULT 0,
    removed       INTEGER NOT NULL DEFAULT 0,
    status        TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('applying', 'pending', 'kept', 'undone', 'conflict')),
    created_ms    INTEGER NOT NULL,
    folder        TEXT
);

CREATE INDEX idx_agent_file_changes_run ON agent_file_changes(run_id);
CREATE INDEX idx_agent_file_changes_session ON agent_file_changes(session_id, created_ms DESC);

CREATE TABLE agent_queue (
    id           TEXT PRIMARY KEY,
    session_id   TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
    ord          INTEGER NOT NULL,
    content_json TEXT NOT NULL,
    created_ms   INTEGER NOT NULL
);

CREATE INDEX idx_agent_queue_session ON agent_queue(session_id, ord);

CREATE TABLE agent_folder_trust (
    path                TEXT PRIMARY KEY,
    trusted             INTEGER NOT NULL CHECK (trusted IN (0, 1)),
    instructions_sha256 TEXT,
    decided_at_ms       INTEGER NOT NULL
);

CREATE TABLE agent_mcp_servers (
    id                 TEXT PRIMARY KEY,
    name               TEXT NOT NULL UNIQUE,
    transport_json     TEXT NOT NULL,
    enabled            INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    trusted            INTEGER NOT NULL DEFAULT 0 CHECK (trusted IN (0, 1)),
    review_fingerprint TEXT,
    updated_at_ms      INTEGER NOT NULL
);

CREATE TABLE agent_mcp_tool_pins (
    server_id TEXT NOT NULL REFERENCES agent_mcp_servers(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    sha256    TEXT NOT NULL,
    PRIMARY KEY (server_id, tool_name)
);

CREATE TABLE agent_skills (
    name    TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
);

-- ==== system ====
CREATE TABLE app_settings (
    section       TEXT PRIMARY KEY,
    version       INTEGER NOT NULL,
    value_json    TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE process_ledger (
    pid         INTEGER NOT NULL,
    pgid        INTEGER NOT NULL,
    started     TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('tool', 'mcp')),
    label       TEXT NOT NULL DEFAULT '',
    recorded_ms INTEGER NOT NULL,
    PRIMARY KEY (pid, started)
);
"#;
