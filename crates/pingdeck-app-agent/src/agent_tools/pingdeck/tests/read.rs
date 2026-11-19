//! The read tools: `get_trace`, `list_requests`, `list_environments` and `query_gateway_log`
//! return metadata, never a request or response body; `get_request` returns one saved request's
//! definition. Every seeded secret stays masked.

use serde_json::json;

use super::super::{
    GetRequestTool, GetTraceTool, ListEnvironmentsTool, ListRequestsTool, QueryGatewayLogTool,
    SendRequestTool,
};
use super::support::{SESSION, TOKEN, WS_A, World, add_provider, call, json_of, log_row, text};
use crate::agent::limits::GET_REQUEST_BODY_BYTES;
use crate::agent::tools::{AgentTool, ToolClass};
use crate::llm::probe::test_support::Reply;

const RESPONSE_BODY: &str = "{\"invoice\":\"INV-1001\",\"card\":\"4111-1111-1111-1111\"}";

async fn sent_world() -> (World, String) {
    let world = World::with(|_| Reply::Json(200, RESPONSE_BODY.to_owned())).await;
    let value = json_of(
        &call(
            &SendRequestTool::new(),
            &world,
            json!({"draft": {"method": "POST", "url": "{{base_url}}/invoices", "body": {"type": "json", "content": "{\"order\":\"ORD-77\"}"}}}),
        )
        .await,
    );
    let id = value["history_id"].as_str().unwrap().to_owned();
    (world, id)
}

#[tokio::test(flavor = "multi_thread")]
async fn get_trace_summary_has_timing_and_status_but_no_body_text() {
    let (world, id) = sent_world().await;
    let out = call(&GetTraceTool::new(), &world, json!({"history_id": id})).await;
    let shown = text(&out);
    for body in ["INV-1001", "4111-1111", "ORD-77"] {
        assert!(!shown.contains(body), "{body} leaked: {shown}");
    }
    let value = json_of(&out);
    assert_eq!(value["section"], "summary");
    assert_eq!(value["summary"]["status"], 200);
    assert_eq!(value["summary"]["source"], "agent");
    assert_eq!(value["summary"]["environment"], "Staging");
    assert!(value["summary"]["timing"]["total_ms"].is_number());
    assert_eq!(value["workspace"]["name"], "Billing platform");
    assert_eq!(
        value["view"],
        json!({"kind": "trace", "history_id": id, "workspace_id": WS_A})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn get_trace_variables_name_secrets_but_never_show_their_value() {
    let world = World::new().await;
    let sent = json_of(
        &call(
            &SendRequestTool::new(),
            &world,
            json!({"request_id": "req_inv"}),
        )
        .await,
    );
    let id = sent["history_id"].as_str().unwrap();
    let out = call(
        &GetTraceTool::new(),
        &world,
        json!({"history_id": id, "section": "variables"}),
    )
    .await;
    let value = json_of(&out);
    let rows = value["variables"].as_array().unwrap();
    let token = rows
        .iter()
        .find(|row| row["name"] == "token")
        .expect("the token variable");
    assert_eq!(token["secret"], true);
    assert!(token.get("value").is_none());
    assert!(!text(&out).contains(TOKEN));
    let base = rows
        .iter()
        .find(|row| row["name"] == "base_url")
        .expect("base_url");
    assert_eq!(base["secret"], false);
    assert_eq!(base["value"], world.base_url());
}

#[tokio::test(flavor = "multi_thread")]
async fn get_trace_cannot_read_the_history_of_another_workspace() {
    let (world, _) = sent_world().await;
    // A row of workspace B, planted straight into the store.
    let entry = {
        let request = crate::workspace::models::Request::new(
            "req_other",
            "ws_b",
            "GET /secret",
            "https://other.test/x",
        )
        .unwrap();
        crate::workspace::ExecutionHistoryEntry::from_success(
            "hist_other_1",
            &request,
            None,
            200,
            "OK",
            "https://other.test/x",
            5,
            "body",
            4,
            1,
            "2026-11-07T00:00:00Z",
        )
        .unwrap()
    };
    world.state.store().save_execution_history(&entry).unwrap();
    let out = call(
        &GetTraceTool::new(),
        &world,
        json!({"history_id": "hist_other_1"}),
    )
    .await;
    assert!(
        out.is_error && text(&out).contains("No such history entry"),
        "{}",
        text(&out)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_requests_returns_ids_places_methods_and_unresolved_url_templates() {
    let world = World::new().await;
    let out = call(&ListRequestsTool::new(), &world, json!({})).await;
    let value = json_of(&out);
    assert_eq!(value["workspace"]["name"], "Billing platform");
    let rows = value["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["request_id"], "req_inv");
    assert_eq!(rows[0]["folder"], "Billing API/Invoices");
    assert_eq!(rows[0]["name"], "GET /invoices");
    assert_eq!(rows[0]["method"], "GET");
    assert_eq!(
        rows[0]["url"], "{{base_url}}/invoices",
        "variables stay unresolved"
    );
    assert_eq!(rows[0]["agent_touched"], false);
    let shown = text(&out);
    assert!(
        !shown.contains("Bearer") && !shown.contains(TOKEN),
        "no header or secret: {shown}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_requests_is_scoped_to_the_bound_workspace() {
    let world = World::new().await;
    let shown = text(&call(&ListRequestsTool::new(), &world, json!({"query": "secret"})).await);
    assert_eq!(
        shown, "No saved requests match.",
        "ws_b's GET /secret is out of reach"
    );
    let value = json_of(&call(&ListRequestsTool::new(), &world, json!({})).await);
    assert!(!value.to_string().contains("req_other"));
    // Rebinding the chat to workspace B shows B's rows and names B.
    world
        .state
        .store()
        .set_session_pingdeck_binding(
            SESSION,
            Some(&crate::workspace::storage::PingDeckBinding {
                workspace_id: "ws_b".to_owned(),
                environment_id: None,
            }),
        )
        .unwrap();
    let value = json_of(&call(&ListRequestsTool::new(), &world, json!({})).await);
    assert_eq!(value["workspace"]["name"], "Other workspace");
    assert_eq!(value["rows"][0]["request_id"], "req_other");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_requests_filters_by_collection_folder_method_and_text() {
    let world = World::new().await;
    {
        let store = world.state.store();
        for (id, name, url) in [
            ("req_post", "POST /refunds", "{{base_url}}/refunds"),
            ("req_loose", "Loose", "{{base_url}}/loose"),
        ] {
            let mut request = crate::workspace::models::Request::new(id, WS_A, name, url).unwrap();
            if id == "req_post" {
                request.method = crate::workspace::models::HttpMethod::Post;
                request.collection_id =
                    Some(crate::workspace::models::CollectionId::new("col_a").unwrap());
            }
            store.save_request(&request).unwrap();
        }
    }
    let tool = ListRequestsTool::new();
    let ids = |args: serde_json::Value| async {
        let out = call(&tool, &world, args).await;
        if text(&out).starts_with("No ") {
            return Vec::new();
        }
        let mut found = json_of(&out)["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["request_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        found.sort();
        found
    };
    assert_eq!(
        ids(json!({"collection": "billing api"})).await,
        vec!["req_inv", "req_post"]
    );
    assert_eq!(ids(json!({"folder": "Invoices"})).await, vec!["req_inv"]);
    assert_eq!(ids(json!({"method": "POST"})).await, vec!["req_post"]);
    assert_eq!(ids(json!({"query": "LOOSE"})).await, vec!["req_loose"]);
    assert_eq!(
        ids(json!({"collection": "Nope"})).await,
        Vec::<String>::new()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_requests_history_rows_add_time_status_and_duration_and_no_bodies() {
    let (world, id) = sent_world().await;
    let out = call(
        &ListRequestsTool::new(),
        &world,
        json!({"source": "history"}),
    )
    .await;
    let value = json_of(&out);
    let row = &value["rows"][0];
    assert_eq!(row["history_id"], id);
    assert_eq!(row["status"], 200);
    assert!(row["duration_ms"].is_number() && row["time"].is_string());
    assert_eq!(row["source"], "agent");
    for body in ["INV-1001", "ORD-77", "4111-1111"] {
        assert!(!text(&out).contains(body));
    }
    let filtered = call(
        &ListRequestsTool::new(),
        &world,
        json!({"source": "history", "method": "GET"}),
    )
    .await;
    assert_eq!(text(&filtered), "No history entries match.");
    let bad = call(
        &ListRequestsTool::new(),
        &world,
        json!({"source": "history", "collection": "x"}),
    )
    .await;
    assert!(bad.is_error);
}

#[tokio::test(flavor = "multi_thread")]
async fn query_gateway_log_returns_columns_without_bodies_headers_or_keys() {
    let world = World::new().await;
    add_provider(&world.state, "p1", "http://127.0.0.1:9", &["m1"]);
    log_row(&world, |entry| {
        entry.source = "gateway".to_owned();
        entry.provider_id = Some("p1".to_owned());
        entry.provider_name = Some("Relay p1".to_owned());
        entry.upstream_model = Some("m1".to_owned());
        entry.client_name = Some("Cursor".to_owned());
        entry.requested_model = "Relay p1/m1".to_owned();
        entry.protocol_out = Some("anthropic".to_owned());
        entry.ttft_ms = Some(310);
        entry.input_tokens = Some(12);
        entry.output_tokens = Some(34);
        entry.cost_micro_usd = Some(1_500);
    });
    let out = call(&QueryGatewayLogTool::new(), &world, json!({})).await;
    let value = json_of(&out);
    let row = &value["rows"][0];
    assert_eq!(row["client"], "Cursor");
    assert_eq!(row["requested_model"], "Relay p1/m1");
    assert_eq!(row["served"], "Relay p1/m1");
    assert_eq!(row["protocol"], "openai_chat -> anthropic");
    assert_eq!(row["status"], 200);
    assert_eq!(row["ttft_ms"], 310);
    assert_eq!(row["input_tokens"], 12);
    assert_eq!(row["output_tokens"], 34);
    assert_eq!(row["cost"], "0.001500 USD");
    let columns: Vec<&str> = row
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    for forbidden in ["body", "header", "key", "response", "capture", "prompt"] {
        assert!(
            !columns.iter().any(|column| column.contains(forbidden)),
            "{forbidden} in {columns:?}"
        );
    }
    assert_eq!(value["view"]["kind"], "gateway_log");
}

#[tokio::test(flavor = "multi_thread")]
async fn query_gateway_log_filters_by_status_class_client_model_text_and_provider() {
    let world = World::new().await;
    add_provider(&world.state, "p1", "http://127.0.0.1:9", &["m1"]);
    add_provider(&world.state, "p2", "http://127.0.0.1:9", &["m2"]);
    log_row(&world, |e| {
        e.status = 200;
        e.client_name = Some("Cursor".into());
        e.requested_model = "Relay p1/fast-model".into();
        e.provider_id = Some("p1".into());
    });
    log_row(&world, |e| {
        e.status = 429;
        e.client_name = Some("Cline".into());
        e.provider_id = Some("p1".into());
        e.error = Some("rate limited by upstream".into());
    });
    log_row(&world, |e| {
        e.status = 502;
        e.client_name = Some("Cline".into());
        e.provider_id = Some("p2".into());
        e.error = Some("bad gateway".into());
    });
    let tool = QueryGatewayLogTool::new();
    let statuses = |value: &serde_json::Value| {
        value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["status"].as_u64().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        statuses(&json_of(
            &call(&tool, &world, json!({"status": "4xx"})).await
        )),
        vec![429]
    );
    assert_eq!(
        statuses(&json_of(&call(&tool, &world, json!({"status": 502})).await)),
        vec![502]
    );
    assert_eq!(
        statuses(&json_of(
            &call(&tool, &world, json!({"status": "5xx"})).await
        )),
        vec![502]
    );
    assert_eq!(
        statuses(&json_of(
            &call(&tool, &world, json!({"client": "cline"})).await
        )),
        vec![502, 429]
    );
    // `model` matches the requested name; the retired `alias` is read as `model`.
    for args in [json!({"model": "FAST"}), json!({"alias": "fast-model"})] {
        assert_eq!(
            statuses(&json_of(&call(&tool, &world, args).await)),
            vec![200]
        );
    }
    assert_eq!(
        statuses(&json_of(
            &call(&tool, &world, json!({"text": "rate limited"})).await
        )),
        vec![429]
    );
    assert_eq!(
        statuses(&json_of(
            &call(&tool, &world, json!({"provider": "Relay p2"})).await
        )),
        vec![502]
    );
    let unknown = call(&tool, &world, json!({"provider": "Nope"})).await;
    assert!(
        unknown.is_error
            && text(&unknown).contains("No provider \"Nope\". Available: Relay p1, Relay p2."),
        "{}",
        text(&unknown)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn query_gateway_log_masks_secrets_inside_error_text_and_skips_other_sources() {
    let world = World::new().await;
    log_row(&world, |e| {
        e.status = 401;
        e.error = Some(format!(
            "upstream said Bearer sk-abcdefghijklmnop1234 and {TOKEN}"
        ));
    });
    log_row(&world, |e| {
        e.source = "probe".to_owned();
        e.status = 599;
    });
    let out = call(&QueryGatewayLogTool::new(), &world, json!({})).await;
    let shown = text(&out);
    assert!(
        !shown.contains("sk-abcdefghijklmnop1234") && !shown.contains(TOKEN),
        "{shown}"
    );
    let value = json_of(&out);
    assert_eq!(value["shown"], 1, "probe rows are not gateway traffic");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_workbench_read_tools_show_names_and_references_but_never_a_secret_value() {
    let world = World::new().await;
    {
        let store = world.state.store();
        let id = |id: &str| crate::workspace::RequestId::new(id).unwrap();
        let workspace = crate::workspace::WorkspaceId::new(WS_A).unwrap();
        let mut request = store
            .get_request(&workspace, &id("req_inv"))
            .unwrap()
            .unwrap();
        let row = |key: &str, value: &str| crate::workspace::KeyValue {
            key: key.to_owned(),
            value: value.to_owned(),
            enabled: true,
            description: None,
        };
        request.headers.push(row("X-Api-Key", "literal-key-0042"));
        request.headers.push(row("Accept", "application/json"));
        request.query_params = vec![row("api_key", "literal-q-0042"), row("page", "2")];
        let tail = "x".repeat(GET_REQUEST_BODY_BYTES);
        request.body = crate::workspace::RequestBody::Json {
            content: format!("{{\"card\":\"{TOKEN}\",\"pad\":\"{tail}\"}}"),
        };
        request.assertions = vec![crate::workspace::models::Assertion::StatusEquals { value: 200 }];
        store.save_request(&request).unwrap();
    }
    let by_id = call(
        &GetRequestTool::new(),
        &world,
        json!({"request_id": "req_inv"}),
    )
    .await;
    let by_path = call(
        &GetRequestTool::new(),
        &world,
        json!({"request_path": "Billing API/Invoices/GET /invoices"}),
    )
    .await;
    let environments = call(&ListEnvironmentsTool::new(), &world, json!({})).await;
    let other = call(
        &GetRequestTool::new(),
        &world,
        json!({"request_id": "req_other"}),
    )
    .await;
    for (label, output) in [
        ("by id", &by_id),
        ("by path", &by_path),
        ("environments", &environments),
    ] {
        let shown = text(output);
        for secret in [TOKEN, "literal-key-0042", "literal-q-0042"] {
            assert!(!shown.contains(secret), "{label}: {secret} leaked");
        }
    }
    assert_eq!(text(&by_id), text(&by_path));
    let shown = text(&by_id);
    let (head, body) = shown
        .split_once("\n\nBody (json, ")
        .expect("the body follows");
    let value: serde_json::Value = serde_json::from_str(head).unwrap();
    assert_eq!(
        value["headers"][0],
        json!({"name": "Authorization", "value": "Bearer {{token}}"})
    );
    assert_eq!(
        value["headers"][1],
        json!({"name": "X-Api-Key", "value_hidden": true})
    );
    assert_eq!(value["headers"][2]["value"], "application/json");
    assert_eq!(
        value["query"][0],
        json!({"name": "api_key", "value_hidden": true})
    );
    assert_eq!(value["url"], "{{base_url}}/invoices");
    assert_eq!(
        (value["folder"].as_str(), value["agent_touched"].as_bool()),
        (Some("Billing API/Invoices"), Some(false))
    );
    assert_eq!(
        value["assertions"][0],
        json!({"StatusEquals": {"value": 200}})
    );
    assert!(
        body.contains("{{secret:token}}"),
        "a known secret value is masked"
    );
    // Past the 8 KiB the model reads, the rest stays behind the runner's read_output handle.
    let head_len = head.len() + "\n\nBody (json, ".len() + body.find(":\n").unwrap() + 2;
    assert_eq!(by_id.cap, Some(head_len + GET_REQUEST_BODY_BYTES));
    let env = json_of(&environments);
    let named = |name: &str| {
        env["environments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .cloned()
            .unwrap()
    };
    let (staging, production) = (named("Staging"), named("Production"));
    assert_eq!(
        (staging["bound"].as_bool(), staging["guarded"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(
        (
            production["bound"].as_bool(),
            production["guarded"].as_bool()
        ),
        (Some(false), Some(true))
    );
    assert_eq!(
        staging["variables"],
        json!([{"name": "base_url", "secret": false}, {"name": "token", "secret": true}])
    );
    assert!(
        staging["declared_hosts"]
            .as_array()
            .unwrap()
            .contains(&json!(world.host()))
    );
    assert!(
        !text(&environments).contains("base_url\",\"value"),
        "no value, not even a plain one"
    );
    assert!(
        other.is_error && text(&other).contains("No saved request req_other"),
        "another workspace is out of reach"
    );
    for tool in [
        &GetRequestTool::new() as &dyn AgentTool,
        &ListEnvironmentsTool::new(),
    ] {
        assert_eq!(
            (tool.spec().class, tool.spec().read_only),
            (ToolClass::PingDeckRead, true)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn history_pages_by_cursor_and_filters_by_status_and_origin() {
    let world = World::with(|seen| {
        let code = if seen.path.contains("bad") { 500 } else { 200 };
        crate::llm::probe::test_support::Reply::json(code, &json!({"ok": code == 200}))
    })
    .await;
    for path in ["a", "b", "bad"] {
        let url = format!("{{{{base_url}}}}/{path}");
        json_of(
            &call(
                &SendRequestTool::new(),
                &world,
                json!({"draft": {"url": url}}),
            )
            .await,
        );
    }
    let tool = ListRequestsTool::new();
    let first = json_of(&call(&tool, &world, json!({"source": "history", "limit": 2})).await);
    assert_eq!(
        (first["shown"].as_u64(), first["has_more"].as_bool()),
        (Some(2), Some(true))
    );
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = json_of(
        &call(
            &tool,
            &world,
            json!({"source": "history", "limit": 2, "cursor": cursor}),
        )
        .await,
    );
    assert_eq!(
        (second["shown"].as_u64(), second["has_more"].as_bool()),
        (Some(1), Some(false))
    );
    assert!(second.get("next_cursor").is_none());
    let ids: Vec<&serde_json::Value> = first["rows"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["rows"].as_array().unwrap())
        .map(|row| &row["history_id"])
        .collect();
    assert_eq!(ids.len(), 3);
    assert!(
        ids.iter()
            .all(|id| ids.iter().filter(|other| other == &id).count() == 1),
        "no row twice"
    );
    let failed = json_of(
        &call(
            &tool,
            &world,
            json!({"source": "history", "status": "5xx", "origin": "agent", "since": "1h"}),
        )
        .await,
    );
    assert_eq!(failed["rows"][0]["status"], 500);
    assert_eq!(failed["shown"], 1);
    let none = call(
        &tool,
        &world,
        json!({"source": "history", "origin": "flow"}),
    )
    .await;
    assert_eq!(text(&none), "No history entries match.");
    for bad in [
        json!({"source": "history", "cursor": "nope"}),
        json!({"status": "4xx"}),
    ] {
        assert!(call(&tool, &world, bad).await.is_error);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_history_page_over_the_size_cap_ends_early_and_the_cursor_skips_no_row() {
    let world = World::new().await;
    let long = format!("https://api.test/{}", "p".repeat(600));
    let request =
        crate::workspace::models::Request::new("req_inv", WS_A, "GET long", &long).unwrap();
    for n in 0..50 {
        let entry = crate::workspace::ExecutionHistoryEntry::from_success(
            format!("hist_long_{n:02}"),
            &request,
            None,
            200,
            "OK",
            &long,
            5,
            "body",
            4,
            1,
            format!("2026-11-07T00:00:{n:02}Z"),
        )
        .unwrap();
        world.state.store().save_execution_history(&entry).unwrap();
    }
    let tool = ListRequestsTool::new();
    let mut seen = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..50 {
        let mut args = json!({"source": "history", "limit": 50});
        if let Some(cursor) = &cursor {
            args["cursor"] = json!(cursor);
        }
        let out = call(&tool, &world, args).await;
        assert!(!text(&out).contains("[truncated"), "{}", text(&out));
        let page = json_of(&out);
        for row in page["rows"].as_array().unwrap() {
            assert!(seen.insert(row["history_id"].as_str().unwrap().to_owned()));
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 50, "every row on some page");
}
