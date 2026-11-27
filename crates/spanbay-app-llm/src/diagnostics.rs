//! "Copy diagnostics": the telemetry-free JSON bundle.
//!
//! Single responsibility: collect the snapshots the bundle is made of — app, gateway, providers,
//! gateway fallback rules, clients, circuits, the last 24 hours of the log and the newest rows — redact the
//! free text, and hand them to `capture::build_diagnostics`, whose last gate refuses (fail
//! closed) to produce text that redaction would still change. No network request is made.

use std::collections::BTreeMap;

use super::logs::DiagnosticsBundleInput;
use super::{current_time_ms, with_connection};
use crate::app::AppState;
use crate::error::AppError;
use crate::llm::capture::{
    self, AppDiag, ClientDiag, DIAGNOSTICS_MAX_ROWS, DiagnosticsInput, DiagnosticsRefusal,
    FallbackDiag, GatewayDiag, LogSummaryDiag, ProviderDiag, RowDiag, redact_text,
};
use crate::llm::gateway::GatewayState;
use crate::llm::status::provider_status;
use crate::llm::store::{
    self, GatewayClient, LogPage, MAX_LOG_PAGE, ModelRef, Provider, RequestLogEntry,
};
use crate::logging::{LogLevel, log_line};

/// Rows of the last 24 hours the summary looks at, at most (five pages).
const SUMMARY_PAGES: usize = 5;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// Rows listed before the selected one when the drawer asks for a row's bodies.
const ROWS_BEFORE_SELECTED: usize = 20;

fn state_word(state: GatewayState) -> &'static str {
    match state {
        GatewayState::Running => "running",
        GatewayState::Stopped => "stopped",
        GatewayState::Starting => "starting",
        GatewayState::Stopping => "stopping",
        GatewayState::Error => "error",
    }
}

fn allow_summary(client: &GatewayClient) -> String {
    if client.allow_all {
        return "all models".to_owned();
    }
    match (client.allowed_aliases.len(), client.allowed_models.len()) {
        (aliases, 0) => format!("{aliases} aliases"),
        (0, models) => format!("{models} models"),
        (aliases, models) => format!("{aliases} aliases, {models} models"),
    }
}

fn class_of(row: &RequestLogEntry) -> &'static str {
    match (row.outcome.as_str(), row.status) {
        ("canceled", _) => "canceled",
        (_, 200..=299) => "2xx",
        (_, 400..=499) => "4xx",
        (_, 500..=599) => "5xx",
        _ => "other",
    }
}

/// The value at the `percentile` (0..=100) of `values`, sorted in place (nearest rank).
fn percentile(values: &mut [u64], percentile: usize) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let rank = (values.len() * percentile).div_ceil(100).max(1);
    values.get(rank - 1).copied()
}

fn summarize_rows(rows: &[RequestLogEntry]) -> LogSummaryDiag {
    let mut summary = LogSummaryDiag::default();
    let (mut ttft, mut latency) = (Vec::new(), Vec::new());
    for row in rows {
        *summary
            .by_status_class
            .entry(class_of(row).to_owned())
            .or_default() += 1;
        *summary.by_source.entry(row.source.clone()).or_default() += 1;
        if let Some(provider) = row.provider_name.as_deref().or(row.provider_id.as_deref()) {
            *summary.by_provider.entry(provider.to_owned()).or_default() += 1;
        }
        latency.push(row.latency_ms);
        ttft.extend(row.ttft_ms);
    }
    summary.ttft_p50_ms = percentile(&mut ttft, 50);
    summary.ttft_p95_ms = percentile(&mut ttft, 95);
    summary.latency_p50_ms = percentile(&mut latency, 50);
    summary.latency_p95_ms = percentile(&mut latency, 95);
    summary
}

fn row_diag(row: &RequestLogEntry, needles: &[String]) -> RowDiag {
    RowDiag {
        request_id: row.request_id.clone().unwrap_or_default(),
        ts_ms: row.ts_ms,
        source: row.source.clone(),
        client_name: row.client_name.clone(),
        alias: row.alias.clone(),
        requested_model: row.requested_model.clone(),
        upstream_model: row.upstream_model.clone(),
        provider_name: row.provider_name.clone(),
        protocol_in: row.protocol_in.clone(),
        protocol_out: row.protocol_out.clone(),
        status: row.status,
        outcome: row.outcome.clone(),
        stream: row.stream,
        translated: row.translated,
        latency_ms: row.latency_ms,
        ttft_ms: row.ttft_ms,
        input_tokens: row.input_tokens,
        output_tokens: row.output_tokens,
        cost_micro: row.cost_micro_usd,
        cost_currency: row.cost_micro_usd.map(|_| row.cost_currency.clone()),
        failure_kind: row.failure_kind.map(|kind| kind.as_str().to_owned()),
        error: row
            .error
            .as_deref()
            .map(|text| redact_text(text, needles).0),
    }
}

/// The newest rows the bundle lists: the newest 50, or (with a selected row) that row and the 20
/// before it.
fn recent_rows(
    connection: &spanbay_store::DbConnection,
    selected: Option<i64>,
) -> Result<Vec<RequestLogEntry>, AppError> {
    let Some(selected) = selected else {
        return store::list_log(
            connection,
            &LogPage {
                limit: DIAGNOSTICS_MAX_ROWS,
                ..LogPage::default()
            },
        );
    };
    let mut rows = vec![
        store::get_log(connection, selected)?
            .ok_or_else(|| AppError::not_found(format!("request {selected} was removed")))?
            .entry,
    ];
    rows.extend(store::list_log(
        connection,
        &LogPage {
            before_id: Some(selected),
            limit: ROWS_BEFORE_SELECTED,
            ..LogPage::default()
        },
    )?);
    Ok(rows)
}

/// The last 24 hours' rows, newest first, from a few pages.
fn day_rows(
    connection: &spanbay_store::DbConnection,
    since_ms: i64,
) -> Result<Vec<RequestLogEntry>, AppError> {
    let mut out: Vec<RequestLogEntry> = Vec::new();
    let mut before = None;
    for _ in 0..SUMMARY_PAGES {
        let page = store::list_log(
            connection,
            &LogPage {
                before_id: before,
                limit: MAX_LOG_PAGE,
                ..LogPage::default()
            },
        )?;
        let Some(last) = page.last() else { break };
        before = Some(last.id);
        let done = page.len() < MAX_LOG_PAGE || last.ts_ms < since_ms;
        out.extend(page.into_iter().filter(|row| row.ts_ms >= since_ms));
        if done {
            break;
        }
    }
    Ok(out)
}

fn provider_diags(
    connection: &spanbay_store::DbConnection,
    now_ms: i64,
) -> Result<Vec<ProviderDiag>, AppError> {
    let providers: Vec<Provider> = store::list_providers(connection)?;
    let keys = store::list_keys(connection, None)?;
    let models = store::list_models(connection, None)?;
    let mut status: BTreeMap<String, String> = BTreeMap::new();
    for inputs in store::provider_status_facts(connection, now_ms)? {
        let word = serde_json::to_value(provider_status(&inputs, now_ms).status)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned));
        if let Some(word) = word {
            status.insert(inputs.provider_id.clone(), word);
        }
    }
    let count = |matching: usize| u32::try_from(matching).unwrap_or(u32::MAX);
    Ok(providers
        .into_iter()
        .map(|provider| ProviderDiag {
            enabled_keys: count(
                keys.iter()
                    .filter(|key| key.provider_id == provider.id && key.enabled)
                    .count(),
            ),
            enabled_models: count(
                models
                    .iter()
                    .filter(|model| model.provider_id == provider.id && model.enabled)
                    .count(),
            ),
            probe_status: status.get(&provider.id).cloned(),
            protocols: provider
                .endpoints
                .iter()
                .map(|endpoint| endpoint.protocol.as_str().to_owned())
                .collect(),
            base_url: Some(provider.base_url.clone()),
            id: provider.id,
            name: provider.name,
        })
        .collect())
}

/// Builds the diagnostics bundle: app, gateway, providers, fallbacks, clients,
/// circuits, a 24-hour log summary and the newest 50 rows — no bodies unless the drawer asked for
/// one row's, provider addresses only when asked (query and user info removed, opaque
/// path segments elided). Free text (the last start error, row errors) is redacted first, and
/// the exact values of every provider key and management token are checked for at the end; a
/// match anywhere refuses the whole bundle (fail closed) and logs the refusal to the app log.
/// The text is at most 256 KB; rows are trimmed from the oldest end. Nothing leaves the process.
///
/// # Errors
/// [`AppError::Validation`] with "Diagnostics could not be made safe. Nothing was copied." (or
/// the 256 KB message), [`AppError::NotFound`] when `include_bodies_of` names a pruned row,
/// [`AppError::Database`] on failure.
pub fn llm_diagnostics_bundle_impl(
    state: &AppState,
    input: DiagnosticsBundleInput,
) -> Result<String, AppError> {
    let now_ms = current_time_ms();
    let settings = state.settings();
    let status = state.llm.gateway.status(&state.llm);
    let health = state.llm.health.snapshot();
    let (providers, chains, clients, day, recent, capture, needles) =
        with_connection(state, |connection| {
            let needles: Vec<String> = store::list_resolved_keys(connection)?
                .into_iter()
                .flat_map(|key| [Some(key.secret), key.management_token])
                .flatten()
                .filter(|secret| !secret.is_empty())
                .collect();
            let capture = match input.include_bodies_of {
                Some(id) => store::get_capture(connection, id)?,
                None => None,
            };
            Ok((
                provider_diags(connection, now_ms)?,
                store::list_fallbacks(connection)?,
                store::list_clients(connection)?,
                day_rows(connection, now_ms - DAY_MS)?,
                recent_rows(connection, input.include_bodies_of)?,
                capture,
                needles,
            ))
        })?;
    let gateway = &settings.ai.gateway;
    let bundle = DiagnosticsInput {
        app: AppDiag {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            language: serde_json::to_value(settings.general.language)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
        },
        gateway: GatewayDiag {
            state: state_word(status.state).to_owned(),
            bind_scope: if status.lan { "lan" } else { "loopback" }.to_owned(),
            port: if status.port == 0 {
                gateway.port
            } else {
                status.port
            },
            token_required: gateway.token_required(),
            lan: gateway.serve_lan,
            allowed_hosts: u32::try_from(gateway.extra_allowed_hosts.len()).unwrap_or(u32::MAX),
            capture_policy: gateway.capture_mode,
            uptime_s: (status.state == GatewayState::Running).then_some(status.uptime_s),
            in_flight: u32::try_from(status.in_flight).unwrap_or(u32::MAX),
            rows_dropped: status.log_dropped,
            captures_dropped: status.captures_dropped,
            auth_failures: status.auth_failures,
            last_start_error: status
                .last_error
                .as_ref()
                .map(|error| redact_text(&error.message, &needles).0),
        },
        providers,
        fallbacks: {
            let mut chains: Vec<FallbackDiag> = chains
                .into_iter()
                .map(|(model, chain)| FallbackDiag {
                    model: model.id_form(),
                    fallbacks: chain.iter().map(ModelRef::id_form).collect(),
                })
                .collect();
            chains.sort_by(|a, b| a.model.cmp(&b.model));
            chains
        },
        clients: clients
            .iter()
            .map(|client| ClientDiag {
                name: client.name.clone(),
                allow_summary: allow_summary(client),
                created_at_ms: client.created_at_ms,
                last_used_at_ms: client.last_used_ms,
                revoked: !client.is_active(),
            })
            .collect(),
        health,
        log_summary: summarize_rows(&day),
        recent_rows: recent.iter().map(|row| row_diag(row, &needles)).collect(),
        include_provider_addresses: input.include_provider_addresses,
        selected_capture: capture,
        needles,
    };
    capture::build_diagnostics(&bundle).map_err(|refusal| {
        if refusal == DiagnosticsRefusal::Unsafe {
            log_line(
                LogLevel::Warn,
                "llm.diagnostics",
                "the diagnostics bundle was refused: redaction would still change its text",
            );
        }
        AppError::validation(refusal.message())
    })
}
