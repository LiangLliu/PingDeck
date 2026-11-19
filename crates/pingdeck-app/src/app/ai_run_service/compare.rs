//! "Run on N models": one request on up to 12 models, its estimate and its export
//!
//! Single responsibility: send the same body to each model with only `model` replaced (the
//! Gemini model travels in the URL the core derives), at most `concurrency` calls at a time
//! (default 3, 1 to 6), stop all of them or one within a second, and give every row its own log
//! row (`source = api`, the run id shared) and history entry. A model whose provider lacks the
//! protocol's endpoint finishes as `Skipped: no <protocol> endpoint` without a request. Variables
//! are resolved once; scripts and assertions belong to the single run (the table shows per-model
//! status, not Tests).

use std::sync::Arc;

use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::compare_frames::LiveFrames;
use super::prepare::{Call, Loaded, load, make_spec, resolve_call};
use super::run::record;
use super::{AiCompareRunInput, AiRequestRunInput};
use crate::app::prod_guard::guard_request;
use crate::app::{AppState, new_run_id};
use crate::cancel::CancelToken;
use crate::error::AppError;
use crate::events::EventSink;
use crate::llm::ai_run::{AiResultDto, AiRunStatus, protocol_label, run_ai_request};
use crate::llm::capture_record::CapturedFrame;
use crate::llm::probe::ExportFile;
use crate::llm::router::RoutingSnapshot;
use crate::llm::store::effective_price;
use crate::llm::usage::{Cost, CostUsage, cost, estimate_tokens};
use crate::llm::wire::Protocol;
use crate::workspace::ai_request::{
    AiTarget, CompareModel, MAX_COMPARE_MODELS, ScriptRequestPolicy, ensure_sendable,
};
use crate::workspace::{HistoryOrigin, HttpMethod, Request, RequestBody, effective_auth};

/// Longest first line of a reply in the table.
const REPLY_LINE_CHARS: usize = 80;
/// Longest reply the CSV export holds.
const CSV_REPLY_CHARS: usize = 2000;

/// One row of the compare table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiCompareRow {
    /// Position in the model list.
    pub index: usize,
    /// Provider id (`custom` for a custom target).
    pub provider_id: String,
    /// Provider name shown next to the model.
    pub provider_name: String,
    /// Model name.
    pub model: String,
    /// How the row ended.
    pub status: AiRunStatus,
    /// The shaped line of a failure (`Skipped: no Responses endpoint`).
    pub message: Option<String>,
    /// HTTP status of the answer.
    pub http_status: Option<u16>,
    /// First line of the reply, at most 80 characters.
    pub reply_line: String,
    /// Time to first token.
    pub ttft_ms: Option<u64>,
    /// Total time.
    pub total_ms: Option<u64>,
    /// Output tokens per second (at least 300 tokens).
    pub tokens_per_second: Option<f64>,
    /// Input tokens.
    pub input_tokens: Option<u64>,
    /// Output tokens.
    pub output_tokens: Option<u64>,
    /// Tokens are estimates (shown with `~`).
    pub tokens_estimated: bool,
    /// The cost, when a price is known.
    pub cost: Option<Cost>,
    /// The row with the lowest TTFT among at least two finished rows ("Fastest").
    pub fastest: bool,
    /// The row with the lowest cost among at least two finished priced rows ("Cheapest").
    pub cheapest: bool,
    /// The full result behind the response tabs; `None` for a skipped row.
    pub result: Option<Box<AiResultDto>>,
}

/// The finished compare run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiCompareResultDto {
    /// Id shared by the rows' log rows and history entries.
    pub run_id: String,
    /// Protocol of the request.
    pub protocol: Protocol,
    /// The request body once (variables resolved; attachment placeholders, never their data).
    pub request_body: String,
    /// One row per model, in the order given.
    pub rows: Vec<AiCompareRow>,
}

/// Events of a compare run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiCompareEvent {
    /// The run begins; row `i` can be stopped with the cancel token `<token>#<i>`.
    Started {
        /// Id of the run.
        run_id: String,
        /// Number of rows.
        total: usize,
    },
    /// A row's call starts.
    RowStarted {
        /// Row position.
        index: usize,
        /// Provider id.
        provider_id: String,
        /// Model name.
        model: String,
    },
    /// Frames a row's stream gained, in the shape of `AiRunEvent::Frames` (`t_ms` counts from the
    /// row's send). At most 30 events a second per row; the last frames always arrive before
    /// `RowDone`, which still carries the whole result.
    RowFrames {
        /// Row position.
        index: usize,
        /// The frames in order.
        items: Vec<CapturedFrame>,
    },
    /// A row ended.
    RowDone {
        /// Row position.
        index: usize,
        /// The row.
        row: Box<AiCompareRow>,
    },
    /// The run ended; the last event.
    Done {
        /// The result.
        result: Box<AiCompareResultDto>,
    },
}

fn checked(input: &AiCompareRunInput) -> Result<(), AppError> {
    if input.models.is_empty() {
        return Err(AppError::validation("Choose at least one model"));
    }
    if input.models.len() > MAX_COMPARE_MODELS {
        return Err(AppError::validation(format!(
            "At most {MAX_COMPARE_MODELS} models per run"
        )));
    }
    if !(1..=6).contains(&input.concurrency) {
        return Err(AppError::validation("Concurrency is 1 to 6"));
    }
    if input
        .models
        .iter()
        .any(|entry| entry.model.trim().is_empty())
    {
        return Err(AppError::validation("A compared model needs a name"));
    }
    Ok(())
}

/// The first non-empty line of `reply`, cut to 80 characters.
pub(super) fn first_line(reply: &str) -> String {
    let line = reply
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.chars().count() > REPLY_LINE_CHARS {
        let mut cut: String = line.chars().take(REPLY_LINE_CHARS - 1).collect();
        cut.push('…');
        cut
    } else {
        line.to_owned()
    }
}

/// Why `provider_id` cannot run `protocol`, when it cannot.
fn skip_reason(
    snapshot: &RoutingSnapshot,
    provider_id: &str,
    protocol: Protocol,
) -> Option<String> {
    let provider = snapshot.providers.get(provider_id)?;
    (!provider
        .endpoints
        .iter()
        .any(|endpoint| endpoint.enabled && endpoint.protocol == protocol))
    .then(|| format!("Skipped: no {} endpoint", protocol_label(protocol)))
}

/// A row that did not run.
pub(super) fn idle_row(
    index: usize,
    entry: &CompareModel,
    provider_name: &str,
    status: AiRunStatus,
    message: String,
) -> AiCompareRow {
    AiCompareRow {
        index,
        provider_id: entry.provider_id.clone(),
        provider_name: provider_name.to_owned(),
        model: entry.model.clone(),
        status,
        message: Some(message),
        http_status: None,
        reply_line: String::new(),
        ttft_ms: None,
        total_ms: None,
        tokens_per_second: None,
        input_tokens: None,
        output_tokens: None,
        tokens_estimated: false,
        cost: None,
        fastest: false,
        cheapest: false,
        result: None,
    }
}

pub(super) fn row_of(
    index: usize,
    entry: &CompareModel,
    provider_name: &str,
    dto: AiResultDto,
) -> AiCompareRow {
    AiCompareRow {
        index,
        provider_id: entry.provider_id.clone(),
        provider_name: provider_name.to_owned(),
        model: entry.model.clone(),
        status: dto.status,
        message: dto.message.clone(),
        http_status: dto.http_status,
        reply_line: first_line(&dto.reply),
        ttft_ms: dto.ttft_ms,
        total_ms: Some(dto.total_ms),
        tokens_per_second: dto.tokens_per_second,
        input_tokens: dto.usage.input_tokens,
        output_tokens: dto.usage.output_tokens,
        tokens_estimated: dto.usage.estimated,
        cost: dto.cost.clone(),
        fastest: false,
        cheapest: false,
        result: Some(Box::new(dto)),
    }
}

/// Marks "Fastest" and "Cheapest" when at least two rows are done. Costs in
/// different currencies are not compared.
pub(super) fn tag_best(rows: &mut [AiCompareRow]) {
    let done: Vec<usize> = rows
        .iter()
        .filter(|row| row.status == AiRunStatus::Done)
        .map(|row| row.index)
        .collect();
    if done.len() < 2 {
        return;
    }
    let fastest = done
        .iter()
        .filter_map(|&at| rows[at].ttft_ms.map(|ttft| (ttft, at)))
        .min();
    if let Some((_, at)) = fastest {
        rows[at].fastest = true;
    }
    let priced: Vec<(i64, usize, String)> = done
        .iter()
        .filter_map(|&at| {
            rows[at]
                .cost
                .as_ref()
                .map(|cost| (cost.micro, at, cost.currency.clone()))
        })
        .collect();
    if priced.len() >= 2
        && priced
            .iter()
            .all(|(_, _, currency)| *currency == priced[0].2)
        && let Some((_, at, _)) = priced.iter().min_by_key(|(micro, _, _)| *micro)
    {
        rows[*at].cheapest = true;
    }
}

/// The request of one row for the history snapshot: its provider and model.
fn row_request(request: &Request, entry: &CompareModel, protocol: Protocol) -> Request {
    let mut row = request.clone();
    if let Some(ai) = row.ai.as_mut() {
        match &mut ai.target {
            AiTarget::Provider {
                provider_id,
                model,
                provider_name,
                ..
            } => {
                provider_id.clone_from(&entry.provider_id);
                model.clone_from(&entry.model);
                provider_name.clear();
            }
            AiTarget::Custom { model } => model.clone_from(&entry.model),
        }
    }
    if protocol != Protocol::Gemini
        && let RequestBody::Json { content } | RequestBody::RawText { content, .. } = &mut row.body
        && let Ok(mut json) = serde_json::from_str::<Value>(content)
    {
        crate::llm::wire::rewrite_model(protocol, &mut json, &entry.model);
        *content = json.to_string();
    }
    row
}

/// Everything the rows share.
struct Shared<'a> {
    state: &'a AppState,
    request: &'a Request,
    context: &'a crate::app::execution_service::ExecutionContext,
    base: &'a Call,
    snapshot: &'a RoutingSnapshot,
    run_id: &'a str,
    keep_exchange: bool,
    token: Option<&'a str>,
    all: &'a CancelToken,
    permits: Arc<Semaphore>,
    sink: &'a dyn EventSink<AiCompareEvent>,
    frames: &'a LiveFrames<'a>,
}

impl Shared<'_> {
    fn provider_name(&self, entry: &CompareModel) -> String {
        if self.base.provider_id.is_none() {
            return "Custom URL".to_owned();
        }
        self.snapshot.providers.get(&entry.provider_id).map_or_else(
            || entry.provider_id.clone(),
            |provider| provider.name.clone(),
        )
    }

    async fn row(&self, index: usize, entry: &CompareModel) -> AiCompareRow {
        let name = self.provider_name(entry);
        let row = self.run_row(index, entry, &name).await;
        self.frames.finish_row(index);
        self.sink.emit(AiCompareEvent::RowDone {
            index,
            row: Box::new(row.clone()),
        });
        row
    }

    async fn run_row(&self, index: usize, entry: &CompareModel, name: &str) -> AiCompareRow {
        if self.base.provider_id.is_some()
            && let Some(reason) = skip_reason(self.snapshot, &entry.provider_id, self.base.protocol)
        {
            return idle_row(index, entry, name, AiRunStatus::Skipped, reason);
        }
        // The row's own token lets the UI stop it alone; Stop all reaches it through `all`.
        let row_id = self.token.map(|token| format!("{token}#{index}"));
        let guard = match self.state.cancellations.register(row_id.as_deref()) {
            Ok(guard) => guard,
            Err(error) => {
                return idle_row(
                    index,
                    entry,
                    name,
                    AiRunStatus::Unsupported,
                    error.message().to_owned(),
                );
            }
        };
        let row_token = guard.token().clone();
        let forward = {
            let all = self.all.clone();
            let row_token = row_token.clone();
            tokio::spawn(async move {
                all.cancelled().await;
                row_token.cancel();
            })
        };
        let permit = crate::cancel::until_cancelled(
            Some(&row_token),
            Arc::clone(&self.permits).acquire_owned(),
        )
        .await;
        let row = match permit {
            Some(Ok(_permit)) => self.call(index, entry, name, &row_token).await,
            _ => idle_row(
                index,
                entry,
                name,
                AiRunStatus::Stopped,
                "Stopped before it started".to_owned(),
            ),
        };
        forward.abort();
        row
    }

    async fn call(
        &self,
        index: usize,
        entry: &CompareModel,
        name: &str,
        cancel: &CancelToken,
    ) -> AiCompareRow {
        self.sink.emit(AiCompareEvent::RowStarted {
            index,
            provider_id: entry.provider_id.clone(),
            model: entry.model.clone(),
        });
        let spec = match self.base.for_model(entry) {
            Ok(call) => make_spec(self.state, &call, self.run_id, self.keep_exchange).await,
            Err(error) => Err(error),
        };
        let spec = match spec {
            Ok(spec) => spec,
            Err(error) => {
                return idle_row(
                    index,
                    entry,
                    name,
                    AiRunStatus::Unsupported,
                    error.message().to_owned(),
                );
            }
        };
        let outcome =
            run_ai_request(&self.state.llm, spec, &self.frames.row_sink(index), cancel).await;
        let dto = outcome.result;
        let origin = HistoryOrigin {
            run_id: Some(self.run_id.to_owned()),
            ..HistoryOrigin::request()
        };
        let request = row_request(self.request, entry, self.base.protocol);
        record(self.state, &request, self.context, &dto, &origin);
        row_of(index, entry, name, dto)
    }
}

/// Runs the request on every model. `Started`, a `RowStarted`, the `RowFrames` of its stream and
/// `RowDone` per row and `Done` go to `sink`; `cancel_token` stops every call within a second,
/// and `<cancel_token>#<index>` one.
///
/// # Errors
/// [`AppError::Validation`] for no model, more than 12, a concurrency outside 1 to 6 or a request
/// that cannot be sent, [`AppError::ConfirmationRequired`] for a guarded environment without
/// confirmation (nothing is sent).
pub async fn ai_compare_run_impl(
    state: &AppState,
    input: AiCompareRunInput,
    cancel_token: Option<String>,
    sink: &dyn EventSink<AiCompareEvent>,
) -> Result<AiCompareResultDto, AppError> {
    checked(&input)?;
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let single: AiRequestRunInput = input.single();
    let Loaded {
        mut request,
        context,
    } = load(
        state,
        &single.workspace_id,
        single.request_id.as_deref(),
        single.request,
        single.environment_id.as_deref(),
    )?;
    ensure_sendable(&request)?;
    guard_request(
        context.environment.as_ref(),
        HttpMethod::Post,
        single.confirmed,
    )?;
    let original = request.clone();
    let dropped = ScriptRequestPolicy::for_request(&original).enforce(&original, &mut request);
    request.auth = effective_auth(&request, context.collection.as_ref(), &context.folders);
    let base = resolve_call(&request, &context, dropped)?;
    let snapshot = state.llm.snapshot().await?;
    let run_id = new_run_id();
    sink.emit(AiCompareEvent::Started {
        run_id: run_id.clone(),
        total: input.models.len(),
    });
    let frames = LiveFrames::new(sink);
    let shared = Shared {
        state,
        request: &request,
        context: &context,
        base: &base,
        snapshot: &snapshot,
        run_id: &run_id,
        keep_exchange: input.keep_exchange,
        token: cancel_token.as_deref(),
        all: guard.token(),
        permits: Arc::new(Semaphore::new(usize::from(input.concurrency))),
        sink,
        frames: &frames,
    };
    let rows = join_all(
        input
            .models
            .iter()
            .enumerate()
            .map(|(index, entry)| shared.row(index, entry)),
    );
    let mut rows = tokio::select! {
        rows = rows => rows,
        never = frames.pump() => match never {},
    };
    tag_best(&mut rows);
    let mut request_body = pingdeck_base::secrets::mask_secrets(&base.body, &base.secrets);
    if request_body.len() > 1024 * 1024 {
        request_body.truncate(request_body.floor_char_boundary(1024 * 1024));
    }
    let result = AiCompareResultDto {
        run_id,
        protocol: base.protocol,
        request_body,
        rows,
    };
    sink.emit(AiCompareEvent::Done {
        result: Box::new(result.clone()),
    });
    Ok(result)
}

/// The most the run can cost, per currency, for the line above Run ("At most ~$0.01 with max
/// tokens 256"): the input estimate plus `max_tokens` output tokens at each model's price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiCompareEstimateDto {
    /// `max_tokens` of the body; `None` is "no cap" and then no figure is given.
    pub max_tokens: Option<u64>,
    /// Estimated input tokens of the body (characters / 4).
    pub input_tokens: u64,
    /// The upper bound per currency in millionths, never converted or added across currencies.
    pub at_most: Vec<(String, i64)>,
    /// Models without a price (their cost is not in the bound).
    pub unpriced_models: usize,
}

/// The output cap the body sets, in any protocol's spelling.
fn max_tokens_of(body: &str) -> Option<u64> {
    let json: Value = serde_json::from_str(body).ok()?;
    [
        "/max_tokens",
        "/max_completion_tokens",
        "/max_output_tokens",
        "/generationConfig/maxOutputTokens",
    ]
    .iter()
    .find_map(|path| json.pointer(path).and_then(Value::as_u64))
}

/// The cost bound of running the request on `input.models` (nothing is sent).
///
/// # Errors
/// As [`ai_compare_run_impl`] for an unusable request or model list.
pub fn ai_compare_estimate_impl(
    state: &AppState,
    input: &AiCompareRunInput,
) -> Result<AiCompareEstimateDto, AppError> {
    if input.models.len() > MAX_COMPARE_MODELS {
        return Err(AppError::validation(format!(
            "At most {MAX_COMPARE_MODELS} models per run"
        )));
    }
    let single = input.single();
    let Loaded {
        mut request,
        context,
    } = load(
        state,
        &single.workspace_id,
        single.request_id.as_deref(),
        single.request,
        single.environment_id.as_deref(),
    )?;
    request.auth = effective_auth(&request, context.collection.as_ref(), &context.folders);
    let base = resolve_call(&request, &context, Vec::new())?;
    let input_tokens = estimate_tokens(&base.body);
    let max_tokens = max_tokens_of(&base.body);
    let mut at_most: Vec<(String, i64)> = Vec::new();
    let mut unpriced_models = 0;
    for entry in &input.models {
        let price = match &base.provider_id {
            Some(_) => {
                let store = state.store();
                effective_price(store.connection(), &entry.provider_id, &entry.model)?
            }
            None => None,
        };
        let (Some(price), Some(max)) = (price, max_tokens) else {
            if max_tokens.is_some() {
                unpriced_models += 1;
            }
            continue;
        };
        let bound = cost(
            &CostUsage {
                input_tokens,
                output_tokens: max,
                tokens_estimated: true,
                ..CostUsage::default()
            },
            &price,
        );
        match at_most
            .iter_mut()
            .find(|(currency, _)| *currency == bound.currency)
        {
            Some((_, micro)) => *micro = micro.saturating_add(bound.micro),
            None => at_most.push((bound.currency, bound.micro)),
        }
    }
    Ok(AiCompareEstimateDto {
        max_tokens,
        input_tokens,
        at_most,
        unpriced_models,
    })
}

/// The format of a compare export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiCompareExportFormat {
    /// The request body once, then one object per model.
    Json,
    /// A header row and one row per model, CRLF-ended (RFC 4180).
    Csv,
}

/// Fixed-point text of a cost in millionths: `0.000020`.
fn money(micro: i64) -> String {
    format!("{}.{:06}", micro / 1_000_000, micro % 1_000_000)
}

/// A CSV field: quoted when it holds a comma, quote or line break; text a spreadsheet would
/// evaluate as a formula gets a leading apostrophe.
fn csv_field(text: &str) -> String {
    let guarded = if text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{text}")
    } else {
        text.to_owned()
    };
    if guarded.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

fn number<T: ToString>(value: Option<T>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

/// Exports a finished compare run as a file for the save dialog.
///
/// # Errors
/// [`AppError::Internal`] only if the JSON cannot be written.
pub fn ai_compare_export_impl(
    result: &AiCompareResultDto,
    format: AiCompareExportFormat,
) -> Result<ExportFile, AppError> {
    let reply_of = |row: &AiCompareRow| {
        row.result
            .as_ref()
            .map_or_else(String::new, |full| full.reply.clone())
    };
    match format {
        AiCompareExportFormat::Json => {
            let body = serde_json::from_str::<Value>(&result.request_body)
                .unwrap_or_else(|_| Value::String(result.request_body.clone()));
            let models: Vec<Value> = result
                .rows
                .iter()
                .map(|row| {
                    json!({
                        "provider": row.provider_name,
                        "model": row.model,
                        "status": row.status.label(),
                        "message": row.message,
                        "reply": reply_of(row),
                        "ttft_ms": row.ttft_ms,
                        "total_ms": row.total_ms,
                        "tokens_per_second": row.tokens_per_second,
                        "tokens_in": row.input_tokens,
                        "tokens_out": row.output_tokens,
                        "cost": row.cost.as_ref().map(|cost| money(cost.micro)),
                        "currency": row.cost.as_ref().map(|cost| cost.currency.clone()),
                    })
                })
                .collect();
            let bytes = serde_json::to_vec_pretty(&json!({
                "run_id": result.run_id,
                "protocol": result.protocol.as_str(),
                "request": body,
                "models": models,
            }))
            .map_err(|error| AppError::internal(format!("cannot write the export: {error}")))?;
            Ok(ExportFile {
                file_name: format!("pingdeck-compare-{}.json", result.run_id),
                bytes,
            })
        }
        AiCompareExportFormat::Csv => {
            let mut out = String::from(
                "Model,Provider,Status,TTFT ms,Total ms,tok/s,Tokens in,Tokens out,Cost,Currency,Reply\r\n",
            );
            for row in &result.rows {
                let reply: String = reply_of(row).chars().take(CSV_REPLY_CHARS).collect();
                let fields = [
                    csv_field(&row.model),
                    csv_field(&row.provider_name),
                    csv_field(row.status.label()),
                    number(row.ttft_ms),
                    number(row.total_ms),
                    row.tokens_per_second
                        .map_or_else(String::new, |tps| format!("{tps:.1}")),
                    number(row.input_tokens),
                    number(row.output_tokens),
                    row.cost
                        .as_ref()
                        .map_or_else(String::new, |cost| money(cost.micro)),
                    row.cost
                        .as_ref()
                        .map_or_else(String::new, |cost| cost.currency.clone()),
                    csv_field(&reply),
                ];
                out.push_str(&fields.join(","));
                out.push_str("\r\n");
            }
            Ok(ExportFile {
                file_name: format!("pingdeck-compare-{}.csv", result.run_id),
                bytes: out.into_bytes(),
            })
        }
    }
}
