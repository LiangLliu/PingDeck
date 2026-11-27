use serde::{Deserialize, Serialize};

use crate::items::ExecutedRequestSummary;
use crate::validation::{WorkspaceError, validate_id};

use super::{
    CallChainTrace, EnvironmentId, HistoryEntryId, HttpMethod, KeyValue, PhaseTiming, Request,
    RequestId, WorkspaceId,
};

/// Maximum bytes of response body kept as a history entry preview.
pub const HISTORY_BODY_PREVIEW_LIMIT_BYTES: usize = 16 * 1024;
/// Maximum serialized bytes of a request snapshot stored with a history entry.
pub const HISTORY_REQUEST_SNAPSHOT_LIMIT_BYTES: usize = 256 * 1024;

/// Whether an executed request succeeded or failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionHistoryResultKind {
    Success,
    Failure,
}

/// Which part of SpanBay produced a history row (`execution_history.source`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistorySource {
    /// A request sent from the Requests module (and "Run again" of one).
    #[default]
    Request,
    /// A request of a collection run.
    Runner,
    /// A request step of a flow run.
    Flow,
    /// A request served by one of the user's mock servers.
    Mock,
    /// A request the chat agent's `send_request` tool sent.
    Agent,
}

impl HistorySource {
    /// Stable database string (`execution_history.source`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Runner => "runner",
            Self::Flow => "flow",
            Self::Mock => "mock",
            Self::Agent => "agent",
        }
    }

    /// Parses the database string back into a source.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "request" => Self::Request,
            "runner" => Self::Runner,
            "flow" => Self::Flow,
            "mock" => Self::Mock,
            "agent" => Self::Agent,
            _ => return None,
        })
    }
}

/// Maximum total bytes of response headers stored with a history entry.
pub const HISTORY_HEADERS_LIMIT_BYTES: usize = 16 * 1024;
/// Largest serialized call chain stored with a history entry; a bigger one is omitted.
pub const HISTORY_CALL_CHAIN_LIMIT_BYTES: usize = 32 * 1024;

/// Checks of one recorded execution: assertions plus script tests that ran. A row with
/// no checks has none (`NULL`), so "unchecked" is never read as "all passed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCheckSummary {
    pub passed: u32,
    pub failed: u32,
}

impl HistoryCheckSummary {
    /// Counts `assertions` and the script tests that were not skipped; `None` when nothing ran.
    pub fn of(
        assertions: &[crate::assertion::AssertionResult],
        tests: &[crate::script_result::ScriptTestResult],
    ) -> Option<Self> {
        let outcomes = assertions.iter().map(|assertion| assertion.passed).chain(
            tests
                .iter()
                .filter(|test| !test.skipped)
                .map(|test| test.passed),
        );
        let (mut passed, mut failed) = (0_u32, 0_u32);
        for ok in outcomes {
            if ok {
                passed = passed.saturating_add(1);
            } else {
                failed = failed.saturating_add(1);
            }
        }
        (passed > 0 || failed > 0).then_some(Self { passed, failed })
    }
}

/// The record a history row points back to besides its run (`source_id`, `run_id`):
/// `execution_history.origin_ref`, stored as `<kind>:<id>`. The two kind strings are persisted
/// and must never be renamed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryOriginRef {
    /// The mock traffic row of the hit (`MockTransactionLog::id`; the server is `source_id`).
    MockTxn { id: String },
    /// The request-log row of an AI call (its `request_id`, `rq_<16 hex>`).
    Call { id: String },
}

impl HistoryOriginRef {
    /// The stored text.
    #[must_use]
    pub fn to_storage(&self) -> String {
        match self {
            Self::MockTxn { id } => format!("mock_txn:{id}"),
            Self::Call { id } => format!("call:{id}"),
        }
    }

    /// Parses the stored text; an unknown kind or an empty id reads as no reference.
    #[must_use]
    pub fn from_storage(value: &str) -> Option<Self> {
        let (kind, id) = value.split_once(':')?;
        if id.is_empty() {
            return None;
        }
        let id = id.to_owned();
        match kind {
            "mock_txn" => Some(Self::MockTxn { id }),
            "call" => Some(Self::Call { id }),
            _ => None,
        }
    }
}

/// A recorded request execution with its response summary or error.
///
/// Every field after `executed_at` was added by history v2 and defaults when absent, so rows and
/// payloads from before the migration still load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionHistoryEntry {
    pub id: HistoryEntryId,
    pub workspace_id: WorkspaceId,
    pub request_id: Option<RequestId>,
    pub environment_id: Option<EnvironmentId>,
    pub result_kind: ExecutionHistoryResultKind,
    pub request_name: String,
    pub method: HttpMethod,
    pub request_url: String,
    pub status_code: u16,
    pub status_text: String,
    pub final_url: String,
    pub elapsed_ms: u128,
    pub body_size_bytes: usize,
    pub attempt_count: u32,
    pub response_body_preview: String,
    pub response_body_truncated: bool,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
    pub executed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_snapshot: Option<Request>,
    /// What produced the row.
    #[serde(default)]
    pub source: HistorySource,
    /// Flow or mock server name at record time (empty for requests and runs of collections the
    /// row names through `request_name`).
    #[serde(default)]
    pub source_name: String,
    /// Id of the flow, mock server or collection behind the row (empty for plain requests).
    #[serde(default)]
    pub source_id: String,
    /// Rows of one collection run or flow run share a run id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// `HTTP/1.1`, `HTTP/2.0`, ... as answered by the server; empty when unknown.
    #[serde(default)]
    pub protocol: String,
    /// Phase timing of the exchange, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<PhaseTiming>,
    /// Name of the environment at record time; survives the environment's deletion.
    #[serde(default)]
    pub environment_name: String,
    /// Response headers, secret-bearing values masked, capped at [`HISTORY_HEADERS_LIMIT_BYTES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_headers: Vec<KeyValue>,
    /// True when `response_headers` was cut at the cap.
    #[serde(default)]
    pub response_headers_truncated: bool,
    /// What was put on the wire, secrets masked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_request: Option<ExecutedRequestSummary>,
    /// Variable, auth and transport trace; omitted above [`HISTORY_CALL_CHAIN_LIMIT_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_chain: Option<CallChainTrace>,
    /// Collection and folder names above the request, outermost first, resolved when the row is
    /// read (never stored): empty for a row without a saved request, or when the collection or
    /// folder the snapshot named has since been deleted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collection_path: Vec<String>,
    /// A failed send: it failed while opening the connection (`true`) or later (`false`); absent
    /// when unknown or when the failure was not a transport failure. The same fact the live
    /// `Network` error carries, kept so a reopened row needs no guess from the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_phase: Option<bool>,
    /// Assertion and script test counts; absent when no check ran (mock hits, failed sends).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_summary: Option<HistoryCheckSummary>,
    /// The mock hit or AI call behind the row; absent for older rows and other sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_ref: Option<HistoryOriginRef>,
    /// Title of the chat behind an `agent` row, resolved when the row is read (never stored);
    /// absent for other sources and when the chat was deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_label: Option<String>,
    /// First message of the agent run behind an `agent` row (the turn the chat reveals), resolved
    /// on read; absent like `origin_label` or when the run left no message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_turn_id: Option<String>,
}

impl ExecutionHistoryEntry {
    /// Builds a success history entry, truncating the body to a bounded preview.
    #[allow(clippy::too_many_arguments)]
    pub fn from_success(
        id: impl AsRef<str>,
        request: &Request,
        environment_id: Option<&str>,
        status_code: u16,
        status_text: impl Into<String>,
        final_url: impl Into<String>,
        elapsed_ms: u128,
        body_text: &str,
        body_size_bytes: usize,
        attempt_count: u32,
        executed_at: impl AsRef<str>,
    ) -> Result<Self, WorkspaceError> {
        let (response_body_preview, response_body_truncated) =
            bounded_body_preview(body_text, HISTORY_BODY_PREVIEW_LIMIT_BYTES);

        Ok(Self {
            id: HistoryEntryId::new(id)?,
            workspace_id: request.workspace_id.clone(),
            request_id: Some(request.id.clone()),
            environment_id: environment_id.map(EnvironmentId::new).transpose()?,
            result_kind: ExecutionHistoryResultKind::Success,
            request_name: request.name.clone(),
            method: request.method,
            request_url: request.url.clone(),
            status_code,
            status_text: status_text.into(),
            final_url: final_url.into(),
            elapsed_ms,
            body_size_bytes,
            attempt_count: attempt_count.max(1),
            response_body_preview,
            response_body_truncated,
            error_kind: None,
            error_message: None,
            executed_at: validate_id("execution timestamp", executed_at.as_ref())?,
            request_snapshot: None,
            source: HistorySource::Request,
            source_name: String::new(),
            source_id: String::new(),
            run_id: None,
            protocol: String::new(),
            timing: None,
            environment_name: String::new(),
            response_headers: Vec::new(),
            response_headers_truncated: false,
            sent_request: None,
            call_chain: None,
            collection_path: Vec::new(),
            connect_phase: None,
            check_summary: None,
            origin_ref: None,
            origin_label: None,
            origin_turn_id: None,
        })
    }

    /// Stores a copy of `request` on the entry when its JSON fits the snapshot size limit. Saved
    /// examples are left out: they describe responses, not what was sent.
    #[must_use]
    pub fn with_request_snapshot(mut self, request: &Request) -> Self {
        let mut snapshot = request.clone();
        snapshot.examples.clear();
        let serialized = serde_json::to_string(&snapshot).ok();
        if serialized
            .as_ref()
            .is_some_and(|json| json.len() <= HISTORY_REQUEST_SNAPSHOT_LIMIT_BYTES)
        {
            self.request_snapshot = Some(snapshot);
        }
        self
    }

    /// Builds a failure history entry with the given error kind and message.
    #[allow(clippy::too_many_arguments)]
    pub fn from_failure(
        id: impl AsRef<str>,
        request: &Request,
        environment_id: Option<&str>,
        error_kind: impl Into<String>,
        error_message: impl Into<String>,
        elapsed_ms: u128,
        attempt_count: u32,
        executed_at: impl AsRef<str>,
    ) -> Result<Self, WorkspaceError> {
        Ok(Self {
            id: HistoryEntryId::new(id)?,
            workspace_id: request.workspace_id.clone(),
            request_id: Some(request.id.clone()),
            environment_id: environment_id.map(EnvironmentId::new).transpose()?,
            result_kind: ExecutionHistoryResultKind::Failure,
            request_name: request.name.clone(),
            method: request.method,
            request_url: request.url.clone(),
            status_code: 0,
            status_text: "Failed".into(),
            final_url: String::new(),
            elapsed_ms,
            body_size_bytes: 0,
            attempt_count: attempt_count.max(1),
            response_body_preview: String::new(),
            response_body_truncated: false,
            error_kind: Some(error_kind.into()),
            error_message: Some(error_message.into()),
            executed_at: validate_id("execution timestamp", executed_at.as_ref())?,
            request_snapshot: None,
            source: HistorySource::Request,
            source_name: String::new(),
            source_id: String::new(),
            run_id: None,
            protocol: String::new(),
            timing: None,
            environment_name: String::new(),
            response_headers: Vec::new(),
            response_headers_truncated: false,
            sent_request: None,
            call_chain: None,
            collection_path: Vec::new(),
            connect_phase: None,
            check_summary: None,
            origin_ref: None,
            origin_label: None,
            origin_turn_id: None,
        })
    }

    /// Tags the row with the part of SpanBay that produced it.
    #[must_use]
    pub fn with_origin(mut self, origin: &HistoryOrigin) -> Self {
        self.source = origin.source;
        self.source_name.clone_from(&origin.source_name);
        self.source_id.clone_from(&origin.source_id);
        self.run_id.clone_from(&origin.run_id);
        self
    }

    /// Points the row at the mock hit or AI call behind it.
    #[must_use]
    pub fn with_origin_ref(mut self, origin_ref: Option<HistoryOriginRef>) -> Self {
        self.origin_ref = origin_ref;
        self
    }

    /// Records the check counts of the execution (`None` when no check ran).
    #[must_use]
    pub fn with_check_summary(mut self, summary: Option<HistoryCheckSummary>) -> Self {
        self.check_summary = summary;
        self
    }

    /// Records the name of the environment the request ran in.
    #[must_use]
    pub fn with_environment_name(mut self, name: &str) -> Self {
        name.clone_into(&mut self.environment_name);
        self
    }

    /// Stores the response-side details of the exchange: protocol, phase timing,
    /// response headers (secret-bearing values masked, capped at
    /// [`HISTORY_HEADERS_LIMIT_BYTES`]), the sent request and, when it fits
    /// [`HISTORY_CALL_CHAIN_LIMIT_BYTES`], the call chain.
    #[must_use]
    pub fn with_response_details(
        mut self,
        timing: PhaseTiming,
        response_headers: &[KeyValue],
        sent_request: Option<ExecutedRequestSummary>,
        call_chain: &CallChainTrace,
    ) -> Self {
        self.protocol.clone_from(&call_chain.transport.protocol);
        self.timing = Some(timing);
        let (headers, truncated) = capped_masked_headers(response_headers);
        self.response_headers = headers;
        self.response_headers_truncated = truncated;
        self.sent_request = sent_request.map(|mut sent| {
            for header in &mut sent.headers {
                header.value = mask_sensitive_header(&header.key, &header.value);
            }
            sent
        });
        self.call_chain = serde_json::to_string(call_chain)
            .is_ok_and(|json| json.len() <= HISTORY_CALL_CHAIN_LIMIT_BYTES)
            .then(|| call_chain.clone());
        self
    }
}

/// What produced a history row and which run it belongs to; the origin of a plain Send is
/// [`HistoryOrigin::request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryOrigin {
    /// The producing part of SpanBay.
    pub source: HistorySource,
    /// Flow, mock server or collection name at record time.
    pub source_name: String,
    /// Flow, mock server or collection id.
    pub source_id: String,
    /// Shared by every row of one collection run or flow run.
    pub run_id: Option<String>,
}

impl HistoryOrigin {
    /// A request sent on its own.
    #[must_use]
    pub fn request() -> Self {
        Self {
            source: HistorySource::Request,
            source_name: String::new(),
            source_id: String::new(),
            run_id: None,
        }
    }

    /// A step of a run (`Runner` for a collection run, `Flow` for a flow run).
    #[must_use]
    pub fn run(
        source: HistorySource,
        source_name: impl Into<String>,
        source_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            source,
            source_name: source_name.into(),
            source_id: source_id.into(),
            run_id: Some(run_id.into()),
        }
    }
}

/// Placeholder stored in place of a masked header value.
const MASKED_HEADER_VALUE: &str = "[redacted]";

/// Masks the value of a credential-bearing header, the names
/// [`spanbay_base::secrets::is_secret_header_name`] accepts: `Authorization: Bearer x`
/// keeps its scheme, `Set-Cookie: sid=abc; Path=/` keeps the cookie name and attributes, other
/// sensitive headers lose the whole value. Any other header is returned unchanged.
#[must_use]
pub fn mask_sensitive_header(name: &str, value: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    if lower == "set-cookie" {
        return match value.split_once(';') {
            Some((pair, attributes)) => {
                format!("{};{attributes}", mask_cookie_pair(pair))
            }
            None => mask_cookie_pair(value),
        };
    }
    if !spanbay_base::secrets::is_secret_header_name(&lower) {
        return value.to_owned();
    }
    match value.trim().split_once(' ') {
        Some((scheme, _)) if lower.ends_with("authorization") => {
            format!("{scheme} {MASKED_HEADER_VALUE}")
        }
        _ => MASKED_HEADER_VALUE.to_owned(),
    }
}

/// `name=value` → `name=[redacted]`; a pair without `=` is masked whole.
fn mask_cookie_pair(pair: &str) -> String {
    match pair.split_once('=') {
        Some((name, _)) => format!("{name}={MASKED_HEADER_VALUE}"),
        None => MASKED_HEADER_VALUE.to_owned(),
    }
}

/// Masks credential-bearing headers and keeps rows until [`HISTORY_HEADERS_LIMIT_BYTES`] of
/// header text is reached; the flag reports that rows were dropped.
pub fn capped_masked_headers(headers: &[KeyValue]) -> (Vec<KeyValue>, bool) {
    let mut total = 0_usize;
    let mut kept = Vec::with_capacity(headers.len());
    for header in headers {
        let value = mask_sensitive_header(&header.key, &header.value);
        total = total.saturating_add(header.key.len() + value.len());
        if total > HISTORY_HEADERS_LIMIT_BYTES {
            return (kept, true);
        }
        kept.push(KeyValue {
            key: header.key.clone(),
            value,
            enabled: header.enabled,
            description: None,
        });
    }
    (kept, false)
}

pub fn bounded_body_preview(body_text: &str, limit_bytes: usize) -> (String, bool) {
    if body_text.len() <= limit_bytes {
        return (body_text.to_owned(), false);
    }

    let boundary = body_text
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= limit_bytes)
        .last()
        .unwrap_or(0);

    (body_text[..boundary].to_owned(), true)
}
