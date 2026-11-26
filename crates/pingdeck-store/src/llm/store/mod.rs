//! SQLite persistence of the LLM data: providers, keys, models with their fallback chains, probe
//! results, the gateway request log.
//!
//! Typed reads and writes over a borrowed [`Connection`]; the data is app-global (no workspace).
//! Callers pick the connection (the app store's or the gateway's own), so nothing here locks. The
//! model catalog lives in `catalog`, the user's pricing in `pricing`, the gateway settings in
//! `app_settings`.
//!
//! Tests: a module's own suite lives in `<module>/tests.rs`; suites that exercise several
//! modules together live in `tests/`.

use pingdeck_base::error::AppError;
use rusqlite::Connection;

mod bulk_enable;
mod captures;
mod clients;
mod facts;
mod health_stats;
mod identity;
mod key_models;
mod key_secrets;
mod keys;
mod ledger;
mod log;
mod model_capabilities;
mod model_listing;
mod models;
mod pricing;
mod probe;
mod probe_runs;
mod provider_secrets;
mod provider_state;
mod providers;
mod routing;
mod sniff_create;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
mod usage;

pub use crate::llm::probe::ProbeResult;
pub use crate::llm::providers::{
    KeyStrategy, LIST_IN_MODELS_DEFAULT_MAX, MAX_SNIFF_EVIDENCE, Provider, SaveProviderOptions,
    SniffPersist, outbound_hosts, provider_lists_direct_ids, redact_proxy_url, validate_proxy_url,
};
pub use pingdeck_llm_domain::llm::records::{
    Alias, AliasTarget, BulkEnableReport, CaptureStats, ClearReport, ClientHashEntry, ClientInput,
    ClientLimits, ClientPolicyInput, ClientWithToken, DEFAULT_LOG_PAGE, FactColumns, FactSource,
    GatewayClient, KeyRunState, KeyRuntime, KeyTest, KeyVerdict, LOG_ROWS_PER_SOURCE, LOG_SOURCES,
    LogDetail, LogFilter, LogPage, LogPageResult, MAX_ALLOW_ENTRIES, MAX_CLIENTS, MAX_LOG_PAGE,
    MAX_NAME_CHARS, MIGRATED_CLIENT_ID, MintedToken, ModelRef, ModelSource, ModelsListing,
    ModelsListingState, ProbeApplyRecord, ProviderKey, ProviderModel, RequestLogEntry, StatusClass,
    TOUCH_INTERVAL_MS, TokenLookup, UndoReport, book_entry, derive_log_reason, match_token,
    mint_token, token_tag,
};

pub use bulk_enable::{EnabledChange, set_models_enabled, set_models_enabled_bulk, undo_bulk};
pub use captures::{
    MAX_SEARCH_RESULTS, MAX_SEARCH_ROWS, SEARCH_TIME_BUDGET, SearchBudget, capture_bytes_total,
    capture_stats, delete_capture, delete_captures, get_capture, search_log_bodies,
};
#[cfg(any(test, feature = "test-support"))]
pub use clients::seed_client_token;
pub use clients::{
    ModelGrant, client_hashes_snapshot, client_verified_at, commit_rotation, create_client,
    create_client_replacing_revoked, delete_client, get_client, grant_model, list_clients,
    mark_client_verified, revoke_client, rotate_client_token, set_client_policy, touch_client,
    update_client,
};
pub use facts::provider_status_facts;
pub use health_stats::{
    HEALTH_HOURS, HEALTH_WINDOW_MS, ProviderLogStats, TYPICAL_REQUESTS, provider_log_stats,
};
pub use identity::{
    FormerName, Purged, list_former_names, mark_reviewed, provider_name_holder, purge_references,
    recent_callers, tombstone_ids, unique_provider_name,
};
pub use key_models::{KeyModel, list_key_models, replace_key_models};
pub use key_secrets::{
    ResolvedKey, get_resolved_key, list_resolved_keys, reveal_key, set_key_management_token,
};
#[cfg(feature = "test-support")]
pub use keys::set_key_enabled_at_for_test;
pub use keys::{
    MAX_GROUP_NAME_CHARS, delete_key, get_key, key_last_used, list_keys, save_key,
    save_key_guarded, set_key_balance, unique_key_name,
};
pub use ledger::{
    HISTORY_WRITES_KEPT, LedgerEntry, delete_applied, delete_applied_for_app,
    delete_applied_for_client, delete_applied_ids, latest_applied, latest_applied_for_app,
    prune_history, record_applied, referenced_backups,
};
pub use log::{
    LogWrite, PruneLimits, PruneReport, clear_log, count_log, get_log, insert_log, list_log,
    log_id_for_request_id, new_request_id, prune_log, query_log_page,
};
pub use model_capabilities::{
    record_measured, record_measured_context, set_model_capabilities_override,
    swap_capability_states,
};
pub use model_listing::{
    ListedModel, ack_model_drift, apply_model_listing, confirm_listed, insert_models_from_sniff,
};
pub use models::{delete_model, get_model, list_models, save_model};
pub use pricing::{
    KeyedPricing, ModelDetailCatalog, PriceBookSnapshot, delete_alias, delete_price_override,
    delete_pricing, effective_price, effective_priced, list_pricing, load_price_book_snapshot,
    model_detail_catalog, pick_rows, price_overrides, resolve_price, save_alias,
    save_price_override, save_pricing, save_provider_pricing,
};
pub use probe::{
    HISTORY_RUNS_PER_PAIR, list_probe_history, list_probe_results, persist_cell, probe_run_results,
    save_probe_result,
};
#[cfg(feature = "test-support")]
pub use probe_runs::reopen_probe_run;
pub use probe_runs::{
    MAX_APPLY_LOG, ProbeRunRow, finish_probe_run, get_probe_apply, get_probe_run,
    latest_probe_apply_at, latest_probe_run, latest_undoable_apply, list_probe_runs,
    record_probe_apply, record_probe_request, set_probe_run_summary, start_probe_run,
    unfinished_probe_runs,
};
pub use provider_secrets::{
    HeaderHints, provider_header_hints, resolve_provider_headers, resolved_custom_balance,
    set_custom_balance,
};
#[cfg(feature = "test-support")]
pub use provider_state::overwrite_provider_base_url_for_test;
pub use provider_state::{set_gateway_family, set_provider_sniff};
pub use providers::{
    delete_provider, get_provider, list_providers, save_provider, save_provider_with,
};
#[cfg(any(test, feature = "test-support"))]
pub use routing::save_legacy_name;
pub use routing::{
    LegacyName, MAX_FALLBACKS, clear_routing_notices, delete_legacy_name, list_fallbacks,
    list_legacy_names, list_routing_notices, set_model_fallbacks,
};
pub use sniff_create::{
    KeyCheck, NewKeyFromSniff, NewProviderFromSniff, SniffChecks, create_provider_from_sniff,
};
pub use usage::{
    FailureCount, MINUTE_RETENTION_DAYS, UsageFailure, UsageLatency, failure_counts, prune_usage,
    today_gateway_rows, upsert_rollup, usage_failures, usage_latency, usage_latency_modes,
    usage_retention_preview, usage_rows,
};

// `atomic` here is the AppError flavour (`crate::sqlite::atomic` needs E: From<rusqlite::Error>).
pub(super) use crate::sqlite::{
    atomic_app as atomic, db, enum_from_text, flag, from_json, json_text, to_i64, to_u64,
};

/// The text of a unit enum as stored in a column (`snake_case`, no quotes).
pub(super) fn enum_text<T: serde::Serialize>(value: &T) -> Result<String, AppError> {
    crate::sqlite::enum_variant_name(value).map_err(AppError::internal)
}

/// Runs `work` in one write transaction: on an autocommit connection it opens `BEGIN IMMEDIATE`,
/// inside a transaction the caller already holds it becomes a savepoint, so the whole sequence
/// commits or rolls back together (see `crate::sqlite::atomic_with`).
///
/// # Errors
/// Whatever `work` returns (everything it wrote is rolled back), or [`AppError::Database`] when
/// the transaction cannot be opened or committed.
pub fn in_write_transaction<T>(
    connection: &Connection,
    work: impl FnOnce(&Connection) -> Result<T, AppError>,
) -> Result<T, AppError> {
    atomic(connection, work)
}

/// The tables whose imported rows keep their original `created_at_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampTable {
    /// `llm_providers`.
    Providers,
    /// `llm_keys`.
    Keys,
}

/// Stamps `created_at_ms` on an imported provider or key row.
///
/// # Errors
/// [`AppError::Database`] when the update fails.
pub fn stamp_llm_created_at(
    connection: &Connection,
    table: StampTable,
    id: &str,
    ms: i64,
) -> Result<(), AppError> {
    let sql = match table {
        StampTable::Providers => "UPDATE llm_providers SET created_at_ms = ?1 WHERE id = ?2",
        StampTable::Keys => "UPDATE llm_keys SET created_at_ms = ?1 WHERE id = ?2",
    };
    connection
        .execute(sql, rusqlite::params![ms, id])
        .map(drop)
        .map_err(db)
}
