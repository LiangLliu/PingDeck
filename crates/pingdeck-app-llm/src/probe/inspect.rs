//! Looking at one cell or one run from outside the grid: "Copy as curl", "Open in API client",
//! the CSV and JSON export and the context-test warning.
//!
//! Single responsibility: rebuild the request a check sends with the engine's own builders and
//! pass it through the capture redaction and curl tables (the one table: the key never appears,
//! `$PROVIDER_API_KEY` stands for it), and serialise stored run rows with the engine's export.

use std::collections::BTreeMap;

use super::plan::provider_of_key;
use super::spec::RunDoc;
use super::types::{ProbeExportFilter, ProbeExportFormat, ProbeRequestPreviewDto};
use crate::app::{AppState, current_time_ms};
use crate::error::AppError;
use crate::llm::capture::{
    BoundedBody, CurlKind, RawExchange, RowFacts, build_curl, open_in_api_client, redact_exchange,
};
use crate::llm::capture_record::TimingJson;
use crate::llm::catalog;
use crate::llm::probe::ExportFile;
use crate::llm::probe::export::{ExportRow, ExportRun, file_name, to_csv, to_json};
use crate::llm::probe::preview::request_preview;
use crate::llm::probe::status::{ReasonContext, reason_line};
use crate::llm::probe::{ContextProbeInfo, ProbeOptions, ProbeStatus, ProbeTest};
use crate::llm::store;
use crate::llm::wire::request_headers;
use crate::{with_connection, with_db_blocking};

/// The cell's request as a runnable command and an API-client draft. `run_id` lends
/// the run's forced protocol. The command carries `$PROVIDER_API_KEY` where the credential goes;
/// provider headers that came from the secret store are left out with a warning.
///
/// # Errors
/// [`AppError::NotFound`] for a key or run that does not exist, [`AppError::Unsupported`] when
/// the check sends no request (the context test, a provider without the needed protocol).
pub async fn llm_probe_request_preview_impl(
    state: &AppState,
    run_id: Option<&str>,
    key_id: &str,
    model_id: &str,
    test: ProbeTest,
) -> Result<ProbeRequestPreviewDto, AppError> {
    let snapshot = state.llm.snapshot().await?;
    let provider_id = provider_of_key(&snapshot, key_id)?;
    let target = snapshot.target_for(&provider_id, Some(key_id), model_id, None)?;
    let models_endpoint = snapshot
        .providers
        .get(&provider_id)
        .and_then(|provider| provider.models_endpoint.clone());
    let protocol = match run_id {
        Some(id) => {
            let id = id.to_owned();
            with_db_blocking(state, move |connection| {
                let row = store::get_probe_run(connection, &id)?
                    .ok_or_else(|| AppError::not_found(format!("probe run {id} not found")))?;
                Ok(RunDoc::of(&row).spec.protocol)
            })
            .await?
        }
        None => None,
    };
    let options = ProbeOptions {
        prompt: state.settings().ai.providers.default_test_prompt.clone(),
        protocol,
        allow_context_probe: false,
        models_endpoint,
        declared_context: None,
        reasoning_model: false,
    };
    let preview = request_preview(&target, test, &options).ok_or_else(|| {
        AppError::unsupported(format!(
            "the {} check sends no request to this provider",
            test.as_str()
        ))
    })?;
    let is_get = preview.body.is_null();
    let secret = target.secret.clone();
    let headers = request_headers(
        preview.protocol,
        target.auth_for(preview.protocol),
        &secret,
        &target.quirks,
    );
    // Every secret-bearing value is a needle, so redaction finds it wherever it hides.
    let needles: Vec<String> = std::iter::once(secret)
        .chain(target.quirks.extra_headers.values().cloned())
        .filter(|needle| !needle.is_empty())
        .collect();
    let body_text = if is_get {
        String::new()
    } else {
        preview.body.to_string()
    };
    let (capture, _) = redact_exchange(
        RawExchange {
            request_log_id: 0,
            ts_ms: current_time_ms(),
            client_protocol: preview.protocol,
            client_req_headers: Vec::new(),
            client_req_body: None,
            hub_request: None,
            upstream_url: preview.url.clone(),
            upstream_req_headers: headers,
            upstream_req_body: Some(BoundedBody {
                original_bytes: body_text.len() as u64,
                text: body_text,
                ..BoundedBody::default()
            }),
            upstream_status: None,
            upstream_resp_headers: Vec::new(),
            upstream_resp_body: None,
            client_resp_headers: Vec::new(),
            client_resp_body: None,
            upstream_frames: None,
            client_frames: None,
            translation_notes: Vec::new(),
            timing: TimingJson::default(),
        },
        &needles,
    );
    let facts = RowFacts {
        protocol_out: Some(preview.protocol),
        requested_model: model_id.to_owned(),
        upstream_model: Some(model_id.to_owned()),
        stream: preview.stream,
        provider_id: Some(provider_id.clone()),
        provider_exists: true,
    };
    let mut curl = build_curl(&capture, &facts, CurlKind::Upstream, "")
        .map_err(|refusal| AppError::unsupported(refusal.reason))?;
    if is_get {
        // A GET check has no body: the shared builder always appends one, so cut it off.
        if let Some((command, _)) = curl.text.split_once(BODY_MARK) {
            curl.text = format!("{command}\n");
        }
    }
    Ok(ProbeRequestPreviewDto {
        provider_id,
        model_id: model_id.to_owned(),
        test,
        curl: curl.text,
        warnings: curl.warnings,
        api_client: (!is_get).then(|| open_in_api_client(&capture, &facts, "")),
    })
}

/// Where the shared curl builder starts the body of the command.
const BODY_MARK: &str = " \\\n  --data-binary @- <<";

fn keeps(filter: &ProbeExportFilter, row: &store::ProbeResult, status: ProbeStatus) -> bool {
    let test = ProbeTest::parse(&row.test);
    filter
        .models
        .as_ref()
        .is_none_or(|models| models.contains(&row.model_id))
        && filter
            .tests
            .as_ref()
            .is_none_or(|tests| test.is_some_and(|test| tests.contains(&test)))
        && filter
            .statuses
            .as_ref()
            .is_none_or(|statuses| statuses.contains(&status))
        && filter
            .key_id
            .as_ref()
            .is_none_or(|key_id| *key_id == row.key_id)
}

/// Exports the rows of a run the grid shows (`filter`) as CSV or JSON, with messages redacted
/// and only the masked key hint. The file name follows
/// `pingdeck-bench-<provider>-<YYYYMMDD-HHMMSS>.<csv|json>` in local time; the core's save dialog
/// writes the bytes.
///
/// # Errors
/// [`AppError::NotFound`] for a run that does not exist, [`AppError::Internal`] when the JSON
/// cannot be written.
pub fn llm_probe_export_impl(
    state: &AppState,
    run_id: &str,
    filter: &ProbeExportFilter,
    format: ProbeExportFormat,
) -> Result<ExportFile, AppError> {
    let timeout_s = u64::from(state.settings().ai.providers.probe_timeout_s);
    let (row, results, provider_name, hints) = with_connection(state, |connection| {
        let row = store::get_probe_run(connection, run_id)?
            .ok_or_else(|| AppError::not_found(format!("probe run {run_id} not found")))?;
        let results = store::probe_run_results(connection, run_id)?;
        let provider_name = match &row.provider_id {
            Some(id) => store::get_provider(connection, id)?.map(|provider| provider.name),
            None => None,
        };
        let mut hints: BTreeMap<String, String> = BTreeMap::new();
        for result in &results {
            if !hints.contains_key(&result.key_id) {
                let hint = store::get_key(connection, &result.key_id)?
                    .map(|key| key.secret_hint)
                    .unwrap_or_default();
                hints.insert(result.key_id.clone(), hint);
            }
        }
        Ok((row, results, provider_name, hints))
    })?;
    let rows: Vec<ExportRow> = results
        .iter()
        .filter_map(|result| {
            let status = ProbeStatus::parse(&result.status_word)?;
            let check = ProbeTest::parse(&result.test)?;
            keeps(filter, result, status).then(|| ExportRow {
                key_hint: hints.get(&result.key_id).cloned().unwrap_or_default(),
                model_id: result.model_id.clone(),
                check,
                status,
                http_status: result.status,
                latency_ms: result.latency_ms,
                ttft_ms: result.ttft_ms,
                tok_per_s: result.tps,
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
                failure_kind: result.failure_kind,
                reason: if status == ProbeStatus::Skipped {
                    result.message.clone()
                } else {
                    reason_line(
                        result,
                        status,
                        &ReasonContext {
                            timeout_s,
                            ..ReasonContext::default()
                        },
                    )
                },
            })
        })
        .collect();
    let provider_id = row
        .provider_id
        .clone()
        .unwrap_or_else(|| "multiple".to_owned());
    let run = ExportRun {
        run_id: row.run_id,
        started_at_ms: row.started_at_ms,
        provider_name: provider_name.unwrap_or_else(|| provider_id.clone()),
        provider_id,
    };
    let (extension, bytes) = match format {
        ProbeExportFormat::Csv => ("csv", to_csv(&run, &rows).into_bytes()),
        ProbeExportFormat::Json => (
            "json",
            serde_json::to_vec_pretty(&to_json(&run, &rows))
                .map_err(|error| AppError::internal(error.to_string()))?,
        ),
    };
    Ok(ExportFile {
        file_name: file_name(
            &run.provider_id,
            run.started_at_ms,
            state.llm.tz_offset_minutes(),
            extension,
        ),
        bytes,
    })
}

/// The structured warning of the context test for one model: the token figures and the text the
/// UI shows before it enables the action.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_context_probe_info_impl(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
) -> Result<ContextProbeInfo, AppError> {
    with_connection(state, |connection| {
        let stored =
            store::get_model(connection, provider_id, model_id)?.and_then(|model| model.context);
        let declared = match stored {
            Some(window) => Some(window),
            None => catalog::lookup(connection, model_id)?.and_then(|entry| entry.max_input_tokens),
        };
        Ok(ContextProbeInfo::for_window(declared))
    })
}
