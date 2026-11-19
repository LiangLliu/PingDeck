//! Strongly typed workspace items introduced by schema v12: mock servers, flows
//! plus their nested documents.
//!
//! The models are storage- and transport-agnostic: mock routes/responses/rules,
//! data buckets, callbacks, flow steps are nested documents
//! serialized into one JSON column each, so a whole item saves in a single
//! transactional row write.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::assertion::AssertionResult;
use crate::models::{
    AiStepSummary, Assertion, CallChainTrace, HttpMethod, KeyValue, PhaseTiming, WorkspaceId,
};
use crate::script_result::{ConsoleEntry as ScriptConsoleEntry, ScriptTestResult};

/// Renders a simple enum as a stable database string and back.
macro_rules! string_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            /// Stable database string for this variant.
            #[must_use]
            pub const fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            /// Parses the stable database string back into a variant.
            #[must_use]
            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

fn default_true() -> bool {
    true
}

fn default_crud_id_key() -> String {
    "id".to_owned()
}

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

/// A mock server definition: listener settings, routes, callbacks and data buckets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockServer {
    pub id: String,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub port: u16,
    pub endpoint_prefix: String,
    /// Latency added to every response (Mockoon's global latency).
    #[serde(default, alias = "global_latency_ms")]
    pub latency_ms: u64,
    pub cors_enabled: bool,
    /// Headers added to every response; a route header with the same name wins.
    #[serde(default, alias = "global_headers")]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub routes: Vec<MockRoute>,
    /// JSON buckets served by `Crud` routes and the `{{data 'name'}}` helper.
    #[serde(default)]
    pub data_buckets: Vec<MockDataBucket>,
    /// Outbound HTTP calls fired after a response is served.
    #[serde(default)]
    pub callbacks: Vec<MockCallback>,
    /// Forwards requests that match no enabled route to `proxy_base_url`.
    #[serde(default)]
    pub proxy_enabled: bool,
    #[serde(default)]
    pub proxy_base_url: String,
    /// Strips `endpoint_prefix` from the path before forwarding.
    #[serde(default)]
    pub proxy_remove_prefix: bool,
    /// Headers added to the forwarded request.
    #[serde(default)]
    pub proxy_req_headers: Vec<KeyValue>,
    /// Headers appended to the proxied response.
    #[serde(default)]
    pub proxy_res_headers: Vec<KeyValue>,
    pub updated_at_ms: i64,
    /// Environment whose plain variables expand `{{name}}` in the proxy target, the proxy
    /// headers and callback URLs and headers (step one); secrets are never expanded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
}

/// How a mock route answers: scripted HTTP response or CRUD over a data bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MockRouteType {
    /// A route answered by its own responses, rules and templating.
    #[default]
    Http,
    /// A CRUD route backed by a [`MockDataBucket`].
    Crud,
}

string_enum!(MockRouteType {
    Http => "http",
    Crud => "crud",
});

/// One mock route: method and path plus the responses it can serve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockRoute {
    pub id: String,
    /// `ANY`, `ALL`, `*` or an HTTP method name.
    pub method: String,
    /// Route pattern such as `/users/:id` or `/files/*`.
    pub path: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub route_type: MockRouteType,
    /// Data bucket id served by a [`MockRouteType::Crud`] route.
    #[serde(default)]
    pub data_bucket_id: String,
    /// Project property that identifies one CRUD item.
    #[serde(default = "default_crud_id_key")]
    pub crud_id_key: String,
    pub response_mode: MockResponseMode,
    #[serde(default)]
    pub responses: Vec<MockResponse>,
    pub position: i64,
}

/// One response a mock route can serve, with its rules, headers and callbacks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockResponse {
    pub id: String,
    pub label: String,
    pub status_code: u16,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    /// A template rendered per request; with a [`MockBodySource`] it holds the Base64 bytes or
    /// the absolute file path instead.
    pub body: String,
    /// Where the bytes of the body come from; `None` renders `body` as a template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_source: Option<MockBodySource>,
    /// A transport fault served instead of this response (after its latency).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault: Option<MockFault>,
    #[serde(default)]
    pub latency_ms: u64,
    pub is_default: bool,
    pub rules_operator: MockRulesOperator,
    #[serde(default)]
    pub rules: Vec<MockRule>,
    /// Callbacks fired after this response is served.
    #[serde(default)]
    pub callback_ids: Vec<String>,
    pub position: i64,
    /// Sends the response as a server-sent event stream instead of `body`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<MockStream>,
}

/// Where the bytes of a non-template response body come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MockBodySource {
    /// `body` is Base64; the decoded bytes are sent as they are (binary bodies).
    Base64,
    /// `body` is the absolute path of a regular file read per request (Mockoon's file body).
    File,
}

/// A transport fault a response simulates instead of answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MockFault {
    /// The connection stays open without a response until the stream time limit, then closes.
    Hang,
    /// The connection is closed without any response bytes.
    Drop,
}

/// A server-sent event stream: the events, the pause between them and how the stream ends.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockStream {
    #[serde(default)]
    pub events: Vec<MockStreamEvent>,
    /// Pause before every event after the first, in milliseconds.
    #[serde(default)]
    pub interval_ms: u64,
    #[serde(default)]
    pub end: MockStreamEnd,
}

/// One server-sent event; `data` is a template rendered per request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockStreamEvent {
    /// The `event:` field; empty sends none (a `message` event).
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub data: String,
}

/// What happens after the last event of a [`MockStream`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MockStreamEnd {
    /// The response ends normally.
    #[default]
    Close,
    /// The connection stays open without sending more (until the stream time limit).
    Hang,
    /// The connection is cut without ending the response.
    Drop,
}

/// A condition a request must satisfy for a response to be selected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockRule {
    pub id: String,
    pub target: MockRuleTarget,
    /// Header/cookie name, query key or dotted JSON path, depending on target.
    pub modifier: String,
    pub operator: MockRuleOperator,
    pub value: String,
    pub invert: bool,
    pub position: i64,
}

/// One JSON bucket of the Mockoon "Data" tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockDataBucket {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub documentation: String,
    /// A JSON array or object; served as-is by `{{data 'name'}}` and CRUD routes.
    #[serde(default)]
    pub value_json: String,
}

/// One outbound request of the Mockoon "Callbacks" tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockCallback {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub documentation: String,
    pub method: HttpMethod,
    pub url: String,
    #[serde(default)]
    pub delay_ms: u64,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub body: String,
}

/// Largest request or response body a [`MockTransactionLog`] row keeps.
pub const MOCK_LOG_BODY_LIMIT_BYTES: usize = 64 * 1024;

/// One request served (or proxied) by a running mock server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MockTransactionLog {
    pub id: String,
    pub server_id: String,
    pub timestamp_ms: i64,
    pub method: String,
    pub path: String,
    pub query: String,
    #[serde(default)]
    pub request_headers: Vec<KeyValue>,
    /// Request body, cut to [`MOCK_LOG_BODY_LIMIT_BYTES`] on a UTF-8 boundary.
    pub request_body: String,
    /// True when `request_body` was cut short.
    #[serde(default)]
    pub request_body_truncated: bool,
    pub matched_route_id: Option<String>,
    pub matched_response_id: Option<String>,
    pub status_code: u16,
    #[serde(default)]
    pub response_headers: Vec<KeyValue>,
    /// Response body, cut to [`MOCK_LOG_BODY_LIMIT_BYTES`] on a UTF-8 boundary.
    pub response_body: String,
    /// True when `response_body` was cut short.
    #[serde(default)]
    pub response_body_truncated: bool,
    /// True when the response came from the proxy instead of a local route.
    #[serde(default)]
    pub proxied: bool,
    pub elapsed_ms: u64,
    /// Server-side phase timings; `None` for rows stored before they were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<MockPhaseTiming>,
    /// How the request was answered, recorded when it was served; `None` = not recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<MockLogOutcome>,
    /// Why the route picked its response; set only for an `outcome` of `route` from an HTTP route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_by: Option<MockSelection>,
    /// `METHOD path` of the matched route as it was when the request was served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_label: Option<String>,
    /// Label (or status code) of the served response as it was when the request was served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_label: Option<String>,
    /// Results of the callbacks the response fired, in completion order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callbacks: Vec<MockCallbackResult>,
    /// The history row this request wrote, when history recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_entry_id: Option<String>,
}

/// How a mock server answered one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MockLogOutcome {
    /// A route (HTTP or CRUD) or the CORS preflight answered.
    Route,
    /// The proxy relayed the upstream response.
    Proxy,
    /// The proxy was tried and failed (`502`).
    ProxyFailed,
    /// No route matched and no proxy is on (`404`).
    NoMatch,
    /// The `Host` header named another host (`403`).
    RejectedHost,
    /// A write from another website (`403`).
    RejectedOrigin,
    /// The connection limit was reached (`503`).
    Busy,
}

/// How a route chose among its responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MockSelectionKind {
    /// The rules of the response matched.
    Rule,
    /// No rules matched; the default response answered.
    Default,
    /// No rules matched and there is no default; the first response answered.
    First,
    /// Sequential mode.
    Sequence,
    /// Random mode.
    Random,
    /// Rules are switched off; the default answered.
    DisabledRules,
    /// The request's `Prefer: code=<status>` header named this response's status.
    Prefer,
}

/// Why a response was chosen, as recorded when the request was served.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockSelection {
    pub kind: MockSelectionKind,
    /// 1-based number of the rule that decided a `Rule` selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_number: Option<u32>,
    /// A copy of that rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<MockRule>,
    /// The value the rule read from the request, cut to 200 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received: Option<String>,
}

/// The outcome of one callback a served response fired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockCallbackResult {
    pub name: String,
    /// The rendered target URL.
    pub url: String,
    /// The status the target answered; `None` when it was not sent or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Why it was not sent or failed (invalid URL, refused target, too many in flight, network).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub elapsed_ms: u64,
}

/// Where a mock server spent the time of one request, in milliseconds. A listener only sees its
/// own side of the exchange (no DNS, connect or TLS), so only these phases exist; each is set
/// only when it was measured. The reply write is not listed: it happens inside the HTTP
/// connection after the handler returned, so it has no timestamp the handler can take.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockPhaseTiming {
    /// Reading the request body (the head is parsed before the handler runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_ms: Option<u32>,
    /// Matching a route, rules and CRUD, and rendering the reply, without any proxy call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_ms: Option<u32>,
    /// Waiting out the configured artificial latency; set only when a latency applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
    /// The upstream call (target check, request, response body) when the proxy was tried,
    /// whether it answered or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_ms: Option<u32>,
}

/// How a route picks among its responses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MockResponseMode {
    #[default]
    FirstMatch,
    Sequential,
    Random,
    Fallback,
    /// Serves the first response without evaluating rules (Mockoon's
    /// "Disable rules" toggle).
    DisableRules,
}

string_enum!(MockResponseMode {
    FirstMatch => "first_match",
    Sequential => "sequential",
    Random => "random",
    Fallback => "fallback",
    DisableRules => "disable_rules",
});

/// Whether all or any of a response's rules must match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MockRulesOperator {
    And,
    Or,
}

string_enum!(MockRulesOperator {
    And => "and",
    Or => "or",
});

/// The part of the request a mock rule inspects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MockRuleTarget {
    Query,
    Header,
    PathParam,
    Body,
    BodyRaw,
    Method,
    Cookie,
    /// 1-based counter of the requests that reached this route.
    RequestNumber,
}

string_enum!(MockRuleTarget {
    Query => "query",
    Header => "header",
    PathParam => "path_param",
    Body => "body",
    BodyRaw => "body_raw",
    Method => "method",
    Cookie => "cookie",
    RequestNumber => "request_number",
});

/// The comparison a mock rule applies to its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MockRuleOperator {
    Equals,
    Contains,
    StartsWith,
    Regex,
    RegexIgnoreCase,
    Exists,
    /// Missing or empty.
    Null,
    /// A JSON `[]`.
    EmptyArray,
}

string_enum!(MockRuleOperator {
    Equals => "equals",
    Contains => "contains",
    StartsWith => "starts_with",
    Regex => "regex",
    RegexIgnoreCase => "regex_ignore_case",
    Exists => "exists",
    Null => "null",
    EmptyArray => "empty_array",
});

// ---------------------------------------------------------------------------
// Flows
// ---------------------------------------------------------------------------

/// A named, ordered sequence of requests with extractions between steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flow {
    pub id: String,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub description: String,
    pub steps: Vec<FlowStep>,
    pub updated_at_ms: i64,
    /// Flow-level run inputs (`flows.options_json`); omitted while they are the defaults.
    #[serde(default, skip_serializing_if = "FlowOptions::is_default")]
    pub options: FlowOptions,
}

/// The stored run inputs of a flow, used when `flow_run` does not pass its own. Stored in plain
/// text in the main database like the rest of the flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowOptions {
    /// Seed variables of every run (the narrowest scope).
    #[serde(default)]
    pub initial_variables: BTreeMap<String, String>,
    /// Whether cookies set by one step are sent by the later steps of the same run.
    #[serde(default = "default_true")]
    pub cookie_session: bool,
}

impl Default for FlowOptions {
    fn default() -> Self {
        Self {
            initial_variables: BTreeMap::new(),
            cookie_session: true,
        }
    }
}

impl FlowOptions {
    /// Whether these are the defaults (nothing worth storing or serializing).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Most extra attempts a step may configure.
pub const FLOW_MAX_RETRIES: u8 = 5;
/// Longest wait between two attempts of a step, in milliseconds.
pub const FLOW_MAX_RETRY_WAIT_MS: u64 = 60_000;
/// Longest per-attempt timeout a step may configure, in milliseconds.
pub const FLOW_MAX_TIMEOUT_MS: u64 = 600_000;

/// One request in a flow, with its overrides and extractions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowStep {
    pub id: String,
    pub name: String,
    /// Saved request to run; empty when the step carries an inline request.
    #[serde(default)]
    pub request_id: String,
    pub delay_ms: u64,
    #[serde(default)]
    pub extractions: Vec<FlowExtraction>,
    /// Checked after the saved request's own assertions.
    #[serde(default)]
    pub assertions: Vec<Assertion>,
    /// Judges the step by its own assertions only, e.g. "expect 404 after the delete".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore_request_assertions: bool,
    pub continue_on_error: bool,
    pub position: i64,
    #[serde(default)]
    pub method_override: Option<HttpMethod>,
    /// Overrides the saved URL, or is the whole URL for an inline step.
    #[serde(default)]
    pub url_override: Option<String>,
    /// Merged over the saved request headers (used directly for an inline step).
    #[serde(default)]
    pub headers_override: Vec<KeyValue>,
    /// Merged over the saved request query params.
    #[serde(default)]
    pub query_override: Vec<KeyValue>,
    /// Replaces the saved body when present (the whole body of an inline step).
    #[serde(default)]
    pub body_override: Option<String>,
    /// How `body_override` is sent; `None` keeps the saved request's body type (an inline step,
    /// which has none, sends plain text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_override_kind: Option<FlowBodyKind>,
    /// Empty runs the step unconditionally. Otherwise the expression is
    /// interpolated and the step is skipped when it evaluates to false.
    #[serde(default)]
    pub condition_expression: String,
    /// Extra attempts after a failed first one, 0–5. Only used together with `retry_on`.
    #[serde(default)]
    pub retries: u8,
    /// Wait between attempts in milliseconds, 0–60,000.
    #[serde(default)]
    pub retry_wait_ms: u64,
    /// The failures that trigger a retry; assertion, script and extraction failures never do.
    #[serde(default)]
    pub retry_on: Vec<FlowRetryOn>,
    /// Timeout of each attempt in milliseconds (1–600,000); `None` keeps the request's own.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// The body type a step's `body_override` is sent as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowBodyKind {
    /// `application/json`.
    Json,
    /// `text/plain`.
    Text,
    /// `application/xml`.
    Xml,
    /// `text/html`.
    Html,
    /// `application/javascript`.
    Javascript,
    /// `application/x-www-form-urlencoded`: `key=value` pairs, one per line.
    FormUrlencoded,
}

/// A value a flow step captures from its response into a variable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowExtraction {
    pub variable_name: String,
    pub source: FlowExtractionSource,
    pub expression: String,
    /// Whether the captured value is a secret: masked in every later step's history and kept out
    /// of stored run summaries. `None` follows [`FlowExtraction::is_secret`]'s default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<bool>,
}

impl FlowExtraction {
    /// The explicit `secret` flag, else whether the extraction looks like a credential: a
    /// credential-like variable name (`token`, `api_key`, `client_secret`), a cookie, or a
    /// credential header.
    #[must_use]
    pub fn is_secret(&self) -> bool {
        self.secret.unwrap_or_else(|| {
            pingdeck_base::secrets::is_secret_header_name(&self.variable_name)
                || self.source == FlowExtractionSource::Cookie
                || (self.source == FlowExtractionSource::Header
                    && pingdeck_base::secrets::is_secret_header_name(&self.expression))
        })
    }
}

/// Where a flow extraction reads its value from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowExtractionSource {
    BodyJsonPath,
    Header,
    StatusCode,
    /// Regex over the response body; capture group 1 or the whole match.
    BodyRegex,
    /// Cookie value by name from `Set-Cookie`.
    Cookie,
    /// The whole response body text.
    ResponseBody,
}

string_enum!(FlowExtractionSource {
    BodyJsonPath => "body_json_path",
    Header => "header",
    StatusCode => "status_code",
    BodyRegex => "body_regex",
    Cookie => "cookie",
    ResponseBody => "response_body",
});

/// Inputs for one run: seed variables, an optional step ceiling or single step, and the run's
/// cookie session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowRunInput {
    #[serde(default)]
    pub initial_variables: BTreeMap<String, String>,
    /// Runs at most this many steps, for "run up to step N" debugging.
    #[serde(default)]
    pub max_steps: Option<usize>,
    /// Runs only the step at this execution-order index (from 0), starting from
    /// `initial_variables` (the caller passes the previous run's variables there).
    #[serde(default)]
    pub only_step: Option<usize>,
    /// Whether cookies set by one step are sent by the later steps of this run; they are never
    /// written to the workspace jar.
    #[serde(default = "default_true")]
    pub cookie_session: bool,
}

impl Default for FlowRunInput {
    fn default() -> Self {
        Self {
            initial_variables: BTreeMap::new(),
            max_steps: None,
            only_step: None,
            cookie_session: true,
        }
    }
}

impl FlowRunInput {
    /// Which steps this input runs.
    #[must_use]
    pub fn scope(&self) -> FlowRunScope {
        match (self.only_step, self.max_steps) {
            (Some(index), _) => FlowRunScope {
                kind: FlowRunScopeKind::Only,
                step_index: Some(index),
            },
            (None, Some(max)) => FlowRunScope {
                kind: FlowRunScopeKind::UpTo,
                step_index: Some(max.saturating_sub(1)),
            },
            (None, None) => FlowRunScope::default(),
        }
    }
}

/// Which steps a run covered: all, the first ones up to a step, or one step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowRunScope {
    pub kind: FlowRunScopeKind,
    /// The last step of an `up_to` run, or the only step of an `only` run (execution order,
    /// from 0); `None` for a full run.
    #[serde(default)]
    pub step_index: Option<usize>,
}

/// The kind of [`FlowRunScope`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowRunScopeKind {
    #[default]
    Full,
    UpTo,
    Only,
}

/// Outcome of a whole flow run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowRunResult {
    /// Identifies this run: the `flow_runs` row, the stream events and the history rows of its
    /// steps (`run_id`) all carry it.
    pub run_id: String,
    pub flow_id: String,
    /// When the run started, in Unix milliseconds.
    pub started_at_ms: i64,
    pub passed: bool,
    pub cancelled: bool,
    pub elapsed_ms: u64,
    /// Number of steps the flow has (not the number that ran: see `max_steps`, failures).
    pub steps_total: usize,
    /// Flow-scope variables after the last executed step: earlier extractions plus what scripts
    /// set with `pm.variables` (see `docs/architecture.md` §7).
    pub variables: Vec<KeyValue>,
    /// Keys of `variables` whose values are secrets (a secret extraction, or a seed with its
    /// name).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_variables: Vec<String>,
    pub steps: Vec<FlowStepResult>,
    /// Which steps this run covered.
    #[serde(default)]
    pub scope: FlowRunScope,
    /// The environment the run used; `None` for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
}

/// What `flow_list` and the results strip show of a past run: no step bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowRunSummary {
    pub run_id: String,
    pub flow_id: String,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub passed: bool,
    pub cancelled: bool,
    /// The first step that failed, if any.
    pub failed_step_id: Option<String>,
    pub failed_step_name: Option<String>,
    /// Steps that ran and passed (skipped steps do not count).
    pub steps_passed: usize,
    pub steps_total: usize,
    pub elapsed_ms: u64,
    /// Every step that finished, in execution order, so a stored run shows per-step
    /// results after a restart; empty for runs stored before steps were kept.
    #[serde(default)]
    pub steps: Vec<FlowStepSummary>,
    /// Which steps the run covered.
    #[serde(default)]
    pub scope: FlowRunScope,
    /// The environment the run used; `None` for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    /// The flow variables the run ended with, so a later "only this step" or "up to" run can
    /// start from them after a restart. Secret values, values a redaction pattern matches and
    /// oversized values are left out and named in `withheld_variables`. `None` when the summary
    /// carries no variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<Vec<KeyValue>>,
    /// Keys of the flow variables whose values were not stored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub withheld_variables: Vec<String>,
}

/// One finished step as a progress event and a stored run report it: no bodies, headers or
/// traces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowStepSummary {
    /// Position of the step in the flow's execution order, from 0.
    pub index: usize,
    pub step_id: String,
    pub step_name: String,
    pub status_code: Option<u16>,
    pub elapsed_ms: u64,
    pub passed: bool,
    pub skipped: bool,
    pub attempts: u32,
    /// The first line of the step's error, when it failed with one.
    pub error: Option<String>,
    /// The history row of the step's last send.
    #[serde(default)]
    pub history_entry_id: Option<String>,
    /// Assertions (request and step) that passed and failed.
    #[serde(default)]
    pub assertions_passed: usize,
    #[serde(default)]
    pub assertions_failed: usize,
    /// How a skipped step's condition read with its variables filled in (secrets masked).
    #[serde(default)]
    pub condition: Option<String>,
    /// Why the step failed, for the localized sentence.
    #[serde(default)]
    pub explanation: Option<FailureExplanation>,
    /// Model, finish reason and cost of an AI request step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiStepSummary>,
}

/// Which failures of one attempt make a step retry (`FlowStep.retries`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowRetryOn {
    /// The request could not be sent or its response could not be read (connect, timeout, TLS).
    #[serde(rename = "network")]
    Network,
    /// The response status is 500–599.
    #[serde(rename = "status_5xx")]
    Status5xx,
    /// The response status is 429.
    #[serde(rename = "status_429")]
    Status429,
}

/// Outcome of one flow step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowStepResult {
    pub step_id: String,
    pub step_name: String,
    pub request_id: String,
    pub status_code: Option<u16>,
    pub elapsed_ms: u64,
    pub passed: bool,
    pub error: Option<String>,
    /// True when `condition_expression` evaluated to false.
    #[serde(default)]
    pub skipped: bool,
    /// How many times the step was sent, retries included. 0 when nothing was sent (skipped
    /// step, or the step's request could not be built); every other result keeps the data of the
    /// last attempt.
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub extracted_variables: Vec<KeyValue>,
    /// The saved request's assertions first, then the step's.
    #[serde(default)]
    pub assertion_results: Vec<AssertionResult>,
    /// How many of the first `assertion_results` come from the saved request.
    #[serde(default)]
    pub request_assertion_count: usize,
    /// The history row of the last send.
    #[serde(default)]
    pub history_entry_id: Option<String>,
    /// How a skipped step's condition read with its variables filled in (secrets masked).
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub executed_request: Option<ExecutedRequestSummary>,
    #[serde(default)]
    pub response_headers: Vec<KeyValue>,
    #[serde(default)]
    pub response_body_preview: String,
    #[serde(default)]
    pub timing: Option<PhaseTiming>,
    #[serde(default)]
    pub console_logs: Vec<ScriptConsoleEntry>,
    /// `pm.test` outcomes of the collection, folder and request scripts.
    #[serde(default)]
    pub script_tests: Vec<ScriptTestResult>,
    #[serde(default)]
    pub call_chain: Option<CallChainTrace>,
    /// Why a failed step failed, from deterministic rules.
    #[serde(default)]
    pub explanation: Option<FailureExplanation>,
    /// Every send of the step, oldest first, so the drawer shows what each retry
    /// answered; empty when nothing was sent (a skipped step, or a request that could not be
    /// built). The step's own fields keep the last attempt.
    #[serde(default)]
    pub attempt_list: Vec<FlowAttemptResult>,
    /// Model, finish reason and cost of an AI request step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiStepSummary>,
}

/// One send of a flow step, retries included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowAttemptResult {
    /// Position of the send, from 1.
    pub attempt: u32,
    /// HTTP status of this send; `None` when it got no answer.
    pub status_code: Option<u16>,
    /// Milliseconds this send took (without the wait before the next one).
    pub elapsed_ms: u64,
    /// Why this send failed, when it did; the next send happened when the step retries that class.
    pub error: Option<String>,
    /// The connection and body phases of this send, like the step's last attempt.
    #[serde(default)]
    pub timing: Option<PhaseTiming>,
}

/// A one-sentence reason and next action for a failed step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureExplanation {
    pub kind: FailureKind,
    /// The sentence to show, e.g. "POST /refunds answered 404 Not Found.".
    pub message: String,
    pub action: ExplanationAction,
    /// The raw failure text the sentence was derived from.
    pub detail: Option<String>,
    /// The values a localized sentence is built from.
    #[serde(default)]
    pub facts: Option<FailureFacts>,
}

/// The parts of a failure a localized sentence names; each is absent when it does not apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    /// A variable name, or the saved request's name for `request_deleted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variable: Option<String>,
}

/// Which rule explained a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// A status assertion failed and the actual status is in the expected status's class.
    StatusMismatch,
    /// The server answered 4xx or 5xx.
    HttpError,
    /// A `{{variable}}` had no value.
    UnresolvedVariable,
    /// An extraction found nothing in the response.
    ExtractionNotFound,
    /// The request never got a response.
    Network,
    /// No response arrived within the timeout.
    Timeout,
    /// The TLS handshake or certificate check failed.
    Tls,
    /// A script threw.
    Script,
    /// An assertion or script test failed on a non-error response.
    AssertionFailed,
    /// The step's saved request no longer exists.
    RequestDeleted,
    /// The step has neither a saved request nor a URL.
    StepIncomplete,
}

/// The one action a failure explanation offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplanationAction {
    EditAssertion,
    OpenResponse,
    AddExtraction,
    CheckNetworkSettings,
}

/// The request a flow step actually sent, secrets already masked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutedRequestSummary {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub body_preview: String,
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Resolves a saved JSON path (Flow extraction, Mock rule or template) in the one dialect the
/// request assertions and the response filter use; see
/// [`pingdeck_response::response_viewer::json_path_value`]. Unsupported syntax is an error.
pub fn json_path_lookup<'a>(
    value: &'a Value,
    path: &str,
) -> Result<Option<std::borrow::Cow<'a, Value>>, String> {
    pingdeck_response::response_viewer::json_path_value(value, path)
}

/// Renders a JSON scalar/object as the text a template or extraction produces.
#[must_use]
pub fn json_value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip_through_storage_strings() {
        for mode in [
            MockResponseMode::FirstMatch,
            MockResponseMode::Sequential,
            MockResponseMode::Random,
            MockResponseMode::Fallback,
            MockResponseMode::DisableRules,
        ] {
            assert_eq!(MockResponseMode::parse(mode.as_str()), Some(mode));
        }
        for route_type in [MockRouteType::Http, MockRouteType::Crud] {
            assert_eq!(MockRouteType::parse(route_type.as_str()), Some(route_type));
        }
        for source in [
            FlowExtractionSource::BodyRegex,
            FlowExtractionSource::Cookie,
            FlowExtractionSource::ResponseBody,
        ] {
            assert_eq!(FlowExtractionSource::parse(source.as_str()), Some(source));
        }
        assert_eq!(
            MockRuleTarget::parse("request_number"),
            Some(MockRuleTarget::RequestNumber)
        );
        assert_eq!(
            MockRuleOperator::parse("regex_ignore_case"),
            Some(MockRuleOperator::RegexIgnoreCase)
        );
        assert_eq!(MockRuleOperator::parse("nope"), None);
    }

    #[test]
    fn mock_items_default_to_enabled_routes_and_id_crud_keys() {
        let route: MockRoute = serde_json::from_value(serde_json::json!({
            "id": "route_1",
            "method": "GET",
            "path": "/users",
            "response_mode": "FirstMatch",
            "position": 0,
        }))
        .expect("minimal route should deserialize");

        assert!(route.enabled);
        assert_eq!(route.route_type, MockRouteType::Http);
        assert_eq!(route.crud_id_key, "id");
        assert_eq!(route.description, "");
    }
}
