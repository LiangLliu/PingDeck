//! Mock route helpers: add a route, build one from a log entry or a recorded exchange, test URL,
//! CRUD listing.

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::mock::{
    MockRecording, encode_base64_body, method_matches, normalized_prefix, route_answers,
    strip_prefix,
};
use crate::workspace::items::{
    MockBodySource, MockResponse, MockResponseMode, MockRoute, MockRouteType, MockRulesOperator,
    MockServer, MockTransactionLog,
};
use crate::workspace::models::KeyValue;

use super::defaults::{mock_bucket_new, mock_route_new, to_position};
use super::new_entity_id;

/// HTTP methods a route created from a log keeps; any other logged method becomes `ANY`.
const LOGGED_METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// Method and URL to open a request tab that exercises a route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockRouteTestRequest {
    /// Upper-case HTTP method (`GET` for `ANY` and CRUD routes).
    pub method: String,
    /// `http://127.0.0.1:<port><endpoint prefix><route path>`.
    pub url: String,
}

/// One endpoint a CRUD route answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockCrudEndpoint {
    /// HTTP method.
    pub method: String,
    /// Full path including the server's endpoint prefix.
    pub path: String,
    /// What the endpoint does.
    pub description: String,
}

/// Appends a new route of `kind` (see `mock_route_new`) to `server`. A CRUD route is linked to
/// the server's first data bucket, creating an empty `items` bucket when there is none. The
/// new route is the last entry of `server.routes`.
pub fn mock_server_add_route(mut server: MockServer, kind: &str) -> Result<MockServer, AppError> {
    let mut route = mock_route_new(kind, server.routes.len())?;
    if route.route_type == MockRouteType::Crud {
        if server.data_buckets.is_empty() {
            server.data_buckets.push(mock_bucket_new("items"));
        }
        route.data_bucket_id = server.data_buckets[0].id.clone();
    }
    server.routes.push(route);
    Ok(server)
}

/// Strips the server's endpoint prefix from a logged path (routes match the path after it);
/// a path outside the prefix is kept as is.
fn path_without_prefix<'a>(server: &MockServer, path: &'a str) -> &'a str {
    strip_prefix(path, &normalized_prefix(&server.endpoint_prefix)).unwrap_or(path)
}

/// `HH:MM:SS UTC` of a Unix-millisecond timestamp.
fn clock_utc(timestamp_ms: i64) -> String {
    let seconds = timestamp_ms.div_euclid(1000).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02} UTC",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

/// Converts a recorded transaction into a replayable route appended after `server`'s routes:
/// its method (`ANY` when not one of the seven HTTP methods), its path without the endpoint
/// prefix (always rooted with a leading `/`, so a logged `OPTIONS *` still validates), and one
/// default response copying the logged status, headers and body. A log row keeps at most the
/// first 64 KiB of the body (`response_body_truncated`); [`mock_route_from_exchange`] builds the
/// route from a recorded exchange with the whole body.
#[must_use]
pub fn mock_route_from_log(server: &MockServer, log: &MockTransactionLog) -> MockRoute {
    logged_route(
        server,
        LoggedResponse {
            method: &log.method,
            path: &log.path,
            status: log.status_code,
            headers: log.response_headers.clone(),
            body: log.response_body.clone(),
            body_source: None,
        },
        format!("Created from the log at {}", clock_utc(log.timestamp_ms)),
        server.routes.len(),
    )
}

/// Response headers a route made from an exchange leaves out: they describe the original
/// transfer, not the content (the mock writes its own length and encoding), or carry a session
/// that must not be saved and exported with the route.
const TRANSFER_HEADERS: [&str; 7] = [
    "content-length",
    "content-encoding",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "date",
    "set-cookie",
];

/// A route like [`mock_route_from_log`], built from a recorded proxied exchange: the whole
/// response body (UTF-8 text without `{{` as a template body, which renders it unchanged;
/// anything else as a Base64 body, so template-like markup replays byte for byte), without
/// [`TRANSFER_HEADERS`], at `position`.
#[must_use]
pub fn mock_route_from_exchange(
    server: &MockServer,
    exchange: &MockRecording,
    position: usize,
) -> MockRoute {
    let headers = exchange
        .headers
        .iter()
        .filter(|(name, _)| {
            !TRANSFER_HEADERS
                .iter()
                .any(|dropped| name.eq_ignore_ascii_case(dropped))
        })
        .map(|(key, value)| KeyValue {
            key: key.clone(),
            value: value.clone(),
            enabled: true,
            description: None,
        })
        .collect();
    let (body, body_source) = match std::str::from_utf8(&exchange.body) {
        Ok(text) if !text.contains("{{") => (text.to_owned(), None),
        _ => (
            encode_base64_body(&exchange.body),
            Some(MockBodySource::Base64),
        ),
    };
    logged_route(
        server,
        LoggedResponse {
            method: &exchange.method,
            path: &exchange.path,
            status: exchange.status,
            headers,
            body,
            body_source,
        },
        format!(
            "Recorded from the proxy at {}",
            clock_utc(exchange.recorded_at_ms)
        ),
        position,
    )
}

/// Routes for the recorded exchanges no route of `server` answers yet (same method and path
/// after the endpoint prefix), appended after its routes in recording order, and how many
/// exchanges were skipped because a route already answers them.
#[must_use]
pub fn mock_routes_from_recordings(
    server: &MockServer,
    recordings: &[MockRecording],
) -> (Vec<MockRoute>, usize) {
    let mut covered = server.clone();
    let mut routes = Vec::new();
    let mut skipped = 0;
    for exchange in recordings {
        let path = rooted(path_without_prefix(server, &exchange.path));
        if route_answers(&covered, &exchange.method, &path) {
            skipped += 1;
            continue;
        }
        let route = mock_route_from_exchange(server, exchange, covered.routes.len());
        covered.routes.push(route.clone());
        routes.push(route);
    }
    (routes, skipped)
}

/// The request and response a route is made from.
struct LoggedResponse<'a> {
    method: &'a str,
    /// The request path, endpoint prefix included.
    path: &'a str,
    status: u16,
    headers: Vec<KeyValue>,
    body: String,
    body_source: Option<MockBodySource>,
}

/// A route with one default response copying `logged`, at `position`.
fn logged_route(
    server: &MockServer,
    logged: LoggedResponse<'_>,
    description: String,
    position: usize,
) -> MockRoute {
    let method = logged.method.to_ascii_uppercase();
    MockRoute {
        id: new_entity_id("route"),
        method: if LOGGED_METHODS.contains(&method.as_str()) {
            method
        } else {
            "ANY".to_owned()
        },
        path: rooted(path_without_prefix(server, logged.path)),
        enabled: true,
        description,
        route_type: MockRouteType::Http,
        data_bucket_id: String::new(),
        crud_id_key: "id".to_owned(),
        response_mode: MockResponseMode::FirstMatch,
        responses: vec![MockResponse {
            id: new_entity_id("resp"),
            label: format!("{} from log", logged.status),
            status_code: logged.status,
            headers: logged.headers,
            body: logged.body,
            body_source: logged.body_source,
            fault: None,
            latency_ms: 0,
            is_default: true,
            rules_operator: MockRulesOperator::And,
            rules: Vec::new(),
            callback_ids: Vec::new(),
            position: 0,
            stream: None,
        }],
        position: to_position(position),
    }
}

/// `http://127.0.0.1:<port><endpoint prefix>`: where `server` answers (a mock server always
/// binds the loopback interface on its configured port).
fn base_url(server: &MockServer) -> String {
    format!(
        "http://127.0.0.1:{}{}",
        server.port,
        normalized_prefix(&server.endpoint_prefix)
    )
}

/// `path` with exactly one leading slash.
fn rooted(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}

/// The request that exercises `route` on `server`: the route's method (`GET` for a CRUD route
/// or a wildcard method) against the server's loopback URL plus endpoint prefix.
#[must_use]
pub fn mock_route_test_request(server: &MockServer, route: &MockRoute) -> MockRouteTestRequest {
    // `method_matches` is true for `ANY`/`ALL`/`*` with any request method, so "" probes wildcards.
    let wildcard = method_matches(&route.method, "");
    MockRouteTestRequest {
        method: if route.route_type == MockRouteType::Crud || wildcard {
            "GET".to_owned()
        } else {
            route.method.trim().to_ascii_uppercase()
        },
        url: format!("{}{}", base_url(server), rooted(&route.path)),
    }
}

/// The endpoints a CRUD `route` generates over its data bucket, with the server's endpoint
/// prefix in the paths (a blank route path shows as `/resource`). Not a CRUD route is a
/// validation error.
pub fn mock_crud_endpoints(
    server: &MockServer,
    route: &MockRoute,
) -> Result<Vec<MockCrudEndpoint>, AppError> {
    if route.route_type != MockRouteType::Crud {
        return Err(AppError::validation(format!(
            "mock route {} is not a CRUD route",
            route.id
        )));
    }
    let path = if route.path.trim().is_empty() {
        "/resource"
    } else {
        route.path.as_str()
    };
    let base = format!(
        "{}{}",
        normalized_prefix(&server.endpoint_prefix),
        rooted(path)
    );
    let item = format!("{base}/:id");
    let endpoint = |method: &str, path: &str, description: &str| MockCrudEndpoint {
        method: method.to_owned(),
        path: path.to_owned(),
        description: description.to_owned(),
    };
    Ok(vec![
        endpoint(
            "GET",
            &base,
            "List everything; ?name=ann filters, ?_limit=10 limits",
        ),
        endpoint("GET", &item, "Read one"),
        endpoint(
            "POST",
            &base,
            "Create, with an id filled in when the body has none",
        ),
        endpoint("PUT", &item, "Replace"),
        endpoint("PATCH", &item, "Merge"),
        endpoint("DELETE", &item, "Remove"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::models::KeyValue;

    /// A logged request to `path` with a one-line reply.
    fn log(path: &str) -> MockTransactionLog {
        MockTransactionLog {
            id: "log_1".into(),
            server_id: "mock_flow".into(),
            timestamp_ms: 0,
            method: "OPTIONS".into(),
            path: path.into(),
            query: String::new(),
            request_headers: Vec::<KeyValue>::new(),
            request_body: String::new(),
            request_body_truncated: false,
            matched_route_id: None,
            matched_response_id: None,
            status_code: 204,
            response_headers: Vec::new(),
            response_body: String::new(),
            response_body_truncated: false,
            proxied: false,
            elapsed_ms: 0,
            ..MockTransactionLog::default()
        }
    }

    #[test]
    fn a_route_made_from_a_log_is_always_rooted() {
        let state = crate::app::AppState::open_in_memory().unwrap();
        super::super::workspace_create_starter(&state, "ws_mock", "Mock").unwrap();
        let mut server = super::super::mock_server_new(&state, "ws_mock").unwrap();
        server.routes.clear();
        server.port = 3000;
        // An asterisk-form request (`OPTIONS *`) is logged with the path `*`.
        let route = mock_route_from_log(&server, &log("*"));
        assert_eq!(route.path, "/*");
        assert_eq!(mock_route_from_log(&server, &log("")).path, "/");

        server.endpoint_prefix = "/api".into();
        assert_eq!(mock_route_from_log(&server, &log("/api")).path, "/");
        assert_eq!(mock_route_from_log(&server, &log("/api/a")).path, "/a");

        server.routes.push(route);
        let issues = crate::app::misc_logic_service::validate_mock_server(&server);
        assert!(
            issues.is_empty(),
            "the new route passes the save validation: {issues:?}"
        );
    }

    fn exchange(method: &str, path: &str, body: &[u8]) -> MockRecording {
        MockRecording {
            log_id: format!("log_{path}"),
            method: method.into(),
            path: path.into(),
            query: String::new(),
            status: 200,
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("content-length".into(), "9".into()),
                ("set-cookie".into(), "sid=1".into()),
            ],
            body: bytes::Bytes::copy_from_slice(body),
            recorded_at_ms: 0,
        }
    }

    #[test]
    fn recorded_exchanges_become_routes_once_with_whole_bodies() {
        let state = crate::app::AppState::open_in_memory().unwrap();
        super::super::workspace_create_starter(&state, "ws_mock", "Mock").unwrap();
        let mut server = super::super::mock_server_new(&state, "ws_mock").unwrap();
        server.endpoint_prefix = "/api".into();
        server.routes = vec![super::super::mock_route_new("GET", 0).unwrap()];
        server.routes[0].method = "GET".into();
        server.routes[0].path = "/users/:id".into();

        let big = vec![b'x'; 100 * 1024];
        let recordings = [
            exchange("GET", "/api/users/7", b"{}"),
            exchange("GET", "/api/orders", &big),
            exchange("GET", "/api/orders", b"again"),
            exchange("GET", "/api/logo", &[0xff, 0xd8, 0x00]),
        ];
        let (routes, skipped) = mock_routes_from_recordings(&server, &recordings);
        // `/users/7` is answered by `/users/:id`; the second `/orders` by the route just made.
        assert_eq!(skipped, 2);
        let paths: Vec<&str> = routes.iter().map(|route| route.path.as_str()).collect();
        assert_eq!(paths, ["/orders", "/logo"]);
        let orders = &routes[0].responses[0];
        assert_eq!((orders.body.len(), orders.body_source), (big.len(), None));
        let keys: Vec<&str> = orders.headers.iter().map(|h| h.key.as_str()).collect();
        assert_eq!(keys, ["content-type"]);
        let logo = &routes[1].responses[0];
        assert_eq!(
            (logo.body.as_str(), logo.body_source),
            ("/9gA", Some(MockBodySource::Base64))
        );
        assert_eq!((routes[0].position, routes[1].position), (1, 2));
    }
}
