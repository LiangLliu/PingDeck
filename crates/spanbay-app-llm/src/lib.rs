#![forbid(unsafe_code)]

//! LLM use-cases: providers, keys, models, aliases, model tests, the catalog, the gateway and
//! client-config writers.
//!
//! Single responsibility: the UI-agnostic entry points the shell calls. Data lives in the app
//! database (`llm::store`); every write bumps the runtime version so the gateway and the chat
//! client see it on their next request. Nothing here holds the store lock across an `.await`.

pub(crate) use spanbay_app::{app, secrets, workspace};
pub(crate) use spanbay_base::{cancel, crypto, error, logging};
pub(crate) use spanbay_client_config as client_config;
pub(crate) use spanbay_llm::llm;
pub(crate) use spanbay_model::{events, settings};
#[cfg(test)]
pub(crate) use spanbay_store::storage;

mod balance;
mod client_rotation;
mod client_snippets;
mod client_verify;
mod client_writers;
mod clients;
#[cfg(test)]
mod clients_tests;
mod diagnostics;
mod gateway;
#[cfg(test)]
mod gateway_tests;
mod impact;
pub mod import_policy;
mod logs;
#[cfg(test)]
mod logs_tests;
mod model_catalog;
mod models;
mod models_drift;
mod overview;
mod price_book;
mod price_table;
mod probe;
mod provider_draft;
mod provider_view;
mod providers;
#[cfg(test)]
mod replay_tests;
mod routing;
mod sniff;
mod sniff_create;
mod start;
#[cfg(test)]
mod start_tests;
mod status;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod uc_support;
mod usage;

pub use balance::{
    BalanceQueryTestDto, KeyBalanceResultDto, llm_balance_custom_delete_impl,
    llm_balance_custom_save_impl, llm_balance_query_test_impl, llm_balance_refresh_if_due_impl,
    llm_provider_balance_impl, start_balance_scheduler, stop_balance_scheduler,
};
pub use client_snippets::{
    SnippetBlockView, SnippetNote, SnippetSet, SnippetTone, llm_client_code_samples_impl,
    llm_client_snippets_impl,
};
pub use client_verify::{
    FirstRequest, VerifyOutcome, VerifyResult, llm_client_first_request_impl,
    llm_client_verify_impl,
};
pub use client_writers::{
    ClientApplyInput, ClientConnectInput, ClientConnectResult, ClientDetection, ClientInspectDto,
    ClientPlanInput, ManagedFile, ManagedTool, OnExisting, RestoreResult, RestoredFile,
    RotateUpdateResult, RotatedFiles, SkippedFile, llm_client_applied_list_impl,
    llm_client_apply_impl, llm_client_connect_impl, llm_client_detect_impl,
    llm_client_inspect_impl, llm_client_kept_purge_impl, llm_client_managed_list_impl,
    llm_client_preview_impl, llm_client_restore_impl, llm_client_restore_original_impl,
    llm_client_rewrite_address_impl, llm_client_rotate_and_update_impl,
};
pub use clients::{
    llm_gateway_client_create_impl, llm_gateway_client_delete_impl, llm_gateway_client_list_impl,
    llm_gateway_client_revoke_impl, llm_gateway_client_rotate_impl,
    llm_gateway_client_set_policy_impl, llm_gateway_client_update_impl,
};
pub use diagnostics::llm_diagnostics_bundle_impl;
pub use gateway::{
    llm_gateway_autostart_impl, llm_gateway_check_settings_impl, llm_gateway_overview_impl,
    llm_gateway_reset_circuits_impl, llm_gateway_restart_impl, llm_gateway_start_impl,
    llm_gateway_status_impl, llm_gateway_stop_impl, llm_gateway_stop_now_impl,
    llm_health_list_impl, llm_health_reset_impl, llm_subscribe_impl, llm_unsubscribe_impl,
};
pub use impact::{
    GatewayRule, Impact, ImpactClient, RecentCaller, RenamePreview, RoutePreview, RoutePreviewItem,
    llm_gateway_rules_impl, llm_model_impact_impl, llm_provider_impact_impl,
    llm_provider_mark_reviewed_impl, llm_provider_rename_preview_impl, llm_route_preview_impl,
};
pub use logs::{
    Availability, DiagnosticsBundleInput, LogDetailDto, ReplayStarted, llm_log_capture_stats_impl,
    llm_log_clear_impl, llm_log_count_impl, llm_log_curl_impl, llm_log_delete_capture_impl,
    llm_log_delete_captures_impl, llm_log_get_impl, llm_log_id_for_request_id_impl,
    llm_log_list_impl, llm_log_open_in_api_client_impl, llm_log_page_impl,
    llm_log_replay_cancel_impl, llm_log_replay_impl, llm_log_search_bodies_impl,
};
pub use model_catalog::{
    llm_model_alias_delete_impl, llm_model_alias_save_impl, llm_model_catalog_config_save_impl,
    llm_model_catalog_list_impl, llm_model_catalog_status_impl, llm_model_catalog_sync_impl,
    llm_model_catalog_user_delete_impl, llm_model_catalog_user_save_impl,
    llm_model_price_detail_impl, llm_model_pricing_delete_impl, llm_model_pricing_save_impl,
    llm_model_resolve_impl, llm_model_unlisted_impl, llm_provider_pricing_list_impl,
    llm_provider_pricing_save_impl,
};
pub use models::{
    llm_model_capabilities_set_impl, llm_models_drift_ack_impl, llm_models_refresh_impl,
    llm_models_set_enabled_impl, llm_models_undo_impl,
};
pub use models_drift::{DriftApplyReport, llm_models_drift_apply_impl};
pub use overview::{
    AliasRow, BaseUrlRow, ClientRow, EndpointRow, GatewayOverviewDto, HostNames, OverviewEmpty,
    TargetRow, TargetState,
};
pub use price_book::{
    EffectivePriceState, EffectivePriceView, ModelPriceDetail, ProviderModelPrice,
    ProviderPricingView, UnlistedModel,
};
pub use price_table::{
    PriceRow, PriceTableQuery, UsageRetentionPreview, llm_price_table_impl,
    llm_usage_retention_preview_impl,
};
pub use probe::{
    AliasTargetVerdict, AliasVerdict, ApplyReportDto, ExportFile, ProbeApplyPreviewDto,
    ProbeCellDto, ProbeEstimateDto, ProbeEvent, ProbeExportFilter, ProbeExportFormat, ProbeItem,
    ProbeRequestPreviewDto, ProbeRunInput, ProbeRunSummaryDto, ProbeSniffInput, ProbeSummary,
    RecoveredRun, llm_context_probe_info_impl, llm_context_probe_run_impl, llm_probe_apply_impl,
    llm_probe_apply_latest_impl, llm_probe_apply_preview_impl, llm_probe_apply_undo_impl,
    llm_probe_estimate_impl, llm_probe_estimate_sniff_impl, llm_probe_export_impl,
    llm_probe_history_list_impl, llm_probe_last_run_impl, llm_probe_recover_impl,
    llm_probe_request_preview_impl, llm_probe_results_impl, llm_probe_run_get_impl,
    llm_probe_run_impl, llm_probe_runs_list_impl,
};
pub use provider_view::{
    ProviderView, llm_provider_get_view_impl, llm_provider_list_view_impl,
    llm_provider_save_view_impl,
};
pub use providers::{
    InputModality, ModelInput, ProviderFromPresetInput, ProviderModelView, SaveProviderKeyInput,
    llm_catalog_lookup_impl, llm_catalog_search_impl, llm_key_clear_cooldown_impl,
    llm_key_delete_impl, llm_key_list_impl, llm_key_reveal_impl, llm_key_save_guarded_impl,
    llm_key_save_impl, llm_model_delete_impl, llm_model_list_impl, llm_model_list_view_impl,
    llm_model_save_impl, llm_presets_impl, llm_provider_create_from_preset_impl,
    llm_provider_delete_impl, llm_provider_list_impl, llm_provider_save_impl,
};
pub use routing::{
    FallbackCandidate, FallbackCapability, FallbackWarning, FallbacksSaveResult, LegacyNameView,
    RenameImpact, RenameRewriteFailure, RenameRewriteResult, llm_legacy_names_clear_impl,
    llm_legacy_names_impl, llm_model_fallback_candidates_impl, llm_model_fallbacks_set_impl,
    llm_provider_rename_impact_impl, llm_provider_rename_rewrite_impl,
    llm_routing_notices_dismiss_impl, llm_routing_notices_impl,
};
pub use sniff::{
    ExtraInput, NormalizedUrlDto, llm_endpoint_preview_impl, llm_provider_sniff_impl,
    llm_url_normalize_impl,
};
pub use sniff_create::{
    DuplicateInfo, DuplicateRef, SniffSelection, UnverifiedExtras,
    llm_provider_add_key_from_sniff_impl, llm_provider_apply_resniff_impl,
    llm_provider_create_from_sniff_impl, llm_provider_create_unverified_impl,
    llm_provider_duplicate_check_impl,
};
pub use spanbay_store::llm::store::GatewayTodayOps;
pub use start::{GatewayStart, StartOptions, StartReport, llm_start_routine_impl};
pub use status::{
    OpenCircuitDto, ProviderHealthDto, TypicalRequestDto, llm_provider_health_impl,
    llm_provider_status_list_impl, llm_status_recompute_impl,
};
pub use usage::{
    llm_price_override_delete_impl, llm_price_override_list_impl, llm_price_override_save_impl,
    llm_today_summary_impl, llm_usage_failures_impl, llm_usage_latency_impl,
    llm_usage_latency_modes_impl, llm_usage_set_tz_offset_impl, llm_usage_summary_impl,
};

use spanbay_store::DbConnection as Connection;

use app::{AppState, current_time_ms};
use error::AppError;
use llm::catalog;
use llm::runtime::DbHandle;
use llm::safe_http::client::ReqwestSafeHttp;
use llm::store::Provider;

pub(crate) fn with_connection<T>(
    state: &AppState,
    work: impl FnOnce(&Connection) -> Result<T, AppError>,
) -> Result<T, AppError> {
    let store = state.store();
    work(store.connection())
}

const MAX_SLUG_LEN: usize = 48;
const MAX_ID_LEN: usize = 64;

fn slug(text: &str) -> String {
    let mut out = String::new();
    for ch in text.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.truncate(MAX_SLUG_LEN);
    let out = out.trim_matches('-').to_owned();
    if out.is_empty() {
        "provider".to_owned()
    } else {
        out
    }
}

fn check_id(kind: &str, id: &str) -> Result<(), AppError> {
    let valid = (1..=MAX_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(AppError::validation(format!(
            "a {kind} id may only use letters, digits, '.', '_' and '-' (1 to {MAX_ID_LEN} characters)"
        )))
    }
}

/// `base` as a slug, numbered (`-2`, `-3`…) past the ids and names of `taken` and the ids of
/// deleted providers (`buried`, lower case): a new provider never inherits an old id.
fn unique_provider_id(base: &str, taken: &[Provider], buried: &[String]) -> String {
    let base = slug(base);
    let mut candidate = base.clone();
    let mut counter = 2;
    while taken.iter().any(|provider| {
        provider.id == candidate || provider.name.trim().to_lowercase() == candidate
    }) || buried.contains(&candidate.to_lowercase())
    {
        candidate = format!("{base}-{counter}");
        counter += 1;
    }
    candidate
}

fn next_provider_position(taken: &[Provider]) -> i64 {
    taken
        .iter()
        .map(|provider| provider.position)
        .max()
        .map_or(0, |last| last.saturating_add(1))
}

pub(crate) fn ensure_catalog(connection: &Connection) -> Result<(), AppError> {
    catalog::seed_bundled_if_empty(connection).map(|_| ())
}

pub(crate) async fn with_db_blocking<T: Send + 'static>(
    state: &AppState,
    work: impl FnOnce(&Connection) -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    let db = state.llm.db().clone();
    match db {
        DbHandle::Path { .. } => tokio::task::spawn_blocking(move || work(&*db.open()?))
            .await
            .map_err(|error| AppError::internal(format!("database task failed: {error}")))?,
        DbHandle::Unavailable => with_connection(state, work),
    }
}

pub(crate) fn safe_http_for(
    state: &AppState,
    origin: &str,
    provider_proxy: &str,
) -> Result<ReqwestSafeHttp, AppError> {
    // One proxy rule for every outbound client.
    ReqwestSafeHttp::for_network(&state.settings().network, origin, provider_proxy)
}

pub(crate) const SAVED_PROVIDER_ALLOW_PRIVATE: bool = true;
