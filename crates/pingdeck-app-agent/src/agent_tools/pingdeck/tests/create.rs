//! `create_request`: a saved request is created or updated without sending, marked agent-touched,
//! validated like the UI and never carries a secret literal.

use serde_json::{Value, json};

use super::super::{CreateRequestTool, ListRequestsTool};
use super::support::{TOKEN, WS_A, WS_B, World, call, json_of, text};
use crate::workspace::models::{HttpMethod, RequestBody};
use crate::workspace::{RequestId, WorkspaceId};

const SECRET_MESSAGE: &str = "Put secrets in an environment variable and reference it as {{name}}.";

fn new_request() -> Value {
    json!({
        "collection": "Billing API", "folder": "Invoices/Refunds", "name": "POST /refunds",
        "method": "POST", "url": "{{base_url}}/refunds",
        "headers": [{"name": "Content-Type", "value": "application/json"}],
        "body": {"type": "json", "content": "{\"amount\": 5}"},
        "description": "Refund an invoice"
    })
}

fn stored(world: &World, id: &str) -> crate::workspace::models::Request {
    world
        .state
        .store()
        .get_request(
            &WorkspaceId::new(WS_A).unwrap(),
            &RequestId::new(id).unwrap(),
        )
        .unwrap()
        .expect("the request is stored")
}

async fn refused(world: &World, args: Value) -> String {
    let before = world.state.store().list_requests(WS_A).unwrap().len();
    let out = call(&CreateRequestTool::new(), world, args).await;
    assert!(out.is_error, "must be refused: {}", text(&out));
    assert_eq!(
        world.state.store().list_requests(WS_A).unwrap().len(),
        before,
        "nothing was saved"
    );
    text(&out)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_is_created_unsent_with_its_folders_and_the_agent_marker() {
    let world = World::new().await;
    let value = json_of(&call(&CreateRequestTool::new(), &world, new_request()).await);
    let id = value["request_id"].as_str().unwrap().to_owned();
    assert_eq!(
        value["message"],
        format!("Created POST /refunds ({id}) in Billing API/Invoices/Refunds")
    );
    assert_eq!(
        value["view"],
        json!({"kind": "saved_request", "request_id": id, "workspace_id": WS_A})
    );
    assert_eq!(value["workspace"]["name"], "Billing platform");
    let request = stored(&world, &id);
    assert_eq!(request.method, HttpMethod::Post);
    assert_eq!(request.url, "{{base_url}}/refunds");
    assert_eq!(request.description, "Refund an invoice");
    assert!(
        matches!(request.body, RequestBody::Json { ref content } if content == "{\"amount\": 5}")
    );
    assert!(world.fake.requests().is_empty(), "creating does not send");
    assert!(world.history().is_empty());
    let touched = world.state.store().agent_touched_request_ids(WS_A).unwrap();
    assert!(touched.contains(&id) && touched.len() == 1);
    let listed = json_of(
        &call(
            &ListRequestsTool::new(),
            &world,
            json!({"query": "refunds"}),
        )
        .await,
    );
    assert_eq!(listed["rows"][0]["agent_touched"], true);
    assert_eq!(listed["rows"][0]["folder"], "Billing API/Invoices/Refunds");
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_folders_are_reused_and_only_missing_ones_are_created() {
    let world = World::new().await;
    let tool = CreateRequestTool::new();
    let folders = |world: &World| {
        let store = world.state.store();
        store
            .list_folders(
                &WorkspaceId::new(WS_A).unwrap(),
                &crate::workspace::models::CollectionId::new("col_a").unwrap(),
            )
            .unwrap()
            .len()
    };
    assert_eq!(folders(&world), 1);
    json_of(&call(&tool, &world, json!({"collection": "Billing API", "folder": "Invoices", "name": "A", "url": "{{base_url}}/a"})).await);
    assert_eq!(folders(&world), 1, "Invoices exists");
    json_of(&call(&tool, &world, json!({"collection": "billing api", "folder": "invoices/Refunds/2026", "name": "B", "url": "{{base_url}}/b"})).await);
    assert_eq!(
        folders(&world),
        3,
        "Refunds and 2026 were created below Invoices"
    );
    json_of(
        &call(
            &tool,
            &world,
            json!({"collection": "Billing API", "name": "Top", "url": "{{base_url}}/t"}),
        )
        .await,
    );
    assert_eq!(folders(&world), 3, "no folder for a top-level request");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_literal_secret_in_a_header_is_refused_with_the_prd_text() {
    let world = World::new().await;
    for headers in [
        json!([{"name": "Authorization", "value": "Bearer sk-live-abcdefghijklmnop1234"}]),
        json!([{"name": "Authorization", "value": "Basic dXNlcjpwYXNz"}]),
        json!([{"name": "x-api-key", "value": "abc123"}]),
        json!([{"name": "Cookie", "value": "sid=abc"}]),
        json!([{"name": "X-Custom", "value": "ghp_abcdefghijklmnopqrstuvwxyz0123456789"}]),
    ] {
        let mut args = new_request();
        args["headers"] = headers;
        assert_eq!(refused(&world, args).await, SECRET_MESSAGE);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_that_is_only_a_variable_reference_is_accepted() {
    let world = World::new().await;
    let mut args = new_request();
    args["headers"] = json!([
        {"name": "Authorization", "value": "Bearer {{token}}"},
        {"name": "x-api-key", "value": "{{api_key}}"},
    ]);
    args["query"] = json!([{"name": "api_key", "value": "{{token}}"}]);
    let value = json_of(&call(&CreateRequestTool::new(), &world, args).await);
    let request = stored(&world, value["request_id"].as_str().unwrap());
    assert_eq!(request.headers[0].value, "Bearer {{token}}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_literal_secret_in_a_query_url_body_or_description_is_refused() {
    let world = World::new().await;
    let cases: [(&str, Value); 6] = [
        (
            "query",
            json!({"query": [{"name": "token", "value": "abc123def"}]}),
        ),
        (
            "url token",
            json!({"url": "https://api.test/x?access_token=abc123def"}),
        ),
        (
            "url password",
            json!({"url": "https://user:hunter2@api.test/x"}),
        ),
        (
            "known secret value in the body",
            json!({"body": {"type": "text", "content": format!("t={TOKEN}")}}),
        ),
        (
            "known secret value in the url",
            json!({"url": format!("https://api.test/{TOKEN}")}),
        ),
        (
            "known secret value in the description",
            json!({"description": format!("use {TOKEN}")}),
        ),
    ];
    for (label, patch) in cases {
        let mut args = new_request();
        for (key, value) in patch.as_object().unwrap() {
            args[key] = value.clone();
        }
        assert_eq!(refused(&world, args).await, SECRET_MESSAGE, "{label}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_request_is_refused_like_the_ui_would() {
    let world = World::new().await;
    let tool = CreateRequestTool::new();
    let mut blank_name = new_request();
    blank_name["name"] = json!("   ");
    assert!(
        refused(&world, blank_name)
            .await
            .contains("`name` must not be blank")
    );
    let mut blank_url = new_request();
    blank_url["url"] = json!("");
    assert!(
        refused(&world, blank_url)
            .await
            .contains("`url` must not be blank")
    );
    let mut no_collection = new_request();
    no_collection["collection"] = json!("Nope");
    assert_eq!(
        refused(&world, no_collection).await,
        "No collection \"Nope\". Available: Billing API."
    );
    let mut bad_form = new_request();
    bad_form["body"] = json!({"type": "form", "content": "a=b"});
    assert!(
        refused(&world, bad_form)
            .await
            .contains("list of {name, value}")
    );
    let _ = tool;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_over_256_kb_is_refused() {
    let world = World::new().await;
    let mut args = new_request();
    args["body"] = json!({"type": "text", "content": "a".repeat(256 * 1024 + 1)});
    assert!(refused(&world, args.clone()).await.contains("256 KB"));
    args["body"] = json!({"type": "text", "content": "a".repeat(256 * 1024)});
    assert!(!call(&CreateRequestTool::new(), &world, args).await.is_error);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_changes_the_named_fields_keeps_the_rest_and_marks_the_request() {
    let world = World::new().await;
    let tool = CreateRequestTool::new();
    assert!(
        world
            .state
            .store()
            .agent_touched_request_ids(WS_A)
            .unwrap()
            .is_empty()
    );
    let value = json_of(
        &call(
            &tool,
            &world,
            json!({"request_id": "req_inv", "method": "POST", "name": "Create invoice"}),
        )
        .await,
    );
    assert_eq!(
        value["message"],
        "Updated Create invoice (req_inv): name, method"
    );
    let request = stored(&world, "req_inv");
    assert_eq!(request.method, HttpMethod::Post);
    assert_eq!(request.name, "Create invoice");
    assert_eq!(
        request.url, "{{base_url}}/invoices",
        "the URL was not named, so it stays"
    );
    assert_eq!(request.headers.len(), 1, "headers stay");
    assert_eq!(
        request.folder_id.as_ref().map(|id| id.as_str()),
        Some("fld_inv"),
        "placement stays"
    );
    assert!(
        world
            .state
            .store()
            .agent_touched_request_ids(WS_A)
            .unwrap()
            .contains("req_inv")
    );
    assert!(world.fake.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_can_move_a_request_to_another_folder() {
    let world = World::new().await;
    let value = json_of(
        &call(
            &CreateRequestTool::new(),
            &world,
            json!({"request_id": "req_inv", "collection": "Billing API", "folder": "Archive"}),
        )
        .await,
    );
    assert!(
        value["message"].as_str().unwrap().contains("location"),
        "{value}"
    );
    let request = stored(&world, "req_inv");
    assert_ne!(
        request.folder_id.as_ref().map(|id| id.as_str()),
        Some("fld_inv")
    );
    assert!(request.folder_id.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_cannot_reach_a_request_of_another_workspace() {
    let world = World::new().await;
    let out = call(
        &CreateRequestTool::new(),
        &world,
        json!({"request_id": "req_other", "name": "Hijacked"}),
    )
    .await;
    assert!(
        out.is_error
            && text(&out).contains("No saved request req_other in workspace \"Billing platform\""),
        "{}",
        text(&out)
    );
    let other = world
        .state
        .store()
        .get_request(
            &WorkspaceId::new(WS_B).unwrap(),
            &RequestId::new("req_other").unwrap(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(other.name, "GET /secret");
    assert!(
        world
            .state
            .store()
            .agent_touched_request_ids(WS_B)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_refuses_a_secret_literal_and_leaves_the_stored_request_alone() {
    let world = World::new().await;
    let out = call(
        &CreateRequestTool::new(),
        &world,
        json!({"request_id": "req_inv", "headers": [{"name": "Authorization", "value": "Bearer sk-abcdefghijklmnop1234"}]}),
    )
    .await;
    assert!(out.is_error && text(&out) == SECRET_MESSAGE);
    assert_eq!(
        stored(&world, "req_inv").headers[0].value,
        "Bearer {{token}}"
    );
    assert!(
        world
            .state
            .store()
            .agent_touched_request_ids(WS_A)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_without_a_workspace_creates_nothing() {
    let world = World::new().await;
    world
        .state
        .store()
        .save_agent_session(&super::support::session("sess_unbound"))
        .unwrap();
    let ctx = world.ctx_in("sess_unbound", "run_1");
    let out =
        crate::agent::tools::AgentTool::call(&CreateRequestTool::new(), &ctx, new_request()).await;
    assert!(out.is_error && text(&out).contains("not bound to a PingDeck workspace"));
    assert_eq!(world.state.store().list_requests(WS_A).unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_patches_rows_lists_what_it_deletes_and_shows_references_in_the_diff() {
    let world = World::new().await;
    let tool = CreateRequestTool::new();
    {
        let mut request = stored(&world, "req_inv");
        let store = world.state.store();
        request.headers.push(crate::workspace::KeyValue {
            key: "X-Trace".to_owned(),
            value: "on".to_owned(),
            enabled: true,
            description: None,
        });
        request.body = RequestBody::Json {
            content: format!("{}1", "a".repeat(3_000)),
        };
        store.save_request(&request).unwrap();
    }
    let patch = json!({
        "request_id": "req_inv", "collection": "Billing API",
        "headers_add": [{"name": "accept", "value": "application/json"}],
        "headers_remove": ["x-trace"],
        "body": {"type": "json", "content": format!("{}2", "a".repeat(3_000))},
    });
    let diff = match super::support::inspect(&tool, &world, patch.clone())
        .unwrap()
        .preview
    {
        Some(crate::agent::tools::ToolApprovalPreview::Diff { unified_diff, .. }) => unified_diff,
        other => panic!("a diff preview, got {other:?}"),
    };
    assert!(
        diff.contains("@@ headers @@\n-X-Trace: on\n+accept: application/json\n"),
        "{diff}"
    );
    assert!(
        diff.contains("@@ body @@"),
        "a change past the shown characters still counts"
    );
    assert!(
        !diff.contains("Bearer"),
        "unchanged rows stay out of the diff"
    );
    let value = json_of(&call(&tool, &world, patch).await);
    assert_eq!(
        value["removed"],
        json!({"headers": ["X-Trace"], "query": []})
    );
    let request = stored(&world, "req_inv");
    let headers: Vec<(&str, &str)> = request
        .headers
        .iter()
        .map(|h| (h.key.as_str(), h.value.as_str()))
        .collect();
    assert_eq!(
        headers,
        [
            ("Authorization", "Bearer {{token}}"),
            ("accept", "application/json")
        ]
    );
    assert_eq!(
        request.folder_id.as_ref().map(|id| id.as_str()),
        Some("fld_inv"),
        "naming its own collection keeps the folder"
    );
    // A whole-list replacement deletes the rows it leaves out; the diff shows the reference.
    let replace = json!({"request_id": "req_inv", "headers": [{"name": "Authorization", "value": "Bearer {{prod_token}}"}]});
    let diff = match super::support::inspect(&tool, &world, replace.clone())
        .unwrap()
        .preview
    {
        Some(crate::agent::tools::ToolApprovalPreview::Diff { unified_diff, .. }) => unified_diff,
        other => panic!("a diff preview, got {other:?}"),
    };
    assert!(diff.contains("-Authorization: Bearer {{token}}\n-accept: application/json\n+Authorization: Bearer {{prod_token}}\n"), "{diff}");
    let value = json_of(&call(&tool, &world, replace).await);
    assert_eq!(value["removed"]["headers"], json!(["accept"]));
    for (bad, message) in [
        (
            json!({"request_id": "req_inv", "headers": [], "headers_add": [{"name": "a", "value": "b"}]}),
            "not both",
        ),
        (
            json!({"request_id": "req_inv", "query_remove": ["nope"]}),
            "No query parameter \"nope\" to remove.",
        ),
    ] {
        let out = call(&tool, &world, bad).await;
        assert!(
            out.is_error && text(&out).contains(message),
            "{}",
            text(&out)
        );
    }
}

/// Folders of workspace A (the store lock is taken once per call, never nested).
fn folder_count(world: &World) -> usize {
    let store = world.state.store();
    store
        .list_collections(WS_A)
        .unwrap()
        .iter()
        .map(|collection| {
            store
                .list_folders(&WorkspaceId::new(WS_A).unwrap(), &collection.id)
                .unwrap()
                .len()
        })
        .sum()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_created_request_is_in_the_change_journal_and_undo_removes_it_with_its_new_folders() {
    let world = World::new().await;
    let folders_before = folder_count(&world);
    let value = json_of(&call(&CreateRequestTool::new(), &world, new_request()).await);
    let id = value["request_id"].as_str().unwrap().to_owned();
    assert!(value.get("undo").is_none(), "the write was journaled");

    let rows = crate::agent_changes_list_impl(&world.state, super::support::SESSION).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0]
            .path
            .ends_with("Billing API/Invoices/Refunds/POST /refunds")
    );
    assert_eq!(rows[0].kind, crate::agent::model::FileChangeKind::Create);
    let diff = crate::agent_change_diff_impl(&world.state, &rows[0].id).unwrap();
    assert!(
        diff.unified_diff
            .contains("+  \"url\": \"{{base_url}}/refunds\"")
    );

    let undone = crate::agent_change_undo_impl(&world.state, &[rows[0].id.clone()], false).unwrap();
    assert_eq!(
        undone[0].status,
        crate::agent::model::FileChangeStatus::Undone
    );
    assert!(
        world
            .state
            .store()
            .get_request(
                &WorkspaceId::new(WS_A).unwrap(),
                &RequestId::new(&id).unwrap()
            )
            .unwrap()
            .is_none()
    );
    let folders_after = folder_count(&world);
    assert_eq!(
        folders_after, folders_before,
        "the folders it created went too"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn undoing_an_update_restores_the_request_and_refuses_when_it_changed_since() {
    let world = World::new().await;
    let created = json_of(&call(&CreateRequestTool::new(), &world, new_request()).await);
    let id = created["request_id"].as_str().unwrap().to_owned();
    // The user owns it from here: their send cleared the marker.
    world
        .state
        .store()
        .clear_request_agent_touched(WS_A, &id)
        .unwrap();
    let update = || json!({"request_id": id, "url": "{{base_url}}/refunds/v2"});
    json_of(&call(&CreateRequestTool::new(), &world, update()).await);
    let latest = |world: &World| {
        crate::agent_changes_list_impl(&world.state, super::support::SESSION).unwrap()[0].clone()
    };
    let row = latest(&world);
    assert_eq!(row.kind, crate::agent::model::FileChangeKind::Edit);

    crate::agent_change_undo_impl(&world.state, std::slice::from_ref(&row.id), false).unwrap();
    let restored = stored(&world, &id);
    assert_eq!(restored.url, "{{base_url}}/refunds");
    assert!(
        !world
            .state
            .store()
            .agent_touched_request_ids(WS_A)
            .unwrap()
            .contains(&id),
        "the marker is back to what it was before the update"
    );

    json_of(&call(&CreateRequestTool::new(), &world, update()).await);
    let row = latest(&world);
    let mut edited = stored(&world, &id);
    edited.description = "changed by the user".into();
    world.state.store().save_request(&edited).unwrap();
    let touched =
        crate::agent_change_undo_impl(&world.state, std::slice::from_ref(&row.id), false).unwrap();
    assert_eq!(
        touched[0].status,
        crate::agent::model::FileChangeStatus::Conflict
    );
    assert_eq!(stored(&world, &id).description, "changed by the user");
}
