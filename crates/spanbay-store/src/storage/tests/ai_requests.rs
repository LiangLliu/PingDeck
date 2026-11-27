//! Storage of `requests.mode` / `requests.ai_json` (AI API mode).

use super::super::WorkspaceStore;
use crate::storage::StorageError;
use spanbay_model::ai_request::{
    AiRequestConfig, AiTarget, CompareModel, RequestMode, non_routable_label,
};
use spanbay_model::models::Assertion;
use spanbay_model::validation::WorkspaceError;
use spanbay_model::wire::Protocol;
use spanbay_model::{Request, Workspace};

fn store() -> WorkspaceStore {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store
        .save_workspace(&Workspace::new("ws", "W").unwrap())
        .unwrap();
    store
}

fn ai_request() -> Request {
    let mut request = Request::new(
        "req_ai",
        "ws",
        "Ask",
        non_routable_label("DeepSeek", "/chat/completions"),
    )
    .unwrap();
    request.mode = RequestMode::AiApi;
    request.ai = Some(AiRequestConfig {
        protocol: Protocol::OpenaiChat,
        target: AiTarget::Provider {
            provider_id: "prov_1".to_owned(),
            provider_name: "DeepSeek".to_owned(),
            base_url_hint: "https://api.deepseek.com".to_owned(),
            model: "deepseek-chat".to_owned(),
        },
        builder: Some(serde_json::json!({"messages": [{"role": "user", "text": "ping"}]})),
        compare_models: vec![CompareModel {
            provider_id: "prov_2".to_owned(),
            model: "other".to_owned(),
        }],
    });
    request.assertions = vec![
        Assertion::AiReplyContains {
            value: "pong".to_owned(),
        },
        Assertion::AiNoError {},
    ];
    request
}

fn columns(store: &WorkspaceStore, id: &str) -> (String, Option<String>) {
    store
        .connection
        .query_row(
            "SELECT mode, ai_json FROM requests WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn ai_requests_round_trip_clear_on_http_and_validate_mode() {
    let store = store();
    let mut request = ai_request();
    store.save_request(&request).unwrap();
    assert_eq!(
        store
            .get_request(&request.workspace_id, &request.id)
            .unwrap(),
        Some(request.clone())
    );
    assert_eq!(store.list_requests("ws").unwrap(), vec![request.clone()]);
    let (mode, ai_json) = columns(&store, "req_ai");
    assert_eq!(mode, "ai_api");
    assert!(!ai_json.unwrap().contains("api_key"));

    request.ai.as_mut().unwrap().builder = None;
    store.save_request(&request).unwrap();
    assert_eq!(
        store
            .get_request(&request.workspace_id, &request.id)
            .unwrap()
            .unwrap()
            .ai
            .unwrap()
            .builder,
        None
    );

    request.mode = RequestMode::Http;
    store.save_request(&request).unwrap();
    assert_eq!(columns(&store, "req_ai"), ("http".to_owned(), None));

    let mut no_settings = ai_request();
    no_settings.ai = None;
    assert!(matches!(
        store.save_request(&no_settings),
        Err(StorageError::Workspace(
            WorkspaceError::InvalidAiRequest { .. }
        ))
    ));
    let mut no_model = ai_request();
    if let Some(AiTarget::Provider { model, .. }) = no_model.ai.as_mut().map(|ai| &mut ai.target) {
        model.clear();
    }
    assert!(matches!(
        store.save_request(&no_model),
        Err(StorageError::Workspace(
            WorkspaceError::InvalidAiRequest { .. }
        ))
    ));
    store
        .connection
        .execute(
            "INSERT INTO requests (id, workspace_id, name, protocol, method, url)
         VALUES ('old', 'ws', 'Old', 'http_rest', 'GET', 'https://example.com')",
            [],
        )
        .unwrap();
    let old = store
        .get_request(
            &request.workspace_id,
            &spanbay_model::RequestId::new("old").unwrap(),
        )
        .unwrap()
        .unwrap();
    assert_eq!((old.mode, old.ai), (RequestMode::Http, None));

    store
        .connection
        .execute("UPDATE requests SET mode = 'telepathy'", [])
        .unwrap();
    assert!(
        store
            .list_requests("ws")
            .unwrap_err()
            .to_string()
            .contains("unknown mode")
    );
}
