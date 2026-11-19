use serde_json::Value;

use super::*;
use crate::executor::{ExecutorOptions, execute_request_with_scopes};
use crate::mock::{MockServerManager, response, route, server};
use crate::workspace::assertions::evaluate_assertions;
use crate::workspace::items::{FailureKind, FlowExtraction, FlowExtractionSource};
use crate::workspace::{Assertion, MockRule, MockRuleOperator, MockRuleTarget};

/// Test step executor: sends the step straight through the executor, without scripts, and
/// evaluates the request's assertions like the Send pipeline does.
async fn direct_step(
    request: Request,
    variables: Vec<EnvironmentVariable>,
) -> Result<ExecutionResult, AppError> {
    let mut execution = execute_request_with_scopes(
        &request,
        VariableScopes::new().with_local_vars(variables),
        ExecutorOptions::default(),
    )
    .await
    .map_err(AppError::from)?;
    execution.assertion_results = evaluate_assertions(&request.assertions, &execution);
    Ok(execution)
}

/// Test condition scopes: only the flow variables, like the direct step executor sees.
async fn direct_scopes(
    _request: Option<Request>,
    variables: Vec<EnvironmentVariable>,
) -> VariableScopes {
    VariableScopes::new().with_local_vars(variables)
}

fn request(id: &str, url: String, headers: Vec<KeyValue>) -> Request {
    let mut request = Request::new(id, "ws_flow", id, url).unwrap();
    request.headers = headers;
    request
}

fn header(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: value.into(),
        enabled: true,
        description: None,
    }
}

fn step(id: &str, request_id: &str, position: i64) -> FlowStep {
    FlowStep {
        id: id.into(),
        name: id.into(),
        request_id: request_id.into(),
        delay_ms: 0,
        extractions: Vec::new(),
        assertions: Vec::new(),
        ignore_request_assertions: false,
        continue_on_error: false,
        position,
        method_override: None,
        url_override: None,
        headers_override: Vec::new(),
        query_override: Vec::new(),
        body_override: None,
        body_override_kind: None,
        condition_expression: String::new(),
        retries: 0,
        retry_wait_ms: 0,
        retry_on: Vec::new(),
        timeout_ms: None,
    }
}

fn flow(id: &str, steps: Vec<FlowStep>) -> Flow {
    Flow {
        id: id.into(),
        workspace_id: crate::workspace::WorkspaceId::new("ws_flow").unwrap(),
        name: id.into(),
        description: String::new(),
        steps,
        updated_at_ms: 0,
        options: Default::default(),
    }
}

#[tokio::test]
async fn flow_extracts_variables_into_the_next_step() {
    let mut profile_route = route(
        "route_profile",
        "GET",
        "/profile",
        vec![
            response(
                "resp_ok",
                200,
                r#"{"name":"Ada"}"#,
                vec![MockRule {
                    id: "rule_token".into(),
                    target: MockRuleTarget::Header,
                    modifier: "x-token".into(),
                    operator: MockRuleOperator::Equals,
                    value: "tok-123".into(),
                    invert: false,
                    position: 0,
                }],
            ),
            response("resp_denied", 401, r#"{"error":"denied"}"#, vec![]),
        ],
    );
    profile_route.responses[1].is_default = true;
    let mut config = server(vec![
        route(
            "route_login",
            "GET",
            "/login",
            vec![response(
                "resp_login",
                200,
                r#"{"token":"tok-123"}"#,
                vec![],
            )],
        ),
        profile_route,
    ]);
    config.cors_enabled = false;

    let manager = MockServerManager::new();
    let bound = manager.start(config).expect("mock server should start");
    let base = format!("http://{bound}");

    let requests = HashMap::from([
        (
            "req_login".to_owned(),
            request("req_login", format!("{base}/login"), vec![]),
        ),
        (
            "req_profile".to_owned(),
            request(
                "req_profile",
                format!("{base}/profile"),
                vec![header("x-token", "{{token}}")],
            ),
        ),
    ]);

    let mut login = step("step_1", "req_login", 0);
    login.extractions = vec![FlowExtraction {
        variable_name: "token".into(),
        source: FlowExtractionSource::BodyJsonPath,
        expression: "token".into(),
        secret: None,
    }];
    login.assertions = vec![Assertion::StatusEquals { value: 200 }];

    let mut profile = step("step_2", "req_profile", 1);
    profile.extractions = vec![FlowExtraction {
        variable_name: "user_name".into(),
        source: FlowExtractionSource::BodyJsonPath,
        expression: "name".into(),
        secret: None,
    }];
    profile.assertions = vec![
        Assertion::StatusEquals { value: 200 },
        Assertion::JsonPathEquals {
            path: "name".into(),
            value: Value::String("Ada".into()),
        },
    ];

    let flow = flow("flow_1", vec![login, profile]);

    let result = run_flow(
        &flow,
        &requests,
        &FlowRunInput::default(),
        &FlowRun::new("run_test", 0),
        direct_scopes,
        direct_step,
    )
    .await;

    manager.stop("mock_flow");

    assert!(result.passed, "flow should pass: {result:?}");
    assert!(!result.cancelled);
    assert_eq!(result.steps.len(), 2);
    assert_eq!(result.steps[0].status_code, Some(200));
    assert_eq!(result.steps[1].status_code, Some(200));
    assert_eq!(
        result.steps[0].extracted_variables,
        vec![KeyValue {
            key: "token".into(),
            value: "tok-123".into(),
            enabled: true,
            description: None,
        }]
    );
    assert!(result.steps[1].assertion_results.iter().all(|a| a.passed));
    assert!(
        result
            .variables
            .iter()
            .any(|variable| variable.key == "user_name" && variable.value == "Ada")
    );

    // Full call-chain detail is kept for debugging.
    let step = &result.steps[0];
    let executed = step.executed_request.as_ref().expect("request summary");
    assert_eq!(executed.method, "GET");
    assert!(executed.url.ends_with("/login"));
    assert!(step.timing.is_some());
    assert!(!step.response_body_preview.is_empty());
    assert!(step.call_chain.is_some());
}

#[tokio::test]
async fn inline_steps_conditions_extractions_and_continue_on_error_work_end_to_end() {
    let mut login_route = route(
        "route_login",
        "POST",
        "/echo",
        vec![response("resp_echo", 201, "token=tok-9", vec![])],
    );
    login_route.responses[0].headers = vec![
        header("Content-Type", "application/json"),
        header("Set-Cookie", "sid=cookie-1; Path=/"),
    ];
    let mut config = server(vec![
        login_route,
        route(
            "route_ping",
            "GET",
            "/ping",
            vec![response("resp_ping", 200, "pong", vec![])],
        ),
    ]);
    config.cors_enabled = false;

    let manager = MockServerManager::new();
    let bound = manager.start(config).expect("mock server should start");
    let requests = HashMap::from([(
        "req_ping".to_owned(),
        request("req_ping", format!("http://{bound}/ping"), vec![]),
    )]);

    let mut inline = step("step_inline", "", 0);
    inline.method_override = Some(crate::workspace::HttpMethod::Post);
    inline.url_override = Some(format!("http://{bound}/echo"));
    inline.body_override = Some(r#"{"inline":true}"#.into());
    inline.query_override = vec![header("page", "2")];
    inline.headers_override = vec![header("x-inline", "1")];
    inline.extractions = vec![
        FlowExtraction {
            variable_name: "match".into(),
            source: FlowExtractionSource::BodyRegex,
            expression: r"tok-\d+".into(),
            secret: None,
        },
        FlowExtraction {
            variable_name: "sid".into(),
            source: FlowExtractionSource::Cookie,
            expression: "sid".into(),
            secret: None,
        },
        FlowExtraction {
            variable_name: "body".into(),
            source: FlowExtractionSource::ResponseBody,
            expression: String::new(),
            secret: None,
        },
    ];

    let mut skipped = step("step_skip", "req_missing", 1);
    skipped.condition_expression = "{{run_skip}} == true".into();

    let mut missing = step("step_missing", "req_missing", 2);
    missing.continue_on_error = true;

    let mut ping = step("step_ping", "req_ping", 3);
    ping.condition_expression = "{{run_ping}} == true".into();
    ping.assertions = vec![Assertion::StatusEquals { value: 200 }];

    let full_flow = flow("flow_all", vec![inline, skipped, missing, ping]);

    let capped = FlowRunInput {
        initial_variables: std::collections::BTreeMap::new(),
        max_steps: Some(1),
        ..FlowRunInput::default()
    };
    let capped_res = run_flow(
        &full_flow,
        &requests,
        &capped,
        &FlowRun::new("run_test", 0),
        direct_scopes,
        direct_step,
    )
    .await;
    assert_eq!(capped_res.steps.len(), 1, "max_steps stops the run");
    let executed = capped_res.steps[0]
        .executed_request
        .as_ref()
        .expect("request summary");
    assert_eq!(
        (executed.method.as_str(), executed.body_preview.as_str()),
        ("POST", r#"{"inline":true}"#)
    );
    assert!(executed.url.contains("page=2"));
    assert!(
        executed
            .headers
            .iter()
            .any(|h| h.key.eq_ignore_ascii_case("x-inline"))
    );
    let extracted = &capped_res.steps[0].extracted_variables;
    assert!(
        extracted
            .iter()
            .any(|v| v.key == "match" && v.value == "tok-9")
    );
    assert!(
        extracted
            .iter()
            .any(|v| v.key == "sid" && v.value == "cookie-1")
    );
    assert!(
        extracted
            .iter()
            .any(|v| v.key == "body" && v.value.contains("tok-9"))
    );

    let input = FlowRunInput {
        initial_variables: std::collections::BTreeMap::from([
            ("run_skip".to_owned(), "false".to_owned()),
            ("run_ping".to_owned(), "true".to_owned()),
        ]),
        max_steps: None,
        ..FlowRunInput::default()
    };
    let result = run_flow(
        &full_flow,
        &requests,
        &input,
        &FlowRun::new("run_test", 0),
        direct_scopes,
        direct_step,
    )
    .await;
    manager.stop("mock_flow");

    assert!(!result.passed);
    assert_eq!(result.steps.len(), 4);
    assert!(result.steps[1].skipped && result.steps[1].passed && result.steps[1].error.is_none());
    assert!(!result.steps[2].passed && result.steps[2].error.is_some());
    assert!(result.steps[3].passed && !result.steps[3].skipped);
}

#[test]
fn step_overrides_merge_rows_and_keep_the_saved_body_type() {
    let mut saved = request(
        "req_saved",
        "http://localhost/".into(),
        vec![header("X-Id", "1")],
    );
    saved.query_params = vec![header("page", "1")];
    let requests = HashMap::from([("req_saved".to_owned(), saved)]);

    let mut overridden = step("step_a", "req_saved", 0);
    overridden.name = "  ".into();
    overridden.headers_override = vec![
        header("x-id", "2"),
        KeyValue {
            enabled: false,
            description: None,
            ..header("x-off", "no")
        },
        header("  ", "blank"),
    ];
    overridden.query_override = vec![
        header("Page", "9"),
        KeyValue {
            enabled: false,
            description: None,
            ..header("off", "no")
        },
        header("", "blank"),
    ];
    let flow_def = flow("flow_rows", vec![overridden.clone()]);
    let resolved = resolve_step_request(&flow_def, &overridden, &requests).unwrap();
    assert_eq!(resolved.headers, vec![header("x-id", "2")]);
    assert_eq!(
        resolved.query_params,
        vec![header("page", "1"), header("Page", "9")]
    );
    assert_eq!(step_name(&overridden), "step_a");
    assert_eq!(blank_result(&overridden, 0).step_name, "step_a");

    let body_of = |saved: RequestBody, kind: Option<FlowBodyKind>, text: &str| {
        let mut request = request("req_body", "http://localhost/".into(), vec![]);
        request.body = saved;
        let requests = HashMap::from([("req_body".to_owned(), request)]);
        let step = FlowStep {
            body_override: Some(text.into()),
            body_override_kind: kind,
            ..step("step_body", "req_body", 0)
        };
        resolve_step_request(&flow("flow_body", vec![step.clone()]), &step, &requests)
            .unwrap()
            .body
    };
    let form = RequestBody::FormUrlEncoded { fields: Vec::new() };
    assert_eq!(
        body_of(form.clone(), None, "a=1&x=y\n\n# off=1\nb = {{b}}\nc"),
        RequestBody::FormUrlEncoded {
            fields: vec![
                header("a", "1&x=y"),
                KeyValue {
                    enabled: false,
                    ..header("off", "1")
                },
                header("b", "{{b}}"),
                header("c", "")
            ]
        }
    );
    let graphql = RequestBody::Graphql {
        query: "{ a }".into(),
        variables_json: Some("{}".into()),
        operation_name: Some("Op".into()),
    };
    assert_eq!(
        body_of(graphql, None, "{ b }"),
        RequestBody::Graphql {
            query: "{ b }".into(),
            variables_json: Some("{}".into()),
            operation_name: Some("Op".into()),
        }
    );
    let xml = RequestBody::RawText {
        content: "<a/>".into(),
        language: Some(RawLanguage::Xml),
    };
    assert_eq!(
        body_of(xml, None, "<b/>"),
        RequestBody::RawText {
            content: "<b/>".into(),
            language: Some(RawLanguage::Xml),
        }
    );
    // The step's own kind wins; an inline step without one sends plain text.
    assert_eq!(
        body_of(form, Some(FlowBodyKind::Json), "{}"),
        RequestBody::Json {
            content: "{}".into()
        }
    );
    assert_eq!(
        body_of(RequestBody::None, None, "hi"),
        RequestBody::RawText {
            content: "hi".into(),
            language: None
        }
    );
}

#[tokio::test]
async fn a_skipped_step_does_not_wait_and_a_broken_condition_fails_the_step() {
    let mut skipped = step("s1", "req_a", 0);
    skipped.delay_ms = 30_000;
    skipped.condition_expression = "{{missing}}".into();
    let mut broken = step("s2", "req_a", 1);
    broken.condition_expression = "{{count}} > 10".into();
    broken.continue_on_error = true;
    let mut last = step("s3", "req_a", 2);
    last.condition_expression = "{{count}} == 'many'".into();
    let input = FlowRunInput {
        initial_variables: std::collections::BTreeMap::from([("count".into(), "many".into())]),
        ..FlowRunInput::default()
    };
    let sent = std::cell::Cell::new(0);
    let started = Instant::now();
    let result = run_flow(
        &flow("f_cond", vec![skipped, broken, last]),
        &stub_requests(),
        &input,
        &FlowRun::new("run_cond", 0),
        direct_scopes,
        |_request: Request, _variables: Vec<EnvironmentVariable>| {
            sent.set(sent.get() + 1);
            async move { Ok(exec(200, "ok")) }
        },
    )
    .await;

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the skipped step waited for its delay"
    );
    assert!(result.steps[0].skipped && result.steps[0].passed);
    let failed = &result.steps[1];
    assert!(!failed.passed && !failed.skipped && failed.attempts == 0);
    assert_eq!(
        failed.error.as_deref(),
        Some(
            "condition cannot be evaluated: '>' compares numbers, but its left side is not a number"
        )
    );
    assert_eq!(failed.condition.as_deref(), Some("many > 10"));
    assert!(result.steps[2].passed && !result.steps[2].skipped);
    assert_eq!((sent.get(), result.passed), (1, false));
}

#[tokio::test]
async fn script_locals_and_secret_extractions_carry_through_the_run() {
    let mut login = step("s1", "req_a", 0);
    login.extractions = vec![
        FlowExtraction {
            variable_name: "access_token".into(),
            source: FlowExtractionSource::BodyJsonPath,
            expression: "token".into(),
            secret: None,
        },
        FlowExtraction {
            variable_name: "user".into(),
            source: FlowExtractionSource::BodyJsonPath,
            expression: "user".into(),
            secret: None,
        },
    ];
    let steps = vec![login, step("s2", "req_a", 1), step("s3", "req_a", 2)];
    let input = FlowRunInput {
        initial_variables: std::collections::BTreeMap::from([
            ("access_token".into(), "old".into()),
            ("drop_me".into(), "x".into()),
        ]),
        ..FlowRunInput::default()
    };
    type Seen = Vec<(String, String, bool)>;
    let seen: std::cell::RefCell<Vec<Seen>> = std::cell::RefCell::new(Vec::new());
    let result = run_flow(
        &flow("f_local", steps),
        &stub_requests(),
        &input,
        &FlowRun::new("run_local", 0),
        direct_scopes,
        |_request: Request, variables: Vec<EnvironmentVariable>| {
            let call = seen.borrow().len();
            let mut snapshot: Seen = variables
                .iter()
                .map(|v| {
                    (
                        v.key.clone(),
                        v.value.clone().unwrap_or_default(),
                        v.is_secret,
                    )
                })
                .collect();
            snapshot.sort();
            seen.borrow_mut().push(snapshot);
            // Step 2's script sets `page` and unsets `drop_me`; the others leave the scope alone.
            let local = (call == 1).then(|| {
                let mut local: std::collections::BTreeMap<String, String> = variables
                    .iter()
                    .filter(|v| v.key != "drop_me")
                    .map(|v| (v.key.clone(), v.value.clone().unwrap_or_default()))
                    .collect();
                local.insert("page".into(), "2".into());
                local
            });
            async move {
                Ok(StepOutput {
                    execution: exec(200, r#"{"token":"tok-new","user":"ada"}"#),
                    local,
                })
            }
        },
    )
    .await;

    assert!(result.passed, "{result:?}");
    let seen = seen.into_inner();
    let row = |key: &str, value: &str, secret: bool| (key.to_owned(), value.to_owned(), secret);
    // The seed named like the secret extraction is a secret from the start.
    assert_eq!(
        seen[0],
        [row("access_token", "old", true), row("drop_me", "x", false)]
    );
    assert_eq!(
        seen[2],
        [
            row("access_token", "tok-new", true),
            row("page", "2", false),
            row("user", "ada", false),
        ]
    );
    assert_eq!(result.secret_variables, ["access_token"]);
}

#[tokio::test]
async fn cancellation_stops_before_step_or_during_step_delay() {
    let mut delayed = step("step_1", "req_ping", 0);
    delayed.delay_ms = 30_000;
    let flow = flow("flow_cancel", vec![delayed]);

    let pre_cancelled = CancelToken::new();
    pre_cancelled.cancel();
    let res = run_flow(
        &flow,
        &HashMap::new(),
        &FlowRunInput::default(),
        &FlowRun {
            cancel: Some(&pre_cancelled),
            ..FlowRun::new("run_test", 0)
        },
        direct_scopes,
        direct_step,
    )
    .await;
    assert!(res.cancelled && !res.passed && res.steps.is_empty());

    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        canceller.cancel();
    });
    let started = Instant::now();
    let result = run_flow(
        &flow,
        &HashMap::new(),
        &FlowRunInput::default(),
        &FlowRun {
            cancel: Some(&cancel),
            ..FlowRun::new("run_test", 0)
        },
        direct_scopes,
        direct_step,
    )
    .await;
    assert!(result.cancelled && result.steps.is_empty());
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn a_cancelled_step_ends_the_flow_and_steps_see_the_flow_variables() {
    let requests = HashMap::from([(
        "req_a".to_owned(),
        request("req_a", "http://127.0.0.1:1".into(), vec![]),
    )]);
    let flow = flow(
        "flow_cancelled_step",
        vec![step("s1", "req_a", 0), step("s2", "req_a", 1)],
    );
    let input = FlowRunInput {
        initial_variables: std::collections::BTreeMap::from([("seed".into(), "1".into())]),
        max_steps: None,
        ..FlowRunInput::default()
    };
    let seen = std::cell::RefCell::new(Vec::new());

    let result = run_flow(
        &flow,
        &requests,
        &input,
        &FlowRun::new("run_test", 0),
        direct_scopes,
        |_request: Request, variables: Vec<EnvironmentVariable>| {
            seen.borrow_mut()
                .extend(variables.into_iter().map(|variable| variable.key));
            async { Err::<ExecutionResult, _>(AppError::cancelled()) }
        },
    )
    .await;

    assert!(result.cancelled);
    assert!(!result.passed);
    assert!(
        result.steps.is_empty(),
        "the cancelled step is not reported"
    );
    assert_eq!(*seen.borrow(), vec!["seed".to_owned()]);
}

/// An execution result with `status` and `body`, without a real exchange behind it.
fn exec(status: u16, body: &str) -> ExecutionResult {
    serde_json::from_value(serde_json::json!({
        "status_code": status, "status_text": "", "url": "http://stub/x", "elapsed_ms": 1,
        "attempt_count": 1, "headers": [], "cookies": [], "body_text": body,
        "body_size_bytes": body.len(),
    }))
    .unwrap()
}

fn retrying(retries: u8, retry_on: Vec<FlowRetryOn>) -> FlowStep {
    FlowStep {
        retries,
        retry_on,
        ..step("s1", "req_a", 0)
    }
}

fn stub_requests() -> HashMap<String, Request> {
    HashMap::from([(
        "req_a".to_owned(),
        request("req_a", "http://stub/x".into(), vec![]),
    )])
}

/// Runs `flow` with a step executor that answers from `outcomes` in order (the last one
/// repeats) and records the timeout each attempt's request carried.
async fn run_with_outcomes(
    flow: &Flow,
    cancel: Option<&CancelToken>,
    outcomes: Vec<Result<ExecutionResult, AppError>>,
) -> (FlowRunResult, Vec<u64>) {
    let timeouts = std::cell::RefCell::new(Vec::new());
    let result = run_flow(
        flow,
        &stub_requests(),
        &FlowRunInput::default(),
        &FlowRun {
            cancel,
            ..FlowRun::new("run_test", 0)
        },
        direct_scopes,
        |request: Request, _variables: Vec<EnvironmentVariable>| {
            let mut timeouts = timeouts.borrow_mut();
            let outcome = outcomes[timeouts.len().min(outcomes.len() - 1)]
                .clone()
                .map(|mut execution| {
                    execution.assertion_results =
                        evaluate_assertions(&request.assertions, &execution);
                    execution
                });
            timeouts.push(request.network_options.timeout_ms);
            async move {
                if let Some(cancel) = cancel
                    && outcome.as_ref().is_ok_and(|exec| exec.status_code == 599)
                {
                    cancel.cancel();
                }
                outcome
            }
        },
    )
    .await;
    (result, timeouts.into_inner())
}

#[tokio::test]
async fn step_retries_handle_statuses_network_classes_final_failures_and_cancellation() {
    let mut step_ok = retrying(2, vec![FlowRetryOn::Status5xx]);
    step_ok.timeout_ms = Some(1_234);
    step_ok.assertions = vec![Assertion::StatusEquals { value: 200 }];
    let (res, timeouts) = run_with_outcomes(
        &flow("f1", vec![step_ok]),
        None,
        vec![Ok(exec(503, "")), Ok(exec(503, "")), Ok(exec(200, "ok"))],
    )
    .await;
    assert!(res.passed && res.steps[0].attempts == 3 && res.steps[0].explanation.is_none());
    assert_eq!(timeouts, [1_234, 1_234, 1_234]);

    let mut exhausted = retrying(1, vec![FlowRetryOn::Status5xx, FlowRetryOn::Status429]);
    exhausted.assertions = vec![Assertion::StatusEquals { value: 200 }];
    let (res, timeouts) = run_with_outcomes(
        &flow("f2", vec![exhausted]),
        None,
        vec![Ok(exec(429, "")), Ok(exec(503, ""))],
    )
    .await;
    assert!(!res.passed && res.steps[0].attempts == 2 && res.steps[0].status_code == Some(503));
    assert_eq!(timeouts, [30_000, 30_000]);

    let mut final_fail = retrying(3, vec![FlowRetryOn::Status5xx]);
    final_fail.assertions = vec![Assertion::StatusEquals { value: 201 }];
    final_fail.extractions = vec![FlowExtraction {
        variable_name: "id".into(),
        source: FlowExtractionSource::BodyJsonPath,
        expression: "data.id".into(),
        secret: None,
    }];
    let (res, timeouts) = run_with_outcomes(
        &flow("f3", vec![final_fail]),
        None,
        vec![Ok(exec(200, "{}"))],
    )
    .await;
    assert!(!res.passed && timeouts.len() == 1 && res.steps[0].attempts == 1);
    assert_eq!(
        res.steps[0].explanation.as_ref().unwrap().kind,
        FailureKind::StatusMismatch
    );

    let outcomes = vec![
        Err(AppError::network(
            "network request failed: connection refused",
        )),
        Ok(exec(200, "ok")),
    ];
    let (res, _) = run_with_outcomes(
        &flow("f4", vec![retrying(1, vec![FlowRetryOn::Network])]),
        None,
        outcomes.clone(),
    )
    .await;
    assert!(res.passed && res.steps[0].attempts == 2);
    let (res, _) = run_with_outcomes(
        &flow("f5", vec![retrying(1, vec![FlowRetryOn::Status5xx])]),
        None,
        outcomes,
    )
    .await;
    assert!(!res.passed && res.steps[0].attempts == 1);
    assert_eq!(
        res.steps[0].explanation.as_ref().unwrap().kind,
        FailureKind::Network
    );

    let (res, timeouts) = run_with_outcomes(
        &flow("f6", vec![retrying(3, Vec::new())]),
        None,
        vec![Ok(exec(503, ""))],
    )
    .await;
    assert_eq!((timeouts.len(), res.steps[0].attempts), (1, 1));

    let mut wait_cancel = retrying(2, vec![FlowRetryOn::Status5xx]);
    wait_cancel.retry_wait_ms = 60_000;
    let cancel = CancelToken::new();
    let (res, timeouts) = run_with_outcomes(
        &flow("f7", vec![wait_cancel]),
        Some(&cancel),
        vec![Ok(exec(599, ""))],
    )
    .await;
    assert!(res.cancelled && !res.passed && res.steps.is_empty() && timeouts.len() == 1);
}

#[tokio::test]
async fn observer_explanation_and_step_serde_defaults_hold() {
    let mut skipped = step("s2", "req_a", 1);
    skipped.condition_expression = "no == yes".into();
    let obs_flow = flow(
        "flow_observed",
        vec![
            retrying(1, vec![FlowRetryOn::Status5xx]),
            skipped,
            step("s3", "req_a", 2),
        ],
    );
    let seen = std::sync::Mutex::new(Vec::new());
    let on_step = |index: usize, result: &FlowStepResult| {
        seen.lock()
            .unwrap()
            .push((index, result.step_id.clone(), result.attempts));
    };
    let outcomes = [exec(503, ""), exec(200, "ok"), exec(200, "ok")];
    let calls = std::cell::Cell::new(0);
    let result = run_flow(
        &obs_flow,
        &stub_requests(),
        &FlowRunInput::default(),
        &FlowRun {
            on_step: Some(&on_step),
            ..FlowRun::new("run_42", 1_700_000_000_000)
        },
        direct_scopes,
        |_request: Request, _variables: Vec<EnvironmentVariable>| {
            let outcome = outcomes[calls.get()].clone();
            calls.set(calls.get() + 1);
            async move { Ok(outcome) }
        },
    )
    .await;
    assert_eq!(
        (
            result.run_id.as_str(),
            result.started_at_ms,
            result.steps_total
        ),
        ("run_42", 1_700_000_000_000, 3)
    );
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (0, "s1".into(), 2),
            (1, "s2".into(), 0),
            (2, "s3".into(), 1)
        ]
    );

    let (explained, _) = run_with_outcomes(
        &flow("flow_explained", vec![step("s1", "req_a", 0)]),
        None,
        vec![Err(AppError::validation("unresolved variable invoice_id"))],
    )
    .await;
    let explanation = explained.steps[0].explanation.as_ref().unwrap();
    assert_eq!(explanation.kind, FailureKind::UnresolvedVariable);
    assert_eq!(
        explanation.message,
        "{{invoice_id}} has no value. Extract it in an earlier step."
    );

    let old: FlowStep = serde_json::from_value(serde_json::json!({
        "id": "s1", "name": "Login", "delay_ms": 0, "continue_on_error": false, "position": 0
    }))
    .unwrap();
    assert_eq!(
        (
            old.retries,
            old.retry_wait_ms,
            old.retry_on.len(),
            old.timeout_ms
        ),
        (0, 0, 0, None)
    );
    let new = FlowStep {
        retries: 2,
        retry_wait_ms: 250,
        retry_on: vec![
            FlowRetryOn::Network,
            FlowRetryOn::Status5xx,
            FlowRetryOn::Status429,
        ],
        timeout_ms: Some(5_000),
        ..old
    };
    let json = serde_json::to_value(&new).unwrap();
    assert_eq!(
        json["retry_on"],
        serde_json::json!(["network", "status_5xx", "status_429"])
    );
    assert_eq!(serde_json::from_value::<FlowStep>(json).unwrap(), new);
}

#[tokio::test]
async fn the_saved_requests_own_assertions_judge_the_step_unless_it_opts_out() {
    let mut requests = stub_requests();
    if let Some(saved) = requests.get_mut("req_a") {
        saved.assertions = vec![Assertion::StatusEquals { value: 201 }];
    }
    let run = |step: FlowStep| {
        let requests = requests.clone();
        async move {
            run_flow(
                &flow("f_fw1", vec![step]),
                &requests,
                &FlowRunInput::default(),
                &FlowRun::new("run_fw1", 0),
                direct_scopes,
                |request: Request, _variables: Vec<EnvironmentVariable>| async move {
                    let mut execution = exec(200, "ok");
                    execution.assertion_results =
                        evaluate_assertions(&request.assertions, &execution);
                    Ok(execution)
                },
            )
            .await
        }
    };

    let judged = run(step("s1", "req_a", 0)).await;
    let only = &judged.steps[0];
    assert!(!judged.passed && !only.passed);
    assert_eq!(only.request_assertion_count, 1);
    assert_eq!(only.assertion_results.len(), 1);
    let explanation = only.explanation.as_ref().unwrap();
    assert_eq!(explanation.kind, FailureKind::StatusMismatch);
    let facts = explanation.facts.as_ref().unwrap();
    assert_eq!(
        (facts.expected.as_deref(), facts.actual.as_deref()),
        (Some("201"), Some("200"))
    );

    let opted_out = run(FlowStep {
        ignore_request_assertions: true,
        assertions: vec![Assertion::StatusEquals { value: 200 }],
        ..step("s1", "req_a", 0)
    })
    .await;
    assert!(opted_out.passed);
    assert_eq!(opted_out.steps[0].request_assertion_count, 0);
    assert_eq!(opted_out.steps[0].assertion_results.len(), 1);
}

#[tokio::test]
async fn only_step_sends_one_request_and_is_summarized_at_its_own_index() {
    let steps = vec![
        step("s1", "req_a", 0),
        step("s2", "", 1),
        FlowStep {
            url_override: Some("http://stub/third".into()),
            ..step("s3", "", 2)
        },
    ];
    let only_flow = flow("f_only", steps);
    let input = FlowRunInput {
        only_step: Some(2),
        ..FlowRunInput::default()
    };
    // The incomplete step 2 is outside the scope: neither validation nor the run touches it.
    validate_runnable(&only_flow, &stub_requests(), &input).unwrap();
    let error =
        validate_runnable(&only_flow, &stub_requests(), &FlowRunInput::default()).unwrap_err();
    assert_eq!(
        error.message(),
        "step 2 \"s2\" has neither a saved request nor a URL"
    );

    let sent = std::cell::RefCell::new(Vec::new());
    let result = run_flow(
        &only_flow,
        &stub_requests(),
        &input,
        &FlowRun::new("run_only", 0),
        direct_scopes,
        |request: Request, _variables: Vec<EnvironmentVariable>| {
            sent.borrow_mut().push(request.url.clone());
            async move { Ok(exec(200, "ok")) }
        },
    )
    .await;

    assert_eq!(*sent.borrow(), ["http://stub/third"]);
    assert_eq!(result.steps.len(), 1);
    let summary = summarize_run(&result, 1);
    assert_eq!(
        summary.scope.kind,
        crate::workspace::items::FlowRunScopeKind::Only
    );
    assert_eq!(
        summary
            .steps
            .iter()
            .map(|step| (step.index, step.step_id.as_str()))
            .collect::<Vec<_>>(),
        [(2, "s3")]
    );
}
