//! Tests for the misc logic use-cases: runner data (pinned by a shared fixture), history
//! paging, save destinations, the snippet catalog and the validation lists.

use super::*;
use crate::app::{
    AppState, EnvironmentVariableInput, SaveCollectionInput, SaveEnvironmentInput, SaveFolderInput,
    collection_save_impl, environment_save_impl, flow_save_impl, folder_save_impl,
    mock_server_save_impl, workspace_create_impl,
};
use crate::workspace::{
    ExecutionHistoryEntry, ExecutionHistoryResultKind, MockResponse, MockResponseMode, MockRoute,
    MockRouteType, MockRulesOperator, MockServer, Request, WorkspaceId,
};
use serde_json::{Value, json};

const RUNNER_CASES: &str = include_str!("../../../../../fixtures/runner-data-cases.json");
const SNIPPET_CATALOG: &str = include_str!("../../../../../fixtures/script-snippet-catalog.json");

fn variable(key: &str, value: Option<&str>) -> EnvironmentVariableInput {
    EnvironmentVariableInput {
        key: key.to_owned(),
        value: value.map(str::to_owned),
        ..Default::default()
    }
}

fn mock_server(workspace_id: &str, port: u16) -> MockServer {
    MockServer {
        id: "mock_1".into(),
        workspace_id: WorkspaceId::new(workspace_id).expect("workspace id"),
        name: "Mock".into(),
        port,
        endpoint_prefix: String::new(),
        latency_ms: 0,
        cors_enabled: true,
        headers: Vec::new(),
        data_buckets: Vec::new(),
        callbacks: Vec::new(),
        proxy_enabled: false,
        proxy_base_url: String::new(),
        proxy_remove_prefix: false,
        proxy_req_headers: Vec::new(),
        proxy_res_headers: Vec::new(),
        routes: vec![MockRoute {
            id: "route_1".into(),
            method: "GET".into(),
            path: "/hello".into(),
            enabled: true,
            description: String::new(),
            route_type: MockRouteType::Http,
            data_bucket_id: String::new(),
            crud_id_key: "id".into(),
            response_mode: MockResponseMode::FirstMatch,
            responses: vec![MockResponse {
                id: "resp_1".into(),
                label: "OK".into(),
                status_code: 200,
                headers: Vec::new(),
                body: String::new(),
                body_source: None,
                fault: None,
                latency_ms: 0,
                is_default: true,
                rules_operator: MockRulesOperator::And,
                rules: Vec::new(),
                callback_ids: Vec::new(),
                position: 0,
                stream: None,
            }],
            position: 0,
        }],
        updated_at_ms: 0,
        environment_id: None,
    }
}

fn state_with_workspace() -> AppState {
    let state = AppState::open_in_memory().expect("state opens");
    workspace_create_impl(&state, "ws_1".into(), "Workspace".into()).expect("workspace");
    state
}

#[test]
fn runner_data_matches_the_fixture_generated_from_the_former_typescript() {
    let fixture: Value = serde_json::from_str(RUNNER_CASES).expect("fixture parses");
    assert_eq!(
        fixture["limits"]["max_iterations"],
        Value::from(RUNNER_MAX_ITERATIONS)
    );
    assert_eq!(
        fixture["limits"]["max_delay_ms"],
        Value::from(RUNNER_MAX_DELAY_MS)
    );
    let cases = fixture["cases"].as_array().expect("cases array");
    assert!(cases.len() > 50);
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let text = case["text"].as_str().expect("text");
        let result = runner_parse_data(text, RunnerDataKind::Auto);
        match (&case["rows"], result) {
            (Value::Null, Err(AppError::Validation { message, .. })) => {
                if let Some(prefix) = case["error_prefix"].as_str() {
                    assert!(message.starts_with(prefix), "{name}: {message}");
                } else {
                    assert_eq!(Some(message.as_str()), case["error"].as_str(), "{name}");
                }
            }
            (Value::Null, other) => panic!("{name}: expected a validation error, got {other:?}"),
            (rows, Ok(data)) => {
                assert_eq!(
                    &serde_json::to_value(&data.rows).expect("rows"),
                    rows,
                    "{name}"
                );
                assert_eq!(
                    serde_json::to_value(&data.columns).expect("columns"),
                    case["columns"],
                    "{name}"
                );
                assert!(data.warnings.is_empty(), "{name}");
            }
            (_, Err(error)) => panic!("{name}: unexpected error {error}"),
        }
    }
}

#[test]
fn runner_data_kind_forces_the_parser_and_warns_about_surplus_rows() {
    let json_as_csv = runner_parse_data("{a}\n1", RunnerDataKind::Csv).expect("csv header");
    assert_eq!(json_as_csv.columns, ["{a}"]);
    let csv_as_json = runner_parse_data("a,b\n1,2", RunnerDataKind::Json).unwrap_err();
    assert!(csv_as_json.message().starts_with("Invalid JSON: "));
    assert!(
        runner_parse_data("  ", RunnerDataKind::Json)
            .expect("blank is no data")
            .rows
            .is_empty()
    );

    let mut csv = String::from("id\n");
    for index in 0..=RUNNER_MAX_ITERATIONS {
        csv.push_str(&format!("{index}\n"));
    }
    let data = runner_parse_data(&csv, RunnerDataKind::Auto).expect("parses");
    assert_eq!(data.rows.len(), RUNNER_MAX_ITERATIONS as usize + 1);
    assert_eq!(data.warnings.len(), 1);
    assert_eq!(
        runner_limits(),
        RunnerLimits {
            max_iterations: 1000,
            max_delay_ms: 60_000
        }
    );
}

#[test]
fn history_page_reports_has_more_in_server_order_and_searches() {
    let state = state_with_workspace();
    let request = Request::new(
        "req_1",
        "ws_1",
        "List users",
        "https://api.example.com/users",
    )
    .expect("request");
    {
        let store = state.store();
        store.save_request(&request).expect("save request");
        for index in 0..5 {
            let entry = ExecutionHistoryEntry::from_success(
                format!("hist_{index}"),
                &request,
                None,
                if index == 3 { 0 } else { 200 },
                "OK",
                "https://api.example.com/users",
                10,
                "{}",
                2,
                1,
                format!("170000000000{index}"),
            )
            .expect("entry");
            let entry = if index == 3 {
                ExecutionHistoryEntry {
                    result_kind: ExecutionHistoryResultKind::Failure,
                    request_name: "Broken call".into(),
                    ..entry
                }
            } else {
                entry
            };
            store.save_execution_history(&entry).expect("save history");
        }
    }
    let ids = |page: &HistoryPageDto| {
        page.entries
            .iter()
            .map(|entry| entry.id.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    let page = |filter: HistoryFilter, limit, cursor: Option<String>| {
        history_page_impl(&state, "ws_1".into(), filter, limit, cursor).expect("page")
    };

    let first = page(HistoryFilter::default(), Some(2), None);
    assert_eq!(ids(&first), ["hist_4", "hist_3"]);
    assert_eq!(first.total, Some(5));
    assert!(first.next_cursor.is_some());
    let second = page(HistoryFilter::default(), Some(2), first.next_cursor.clone());
    assert_eq!(ids(&second), ["hist_2", "hist_1"]);
    assert_eq!(second.total, None);
    let last = page(
        HistoryFilter::default(),
        Some(2),
        second.next_cursor.clone(),
    );
    assert_eq!(ids(&last), ["hist_0"]);
    assert_eq!(last.next_cursor, None);
    let exact = page(HistoryFilter::default(), Some(5), None);
    assert_eq!((exact.entries.len(), exact.next_cursor), (5, None));
    let default_size = page(HistoryFilter::default(), None, None);
    assert_eq!(default_size.entries.len(), 5);

    let blank = HistoryFilter {
        query: "  ".into(),
        ..HistoryFilter::default()
    };
    assert_eq!(page(blank, Some(10), None).entries.len(), 5);
    let failures = HistoryFilter {
        include_failed: true,
        ..HistoryFilter::default()
    };
    assert_eq!(ids(&page(failures, None, None)), ["hist_3"]);
    let server_errors = HistoryFilter {
        status_classes: vec![5],
        ..HistoryFilter::default()
    };
    assert!(page(server_errors, None, None).entries.is_empty());
    let text = HistoryFilter {
        query: " BROKEN ".into(),
        ..HistoryFilter::default()
    };
    let found = page(text, Some(1), None);
    assert_eq!(
        (ids(&found), found.next_cursor, found.total),
        (vec!["hist_3".to_owned()], None, Some(1))
    );

    let invalid = history_page_impl(
        &state,
        "ws_1".into(),
        HistoryFilter::default(),
        None,
        Some("not-a-cursor".into()),
    )
    .expect_err("a foreign cursor is rejected");
    assert!(
        matches!(invalid, AppError::Validation { .. }),
        "{invalid:?}"
    );

    let cleared = crate::app::history_clear_filtered_impl(
        &state,
        "ws_1".into(),
        HistoryFilter {
            query: "broken".into(),
            ..HistoryFilter::default()
        },
    )
    .expect("clear filtered");
    assert_eq!(cleared, 1);
    assert_eq!(page(HistoryFilter::default(), None, None).entries.len(), 4);
}

#[test]
fn save_destinations_order_collections_and_nested_folders() {
    let state = state_with_workspace();
    let collection = |id: &str, name: &str, position: i64| SaveCollectionInput {
        id: id.into(),
        workspace_id: "ws_1".into(),
        name: name.into(),
        position,
        ..Default::default()
    };
    let folder = |id: &str, collection: &str, parent: Option<&str>, name: &str, position: i64| {
        SaveFolderInput {
            id: id.into(),
            collection_id: collection.into(),
            parent_folder_id: parent.map(str::to_owned),
            name: name.into(),
            position,
            ..Default::default()
        }
    };
    collection_save_impl(&state, collection("c_late", "Alpha", 2)).expect("collection");
    collection_save_impl(&state, collection("c_b", "bravo", 1)).expect("collection");
    collection_save_impl(&state, collection("c_a", "Apple", 1)).expect("collection");
    folder_save_impl(&state, folder("f_z", "c_a", None, "Zeta", 0)).expect("folder");
    folder_save_impl(&state, folder("f_b", "c_a", None, "beta", 0)).expect("folder");
    folder_save_impl(&state, folder("f_a", "c_a", None, "Alpha", 0)).expect("folder");
    folder_save_impl(&state, folder("f_deep", "c_a", Some("f_b"), "Deep", 0)).expect("folder");
    folder_save_impl(&state, folder("f_first", "c_a", None, "Zzz", -1)).expect("folder");
    folder_save_impl(&state, folder("f_other", "c_b", None, "Other", 0)).expect("folder");

    let rows = request_save_destinations(&state, "ws_1".into()).expect("rows");
    let summary: Vec<_> = rows
        .iter()
        .map(|row| (row.key.as_str(), row.depth))
        .collect();
    assert_eq!(
        summary,
        [
            ("collection:c_a", 0),
            ("folder:f_first", 1),
            ("folder:f_a", 1),
            ("folder:f_b", 1),
            ("folder:f_deep", 2),
            ("folder:f_z", 1),
            ("collection:c_b", 0),
            ("folder:f_other", 1),
            ("collection:c_late", 0),
        ]
    );
    assert_eq!(rows[4].collection_id, "c_a");
    assert_eq!(rows[4].folder_id.as_deref(), Some("f_deep"));
    assert_eq!(rows[0].kind, SaveDestinationKind::Collection);
    assert_eq!(rows[0].folder_id, None);
    assert!(
        request_save_destinations(&state, "ws_missing".into())
            .expect("unknown workspace")
            .is_empty()
    );
}

#[test]
fn snippet_catalog_matches_the_former_script_tab_templates() {
    let catalog = script_snippet_catalog();
    assert_eq!(
        serde_json::to_value(&catalog).expect("catalog serializes"),
        serde_json::from_str::<Value>(SNIPPET_CATALOG).expect("fixture parses")
    );
    assert_eq!(catalog.len(), 8);
    let mut ids: Vec<_> = catalog.iter().map(|snippet| snippet.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 8);
    assert_eq!(
        catalog
            .iter()
            .filter(|snippet| snippet.phase == ScriptPhase::Pre)
            .count(),
        4
    );
    assert!(catalog.iter().all(|snippet| snippet.code.ends_with('\n')));
}

#[test]
fn collection_and_environment_issues_list_everything_the_save_rejects() {
    let rows = [
        variable("ok", Some("1")),
        variable("  ", Some("x")),
        variable("late", None),
    ];
    let issues = validate_collection("  ", &rows);
    let fields: Vec<_> = issues.iter().map(|i| i.field.as_str()).collect();
    assert_eq!(fields, ["name", "variables[1]", "variables[2]"]);
    assert_eq!(issues[0].message, "collection name cannot be blank");

    // The save use-cases return the first issue's text.
    let state = state_with_workspace();
    let save = collection_save_impl(
        &state,
        SaveCollectionInput {
            id: "c_1".into(),
            workspace_id: "ws_1".into(),
            variables: rows.to_vec(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(save.message(), issues[0].message);
    let only_rows = validate_collection("Name", &rows);
    assert_eq!(only_rows, validate_globals(&rows));
    let row_save = collection_save_impl(
        &state,
        SaveCollectionInput {
            id: "c_2".into(),
            workspace_id: "ws_1".into(),
            name: "Name".into(),
            variables: rows.to_vec(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(row_save.message(), only_rows[0].message);

    let env_issues = validate_environment("", &rows);
    assert_eq!(env_issues[0].message, "environment name cannot be blank");
    let env_save = environment_save_impl(
        &state,
        SaveEnvironmentInput {
            id: "e_1".into(),
            workspace_id: "ws_1".into(),
            variables: rows.to_vec(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(env_save.message(), env_issues[0].message);

    assert!(validate_collection("Fine", &[variable("k", Some("v"))]).is_empty());
    assert!(validate_environment("Fine", &[]).is_empty());
}

#[test]
fn mock_server_issues_cover_every_rule_and_match_the_save_error() {
    let state = state_with_workspace();
    let mut server = mock_server("ws_1", 4010);
    assert!(validate_mock_server(&server).is_empty());

    server.id = " ".into();
    server.name = "".into();
    server.port = 0;
    server.latency_ms = 60_001;
    server.routes[0].id = "".into();
    server.routes[0].path = "hello".into();
    server.routes[0].method = "FETCH".into();
    server.routes[0].route_type = MockRouteType::Crud;
    server.routes[0].data_bucket_id = " ".into();
    server.routes[0].responses[0].status_code = 99;
    server.routes[0].responses[0].latency_ms = 60_001;
    let issues = validate_mock_server(&server);
    let fields: Vec<_> = issues.iter().map(|i| i.field.as_str()).collect();
    assert_eq!(
        fields,
        [
            "id",
            "name",
            "port",
            "latency_ms",
            "routes[0].id",
            "routes[0].path",
            "routes[0].method",
            "routes[0].data_bucket_id",
            "routes[0].responses[0].status_code",
            "routes[0].responses[0].latency_ms",
        ]
    );
    let error = mock_server_save_impl(&state, server.clone()).unwrap_err();
    assert_eq!(error.message(), issues[0].message);
    server.id = "m1".into();
    let error = mock_server_save_impl(&state, server).unwrap_err();
    assert_eq!(error.message(), "mock server name cannot be blank");
}

fn rule(
    id: &str,
    operator: crate::workspace::MockRuleOperator,
    value: &str,
) -> crate::workspace::MockRule {
    crate::workspace::MockRule {
        id: id.into(),
        target: crate::workspace::MockRuleTarget::Query,
        modifier: "q".into(),
        operator,
        value: value.into(),
        invert: false,
        position: 0,
    }
}

#[test]
fn unsupported_regex_rules_are_rejected_on_save_and_start_but_other_operators_are_ignored() {
    use crate::workspace::MockRuleOperator::{Contains, Equals, Regex, RegexIgnoreCase};
    let state = state_with_workspace();
    let mut server = mock_server("ws_1", 4011);
    server.routes[0].path = "/users".into();
    server.routes[0].responses[0].label = "Not found".into();
    server.routes[0].responses[0].rules = vec![
        rule("r1", Equals, "(?=x)"),
        rule("r2", Contains, r"(a)\1"),
        rule("r3", Regex, r"^\d+$"),
        rule("r4", RegexIgnoreCase, "^(?!admin)"),
    ];
    let issues = validate_mock_server(&server);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].field, "routes[0].responses[0].rules[3].value");
    assert_eq!(
        issues[0].message,
        "mock route /users, response \"Not found\", rule 4: regular expression not supported: negative look-ahead (?!…) — use a simpler pattern"
    );
    let error = mock_server_save_impl(&state, server.clone()).unwrap_err();
    assert!(matches!(error, AppError::Validation { .. }), "{error:?}");
    assert_eq!(error.message(), issues[0].message);

    // A server stored before the check existed is refused at start, not served half-working.
    state.store().save_mock_server(&server).expect("stored");
    let error = crate::app::mock_server_start_impl(&state, "ws_1".into(), server.id.to_string())
        .unwrap_err();
    assert_eq!(error.message(), issues[0].message);

    server.routes[0].responses[0].rules[3].value = "^admin".into();
    assert!(validate_mock_server(&server).is_empty());
    mock_server_save_impl(&state, server).expect("a valid regex saves");
}

#[test]
fn unsupported_body_regex_extractions_are_rejected_on_flow_save() {
    use crate::workspace::items::{Flow, FlowStep};
    let state = state_with_workspace();
    let step: FlowStep = serde_json::from_value(serde_json::json!({
        "id": "s1", "name": "Login", "delay_ms": 0, "continue_on_error": false, "position": 0,
        "extractions": [
            {"variable_name": "a", "source": "BodyRegex", "expression": r#""id":(\d+)"#},
            {"variable_name": "b", "source": "BodyRegex", "expression": "(?<=id:)\\d+"},
            {"variable_name": "c", "source": "Header", "expression": "(?=x)"}
        ]
    }))
    .expect("step");
    let mut flow = Flow {
        id: "f1".into(),
        workspace_id: crate::workspace::WorkspaceId::new("ws_1").expect("id"),
        name: "Flow".into(),
        description: String::new(),
        steps: vec![step],
        updated_at_ms: 0,
        options: Default::default(),
    };
    let issues = validate_flow(&flow);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].field, "steps[0].extractions[1].expression");
    assert_eq!(
        issues[0].message,
        "flow step \"Login\", extraction 2 (b): regular expression not supported: look-behind (?<=…) — use a simpler pattern"
    );
    let error = flow_save_impl(&state, flow.clone()).unwrap_err();
    assert!(matches!(error, AppError::Validation { .. }), "{error:?}");
    assert_eq!(error.message(), issues[0].message);

    flow.steps[0].extractions[1].expression = r"id:(\d+)".into();
    assert!(validate_flow(&flow).is_empty());
}

#[test]
fn malformed_step_conditions_are_rejected_on_flow_save_naming_the_step_and_field() {
    use crate::workspace::items::{Flow, FlowStep};
    let state = state_with_workspace();
    let step = |id: &str, condition: &str| -> FlowStep {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": "Check", "delay_ms": 0, "continue_on_error": false, "position": 0,
            "condition_expression": condition
        }))
        .expect("step")
    };
    let mut flow = Flow {
        id: "f_cond".into(),
        workspace_id: crate::workspace::WorkspaceId::new("ws_1").expect("id"),
        name: "Flow".into(),
        description: String::new(),
        steps: vec![
            step("ok", "{{count}} > 10 and {{plan}} == 'pro'"),
            step("bad", "{{count}} = 10"),
        ],
        updated_at_ms: 0,
        options: Default::default(),
    };
    let issues = validate_flow(&flow);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].field, "steps[1].condition_expression");
    assert_eq!(
        issues[0].message,
        "flow step \"Check\" condition: use '==' to compare"
    );
    let error = flow_save_impl(&state, flow.clone()).unwrap_err();
    assert_eq!(error.message(), issues[0].message);
    match &error {
        AppError::Validation { keys, .. } => assert_eq!(
            keys.as_deref().map(Vec::as_slice),
            Some(["bad".to_owned(), "condition_expression".to_owned()].as_slice())
        ),
        other => panic!("{other:?}"),
    }

    flow.steps[1].condition_expression = "{{count}} == 10".into();
    assert!(flow_save_impl(&state, flow).is_ok());
}

#[test]
fn flow_issues_cover_every_rule_and_match_the_save_error() {
    use crate::workspace::items::{Flow, FlowStep};
    let state = state_with_workspace();
    let step = |id: &str, delay_ms: u64| -> FlowStep {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": "s", "delay_ms": delay_ms, "continue_on_error": false, "position": 0
        }))
        .expect("step")
    };
    let flow = Flow {
        id: " ".into(),
        workspace_id: crate::workspace::WorkspaceId::new("ws_1").expect("id"),
        name: "".into(),
        description: String::new(),
        steps: vec![step("a", 0), step("", 0), step("a", 60_001)],
        updated_at_ms: 0,
        options: Default::default(),
    };
    let issues = validate_flow(&flow);
    let summary: Vec<_> = issues
        .iter()
        .map(|i| (i.field.as_str(), i.message.as_str()))
        .collect();
    assert_eq!(
        summary,
        [
            ("name", "flow name cannot be blank"),
            ("id", "flow id cannot be blank"),
            ("steps[1].id", "step id cannot be blank"),
            ("steps[2].id", "duplicate step id a in flow"),
            (
                "steps[2].delay_ms",
                "flow step a delay cannot exceed 60000 ms"
            ),
        ]
    );
    let error = flow_save_impl(&state, flow).unwrap_err();
    assert_eq!(error.message(), "flow name cannot be blank");
}

#[test]
fn folder_and_globals_validation_match_their_saves() {
    use crate::app::{EnvironmentVariableInput, GlobalsSaveInput, SaveFolderInput};
    let state = crate::app::AppState::open_in_memory().unwrap();
    crate::app::workspace_create_impl(&state, "ws_v".into(), "V".into()).unwrap();
    assert!(validate_folder("Ok").is_empty());
    let issues = validate_folder("   ");
    let save = crate::app::folder_save_impl(
        &state,
        SaveFolderInput {
            id: "fold_v".into(),
            collection_id: "col_v".into(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(issues[0].message, save.message());

    let row = EnvironmentVariableInput {
        value: Some("v".into()),
        ..Default::default()
    };
    let issues = validate_globals(std::slice::from_ref(&row));
    let save = crate::app::globals_save_impl(
        &state,
        GlobalsSaveInput {
            workspace_id: "ws_v".into(),
            variables: vec![row],
            base_revision: None,
        },
    )
    .unwrap_err();
    assert_eq!(issues[0].message, save.message());
}

/// `fixtures/demo-conformance/mock-validation.json`: the issue fields, also asserted against the
/// demo's copy of the rules (`src/features/mock/demoRules.test.ts`).
#[test]
fn mock_server_issues_match_the_shared_conformance_fixture() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/demo-conformance/mock-validation.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let mut server = serde_json::to_value(mock_server("ws_1", 4010)).unwrap();
        for pair in case["patch"].as_array().unwrap() {
            let pointer = format!("/{}", pair[0].as_str().unwrap().replace('.', "/"));
            *server.pointer_mut(&pointer).unwrap() = pair[1].clone();
        }
        let server: MockServer = serde_json::from_value(server).unwrap();
        let fields: Vec<String> = validate_mock_server(&server)
            .into_iter()
            .map(|issue| issue.field)
            .collect();
        assert_eq!(json!(fields), case["fields"], "{}", case["name"]);
    }
}
