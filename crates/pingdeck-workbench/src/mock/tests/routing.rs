//! Route matching, rules, sequencing and CORS behaviour of the live mock server.

use std::net::TcpStream;
use std::time::Duration;

use crate::workspace::items::{MockResponse, MockResponseMode, MockRuleOperator, MockRuleTarget};
use crate::workspace::models::KeyValue;

use super::super::server::MockServerManager;
use super::support::*;

#[test]
fn serves_param_routes_hot_reloads_and_matches_wildcards() {
    use super::super::routing::match_path_pattern;

    assert!(match_path_pattern("/files/*", "/files").is_some());
    assert!(match_path_pattern("/files/*", "/files/a/b").is_some());
    assert!(match_path_pattern("/a/*/b", "/a/x/y").is_none());
    assert!(match_path_pattern("/a/*/b", "/a/*/b").is_some());
    let pattern = format!("/{}end", ":p?/".repeat(40));
    let path = format!("/{}", "x/".repeat(60));
    let started = std::time::Instant::now();
    assert!(
        match_path_pattern(&pattern, &path).is_none()
            && match_path_pattern(&pattern, "/end").is_some()
    );
    assert!(started.elapsed() < Duration::from_secs(1));

    let mut config = server(vec![route(
        "route_user",
        "GET",
        "/users/:id",
        vec![response(
            "resp_user",
            202,
            "id={{urlParam 'id'}};ping={{params.id}};method={{method}}",
            vec![],
        )],
    )]);
    config.endpoint_prefix = "/api".into();

    let manager = MockServerManager::new();
    let bound = manager.start(config.clone()).unwrap();
    assert_eq!(
        get(bound, "/api/users/42"),
        (
            202,
            get(bound, "/api/users/42").1,
            "id=42;ping=42;method=GET".into()
        )
    );
    assert_eq!(get(bound, "/users/42").0, 404);

    let mut disabled = route(
        "route_disabled",
        "GET",
        "/disabled",
        vec![response("resp_disabled", 200, "disabled", vec![])],
    );
    disabled.enabled = false;
    let mut reloaded = config;
    reloaded.routes = vec![
        disabled,
        route(
            "route_docs",
            "GET",
            "/docs/:lang?",
            vec![response("resp_docs", 200, "lang={{params.lang}}", vec![])],
        ),
        route(
            "route_files",
            "GET",
            "/files/*",
            vec![response("resp_files", 200, "file", vec![])],
        ),
        route(
            "route_any",
            "ALL",
            "/any",
            vec![response("resp_any", 200, "any", vec![])],
        ),
        route(
            "route_star",
            "*",
            "/star",
            vec![response("resp_star", 200, "star", vec![])],
        ),
    ];
    assert!(manager.update(reloaded));
    assert_eq!(get(bound, "/api/users/42").0, 404);
    assert_eq!(get(bound, "/api/disabled").0, 404);
    assert_eq!(get(bound, "/api/docs").2, "lang=");
    assert_eq!(get(bound, "/api/docs/en").2, "lang=en");
    assert_eq!(get(bound, "/api/files/a/b/c.txt").2, "file");
    assert_eq!(get(bound, "/api/files").2, "file");
    assert_eq!(
        send(
            bound,
            "POST /api/docs HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\n\r\n"
        )
        .0,
        404
    );
    assert_eq!(send_with_body(bound, "DELETE", "/api/any", "").2, "any");
    assert_eq!(send_with_body(bound, "PUT", "/api/star", "x").2, "star");

    assert!(manager.stop("mock_flow") && !manager.is_running("mock_flow"));
    assert!(TcpStream::connect(bound).is_err());
}

#[test]
fn matches_rules_modes_cors_and_header_overrides() {
    let mut search = route(
        "route_search",
        "GET",
        "/search",
        vec![
            response(
                "resp_page_two",
                200,
                "page-two",
                vec![rule(
                    MockRuleTarget::Query,
                    "page",
                    MockRuleOperator::Equals,
                    "2",
                )],
            ),
            response("resp_search_default", 200, "default", vec![]),
        ],
    );
    search.responses[1].position = 1;
    search.responses[1].is_default = true;

    let mut body_rules = route(
        "route_post",
        "POST",
        "/echo",
        vec![
            response(
                "resp_echo",
                201,
                "created",
                vec![
                    rule(
                        MockRuleTarget::Header,
                        "x-api-key",
                        MockRuleOperator::Equals,
                        "secret",
                    ),
                    rule(
                        MockRuleTarget::Body,
                        "user.name",
                        MockRuleOperator::Equals,
                        "Ada",
                    ),
                ],
            ),
            response("resp_echo_default", 400, "rejected", vec![]),
        ],
    );
    body_rules.responses[1].is_default = true;

    let mut inverted_rule = rule(
        MockRuleTarget::Header,
        "x-mode",
        MockRuleOperator::Equals,
        "disabled",
    );
    inverted_rule.invert = true;
    let invert_route = route(
        "route_invert",
        "GET",
        "/invert",
        vec![
            response("resp_invert", 200, "inverted", vec![inverted_rule]),
            response("resp_invert_default", 500, "should-not-match", vec![]),
        ],
    );

    let mut seq = route(
        "route_seq",
        "GET",
        "/seq",
        vec![
            response("resp_1", 200, "one", vec![]),
            response("resp_2", 200, "two", vec![]),
        ],
    );
    seq.response_mode = MockResponseMode::Sequential;
    seq.responses[1].position = 1;

    let mut count_route = route(
        "route_count",
        "GET",
        "/count",
        vec![
            response(
                "resp_first",
                200,
                "first",
                vec![rule(
                    MockRuleTarget::RequestNumber,
                    "",
                    MockRuleOperator::Equals,
                    "1",
                )],
            ),
            response("resp_later", 200, "later", vec![]),
        ],
    );
    count_route.responses[1].is_default = true;

    let mut regex_route = route(
        "route_regex",
        "GET",
        "/regex",
        vec![
            response(
                "resp_exact",
                200,
                "exact",
                vec![rule(
                    MockRuleTarget::Header,
                    "x-token",
                    MockRuleOperator::Regex,
                    "^tok-\\d+$",
                )],
            ),
            response(
                "resp_ci",
                201,
                "ci",
                vec![rule(
                    MockRuleTarget::Header,
                    "x-token",
                    MockRuleOperator::RegexIgnoreCase,
                    "^TOK-\\d+$",
                )],
            ),
            response("resp_default", 400, "no-match", vec![]),
        ],
    );
    regex_route.responses[2].is_default = true;

    let mut fb_gated = route(
        "route_gated",
        "GET",
        "/fb",
        vec![response(
            "resp_gated",
            200,
            "gated",
            vec![rule(
                MockRuleTarget::Query,
                "x",
                MockRuleOperator::Equals,
                "1",
            )],
        )],
    );
    fb_gated.response_mode = MockResponseMode::Fallback;
    let mut fb_plain = route(
        "route_plain",
        "GET",
        "/fb",
        vec![response("resp_plain", 200, "plain", vec![])],
    );
    fb_plain.position = 1;
    let mut fb_404 = route(
        "route_fb404",
        "GET",
        "/fb404",
        vec![response(
            "resp_gated",
            200,
            "gated",
            vec![rule(
                MockRuleTarget::Query,
                "x",
                MockRuleOperator::Equals,
                "1",
            )],
        )],
    );
    fb_404.response_mode = MockResponseMode::Fallback;

    let mut random = route(
        "route_random",
        "GET",
        "/random",
        ["a", "b", "c"]
            .iter()
            .map(|b| response(b, 200, b, vec![]))
            .collect(),
    );
    random.response_mode = MockResponseMode::Random;

    let mut cookies = response("resp_cookies", 200, "ok", vec![]);
    cookies.headers = ["a=1", "b=2"]
        .iter()
        .map(|v| KeyValue {
            key: "Set-Cookie".into(),
            value: (*v).into(),
            enabled: true,
            description: None,
        })
        .collect();
    let cookie_route = route("route_cookies", "GET", "/cookies", vec![cookies]);

    let mut config = server(vec![
        search,
        body_rules,
        invert_route,
        seq,
        count_route,
        regex_route,
        fb_gated,
        fb_plain,
        fb_404,
        random,
        cookie_route,
    ]);
    config.cors_enabled = true;
    config.headers = vec![
        KeyValue {
            key: "set-cookie".into(),
            value: "global=0".into(),
            enabled: true,
            description: None,
        },
        KeyValue {
            key: "x-global".into(),
            value: "g".into(),
            enabled: true,
            description: None,
        },
    ];

    let manager = MockServerManager::new();
    let bound = manager.start(config).unwrap();

    assert_eq!(get(bound, "/search?page=2").2, "page-two");
    assert_eq!(get(bound, "/search?page=3").2, "default");
    assert_eq!(
        send(bound, "POST /echo HTTP/1.1\r\nHost: 127.0.0.1\r\nx-api-key: secret\r\nContent-Length: 31\r\n\r\n{\"user\":{\"name\":\"Ada\",\"age\":3}}").2,
        "created"
    );
    assert_eq!(
        send(bound, "POST /echo HTTP/1.1\r\nHost: 127.0.0.1\r\nx-api-key: wrong\r\nContent-Length: 31\r\n\r\n{\"user\":{\"name\":\"Ada\",\"age\":3}}").2,
        "rejected"
    );
    assert_eq!(get(bound, "/invert").2, "inverted");

    assert_eq!(
        (
            get(bound, "/seq").2.as_str(),
            get(bound, "/seq").2.as_str(),
            get(bound, "/seq").2.as_str()
        ),
        ("one", "two", "one")
    );
    assert!(manager.clear_logs("mock_flow") && manager.logs("mock_flow").is_empty());

    assert_eq!(
        (
            get(bound, "/count").2.as_str(),
            get(bound, "/count").2.as_str()
        ),
        ("first", "later")
    );
    let rx = |tok: &str| {
        send(
            bound,
            &format!("GET /regex HTTP/1.1\r\nHost: 127.0.0.1\r\nx-token: {tok}\r\n\r\n"),
        )
        .2
    };
    assert_eq!(
        (
            rx("tok-42").as_str(),
            rx("TOK-42").as_str(),
            rx("nope").as_str()
        ),
        ("exact", "ci", "no-match")
    );

    assert_eq!(
        (
            get(bound, "/fb?x=1").2.as_str(),
            get(bound, "/fb?x=2").2.as_str(),
            get(bound, "/fb404?x=2").0
        ),
        ("gated", "plain", 404)
    );

    let mut seen: Vec<String> = (0..60).map(|_| get(bound, "/random").2).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen, ["a", "b", "c"]);

    let (_, headers, _) = get(bound, "/cookies");
    let set_cookies: Vec<&str> = headers
        .iter()
        .filter(|(k, _)| k == "set-cookie")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(
        (
            set_cookies.as_slice(),
            header(&headers, "x-global"),
            header(&headers, "access-control-allow-origin")
        ),
        (&["a=1", "b=2"][..], "g", "*")
    );

    let (status, headers, body) = send(
        bound,
        "OPTIONS /search HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://localhost:3000\r\nAccess-Control-Request-Method: GET\r\nAccess-Control-Request-Headers: x-token\r\n\r\n",
    );
    assert_eq!(
        (
            status,
            body.is_empty(),
            header(&headers, "access-control-allow-origin"),
            header(&headers, "access-control-allow-headers")
        ),
        (204, true, "http://localhost:3000", "x-token")
    );
}

/// Mockoon's `chooseResponse`: a response without rules never matches, the default's
/// rules take part in order, DisableRules answers with the default, Fallback yields on no match.
#[test]
fn selects_responses_with_mockoon_semantics() {
    let query = |key: &str, value: &str| {
        vec![rule(
            MockRuleTarget::Query,
            key,
            MockRuleOperator::Equals,
            value,
        )]
    };
    let ordered = |id: &str, path: &str, mut responses: Vec<MockResponse>| {
        for (position, r) in (0_i64..).zip(responses.iter_mut()) {
            r.position = position;
        }
        route(id, "GET", path, responses)
    };
    // A (x=1), C (no rules, before the default), B (default).
    let mut shadow = ordered(
        "route_shadow",
        "/shadow",
        vec![
            response("a", 200, "A", query("x", "1")),
            response("c", 200, "C", vec![]),
            response("b", 200, "B", vec![]),
        ],
    );
    shadow.responses[2].is_default = true;
    // The default comes first and carries a rule: it wins when that rule matches.
    let mut ruled_default = ordered(
        "route_ruled",
        "/ruled",
        vec![
            response("d", 200, "D", query("y", "1")),
            response("e", 200, "E", query("y", "1")),
        ],
    );
    ruled_default.responses[0].is_default = true;
    let mut off = ordered(
        "route_off",
        "/off",
        vec![
            response("first", 200, "first", query("x", "1")),
            response("dflt", 200, "default", vec![]),
        ],
    );
    off.responses[1].is_default = true;
    off.response_mode = MockResponseMode::DisableRules;
    let mut fall = ordered(
        "route_fall",
        "/fall",
        vec![
            response("gated", 200, "gated", query("x", "1")),
            response("plain", 200, "plain", vec![]),
        ],
    );
    fall.responses[1].is_default = true;
    fall.response_mode = MockResponseMode::Fallback;
    let mut after_fall = route(
        "route_after",
        "GET",
        "/fall",
        vec![response("after", 200, "next route", vec![])],
    );
    after_fall.position = 9;
    let no_default = ordered(
        "route_nodefault",
        "/nodefault",
        vec![
            response("n1", 200, "N1", query("x", "1")),
            response("n2", 200, "N2", vec![]),
        ],
    );

    let manager = MockServerManager::new();
    let bound = manager
        .start(server(vec![
            shadow,
            ruled_default,
            off,
            fall,
            after_fall,
            no_default,
        ]))
        .unwrap();
    let body = |path: &str| get(bound, path).2;
    assert_eq!(
        [
            body("/shadow?x=1"),
            body("/shadow?x=2"),
            body("/ruled?y=1"),
            body("/off?x=1"),
            body("/fall?x=1"),
            body("/fall?x=2"),
            body("/nodefault?x=2"),
        ],
        ["A", "B", "D", "default", "gated", "next route", "N1"]
    );
}

#[test]
fn brace_params_and_encoded_literal_segments_match() {
    let mut config = server(vec![
        route(
            "route_user",
            "GET",
            "/用户/{id}",
            vec![response("resp_user", 200, "user={{params.id}}", vec![])],
        ),
        route(
            "route_users",
            "GET",
            "/用户",
            vec![response("resp_users", 200, "users", vec![])],
        ),
        route(
            "route_space",
            "GET",
            "/a b",
            vec![response("resp_space", 200, "space", vec![])],
        ),
    ]);
    config.endpoint_prefix = "/api".into();
    let manager = MockServerManager::new();
    let bound = manager.start(config).unwrap();

    assert_eq!(get(bound, "/api/%E7%94%A8%E6%88%B7/1").2, "user=1");
    assert_eq!(get(bound, "/api/%E7%94%A8%E6%88%B7").2, "users");
    assert_eq!(get(bound, "/api/a%20b").2, "space");
    assert_eq!(get(bound, "/api/%E7%94%A8%E6%88%B7/1/2").0, 404);
}
