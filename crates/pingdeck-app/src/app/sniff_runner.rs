//! Shared provider sniff execution helper used by both `ai_run_service::detect` and
//! `llm_service::sniff`. Keeping this helper in `pingdeck-app` avoids an upward dependency from
//! `ai_run_service` onto `llm_service`.

use crate::app::{AppState, current_time_ms};
use crate::error::AppError;
use crate::events::EventSink;
use crate::llm::catalog::{self, CatalogIndex};
use crate::llm::safe_http::SafeHttp;
use crate::llm::safe_http::client::ReqwestSafeHttp;
use crate::llm::sniff::{
    SniffContext, SniffError, SniffEvent, SniffInput, SniffReport, StoredProviderView,
    normalize_base_url, sniff_provider,
};

/// Maps a sniffer failure to the use-case error; stable codes stay readable in the message.
pub fn sniff_error(error: SniffError) -> AppError {
    match &error {
        SniffError::InvalidInput { reason } => {
            AppError::validation(format!("{error} (invalid_input: {reason})"))
        }
        SniffError::BlockedAddress { .. } => {
            AppError::validation(format!("{error} (blocked_address)"))
        }
        _ => AppError::from(error),
    }
}

/// Execution parameters for [`run_sniff_scan`].
pub struct SniffScanParams {
    pub input: SniffInput,
    pub stored: Option<StoredProviderView>,
    pub index: CatalogIndex,
    pub uses_proxy: bool,
    pub cancel_token: Option<String>,
}

/// Runs a sniff scan using `http`, loads the bundled/stored catalog, caches the finished report,
/// and returns it.
pub async fn run_sniff_scan(
    state: &AppState,
    http: &dyn SafeHttp,
    params: SniffScanParams,
    sink: &dyn EventSink<SniffEvent>,
) -> Result<SniffReport, AppError> {
    let SniffScanParams {
        input,
        stored,
        index,
        uses_proxy,
        cancel_token,
    } = params;
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let lookup = |id: &str| index.lookup(id);
    let context = SniffContext {
        now_ms: current_time_ms(),
        app_version: env!("CARGO_PKG_VERSION"),
        auth_memory: &state.llm.services.auth_memory,
        uses_proxy,
        catalog: &lookup,
        stored: stored.as_ref(),
    };
    let report = sniff_provider(http, input, &context, sink, guard.token())
        .await
        .map_err(sniff_error)?;
    state
        .llm
        .services
        .sniff_cache
        .insert(report.clone(), current_time_ms());
    Ok(report)
}

/// Runs an unverified URL sniff (no saved provider) with network settings from `state`.
pub async fn run_unverified_sniff(
    state: &AppState,
    input: SniffInput,
    cancel_token: Option<String>,
    sink: &dyn EventSink<SniffEvent>,
) -> Result<SniffReport, AppError> {
    let origin = match input.base_url.trim() {
        "" => String::new(),
        typed => normalize_base_url(typed).map_err(sniff_error)?.origin,
    };
    let own_proxy = input.proxy_url.clone().unwrap_or_default();
    let http = ReqwestSafeHttp::for_network(&state.settings().network, &origin, &own_proxy)?;
    let index = state
        .with_llm_db_blocking(|connection| {
            catalog::seed_bundled_if_empty(connection)?;
            CatalogIndex::load(connection)
        })
        .await?;
    let uses_proxy = http.uses_proxy();
    run_sniff_scan(
        state,
        &http,
        SniffScanParams {
            input,
            stored: None,
            index,
            uses_proxy,
            cancel_token,
        },
        sink,
    )
    .await
}
