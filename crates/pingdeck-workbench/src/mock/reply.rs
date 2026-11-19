//! Builds the reply for a parsed request: route dispatch, decoration, latency and CORS preflight.
//! Latency is carried on the reply; the connection handler waits it out asynchronously.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use serde_json::Value;

use crate::workspace::items::{
    MockBodySource, MockCallbackResult, MockFault, MockLogOutcome, MockResponse, MockRoute,
    MockRouteType, MockSelection, MockServer, MockStream, MockStreamEnd,
};
use crate::workspace::models::KeyValue;

use super::callbacks::{PendingCallback, prepare_callbacks};
use super::crud::{crud_reply, find_crud_route};
use super::http::{MAX_BODY_BYTES, ParsedRequest, json_headers};
use super::proxy::proxy_reply;
use super::routing::{find_route, header_value, parse_cookies};
use super::server::{ServerState, lock};
use super::template::{TemplateContext, render};

/// Configured latency is clamped so a bad value cannot pin a worker slot.
pub(super) const MAX_LATENCY_MS: u64 = 60_000;

/// Most events one stream response sends; later ones are left out.
pub(super) const MAX_STREAM_EVENTS: usize = 100;

/// Longest a stream response holds its connection slot, pauses and a hang included; events
/// that would start later are left out.
pub(super) const MAX_STREAM_MS: u64 = 60_000;

/// Body of a mock response: a buffered reply or a server-sent event stream.
pub(super) type ReplyBody = UnsyncBoxBody<Bytes, std::io::Error>;

/// The rendered events of a stream response and how to pace and end them.
pub(super) struct StreamPlan {
    /// One `event:`/`data:` block per event, each ending in a blank line.
    pub(super) frames: Vec<String>,
    pub(super) interval: Duration,
    pub(super) end: MockStreamEnd,
}

/// A fully built reply plus the route/response ids it came from, for logging.
pub(super) struct MockReply {
    pub(super) status: u16,
    pub(super) headers: Vec<(String, String)>,
    /// The body bytes as sent (the log and history show them decoded as UTF-8).
    pub(super) body: Bytes,
    pub(super) route_id: Option<String>,
    pub(super) response_id: Option<String>,
    /// True when the response was relayed from the upstream proxy.
    pub(super) proxied: bool,
    /// How long to hold the reply back (configured latency, already clamped).
    pub(super) latency: Duration,
    /// How the request was answered, for the log.
    pub(super) outcome: MockLogOutcome,
    /// Why the route chose this response (HTTP routes only).
    pub(super) selection: Option<MockSelection>,
    /// `METHOD path` of the route that answered.
    pub(super) route_label: Option<String>,
    /// Label (or status code) of the response that answered.
    pub(super) response_label: Option<String>,
    /// Callbacks to fire once the exchange is logged.
    pub(super) callbacks: Vec<PendingCallback>,
    /// Callbacks that were dropped before firing (too many in flight).
    pub(super) callback_results: Vec<MockCallbackResult>,
    /// Set for a stream response: `body` then only holds the whole stream for the log.
    pub(super) stream: Option<StreamPlan>,
    /// A transport fault served instead of the reply, once the latency has passed.
    pub(super) fault: Option<MockFault>,
}

impl MockReply {
    /// A JSON reply with no response id.
    pub(super) fn json(status: u16, body: &Value, route_id: Option<String>) -> Self {
        Self {
            status,
            headers: json_headers(),
            body: Bytes::from(body.to_string()),
            route_id,
            response_id: None,
            proxied: false,
            latency: Duration::ZERO,
            outcome: MockLogOutcome::Route,
            selection: None,
            route_label: None,
            response_label: None,
            callbacks: Vec::new(),
            callback_results: Vec::new(),
            stream: None,
            fault: None,
        }
    }

    /// The same reply logged with another outcome.
    pub(super) fn with_outcome(mut self, outcome: MockLogOutcome) -> Self {
        self.outcome = outcome;
        self
    }

    fn not_found(request: &ParsedRequest) -> Self {
        Self::json(
            404,
            &serde_json::json!({ "error": "not_found", "path": request.path }),
            None,
        )
        .with_outcome(MockLogOutcome::NoMatch)
    }
}

/// Picks the reply for one request. A cross-site write ([`is_cross_site_write`]) is answered
/// `403` before anything else, so it never reaches a route counter, a CRUD bucket, a callback
/// or the proxy. Requests outside the endpoint prefix (`relative_path` is `None`) are never
/// routed locally. Inside it the order is: explicit HTTP route, CRUD route, proxy, CORS
/// preflight, 404. HTTP routes come first so `GET /users/me` or `POST /users/login`
/// stay reachable beside a CRUD route at `/users`, whose `/users/<id>` captures only what no
/// explicit route claimed. `proxy_time` is set to the time the upstream call took when the
/// proxy was tried (even when it failed).
pub(super) async fn build_reply(
    config: &MockServer,
    state: &ServerState,
    request: &ParsedRequest,
    relative_path: Option<&str>,
    base_url: &str,
    peer_ip: &str,
    proxy_time: &mut Option<Duration>,
) -> MockReply {
    if is_cross_site_write(&request.method, &request.headers) {
        return MockReply::json(
            403,
            &serde_json::json!({
                "error": "cross_site_write",
                "message": "write requests from other websites are refused",
            }),
            None,
        )
        .with_outcome(MockLogOutcome::RejectedOrigin);
    }
    let cookies = parse_cookies(&request.headers);
    let body_json: Option<Value> = serde_json::from_str(&request.body).ok();
    let no_buckets = HashMap::new();
    let live_buckets;
    let mut context = TemplateContext {
        method: &request.method,
        path: &request.path,
        query: &request.query,
        headers: &request.headers,
        cookies: &cookies,
        params: HashMap::new(),
        body: &request.body,
        body_json: body_json.as_ref(),
        peer_ip,
        base_url,
        buckets: &no_buckets,
        bucket_defs: &config.data_buckets,
    };

    if let Some(path) = relative_path {
        let routed = find_route(
            config,
            &state.counters,
            &state.route_hits,
            request,
            path,
            &cookies,
            body_json.as_ref(),
        );
        if let Some((route, params, response, selection)) = routed {
            context.params = params;
            if response_uses_data(config, &response) {
                live_buckets = bucket_snapshot(state);
                context.buckets = &live_buckets;
            }
            let file_body = match response.body_source {
                Some(MockBodySource::File) if response.stream.is_none() => {
                    Some(read_body_file_off_runtime(response.body.trim().to_owned()).await)
                }
                _ => None,
            };
            let mut reply = serve_route(config, request, &route, &response, &context, file_body);
            reply.selection = Some(selection);
            (reply.callbacks, reply.callback_results) =
                prepare_callbacks(config, &response, &context);
            return reply;
        }
        if let Some((route, item_id)) = find_crud_route(config, &request.method, path) {
            if request.method == "OPTIONS" && config.cors_enabled {
                return preflight_reply(request);
            }
            let reply = crud_reply(
                state,
                route,
                item_id.as_deref(),
                request,
                body_json.as_ref(),
            );
            if headers_use_data(&config.headers) {
                live_buckets = bucket_snapshot(state);
                context.buckets = &live_buckets;
            }
            let mut reply = decorate_crud_reply(config, request, &context, reply);
            reply.route_label = Some(route_label(route));
            return reply;
        }
    }

    if config.proxy_enabled && !config.proxy_base_url.trim().is_empty() {
        if proxy_uses_data(config) {
            live_buckets = bucket_snapshot(state);
            context.buckets = &live_buckets;
        }
        let proxy_started = Instant::now();
        let reply = proxy_reply(
            config,
            &state.client,
            request,
            relative_path,
            &context,
            state.bound,
        )
        .await;
        *proxy_time = Some(proxy_started.elapsed());
        return reply;
    }

    if request.method == "OPTIONS" && config.cors_enabled {
        return preflight_reply(request);
    }

    MockReply::not_found(request)
}

/// Copy of the live buckets keyed by id; one clone per request, and only for
/// templates that reference `{{data ...}}`. `{{data 'name'}}` resolves names
/// through [`TemplateContext::bucket_defs`], so CRUD mutations from earlier
/// calls are visible without cloning each bucket twice.
fn bucket_snapshot(state: &ServerState) -> HashMap<String, Value> {
    lock(&state.buckets).snapshot()
}

/// Whether `template` calls the `data` helper, as an expression (`{{data 'x'}}`) or as a
/// sub-expression (`{{#each (data 'x')}}`).
pub(super) fn template_uses_data(template: &str) -> bool {
    template.contains("{{")
        && template.match_indices("data").any(|(index, _)| {
            let opens_call = template[..index].trim_end().ends_with(['{', '(']);
            opens_call
                && template[index + 4..].starts_with(|character: char| {
                    character.is_whitespace() || character == '\'' || character == '"'
                })
        })
}

fn headers_use_data(headers: &[KeyValue]) -> bool {
    headers
        .iter()
        .filter(|header| header.enabled)
        .any(|header| template_uses_data(&header.value))
}

/// Whether serving `response` renders any template that reads a data bucket:
/// the body, global and route headers, or an attached callback.
pub(super) fn response_uses_data(config: &MockServer, response: &MockResponse) -> bool {
    template_uses_data(&response.body)
        || response.stream.as_ref().is_some_and(|stream| {
            stream
                .events
                .iter()
                .any(|event| template_uses_data(&event.data))
        })
        || headers_use_data(&response.headers)
        || headers_use_data(&config.headers)
        || response
            .callback_ids
            .iter()
            .filter_map(|id| config.callbacks.iter().find(|callback| &callback.id == id))
            .any(|callback| {
                template_uses_data(&callback.url)
                    || template_uses_data(&callback.body)
                    || headers_use_data(&callback.headers)
            })
}

/// Whether the proxy request/response header templates read a data bucket.
pub(super) fn proxy_uses_data(config: &MockServer) -> bool {
    headers_use_data(&config.proxy_req_headers) || headers_use_data(&config.proxy_res_headers)
}

fn serve_route(
    config: &MockServer,
    request: &ParsedRequest,
    route: &MockRoute,
    response: &MockResponse,
    context: &TemplateContext<'_>,
    file_body: Option<Result<Bytes, String>>,
) -> MockReply {
    let own_headers = response
        .headers
        .iter()
        .filter(|header| header.enabled)
        .map(|header| (header.key.clone(), render(&header.value, context)));
    let headers = decorated_headers(config, request, context, own_headers);
    let latency = latency_for(config.latency_ms.saturating_add(response.latency_ms));

    let response_label = if response.label.trim().is_empty() {
        response.status_code.to_string()
    } else {
        response.label.trim().to_owned()
    };
    let reply = MockReply {
        route_label: Some(route_label(route)),
        response_label: Some(response_label),
        fault: response.fault,
        ..MockReply::json(response.status_code, &Value::Null, Some(route.id.clone()))
    };
    let Some(stream) = &response.stream else {
        return match response_body(response, context, file_body) {
            Ok(body) => reply
                .with_body(headers, body, latency)
                .with_response(response.id.clone()),
            Err(message) => MockReply {
                route_label: reply.route_label,
                response_label: reply.response_label,
                ..MockReply::json(
                    500,
                    &serde_json::json!({ "error": "mock_body_unavailable", "message": message }),
                    Some(route.id.clone()),
                )
            }
            .with_response(response.id.clone()),
        };
    };
    let mut headers = headers;
    for (name, value) in [
        ("Content-Type", "text/event-stream"),
        ("Cache-Control", "no-cache"),
    ] {
        if !headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case(name))
        {
            headers.push((name.to_owned(), value.to_owned()));
        }
    }
    let plan = plan_stream(stream, |data| render(data, context));
    let mut reply = reply
        .with_body(headers, plan.frames.concat(), latency)
        .with_response(response.id.clone());
    reply.stream = Some(plan);
    reply
}

/// The bytes of a buffered response: the rendered template, the decoded Base64 text, or the
/// file at the configured absolute path (a regular file of at most [`MAX_BODY_BYTES`], read
/// per request off the runtime threads by [`read_body_file_off_runtime`] into `file_body`, its
/// path never templated). The error says why the body cannot be served.
fn response_body(
    response: &MockResponse,
    context: &TemplateContext<'_>,
    file_body: Option<Result<Bytes, String>>,
) -> Result<Bytes, String> {
    match response.body_source {
        None => Ok(Bytes::from(render(&response.body, context))),
        Some(MockBodySource::Base64) => decode_base64_body(&response.body),
        Some(MockBodySource::File) => {
            file_body.unwrap_or_else(|| Err("the body file was not read".to_owned()))
        }
    }
}

/// Longest a file body read may take before the request answers 500.
const FILE_BODY_TIMEOUT: Duration = Duration::from_secs(10);

/// [`read_body_file`] on the blocking pool, so a slow mount never stalls the mock runtime's two
/// worker threads, bounded by [`FILE_BODY_TIMEOUT`].
async fn read_body_file_off_runtime(path: String) -> Result<Bytes, String> {
    let read = tokio::task::spawn_blocking(move || read_body_file(&path));
    match tokio::time::timeout(FILE_BODY_TIMEOUT, read).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("the body file cannot be read".to_owned()),
        Err(_) => Err(format!(
            "reading the body file took longer than {} s",
            FILE_BODY_TIMEOUT.as_secs()
        )),
    }
}

/// Standard or URL-safe Base64, padded or not, with whitespace and line breaks ignored; the error
/// says why the text cannot be a [`MockBodySource::Base64`] body.
///
/// # Errors
/// The text is not Base64, or decodes to more than the 1 MiB body cap.
pub fn decode_base64_body(text: &str) -> Result<Bytes, String> {
    use base64::Engine as _;
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    const LENIENT: GeneralPurposeConfig =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let standard = GeneralPurpose::new(&base64::alphabet::STANDARD, LENIENT);
    let url_safe = GeneralPurpose::new(&base64::alphabet::URL_SAFE, LENIENT);
    let bytes = standard
        .decode(&compact)
        .or_else(|_| url_safe.decode(&compact))
        .map_err(|error| format!("the Base64 body is not valid: {error}"))?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(format!("the decoded body exceeds {MAX_BODY_BYTES} bytes"));
    }
    Ok(Bytes::from(bytes))
}

/// `bytes` as the text of a [`MockBodySource::Base64`] body (standard alphabet, padded).
#[must_use]
pub fn encode_base64_body(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The file a file body names: an absolute path to a regular file of at most
/// [`MAX_BODY_BYTES`]. The type is checked before opening, so a FIFO or device never blocks
/// in `open`.
fn read_body_file(path: &str) -> Result<Bytes, String> {
    use std::io::Read as _;
    let path = std::path::Path::new(path);
    if !path.is_absolute() {
        return Err("the body file path must be absolute".to_owned());
    }
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("the body file cannot be read: {error}"))?;
    if !metadata.is_file() {
        return Err("the body file path does not name a regular file".to_owned());
    }
    let limit = u64::try_from(MAX_BODY_BYTES).unwrap_or(u64::MAX);
    if metadata.len() > limit {
        return Err(format!("the body file exceeds {MAX_BODY_BYTES} bytes"));
    }
    let file = std::fs::File::open(path)
        .map_err(|error| format!("the body file cannot be read: {error}"))?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("the body file cannot be read: {error}"))?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(format!("the body file exceeds {MAX_BODY_BYTES} bytes"));
    }
    Ok(Bytes::from(bytes))
}

/// Renders the events of `stream` that fit its limits: at most [`MAX_STREAM_EVENTS`], and only
/// those that start within [`MAX_STREAM_MS`] at the configured interval.
pub(super) fn plan_stream(stream: &MockStream, render: impl Fn(&str) -> String) -> StreamPlan {
    let interval_ms = stream.interval_ms.min(MAX_STREAM_MS);
    let in_time = match interval_ms {
        0 => usize::MAX,
        ms => usize::try_from(MAX_STREAM_MS / ms + 1).unwrap_or(usize::MAX),
    };
    let frames = stream
        .events
        .iter()
        .take(MAX_STREAM_EVENTS.min(in_time))
        .map(|event| sse_frame(&event.event, &render(&event.data)))
        .collect();
    StreamPlan {
        frames,
        interval: Duration::from_millis(interval_ms),
        end: stream.end,
    }
}

/// One event in the `text/event-stream` format: an `event:` line unless the name is blank,
/// one `data:` line per line of `data`, then a blank line. Line breaks in the name are dropped
/// so it cannot start another field.
fn sse_frame(event: &str, data: &str) -> String {
    let mut frame = String::with_capacity(data.len() + event.len() + 16);
    let name: String = event
        .chars()
        .filter(|character| !matches!(character, '\r' | '\n'))
        .collect();
    if !name.trim().is_empty() {
        frame.push_str("event: ");
        frame.push_str(name.trim());
        frame.push('\n');
    }
    for line in data.split('\n') {
        frame.push_str("data: ");
        frame.push_str(line.strip_suffix('\r').unwrap_or(line));
        frame.push('\n');
    }
    frame.push('\n');
    frame
}

/// The body that sends `plan`: the first event at once, then one per interval; then it ends,
/// waits until [`MAX_STREAM_MS`] after the start (hang), or one more interval later fails so the
/// connection is cut without the final chunk (drop). Stopping the server aborts it.
pub(super) fn stream_body(plan: StreamPlan) -> ReplyBody {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(MAX_STREAM_MS);
    let StreamPlan {
        frames,
        interval,
        end,
    } = plan;
    let state = (frames.into_iter(), false, Some(end));
    let events = futures_util::stream::unfold(state, move |(mut frames, sent, end)| async move {
        if let Some(frame) = frames.next() {
            if sent && !interval.is_zero() {
                tokio::time::sleep(interval).await;
            }
            return Some((Ok(Frame::data(Bytes::from(frame))), (frames, true, end)));
        }
        match end? {
            MockStreamEnd::Close => None,
            MockStreamEnd::Hang => {
                tokio::time::sleep_until(deadline).await;
                None
            }
            MockStreamEnd::Drop => {
                // A pause first, so the last event is flushed before the connection is cut.
                tokio::time::sleep(interval.max(Duration::from_millis(20))).await;
                Some((
                    Err(std::io::Error::other("mock stream dropped")),
                    (frames, true, None),
                ))
            }
        }
    });
    StreamBody::new(events).boxed_unsync()
}

/// `METHOD path` of a route as the log shows it; a CRUD route shows as `CRUD path`.
fn route_label(route: &MockRoute) -> String {
    let method = if route.route_type == MockRouteType::Crud {
        "CRUD".to_owned()
    } else {
        route.method.trim().to_ascii_uppercase()
    };
    format!("{method} {}", route.path)
}

impl MockReply {
    fn with_body(
        mut self,
        headers: Vec<(String, String)>,
        body: impl Into<Bytes>,
        latency: Duration,
    ) -> Self {
        self.headers = headers;
        self.body = body.into();
        self.latency = latency;
        self
    }

    fn with_response(mut self, response_id: String) -> Self {
        self.response_id = Some(response_id);
        self
    }
}

/// Global headers (minus those `own` overrides by name), then `own` headers, then the CORS headers.
/// Duplicates inside one list (two `Set-Cookie`) are all kept.
fn decorated_headers(
    config: &MockServer,
    request: &ParsedRequest,
    context: &TemplateContext<'_>,
    own: impl IntoIterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    let own: Vec<(String, String)> = own.into_iter().collect();
    let mut headers: Vec<(String, String)> = config
        .headers
        .iter()
        .filter(|header| header.enabled)
        .filter(|header| {
            !own.iter()
                .any(|(key, _)| key.eq_ignore_ascii_case(&header.key))
        })
        .map(|header| (header.key.clone(), render(&header.value, context)))
        .collect();
    headers.extend(own);
    if config.cors_enabled
        && let Some(origin) = cors_origin(&request.headers)
    {
        headers.push(("Access-Control-Allow-Origin".into(), origin.to_owned()));
        headers.push(("Access-Control-Expose-Headers".into(), "*".into()));
    }
    headers
}

/// The configured latency, clamped to [`MAX_LATENCY_MS`].
fn latency_for(latency_ms: u64) -> Duration {
    Duration::from_millis(latency_ms.min(MAX_LATENCY_MS))
}

/// Gives a CRUD reply the same global headers, CORS headers and latency as a route reply.
fn decorate_crud_reply(
    config: &MockServer,
    request: &ParsedRequest,
    context: &TemplateContext<'_>,
    mut reply: MockReply,
) -> MockReply {
    let own_headers = std::mem::take(&mut reply.headers);
    reply.headers = decorated_headers(config, request, context, own_headers);
    reply.latency = latency_for(config.latency_ms);
    reply
}

/// Empty `204` answer to a CORS preflight no route claimed.
fn preflight_reply(request: &ParsedRequest) -> MockReply {
    MockReply::json(204, &Value::Null, None).with_body(
        preflight_headers(&request.headers),
        String::new(),
        Duration::ZERO,
    )
}

/// Whether a request other than `GET`/`HEAD`/`OPTIONS` comes from another website: its
/// `Origin` is not loopback (`null` included), or it has no `Origin` and says
/// `Sec-Fetch-Site: cross-site`. A page can send such a request without a preflight (a form,
/// or `fetch` with a `text/plain` body), so withholding CORS alone would not stop the write.
/// A loopback page (a local front end under development) keeps working.
fn is_cross_site_write(method: &str, headers: &[(String, String)]) -> bool {
    if ["GET", "HEAD", "OPTIONS"]
        .iter()
        .any(|safe| method.eq_ignore_ascii_case(safe))
    {
        return false;
    }
    match header_value(headers, "origin") {
        Some(origin) => !origin_is_loopback(origin),
        None => header_value(headers, "sec-fetch-site")
            .is_some_and(|site| site.trim().eq_ignore_ascii_case("cross-site")),
    }
}

fn origin_is_loopback(origin: &str) -> bool {
    url::Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| {
            crate::net_guard::classify_host_literal(&host)
                == Some(crate::net_guard::HostClass::Loopback)
        })
}

/// The `Access-Control-Allow-Origin` value for a request, or `None` to withhold CORS headers.
/// A page can only read a mock reply when its origin is loopback (a local front end under
/// development); writes from other origins are refused by [`is_cross_site_write`]. A request
/// without `Origin` is not cross-origin and gets `*`.
fn cors_origin(request_headers: &[(String, String)]) -> Option<&str> {
    let Some(origin) = header_value(request_headers, "origin") else {
        return Some("*");
    };
    origin_is_loopback(origin).then_some(origin)
}

fn preflight_headers(request_headers: &[(String, String)]) -> Vec<(String, String)> {
    let Some(origin) = cors_origin(request_headers) else {
        return Vec::new();
    };
    let allow_headers =
        header_value(request_headers, "access-control-request-headers").unwrap_or("*");

    vec![
        ("Access-Control-Allow-Origin".into(), origin.to_owned()),
        ("Access-Control-Allow-Methods".into(), "*".into()),
        (
            "Access-Control-Allow-Headers".into(),
            allow_headers.to_owned(),
        ),
        ("Access-Control-Max-Age".into(), "600".into()),
        ("Vary".into(), "Origin".into()),
    ]
}
