//! What a finished AI run becomes outside the core call: the response scripts see, the console
//! line and the history entry.
//!
//! Single responsibility: pure conversions from an [`AiResultDto`]. The history entry is one row
//! per run with the AI protocol, the request snapshot (so `mode` and `ai` are in it),
//! the phase timing and the merged reply as the response preview, not the raw event stream.

use crate::app::{command_error, execution_history_id, execution_history_timestamp};
use crate::error::AppError;
use crate::executor::{ExecutionHeader, ExecutionResult, SentRequest, suggest_view};
use crate::llm::ai_run::{AiResultDto, AiRunStatus};
use crate::workspace::ai_request::{AiScriptResult, AiScriptToolCall, AiScriptUsage};
use crate::workspace::items::ExecutedRequestSummary;
use crate::workspace::models::{
    AiStepSummary, CallChainTrace, HISTORY_BODY_PREVIEW_LIMIT_BYTES, HistoryCheckSummary,
    HistoryOriginRef, PhaseTiming, bounded_body_preview, capped_masked_headers,
};
use crate::workspace::{
    Environment, ExecutionHistoryEntry, HistoryOrigin, HttpMethod, KeyValue, Request,
};
use pingdeck_model::assertion::AiResultView;
use pingdeck_model::script_result::{ConsoleEntry, ConsoleLevel};

/// The figure of a token count: `9`, `~9` when estimated, `–` when unknown.
fn tokens(count: Option<u64>, estimated: bool) -> String {
    match count {
        Some(count) if estimated => format!("~{count}"),
        Some(count) => count.to_string(),
        None => "–".to_owned(),
    }
}

/// `AI deepseek-v4-flash · 612 ms · 9 in / 2 out`.
pub(super) fn console_entry(dto: &AiResultDto) -> ConsoleEntry {
    let usage = &dto.usage;
    ConsoleEntry {
        level: ConsoleLevel::Info,
        message: format!(
            "AI {} · {} ms · {} in / {} out",
            dto.model,
            dto.total_ms,
            tokens(usage.input_tokens, usage.estimated),
            tokens(usage.output_tokens, usage.estimated),
        ),
        source: "AI".to_owned(),
    }
}

/// The data behind `pm.response.ai`.
pub(super) fn script_result(dto: &AiResultDto) -> AiScriptResult {
    AiScriptResult {
        reply: dto.reply.clone(),
        reasoning: dto.reasoning.clone(),
        tool_calls: dto
            .tool_calls
            .iter()
            .map(|call| AiScriptToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect(),
        finish_reason: dto.finish_reason.clone(),
        model: Some(
            dto.response_model
                .clone()
                .unwrap_or_else(|| dto.model.clone()),
        ),
        usage: AiScriptUsage {
            input_tokens: dto.usage.input_tokens,
            output_tokens: dto.usage.output_tokens,
            cached_tokens: dto.usage.cached_tokens,
            reasoning_tokens: dto.usage.reasoning_tokens,
        },
        ttft_ms: dto.ttft_ms,
        total_ms: dto.total_ms,
        tokens_per_second: dto.tokens_per_second,
        event_count: dto.event_count,
    }
}

/// What a collection run row or flow step shows of the run: a run that did not end with an
/// answer (`Done`) fails with its in-stream error, message or status word.
pub(super) fn step_summary(dto: &AiResultDto) -> AiStepSummary {
    let cost = dto.cost.as_ref();
    AiStepSummary {
        model: dto
            .response_model
            .clone()
            .unwrap_or_else(|| dto.model.clone()),
        finish_reason: dto.finish_reason.clone(),
        cost_micro: cost.map(|cost| cost.micro),
        cost_currency: cost.map(|cost| cost.currency.clone()),
        cost_estimated: cost.is_some_and(|cost| cost.estimated),
        error: (dto.status != AiRunStatus::Done).then(|| {
            dto.stream_error
                .clone()
                .or_else(|| dto.message.clone())
                .unwrap_or_else(|| dto.status.label().to_owned())
        }),
    }
}

/// What the AI assertion kinds read. A run with no HTTP answer counts as status 502, so
/// `No error` never passes for an unreachable host.
pub(super) fn view(dto: &AiResultDto) -> AiResultView<'_> {
    AiResultView {
        status: dto.http_status.unwrap_or(502),
        reply: &dto.reply,
        finish_reason: dto.finish_reason.as_deref(),
        error: dto.stream_error.as_deref().or_else(|| {
            dto.http_status
                .is_none()
                .then_some(dto.message.as_deref())
                .flatten()
        }),
        ttft_ms: dto.ttft_ms,
        input_tokens: dto.usage.input_tokens,
        output_tokens: dto.usage.output_tokens,
    }
}

fn status_text(dto: &AiResultDto) -> String {
    if dto.status == AiRunStatus::Done {
        http::StatusCode::from_u16(dto.http_status.unwrap_or(200))
            .ok()
            .and_then(|code| code.canonical_reason())
            .unwrap_or("OK")
            .to_owned()
    } else {
        dto.status.label().to_owned()
    }
}

/// The response as the HTTP scripts and assertions see it: the raw body (the event-stream text
/// for a stream, so `.json()` throws on it) next to `pm.response.ai`.
pub(super) fn execution_result(dto: &AiResultDto, raw_body: &str) -> ExecutionResult {
    let headers: Vec<ExecutionHeader> = dto
        .response_headers
        .iter()
        .map(|header| ExecutionHeader {
            key: header.name.clone(),
            value: header.value.clone(),
        })
        .collect();
    let sent = SentRequest {
        method: dto.request.method.clone(),
        url: dto.request.url.clone(),
        headers: dto
            .request
            .headers
            .iter()
            .map(|header| ExecutionHeader {
                key: header.name.clone(),
                value: header.value.clone(),
            })
            .collect(),
        body_text: Some(dto.request.body.clone()),
        body_size_bytes: dto.request.body_bytes,
        body_truncated: dto.request.body_truncated,
        implicit_headers: Vec::new(),
    };
    ExecutionResult {
        status_code: dto.http_status.unwrap_or(0),
        status_text: status_text(dto),
        url: dto.request.url.clone(),
        elapsed_ms: u128::from(dto.total_ms),
        attempt_count: 1,
        suggested_view: suggest_view(&headers, raw_body),
        headers,
        cookies: Vec::new(),
        body_text: raw_body.to_owned(),
        body_size_bytes: raw_body.len(),
        raw_body: None,
        body_handle: None,
        body_truncated: false,
        body_total_bytes: None,
        body_incomplete: None,
        saved_body_path: None,
        body_base64: None,
        raw_body_available: false,
        sent_request: Some(sent),
        assertion_results: Vec::new(),
        script_tests: Vec::new(),
        console: Vec::new(),
        script_error: None,
        next_request: None,
        stop_run: false,
        timing: phase_timing(dto),
        call_chain: CallChainTrace::default(),
        history_id: None,
        history_saved: false,
        ai: None,
        cookie_report: Default::default(),
    }
}

/// The three-phase timing history keeps: time to the response head (queue and connection setup
/// included), the rest, and the total. A streamed run also keeps the time to its first answer
/// content (TTFT), which the duration cell shows. The full shape lives in the
/// run's log row.
pub(super) fn phase_timing(dto: &AiResultDto) -> PhaseTiming {
    let total = u128::from(dto.total_ms);
    let request = dto.timing.attempts.last().map_or(total, |attempt| {
        u128::from(dto.timing.queue_ms + attempt.setup_ms() + attempt.headers_ms.ms().unwrap_or(0))
            .min(total)
    });
    PhaseTiming {
        request_ms: request,
        response_ms: total - request,
        total_ms: total,
        first_token_ms: dto.ttft_ms,
        ..PhaseTiming::default()
    }
}

/// The history entry of a run that has a result. A run with an HTTP answer is a success row (the
/// status says what the server said, like any HTTP history row); one without is a failure row.
///
/// # Errors
/// [`AppError::Validation`] when the entry cannot be built (an invalid id or timestamp).
pub(super) fn success_entry(
    request: &Request,
    environment: Option<&Environment>,
    dto: &AiResultDto,
    origin: &HistoryOrigin,
) -> Result<ExecutionHistoryEntry, AppError> {
    let preview = if dto.reply.is_empty() {
        dto.message.clone().unwrap_or_default()
    } else {
        dto.reply.clone()
    };
    let environment_id = environment.map(|environment| environment.id.as_str());
    let mut entry = match dto.http_status {
        Some(status) => ExecutionHistoryEntry::from_success(
            execution_history_id(&request.id),
            request,
            environment_id,
            status,
            status_text(dto),
            &dto.request.url,
            u128::from(dto.total_ms),
            &preview,
            preview.len(),
            1,
            execution_history_timestamp(),
        ),
        None => ExecutionHistoryEntry::from_failure(
            execution_history_id(&request.id),
            request,
            environment_id,
            dto.status.label(),
            dto.message.clone().unwrap_or_default(),
            u128::from(dto.total_ms),
            1,
            execution_history_timestamp(),
        ),
    }
    .map_err(command_error)?
    .with_request_snapshot(request)
    .with_origin(origin)
    .with_environment_name(environment.map_or("", |environment| environment.name.as_str()))
    .with_check_summary(HistoryCheckSummary::of(
        &dto.assertion_results,
        &dto.script_tests,
    ))
    .with_origin_ref(
        dto.log_request_id
            .clone()
            .map(|id| HistoryOriginRef::Call { id }),
    );
    entry.method = HttpMethod::Post;
    entry.protocol = dto.protocol.as_str().to_owned();
    entry.timing = Some(phase_timing(dto));
    let response_headers: Vec<KeyValue> = dto
        .response_headers
        .iter()
        .map(|header| KeyValue {
            key: header.name.clone(),
            value: header.value.clone(),
            enabled: true,
            description: None,
        })
        .collect();
    (entry.response_headers, entry.response_headers_truncated) =
        capped_masked_headers(&response_headers);
    let (body_preview, _) =
        bounded_body_preview(&dto.request.body, HISTORY_BODY_PREVIEW_LIMIT_BYTES);
    entry.sent_request = Some(ExecutedRequestSummary {
        method: dto.request.method.clone(),
        url: dto.request.url.clone(),
        headers: dto
            .request
            .headers
            .iter()
            .map(|header| KeyValue {
                key: header.name.clone(),
                value: header.value.clone(),
                enabled: true,
                description: None,
            })
            .collect(),
        body_preview,
    });
    Ok(entry)
}

/// The history entry of a run that failed before the core call (a script error, an unresolved
/// variable, a provider without the protocol's endpoint).
///
/// # Errors
/// [`AppError::Validation`] when the entry cannot be built.
pub(super) fn failure_entry(
    request: &Request,
    environment: Option<&Environment>,
    kind: &str,
    message: &str,
    elapsed_ms: u128,
    origin: &HistoryOrigin,
) -> Result<ExecutionHistoryEntry, AppError> {
    let mut entry = ExecutionHistoryEntry::from_failure(
        execution_history_id(&request.id),
        request,
        environment.map(|environment| environment.id.as_str()),
        kind,
        message,
        elapsed_ms,
        1,
        execution_history_timestamp(),
    )
    .map_err(command_error)?
    .with_request_snapshot(request)
    .with_origin(origin)
    .with_environment_name(environment.map_or("", |environment| environment.name.as_str()));
    entry.method = HttpMethod::Post;
    if let Some(ai) = &request.ai {
        entry.protocol = ai.protocol.as_str().to_owned();
    }
    Ok(entry)
}
