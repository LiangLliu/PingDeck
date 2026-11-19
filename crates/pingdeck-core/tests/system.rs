//! System use-cases through the shipped functions: app info, diagnostics, counts, the
//! aggregated AI read and revealing saved exports.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use pingdeck_core::agent::mcp::McpTransportConfig;
use pingdeck_core::agent::model::AgentMcpServer;
use pingdeck_core::agent::tools::ToolClass;
use pingdeck_core::app::{
    AppState, ProviderFromPresetInput, SaveCollectionInput, SaveEnvironmentInput, SaveRequestInput,
    SettingsSecret, ShellInfo, agent_mcp_server_save_impl, ai_config_get_impl,
    app_diagnostics_impl, app_info_impl, collection_save_impl, environment_save_impl,
    flow_save_impl, llm_gateway_client_create_impl, llm_provider_create_from_preset_impl,
    mock_server_save_impl, request_execute_draft_impl, request_save_impl, reveal_path_validate,
    settings_secret_set_impl, settings_update_impl, workspace_counts_impl, workspace_create_impl,
};
use pingdeck_core::error::AppError;
use pingdeck_core::llm::store::ClientInput;
use pingdeck_core::model::items::{
    MockResponse, MockResponseMode, MockRoute, MockRouteType, MockRulesOperator,
};
use pingdeck_core::model::settings::SettingsSection;
use pingdeck_core::model::{
    AuthConfig, Flow, HttpMethod, MockServer, NetworkOptions, ProtocolKind, RequestBody,
    WorkspaceId,
};

fn temp_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("pingdeck-{nanos}-{name}"))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("free port probe should bind")
        .local_addr()
        .expect("free port probe should have an address")
        .port()
}

fn draft_request_input(id: &str, workspace_id: &str, url: String) -> SaveRequestInput {
    SaveRequestInput {
        id: id.into(),
        workspace_id: workspace_id.into(),
        collection_id: None,
        folder_id: None,
        name: "Draft".into(),
        protocol: ProtocolKind::HttpRest,
        method: HttpMethod::Get,
        url,
        query_params: Vec::new(),
        path_variables: Vec::new(),
        headers: Vec::new(),
        auth: AuthConfig::None,
        body: RequestBody::None,
        network_options: NetworkOptions::default(),
        assertions: Vec::new(),
        description: String::new(),
        pre_request_script: String::new(),
        test_script: String::new(),
        position: 0,
        mode: Default::default(),
        ai: None,
        examples: None,
    }
}

fn mock_server_fixture(workspace_id: &str, port: u16) -> MockServer {
    MockServer {
        id: "mock_cmd".into(),
        workspace_id: WorkspaceId::new(workspace_id).expect("workspace id should be valid"),
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
            id: "route_hello".into(),
            method: "GET".into(),
            path: "/hello".into(),
            enabled: true,
            description: String::new(),
            route_type: MockRouteType::Http,
            data_bucket_id: String::new(),
            crud_id_key: "id".into(),
            response_mode: MockResponseMode::FirstMatch,
            responses: vec![MockResponse {
                id: "resp_hello".into(),
                label: "OK".into(),
                status_code: 200,
                headers: Vec::new(),
                body: r#"{"hello":"{{query.who}}"}"#.into(),
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

fn shell() -> ShellInfo {
    ShellInfo {
        app_version: "1.2.3".into(),
        webview_version: "WebKit 9".into(),
    }
}

/// A profile that holds one of everything private: a provider with a key, a client token, an MCP
/// bearer header, a search key and settings that name hosts, paths and a proxy password.
/// Returns the strings that must never appear in what leaves the machine.
fn private_profile(state: &AppState) -> Vec<String> {
    llm_provider_create_from_preset_impl(
        state,
        ProviderFromPresetInput {
            preset_id: "deepseek".into(),
            region_id: None,
            placeholders: BTreeMap::new(),
            name: None,
            secret: Some("sk-deepseek-very-secret-123456".into()),
        },
    )
    .unwrap();
    let client = llm_gateway_client_create_impl(
        state,
        ClientInput {
            name: "Laptop".into(),
            allow_all: true,
            allowed_aliases: Vec::new(),
            allowed_models: Vec::new(),
        },
    )
    .unwrap();
    agent_mcp_server_save_impl(
        state,
        AgentMcpServer {
            id: String::new(),
            name: "web".into(),
            transport: McpTransportConfig::Http {
                url: "https://mcp.corp.internal/mcp".into(),
                headers: BTreeMap::from([(
                    "Authorization".into(),
                    "Bearer mcp-secret-token-0123456789".into(),
                )]),
            },
            enabled: false,
            trusted: false,
            updated_at_ms: 0,
        },
        true,
    )
    .unwrap();
    settings_secret_set_impl(
        state,
        SettingsSecret::SearchTavily,
        "tvly-secret-key-9876543210",
    )
    .unwrap();
    settings_update_impl(
        state,
        SettingsSection::Network,
        &json!({
            "proxy_url": "http://user:pa55word@proxy.corp.internal:8080",
            "no_proxy": ["intranet.corp.internal"],
            "ca_bundle_path": "/Users/me/corp-ca.pem",
        }),
    )
    .unwrap();
    settings_update_impl(
        state,
        SettingsSection::Ai,
        &json!({
            "search": { "searxng_base_url": "https://search.corp.internal" },
            "agent": {
                "default_workspace": "client-acme-project",
                "skill_dirs": ["/Users/me/private-skills"],
                "blocked_commands": ["private-wipe *"],
            },
            "providers": { "default_test_prompt": "my confidential prompt" },
        }),
    )
    .unwrap();
    settings_update_impl(
        state,
        SettingsSection::Sync,
        &json!({ "directory": "/Users/me/sync-secret" }),
    )
    .unwrap();
    [
        "sk-deepseek",
        "very-secret",
        &client.token,
        "mcp-secret-token",
        "tvly-secret",
        "pa55word",
        "proxy.corp",
        "intranet.corp",
        "corp-ca",
        "search.corp",
        "mcp.corp",
        "client-acme",
        "private-skills",
        "private-wipe",
        "sync-secret",
        "confidential",
        "deepseek",
        "Laptop",
    ]
    .map(str::to_owned)
    .into()
}

#[test]
fn app_info_reports_versions_places_and_a_working_secret_store() {
    let state = AppState::open_in_memory().unwrap();
    let info = app_info_impl(&state, &shell()).unwrap();
    assert_eq!(
        (info.app_version.as_str(), info.webview_version.as_str()),
        ("1.2.3", "WebKit 9")
    );
    assert_eq!(info.core_version, env!("CARGO_PKG_VERSION"));
    assert!(info.schema_version > 0);
    assert_eq!((info.data_dir.as_str(), info.db_path.as_str()), ("", ""));
    assert!(info.secret_store.ok && info.secret_store.error.is_none());
    assert!(!info.os.is_empty() && !info.arch.is_empty());

    let db = temp_path("info.sqlite3");
    let file_state = AppState::open(&db).unwrap();
    let on_disk = app_info_impl(&file_state, &shell()).unwrap();
    assert_eq!(
        on_disk.db_path,
        pingdeck_core::platform::normalize_display_path(&db)
    );
    assert_eq!(
        on_disk.data_dir,
        pingdeck_core::platform::normalize_display_path(db.parent().unwrap())
    );
}

#[test]
fn diagnostics_say_what_is_set_and_never_what_it_is() {
    let state = AppState::open_in_memory().unwrap();
    let private = private_profile(&state);
    let text = app_diagnostics_impl(&state, &shell()).unwrap();
    assert!(text.starts_with("PingDeck 1.2.3 (core "), "{text}");
    for line in [
        "Webview: WebKit 9",
        "Secret store: sqlite (ok)",
        "Enabled providers: 1",
        "Gateway: stopped",
        "\"timeout_ms\": 30000",
        "\"proxy_url\": \"<set>\"",
        "\"no_proxy\": \"<1 entries>\"",
        "\"default_workspace\": \"<set>\"",
        "\"theme\": \"system\"",
    ] {
        assert!(text.contains(line), "missing {line:?} in\n{text}");
    }
    let lower = text.to_lowercase();
    for needle in &private {
        assert!(
            !lower.contains(&needle.to_lowercase()),
            "{needle:?} leaked:\n{text}"
        );
    }
    assert!(!text.contains("://") && !text.contains('@'), "{text}");
}

#[test]
fn the_secret_store_probe_leaves_nothing_behind() {
    let state = AppState::open_in_memory().unwrap();
    app_info_impl(&state, &shell()).unwrap();
    assert!(!state.secrets().hint("app.health_probe").unwrap().has_secret);
}

#[tokio::test]
async fn workspace_counts_cover_each_kind_of_row_of_one_workspace() {
    let state = AppState::open_in_memory().unwrap();
    for id in ["ws_a", "ws_b"] {
        workspace_create_impl(&state, id.into(), id.into()).unwrap();
    }
    let empty = workspace_counts_impl(&state, "ws_a").unwrap();
    assert_eq!(
        empty,
        pingdeck_core::model::hierarchy::WorkspaceCounts::default()
    );

    collection_save_impl(
        &state,
        SaveCollectionInput {
            id: "col_a".into(),
            workspace_id: "ws_a".into(),
            name: "API".into(),
            description: String::new(),
            auth: AuthConfig::Inherit,
            variables: Vec::new(),
            pre_request_script: String::new(),
            test_script: String::new(),
            position: 0,
        },
    )
    .unwrap();
    for id in ["req_1", "req_2"] {
        let mut request = draft_request_input(id, "ws_a", "http://127.0.0.1:1/x".into());
        request.collection_id = Some("col_a".into());
        request_save_impl(&state, request).unwrap();
    }
    environment_save_impl(
        &state,
        SaveEnvironmentInput {
            id: "env_a".into(),
            workspace_id: "ws_a".into(),
            name: "Local".into(),
            variables: Vec::new(),
            confirm_before_send: None,
            confirm_safe_methods: None,
            declared_hosts: None,
            base_revision: None,
        },
    )
    .unwrap();
    mock_server_save_impl(&state, mock_server_fixture("ws_a", free_port())).unwrap();
    flow_save_impl(
        &state,
        Flow {
            id: "flow_a".into(),
            workspace_id: WorkspaceId::new("ws_a").unwrap(),
            name: "Checkout".into(),
            description: String::new(),
            steps: Vec::new(),
            updated_at_ms: 0,
            options: Default::default(),
        },
    )
    .unwrap();
    request_execute_draft_impl(
        &state,
        pingdeck_core::app::ExecuteDraftRequestInput {
            request: draft_request_input("req_draft", "ws_a", "http://127.0.0.1:1/x".into()),
            environment_id: None,
            confirmed: None,
            save_to_history: None,
        },
        None,
    )
    .await
    .unwrap_err();

    let counts = workspace_counts_impl(&state, "ws_a").unwrap();
    assert_eq!(
        (
            counts.collections,
            counts.requests,
            counts.environments,
            counts.mock_servers,
            counts.flows,
            counts.history_entries
        ),
        (1, 2, 1, 1, 1, 1)
    );
    assert_eq!(
        workspace_counts_impl(&state, "ws_b").unwrap(),
        pingdeck_core::model::hierarchy::WorkspaceCounts::default()
    );
    assert!(matches!(
        workspace_counts_impl(&state, "ws_missing"),
        Err(AppError::NotFound { .. })
    ));
    assert!(matches!(
        workspace_counts_impl(&state, "  "),
        Err(AppError::Validation { .. } | AppError::NotFound { .. })
    ));
}

#[tokio::test]
async fn the_ai_snapshot_lists_the_configuration_and_holds_no_secret() {
    let state = AppState::open_in_memory().unwrap();
    let private = private_profile(&state);
    let config = ai_config_get_impl(&state).await.unwrap();
    assert!(config.degraded.is_empty(), "{:?}", config.degraded);
    assert_eq!(config.providers.len(), 1);
    assert_eq!(config.keys.len(), 1);
    assert!(!config.keys[0].secret_hint.is_empty());
    assert_eq!(config.clients.len(), 1);
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(config.statuses.len(), 1);
    assert!(config.secret_store.ok);
    assert_eq!(
        config.blocked.len(),
        pingdeck_core::agent::permissions::PINNED_RULES.len() + 1,
        "the pinned rules and the added one"
    );
    assert_eq!(config.skill_folders.len(), 1);
    assert!(!config.skill_folders[0].scanned);
    assert!(config.last_seq <= state.llm.events().last_seq());
    // Every built-in tool, with the class and the Allow rule the Tools page shows.
    let read = config
        .tools
        .iter()
        .find(|tool| tool.info.name == "read_file")
        .expect("read_file is listed");
    assert_eq!(read.class, ToolClass::Read);
    assert!(read.allow_offered && !read.is_new);
    let shell = config
        .tools
        .iter()
        .find(|tool| tool.info.name == "shell")
        .expect("shell is listed");
    assert_eq!(shell.class, ToolClass::Exec);
    assert!(!shell.allow_offered, "a standing Allow is never offered");

    let text = serde_json::to_string(&config).unwrap();
    // Names and URLs of configured records are the page's own content; only secrets are
    // checked here: keys, tokens, the proxy password and the MCP bearer value.
    for secret in &private[..6] {
        assert!(!text.contains(secret.as_str()), "{secret:?} leaked: {text}");
    }
}

#[tokio::test]
async fn saved_exports_may_be_shown_and_other_files_may_not() {
    let state = AppState::open_in_memory().unwrap();
    workspace_create_impl(&state, "ws_x".into(), "X".into()).unwrap();
    collection_save_impl(
        &state,
        SaveCollectionInput {
            id: "col_x".into(),
            workspace_id: "ws_x".into(),
            name: "API".into(),
            description: String::new(),
            auth: AuthConfig::Inherit,
            variables: Vec::new(),
            pre_request_script: String::new(),
            test_script: String::new(),
            position: 0,
        },
    )
    .unwrap();
    let chosen = temp_path("export.postman_collection.json");
    let other = temp_path("other.json");
    std::fs::write(&other, "{}").unwrap();
    assert!(reveal_path_validate(&state, chosen.to_str().unwrap()).is_err());

    let saved = pingdeck_core::app::postman_export_save_to_path_impl(
        &state,
        "ws_x".into(),
        "col_x".into(),
        None,
        false,
        Some(chosen.clone()),
    )
    .unwrap();
    assert!(saved.saved);
    assert!(reveal_path_validate(&state, chosen.to_str().unwrap()).is_ok());
    let refused = reveal_path_validate(&state, other.to_str().unwrap()).unwrap_err();
    assert!(
        matches!(refused, AppError::Validation { .. }),
        "{refused:?}"
    );
}
