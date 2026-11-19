//! Config sync tests: deterministic export, secret handling, preview classes, merge rules.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::agent::mcp::McpTransportConfig;
use crate::agent::model::AgentMcpServer;
use crate::app::{
    AppState, EnvironmentVariableInput, SaveEnvironmentInput, SettingsSecret, TlsCertificateDto,
    certificate_save_impl, environment_list_impl, environment_save_impl, flow_list_impl,
    flow_save_impl, globals_get_impl, mock_server_list_impl, mock_server_save_impl,
    request_list_impl, settings_get_impl, settings_secret_set_impl, settings_update_impl,
    workspace_create_impl,
};
use crate::llm::store::{ModelRef, Provider, ProviderModel};
use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint};
use crate::secrets::{SEARCH_TAVILY, SecretRef};
use crate::settings::SettingsSection;
use crate::sync_service::{
    sync_export_impl, sync_folder_risk_impl, sync_import_impl, sync_preview_impl,
    sync_remove_secrets_impl,
};
use crate::workspace::ai_request::{AiRequestConfig, AiTarget, RequestMode, non_routable_label};
use crate::workspace::items::MockCallback;
use crate::workspace::models::{
    Assertion, AuthConfig, Collection, EnvironmentVariable, Folder, HttpMethod, KeyValue, Request,
    RequestBody, TlsCertificateId, TlsCertificateKind,
};
use crate::workspace::{Flow, WorkspaceId};
use pingdeck_app_agent::{agent_mcp_server_list_impl, agent_mcp_server_save_impl};
use pingdeck_app_llm::{
    SaveProviderKeyInput, llm_key_list_impl, llm_key_save_impl, llm_provider_list_impl,
    llm_provider_save_impl,
};

use super::{SyncChange, SyncChoice, SyncImportOptions, SyncKind, SyncOverride};

const FAR_FUTURE_MS: i64 = 4_102_444_800_000;
const SECRET_ENV: &str = "env-secret-value";
const SECRET_GLOBAL: &str = "global-secret-value";
const SECRET_KEY: &str = "sk-live-secret";
const SECRET_MCP: &str = "mcp-secret-value";
const SECRET_SEARCH: &str = "tvly-secret-value";

fn mcp_servers(state: &AppState) -> Vec<AgentMcpServer> {
    agent_mcp_server_list_impl(state)
        .unwrap()
        .into_iter()
        .map(|view| view.server)
        .collect()
}

fn without_home_skills(state: AppState) -> AppState {
    settings_update_impl(
        &state,
        SettingsSection::Ai,
        &json!({ "agent": { "skill_dirs": [] } }),
    )
    .unwrap();
    state
}

fn file_state(db: &Path) -> AppState {
    without_home_skills(AppState::open(db).unwrap())
}

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("pd-sync-{name}-{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let relative = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn text_of(files: &BTreeMap<String, Vec<u8>>) -> String {
    files
        .values()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .collect()
}

fn find(files: &BTreeMap<String, Vec<u8>>, needle: &str) -> String {
    files
        .keys()
        .find(|path| path.contains(needle))
        .unwrap_or_else(|| panic!("no file {needle}"))
        .clone()
}

fn variable(key: &str, value: &str, secret: bool) -> EnvironmentVariableInput {
    EnvironmentVariableInput {
        id: None,
        key: key.to_owned(),
        value: Some(value.to_owned()),
        is_secret: secret,
        enabled: true,
        note: String::new(),
    }
}

fn environment(state: &AppState, id: &str, name: &str, variables: Vec<EnvironmentVariableInput>) {
    environment_save_impl(
        state,
        SaveEnvironmentInput {
            id: id.to_owned(),
            workspace_id: "ws1".to_owned(),
            name: name.to_owned(),
            variables,
            confirm_before_send: Some(true),
            confirm_safe_methods: Some(true),
            declared_hosts: None,
            base_revision: None,
        },
    )
    .unwrap();
}

fn provider() -> Provider {
    Provider {
        id: "openai".to_owned(),
        name: "OpenAI".to_owned(),
        category: "custom".to_owned(),
        base_url: "https://api.example.com".to_owned(),
        auth: AuthStyle::Bearer,
        endpoints: vec![ProviderEndpoint {
            protocol: Protocol::OpenaiChat,
            path_prefix: "/v1".to_owned(),
            auth: None,
            url_mode: Default::default(),
            enabled: true,
        }],
        enabled: true,
        ..Default::default()
    }
}

fn mcp_server(name: &str, transport: McpTransportConfig) -> AgentMcpServer {
    AgentMcpServer {
        id: String::new(),
        name: name.into(),
        transport,
        enabled: true,
        trusted: false,
        updated_at_ms: 0,
    }
}

fn ai_request(
    id: &str,
    collection: Option<&str>,
    provider_id: &str,
    base_url_hint: &str,
) -> Request {
    let mut request = Request::new(
        id,
        "ws1",
        "Ask",
        non_routable_label("OpenAI", "/v1/chat/completions"),
    )
    .unwrap();
    request.method = HttpMethod::Post;
    if let Some(collection) = collection {
        request = request.with_collection(collection).unwrap();
    }
    request.mode = RequestMode::AiApi;
    request.ai = Some(AiRequestConfig {
        protocol: Protocol::OpenaiChat,
        target: AiTarget::Provider {
            provider_id: provider_id.to_owned(),
            provider_name: "OpenAI".to_owned(),
            base_url_hint: base_url_hint.to_owned(),
            model: "gpt-4o".to_owned(),
        },
        builder: None,
        compare_models: Vec::new(),
    });
    request.body = RequestBody::Json {
        content: json!({"model": "gpt-4o", "messages": []}).to_string(),
    };
    request.assertions = vec![
        Assertion::AiReplyContains {
            value: "pong".to_owned(),
        },
        Assertion::AiNoError {},
    ];
    request
}

fn populated() -> AppState {
    let state = AppState::open_in_memory().unwrap();
    workspace_create_impl(&state, "ws1".into(), "Main Workspace".into()).unwrap();
    environment(
        &state,
        "env1",
        "Dev",
        vec![
            variable("BASE", "http://dev", false),
            variable("API_KEY", SECRET_ENV, true),
        ],
    );
    {
        let store = state.store();
        store
            .save_collection(&Collection::new("col1", "ws1", "Users").unwrap())
            .unwrap();
        store
            .save_folder(&Folder::new("fold1", "col1", "Admin").unwrap())
            .unwrap();
        let request = Request::new("req1", "ws1", "List users", "https://example.com/users")
            .unwrap()
            .with_collection("col1")
            .unwrap()
            .with_folder("fold1")
            .unwrap();
        store.save_request(&request).unwrap();
        store
            .save_request(&Request::new("req2", "ws1", "Loose", "https://example.com/x").unwrap())
            .unwrap();
        store
            .save_global_variables(
                "ws1",
                &[
                    EnvironmentVariable::masked("TOKEN", SECRET_GLOBAL).unwrap(),
                    EnvironmentVariable::plain("G", "1").unwrap(),
                ],
            )
            .unwrap();
    }
    mock_server_save_impl(
        &state,
        serde_json::from_value(json!({
            "id": "mock1", "workspace_id": "ws1", "name": "Mock", "port": 4310,
            "endpoint_prefix": "", "cors_enabled": true, "updated_at_ms": 0
        }))
        .unwrap(),
    )
    .unwrap();
    flow_save_impl(
        &state,
        Flow {
            id: "flow1".into(),
            workspace_id: WorkspaceId::new("ws1").unwrap(),
            name: "Smoke".into(),
            description: String::new(),
            steps: Vec::new(),
            updated_at_ms: 0,
            options: Default::default(),
        },
    )
    .unwrap();
    llm_provider_save_impl(&state, provider()).unwrap();
    llm_key_save_impl(
        &state,
        SaveProviderKeyInput {
            id: "key1".into(),
            provider_id: "openai".into(),
            name: "main".into(),
            secret: Some(SECRET_KEY.into()),
            enabled: true,
            position: 0,
            api_user_id: None,
            management_token: None,
            notes: None,
            group_name: None,
            protocols: None,
        },
    )
    .unwrap();
    for model in ["gpt-4o", "gpt-4o-mini"] {
        pingdeck_app_llm::llm_model_save_impl(
            &state,
            ProviderModel {
                provider_id: "openai".into(),
                model_id: model.into(),
                ..ProviderModel::default()
            },
        )
        .unwrap();
    }
    pingdeck_app_llm::llm_model_fallbacks_set_impl(
        &state,
        "openai",
        "gpt-4o",
        vec![ModelRef {
            provider_id: "openai".into(),
            model: "gpt-4o-mini".into(),
        }],
    )
    .unwrap();
    agent_mcp_server_save_impl(
        &state,
        mcp_server(
            "files",
            McpTransportConfig::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "server".into()],
                env: BTreeMap::from([("TOKEN".to_owned(), SECRET_MCP.to_owned())]),
                env_allow: Vec::new(),
                cwd: None,
            },
        ),
        true,
    )
    .unwrap();
    settings_secret_set_impl(&state, SettingsSecret::SearchTavily, SECRET_SEARCH).unwrap();
    settings_update_impl(
        &state,
        SettingsSection::Network,
        &json!({ "timeout_ms": 5000 }),
    )
    .unwrap();
    state
}

fn key_secret(state: &AppState) -> Option<String> {
    state
        .store()
        .secret_get(SecretRef::llm_key("key1").as_str())
        .unwrap()
}

fn export(state: &AppState, dir: &Path) -> crate::sync::SyncExportResult {
    sync_export_impl(state, Some(dir.display().to_string())).unwrap()
}

fn export_text(state: &AppState, name: &str) -> (PathBuf, String) {
    let dir = temp_dir(name);
    export(state, &dir);
    let text = text_of(&files(&dir));
    (dir, text)
}

fn import(
    state: &AppState,
    dir: &Path,
    options: &SyncImportOptions,
) -> crate::sync::SyncImportResult {
    sync_import_impl(state, Some(dir.display().to_string()), options).unwrap()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn preview_entry(state: &AppState, dir: &Path, kind: SyncKind, key: &str) -> super::SyncDiffEntry {
    sync_preview_impl(state, Some(dir.display().to_string()))
        .unwrap()
        .entries
        .into_iter()
        .find(|entry| entry.kind == kind && entry.key == key)
        .unwrap_or_else(|| panic!("no {kind:?} entry {key}"))
}

fn provider_file(dir: &Path) -> (PathBuf, Value) {
    let file = dir.join(find(&files(dir), "llm/providers"));
    let doc = read_json(&file);
    (file, doc)
}

#[test]
fn re_export_layout_placeholders_credentials_classifier_and_auto_export_git_guard() {
    let state = populated();
    let header = |key: &str, value: &str| KeyValue {
        key: key.to_owned(),
        value: value.to_owned(),
        enabled: true,
        description: None,
    };
    {
        let store = state.store();
        let mut request = Request::new("req3", "ws1", "Secure", "https://example.com/s")
            .unwrap()
            .with_collection("col1")
            .unwrap();
        request.auth = AuthConfig::Bearer {
            token_ref: "bearer-literal".into(),
        };
        request.headers = vec![
            header("Authorization", "Bearer header-literal"),
            header("X-Api-Key", "{{api_key_var}}"),
            header("Accept", "application/json"),
        ];
        store.save_request(&request).unwrap();
        let mut loose =
            Request::new("req4", "ws1", "Loose secure", "https://example.com/l").unwrap();
        loose.auth = AuthConfig::Basic {
            username: "me".into(),
            password_ref: "{{pw_var}}".into(),
        };
        loose.headers = vec![header("Cookie", "sid=loose-cookie")];
        store.save_request(&loose).unwrap();
    }
    let mut mock = mock_server_list_impl(&state, "ws1".into())
        .unwrap()
        .remove(0);
    mock.proxy_req_headers = vec![header("Authorization", "Bearer mock-proxy-secret")];
    mock.callbacks = vec![MockCallback {
        id: "cb1".into(),
        name: "hook".into(),
        documentation: String::new(),
        method: HttpMethod::Post,
        url: "https://hook.example/x".into(),
        delay_ms: 0,
        headers: vec![header("X-Api-Key", "mock-callback-secret")],
        body: String::new(),
    }];
    mock_server_save_impl(&state, mock).unwrap();

    let dir = temp_dir("identical");
    let first = export(&state, &dir);
    assert!(first.files_written > 0 && !first.secrets_included);
    let before = files(&dir);
    let second = export(&state, &dir);
    assert_eq!((second.files_written, second.files_removed), (0, 0));
    assert_eq!(files(&dir), before);

    for needle in [
        "manifest.json",
        "settings.json",
        "agent/skills.json",
        "workspaces/main-workspace-",
        "/collections/users-",
        "/environments/dev-",
        "/mocks/mock-",
        "/flows/smoke-",
        "/globals.json",
        "/requests.json",
        "llm/providers/openai-",
        "agent/mcp-servers/files.json",
    ] {
        find(&before, needle);
    }
    let raw = String::from_utf8(before[&find(&before, "/environments/dev-")].clone()).unwrap();
    assert!(raw.ends_with("}\n") && raw.contains("\n  \"environment\""));
    let manifest = read_json(&dir.join("manifest.json"));
    assert_eq!(manifest["format"], "pingdeck.config-sync");
    assert_eq!(manifest["version"], 2);
    assert!(manifest["app_version"].is_string() && manifest["exported_at_ms"].is_i64());

    let all = text_of(&before);
    let all_secrets = [
        SECRET_ENV,
        SECRET_GLOBAL,
        SECRET_KEY,
        SECRET_MCP,
        SECRET_SEARCH,
        "bearer-literal",
        "header-literal",
        "loose-cookie",
        "mock-proxy-secret",
        "mock-callback-secret",
    ];
    for secret in all_secrets {
        assert!(!all.contains(secret), "{secret} leaked");
    }
    for kept in ["{{api_key_var}}", "{{pw_var}}", "application/json"] {
        assert!(all.contains(kept), "{kept} was masked");
    }
    for slot in [
        "API_KEY",
        "TOKEN",
        "llm.openai.key.key1",
        "mcp.files.env.TOKEN",
        "ai.search.tavily",
        "request.req3.auth.token_ref",
        "request.req3.header.Authorization",
        "request.req4.header.Cookie",
        "mock.mock1.proxy_header.Authorization",
        "mock.mock1.callback.cb1.header.X-Api-Key",
    ] {
        assert!(
            all.contains(&format!("{{{{secret:{slot}}}}}")),
            "{slot} missing"
        );
    }
    for (kind, slot, expected) in [
        (SyncKind::LlmProvider, "llm.openai.key.key1", true),
        (
            SyncKind::LlmProvider,
            "llm.openai.header.X-Custom-Key",
            true,
        ),
        (SyncKind::McpServer, "mcp.srv.env.TOKEN", true),
        (SyncKind::Settings, "ai.search.tavily", true),
        (SyncKind::Environment, "API_KEY", true),
        (
            SyncKind::Mock,
            "mock.mock1.proxy_header.Authorization",
            true,
        ),
        (SyncKind::LooseRequests, "request.req4.header.Cookie", true),
        (
            SyncKind::LlmProvider,
            "settings.network.proxy_url.userinfo",
            false,
        ),
        (SyncKind::Settings, "llm.openai.key.key1", false),
        (SyncKind::Mock, "request.req4.header.Cookie", false),
    ] {
        assert_eq!(
            super::secrets::slot_in_table(kind, slot),
            expected,
            "{slot}"
        );
    }
    let preview = sync_preview_impl(&state, Some(dir.display().to_string())).unwrap();
    assert!(preview.entries.is_empty() && preview.warnings.is_empty());

    environment(
        &state,
        "env1",
        "Dev",
        vec![
            variable("BASE", "http://other", false),
            variable("API_KEY", "newer-local-secret", true),
        ],
    );
    let env_file = dir.join(find(&before, "/environments/dev-"));
    let mut doc = read_json(&env_file);
    doc["updated_at_ms"] = json!(FAR_FUTURE_MS);
    write_json(&env_file, &doc);
    assert_eq!(
        import(&state, &dir, &SyncImportOptions::default()).updated,
        1
    );
    let envs = state.store().list_environments("ws1").unwrap();
    let value = |key: &str| {
        envs[0]
            .variables
            .iter()
            .find(|v| v.key == key)
            .unwrap()
            .value
            .clone()
            .unwrap()
    };
    assert_eq!(
        (value("BASE").as_str(), value("API_KEY").as_str()),
        ("http://dev", "newer-local-secret")
    );
    fs::remove_dir_all(dir).unwrap();

    let root = temp_dir("classify");
    let plain = root.join("plain");
    let repo = root.join("repo/sub");
    let dropbox = root.join("Dropbox/pingdeck");
    for d in [&plain, &repo, &dropbox] {
        fs::create_dir_all(d).unwrap();
    }
    fs::create_dir_all(root.join("repo/.git")).unwrap();
    assert!(!super::classify(&plain).risky);
    let git = super::classify(&repo);
    assert!(
        git.in_git && git.risky && git.location.as_deref().is_some_and(|l| l.ends_with("repo"))
    );
    let cloud = super::classify(&dropbox);
    assert_eq!(cloud.cloud_service.as_deref(), Some("Dropbox"));
    assert!(cloud.risky);

    settings_update_impl(
        &state,
        SettingsSection::Sync,
        &json!({"include_secrets": true, "directory": plain.display().to_string()}),
    )
    .unwrap();
    export(&state, &plain);
    let with_secrets = text_of(&files(&plain));
    assert!(with_secrets.contains(SECRET_KEY) && with_secrets.contains("bearer-literal"));
    assert!(
        sync_folder_risk_impl(&state, None)
            .unwrap()
            .holds_secret_values
    );
    let held = super::folder::files_with_secrets(&plain);
    assert!(held.iter().any(|p| p.contains("/collections/")));
    assert!(held.iter().any(|p| p.contains("/mocks/")));
    assert!(held.iter().any(|p| p.ends_with("/requests.json")));

    assert!(
        sync_remove_secrets_impl(&state, None)
            .unwrap()
            .files_rewritten
            >= 1
    );
    let after = text_of(&files(&plain));
    for secret in [
        SECRET_KEY,
        SECRET_SEARCH,
        SECRET_MCP,
        "newer-local-secret",
        "bearer-literal",
        "header-literal",
        "loose-cookie",
        "mock-proxy-secret",
        "mock-callback-secret",
    ] {
        assert!(!after.contains(secret), "{secret} still in the folder");
    }
    assert!(after.contains("{{secret:llm.openai.key.key1}}"));
    assert!(
        !sync_folder_risk_impl(&state, None)
            .unwrap()
            .holds_secret_values
    );
    assert!(super::folder::files_with_secrets(&plain).is_empty());

    let db = root.join("app.sqlite");
    let sync_dir = root.join("repo/sync");
    let auto_plain = root.join("auto-plain");
    let fstate = file_state(&db);
    workspace_create_impl(&fstate, "ws1".into(), "Main".into()).unwrap();
    let configure = |dir: &Path| {
        settings_update_impl(
            &fstate,
            SettingsSection::Sync,
            &json!({"directory": dir.display().to_string(), "auto_export": true, "include_secrets": true}),
        )
        .unwrap();
    };
    configure(&sync_dir);
    let error = crate::sync_service::auto_export_job(&db, None).unwrap_err();
    assert!(error.message().contains("git or cloud-synced folder") && !sync_dir.exists());
    configure(&auto_plain);
    assert!(crate::sync_service::auto_export_job(&db, None).unwrap());
    drop(fstate);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn export_on_one_state_imports_into_a_fresh_one_and_relinks_ai_requests() {
    let a = populated();
    settings_update_impl(
        &a,
        SettingsSection::Sync,
        &json!({ "include_secrets": true }),
    )
    .unwrap();
    certificate_save_impl(
        &a,
        TlsCertificateDto {
            id: TlsCertificateId::new("cert1").unwrap(),
            workspace_id: WorkspaceId::new("ws1").unwrap(),
            kind: TlsCertificateKind::Client,
            host_pattern: "api.test".into(),
            cert_path: "/c.pem".into(),
            key_path: Some("/k.pem".into()),
            has_passphrase: false,
            passphrase: Some("pp-never-exported".into()),
            enabled: true,
        },
    )
    .unwrap();
    let (dir, exported_text) = export_text(&a, "roundtrip");
    assert!(exported_text.contains(SECRET_SEARCH) && exported_text.contains(SECRET_KEY));
    assert!(
        !exported_text.contains("pp-never-exported") && !exported_text.contains("token_sha256")
    );

    let b = AppState::open_in_memory().unwrap();
    let preview = sync_preview_impl(&b, Some(dir.display().to_string())).unwrap();
    assert!(
        preview
            .entries
            .iter()
            .any(|e| e.kind == SyncKind::Workspace && e.change == SyncChange::Added)
    );
    let result = import(&b, &dir, &SyncImportOptions::default());
    assert!(result.skipped_with_reason.is_empty() && result.added >= 9);

    let imported = environment_list_impl(&b, "ws1".into()).unwrap();
    assert_eq!(environment_list_impl(&a, "ws1".into()).unwrap(), imported);
    assert!(
        imported
            .iter()
            .all(|env| env.confirm_before_send && env.confirm_safe_methods)
    );
    let sorted = |state: &AppState| {
        let mut requests = request_list_impl(state, "ws1".into()).unwrap();
        requests.sort_by(|x, y| x.id.as_str().cmp(y.id.as_str()));
        requests
    };
    assert_eq!(sorted(&a), sorted(&b));
    assert_eq!(
        globals_get_impl(&a, "ws1".into()).unwrap(),
        globals_get_impl(&b, "ws1".into()).unwrap()
    );
    assert_eq!(
        mock_server_list_impl(&a, "ws1".into()).unwrap().len(),
        mock_server_list_impl(&b, "ws1".into()).unwrap().len()
    );
    assert_eq!(
        flow_list_impl(&a, "ws1".into()).unwrap().len(),
        flow_list_impl(&b, "ws1".into()).unwrap().len()
    );
    assert_eq!(
        llm_provider_list_impl(&a).unwrap()[0].id,
        llm_provider_list_impl(&b).unwrap()[0].id
    );
    assert_eq!(key_secret(&b).as_deref(), Some(SECRET_KEY));
    // A key new to this machine arrives switched off (C-S1); everything else is the same.
    let switched_off: Vec<_> = llm_key_list_impl(&a, None)
        .unwrap()
        .into_iter()
        .map(|key| crate::llm::store::ProviderKey {
            enabled: false,
            runtime: key.runtime.map(|runtime| crate::llm::store::KeyRuntime {
                state: crate::llm::store::KeyRunState::Disabled,
                ..runtime
            }),
            ..key
        })
        .collect();
    assert_eq!(switched_off, llm_key_list_impl(&b, None).unwrap());
    let models_b = pingdeck_app_llm::llm_model_list_impl(&b, None).unwrap();
    assert_eq!(
        pingdeck_app_llm::llm_model_list_impl(&a, None).unwrap(),
        models_b
    );
    assert_eq!(models_b[0].fallbacks.len(), 1);
    assert_eq!(mcp_servers(&a)[0].transport, mcp_servers(&b)[0].transport);
    assert!(!mcp_servers(&b)[0].enabled && !mcp_servers(&b)[0].trusted);
    assert!(
        preview
            .warnings
            .iter()
            .any(|w| w.contains("MCP server files will be imported disabled"))
    );
    assert_eq!(settings_get_impl(&b).network.timeout_ms, 5000);
    assert_eq!(
        b.secrets().get(SEARCH_TAVILY).unwrap().as_deref(),
        Some(SECRET_SEARCH)
    );
    assert_eq!(settings_get_impl(&b).ai.search.tavily_key_hint, "…alue");

    // C-S1: a provider the import brought in waits for review; the mark is not part of the file.
    let imported = crate::llm::store::list_providers(b.store().connection()).unwrap();
    assert!(imported.iter().all(|p| p.review_pending_since_ms.is_some()));
    let again = sync_preview_impl(&b, Some(dir.display().to_string())).unwrap();
    assert!(again.entries.is_empty());
    let mut enabled = mcp_servers(&b).remove(0);
    let enabled_id = enabled.id.clone();
    enabled.enabled = true;
    agent_mcp_server_save_impl(&b, enabled, true).unwrap();
    b.store()
        .sync_stamp_mcp_server(&enabled_id, mcp_servers(&a)[0].updated_at_ms)
        .unwrap();
    // Keys arrive switched off (C-S1); the user switches this one on, which the file records.
    let key = llm_key_list_impl(&b, None).unwrap().remove(0);
    crate::llm::store::set_key_enabled_at_for_test(
        b.store().connection(),
        &key.id,
        true,
        key.updated_at_ms,
    )
    .unwrap();
    settings_update_impl(
        &b,
        SettingsSection::Sync,
        &json!({ "include_secrets": true }),
    )
    .unwrap();
    let dir_b = temp_dir("roundtrip-b");
    export(&b, &dir_b);
    for (path, bytes) in files(&dir) {
        if path != "manifest.json" {
            let after = files(&dir_b)
                .get(&path)
                .map(|b| String::from_utf8_lossy(b).into_owned());
            assert_eq!(after, Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
    fs::remove_dir_all(dir).unwrap();
    fs::remove_dir_all(dir_b).unwrap();

    // AI request relinking by base_url when provider id differs on another machine.
    a.store()
        .save_request(&ai_request(
            "req_ai",
            Some("col1"),
            "openai",
            "https://api.example.com",
        ))
        .unwrap();
    let ai_dir = temp_dir("ai-relink-url");
    export(&a, &ai_dir);
    for path in files(&ai_dir).keys().filter(|p| p.starts_with("llm/")) {
        fs::remove_file(ai_dir.join(path)).unwrap();
    }
    let c = AppState::open_in_memory().unwrap();
    let mut local = provider();
    local.id = "local_openai".into();
    llm_provider_save_impl(&c, local).unwrap();
    assert!(
        import(&c, &ai_dir, &SyncImportOptions::default())
            .skipped_with_reason
            .is_empty()
    );
    let requests = request_list_impl(&c, "ws1".into()).unwrap();
    let req = requests.iter().find(|r| r.id.as_str() == "req_ai").unwrap();
    assert_eq!(req.mode, RequestMode::AiApi);
    assert!(req.is_provider_target());
    assert_eq!(
        req.ai.as_ref().unwrap().target,
        AiTarget::Provider {
            provider_id: "local_openai".to_owned(),
            provider_name: "OpenAI".to_owned(),
            base_url_hint: "https://api.example.com".to_owned(),
            model: "gpt-4o".to_owned(),
        }
    );
    assert_eq!(
        req.assertions,
        vec![
            Assertion::AiReplyContains {
                value: "pong".to_owned()
            },
            Assertion::AiNoError {},
        ]
    );
    assert!(
        sync_preview_impl(&c, Some(ai_dir.display().to_string()))
            .unwrap()
            .entries
            .iter()
            .all(|e| e.kind != SyncKind::Collection)
    );
    fs::remove_dir_all(ai_dir).unwrap();
}

#[test]
fn preview_and_import_merge_rules_respect_lww_ties_overrides_and_removal_opt_in() {
    let state = populated();
    environment(
        &state,
        "env2",
        "Staging",
        vec![variable("BASE", "http://staging", false)],
    );
    environment(
        &state,
        "env3",
        "Prod",
        vec![variable("BASE", "http://prod", false)],
    );
    let dir = temp_dir("classes");
    export(&state, &dir);
    let exported = files(&dir);

    let dev = dir.join(find(&exported, "/environments/dev-"));
    let mut dev_doc = read_json(&dev);
    let tie_ts = dev_doc["updated_at_ms"].clone();
    dev_doc["environment"]["variables"][0]["value"] = json!("http://changed");
    dev_doc["updated_at_ms"] = json!(FAR_FUTURE_MS);
    write_json(&dev, &dev_doc);

    let staging = dir.join(find(&exported, "/environments/staging-"));
    let mut staging_doc = read_json(&staging);
    staging_doc["environment"]["variables"][0]["value"] = json!("http://older");
    staging_doc["updated_at_ms"] = json!(1);
    write_json(&staging, &staging_doc);

    fs::remove_file(dir.join(find(&exported, "/environments/prod-"))).unwrap();

    let mut added = dev_doc.clone();
    added["environment"]["id"] = json!("env9");
    added["environment"]["name"] = json!("QA");
    let qa_path = dev.with_file_name("qa.json");
    write_json(&qa_path, &added);

    let preview = sync_preview_impl(&state, Some(dir.display().to_string())).unwrap();
    let change = |key: &str| {
        preview
            .entries
            .iter()
            .find(|e| e.key == key)
            .map(|e| e.change)
    };
    assert_eq!(change("env1"), Some(SyncChange::Changed));
    assert_eq!(change("env2"), Some(SyncChange::Conflict));
    assert_eq!(change("env3"), Some(SyncChange::Removed));
    // This machine exported Prod into the folder: its absence is another machine's deletion.
    assert!(
        preview
            .entries
            .iter()
            .any(|e| e.key == "env3" && e.deleted_in_folder)
    );
    assert_eq!(change("env9"), Some(SyncChange::Added));
    assert_eq!(preview.entries.len(), 4);
    assert_eq!(preview.conflicts.len(), 1);
    assert_eq!(preview.conflicts[0].key, "env2");
    let counts = preview
        .counts
        .iter()
        .find(|c| c.kind == SyncKind::Environment)
        .unwrap();
    assert_eq!(
        (
            counts.added,
            counts.changed,
            counts.removed,
            counts.conflicts
        ),
        (1, 1, 1, 1)
    );
    assert!(preview.conflicts[0].local_updated_at_ms > preview.conflicts[0].incoming_updated_at_ms);

    // Tie timestamp: incoming wins; older incoming keeps local; removal needs opt-in.
    fs::remove_file(&qa_path).unwrap();
    dev_doc["environment"]["variables"][0]["value"] = json!("http://tie");
    dev_doc["updated_at_ms"] = tie_ts;
    write_json(&dev, &dev_doc);

    let result = import(&state, &dir, &SyncImportOptions::default());
    assert_eq!(
        (
            result.added,
            result.updated,
            result.removed,
            result.kept_local
        ),
        (0, 1, 0, 1)
    );
    let envs = environment_list_impl(&state, "ws1".into()).unwrap();
    let base = |name: &str| {
        envs.iter().find(|e| e.name == name).unwrap().variables[0]
            .value
            .clone()
            .unwrap()
    };
    assert_eq!(base("Dev"), "http://tie");
    assert_eq!(base("Staging"), "http://staging");
    assert!(envs.iter().any(|e| e.name == "Prod"));

    let overridden = import(
        &state,
        &dir,
        &SyncImportOptions {
            apply_removals: false,
            overrides: vec![SyncOverride {
                kind: SyncKind::Environment,
                key: "env2".into(),
                choice: SyncChoice::Incoming,
            }],
            ..SyncImportOptions::default()
        },
    );
    assert_eq!(overridden.updated, 1);
    assert_eq!(
        environment_list_impl(&state, "ws1".into())
            .unwrap()
            .iter()
            .find(|e| e.name == "Staging")
            .unwrap()
            .variables[0]
            .value
            .as_deref(),
        Some("http://older")
    );

    let removed = import(
        &state,
        &dir,
        &SyncImportOptions {
            apply_removals: true,
            ..SyncImportOptions::default()
        },
    );
    assert_eq!(removed.removed, 1);
    assert!(
        !environment_list_impl(&state, "ws1".into())
            .unwrap()
            .iter()
            .any(|e| e.name == "Prod")
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_oversized_and_special_files_are_refused() {
    let state = populated();
    let dir = temp_dir("invalid");
    export(&state, &dir);
    let exported = files(&dir);
    let dev_rel = find(&exported, "/environments/dev-");
    fs::write(dir.join(&dev_rel), b"{ not json").unwrap();
    let error = sync_preview_impl(&state, Some(dir.display().to_string())).unwrap_err();
    assert!(error.message().contains(&dev_rel) && error.message().contains("invalid JSON"));
    assert!(
        sync_import_impl(
            &state,
            Some(dir.display().to_string()),
            &SyncImportOptions::default()
        )
        .is_err()
    );

    fs::write(dir.join(&dev_rel), vec![b' '; 5 * 1024 * 1024 + 1]).unwrap();
    let error = sync_preview_impl(&state, Some(dir.display().to_string())).unwrap_err();
    assert!(error.message().contains(&dev_rel) && error.message().contains("limit"));

    fs::remove_file(dir.join("manifest.json")).unwrap();
    assert!(
        sync_preview_impl(&state, Some(dir.display().to_string()))
            .unwrap_err()
            .message()
            .contains("manifest.json")
    );
    fs::remove_dir_all(dir).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let (dir, _) = export_text(&state, "special");
        let read = |dir: &Path| {
            let (tx, rx) = std::sync::mpsc::channel();
            let dir = dir.to_path_buf();
            std::thread::spawn(move || {
                tx.send(super::read_dir(&dir).err().map(|e| e.message().to_owned()))
            });
            rx.recv_timeout(Duration::from_secs(30))
                .expect("read_dir hung")
        };
        assert_eq!(read(&dir), None);

        let settings = dir.join("settings.json");
        fs::remove_file(&settings).unwrap();
        symlink("/dev/zero", &settings).unwrap();
        let message = read(&dir).expect("a symlinked settings.json is refused");
        assert!(message.contains("settings.json") && message.contains("symbolic link"));
        fs::remove_file(&settings).unwrap();

        let alias = dir.join(find(&files(&dir), "llm/providers"));
        fs::remove_file(&alias).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&alias)
                .status()
                .unwrap()
                .success()
        );
        let message = read(&dir).expect("a FIFO is refused");
        assert!(message.contains("not a regular file"));
        fs::remove_file(&alias).unwrap();

        // A linked folder where a sync folder belongs is refused, not followed or skipped.
        let flows = dir
            .join(find(&files(&dir), "/flows/"))
            .parent()
            .unwrap()
            .to_path_buf();
        let real = flows.with_file_name("flows-real");
        fs::rename(&flows, &real).unwrap();
        symlink(&real, &flows).unwrap();
        let message = read(&dir).expect("a linked folder is refused");
        assert!(message.contains("flows") && message.contains("symbolic link"));
        fs::remove_file(&flows).unwrap();
        fs::rename(&real, &flows).unwrap();

        // A cloud stand-in of a file not downloaded yet fails the read.
        let dev = dir.join(find(&files(&dir), "/environments/dev-"));
        let name = dev.file_name().unwrap().to_string_lossy().into_owned();
        let stand_in = dev.with_file_name(format!(".{name}.icloud"));
        fs::rename(&dev, &stand_in).unwrap();
        let message = read(&dir).expect("a cloud stand-in is refused");
        assert!(message.contains(&name) && message.contains("not downloaded"));
        fs::rename(&stand_in, &dev).unwrap();

        // An unreadable folder is an error, never an empty folder (skipped where permissions
        // do not apply, as for root).
        use std::os::unix::fs::PermissionsExt;
        let environments = dev.parent().unwrap().to_path_buf();
        fs::set_permissions(&environments, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&environments).is_err() {
            let message = read(&dir).expect("an unreadable folder is refused");
            assert!(message.contains("environments"), "{message}");
        }
        fs::set_permissions(&environments, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(read(&dir), None);

        let manifest = dir.join("manifest.json");
        fs::remove_file(&manifest).unwrap();
        symlink("/dev/zero", &manifest).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (state2, dir2) = (Arc::new(state), dir.clone());
        std::thread::spawn(move || {
            tx.send(sync_export_impl(&state2, Some(dir2.display().to_string())).is_ok())
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(30))
                .expect("export hung")
        );
        assert!(
            fs::symlink_metadata(&manifest)
                .unwrap()
                .file_type()
                .is_file()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn url_credentials_queries_extra_headers_and_mcp_flag_args_are_masked_and_slot_scoped() {
    const USER_BASE: &str = "base-user:base-pass-secret";
    const QUERY_BASE: &str = "api_key=base-query-secret";
    const USER_PPROXY: &str = "pproxy-user:pproxy-pass-secret";
    const QUERY_PPROXY: &str = "token=pproxy-query-secret";
    const HEADER_SECRET: &str = "extra-header-secret-value";
    const USER_NPROXY: &str = "net-user:net-pass-secret";
    const QUERY_NPROXY: &str = "auth=net-query-secret";
    const USER_SEARX: &str = "searx-user:searx-pass-secret";
    const QUERY_SEARX: &str = "key=searx-query-secret";
    const USER_MCP: &str = "mcp-user:mcp-pass-secret";
    const QUERY_MCP: &str = "secret=mcp-query-secret";
    const ARG_EQ_SECRET: &str = "arg-eq-secret-value";
    const ARG_NEXT_SECRET: &str = "arg-next-secret-value";

    let a = without_home_skills(AppState::open_in_memory().unwrap());
    let mut p = provider();
    p.proxy_url = format!("http://{USER_PPROXY}@proxy.example.com:8080?{QUERY_PPROXY}");
    p.quirks
        .extra_headers
        .insert("X-Custom-Key".into(), HEADER_SECRET.into());
    llm_provider_save_impl(&a, p).unwrap();
    let stored_p = llm_provider_list_impl(&a).unwrap().remove(0);
    crate::llm::store::overwrite_provider_base_url_for_test(
        a.store().connection(),
        &stored_p.id,
        &format!("https://{USER_BASE}@api.example.com/v1?{QUERY_BASE}"),
    )
    .unwrap();
    agent_mcp_server_save_impl(
        &a,
        mcp_server(
            "stdio-srv",
            McpTransportConfig::Stdio {
                command: "npx".into(),
                args: vec![
                    format!("--api-key={ARG_EQ_SECRET}"),
                    "--auth-token".into(),
                    ARG_NEXT_SECRET.into(),
                    "--plain-flag=kept".into(),
                ],
                env: BTreeMap::new(),
                env_allow: Vec::new(),
                cwd: None,
            },
        ),
        true,
    )
    .unwrap();
    agent_mcp_server_save_impl(
        &a,
        mcp_server(
            "http-srv",
            McpTransportConfig::Http {
                url: format!("https://{USER_MCP}@mcp.example.com/sse?{QUERY_MCP}"),
                headers: BTreeMap::new(),
            },
        ),
        true,
    )
    .unwrap();
    settings_update_impl(
        &a,
        SettingsSection::Network,
        &json!({ "proxy_url": format!("http://{USER_NPROXY}@127.0.0.1:8888?{QUERY_NPROXY}") }),
    )
    .unwrap();
    settings_update_impl(
        &a,
        SettingsSection::Ai,
        &json!({ "search": { "searxng_base_url": format!("https://{USER_SEARX}@search.example.com?{QUERY_SEARX}") } }),
    )
    .unwrap();

    let (dir, all) = export_text(&a, "deep-secrets");
    for secret in [
        USER_BASE,
        QUERY_BASE,
        USER_PPROXY,
        QUERY_PPROXY,
        HEADER_SECRET,
        USER_NPROXY,
        QUERY_NPROXY,
        USER_SEARX,
        QUERY_SEARX,
        USER_MCP,
        QUERY_MCP,
        ARG_EQ_SECRET,
        ARG_NEXT_SECRET,
    ] {
        assert!(!all.contains(secret), "{secret} leaked");
    }
    assert!(all.contains("--plain-flag=kept"));
    for expected in [
        "https://{{secret:llm.openai.base_url.userinfo}}@api.example.com/v1?{{secret:llm.openai.base_url.query}}",
        "{{secret:llm.openai.header.X-Custom-Key}}",
        "--api-key={{secret:mcp.stdio-srv.arg.--api-key}}",
        "{{secret:mcp.stdio-srv.arg.--auth-token}}",
        "https://{{secret:mcp.http-srv.url.userinfo}}@mcp.example.com/sse?{{secret:mcp.http-srv.url.query}}",
    ] {
        assert!(all.contains(expected), "missing {expected}");
    }

    let same_preview = sync_preview_impl(&a, Some(dir.display().to_string())).unwrap();
    assert!(same_preview.entries.is_empty() && same_preview.warnings.is_empty());

    let b = AppState::open_in_memory().unwrap();
    let fresh = import(&b, &dir, &SyncImportOptions::default());
    assert!(fresh.warnings.len() >= 11);
    let bp = &llm_provider_list_impl(&b).unwrap()[0];
    assert_eq!(bp.base_url, "https://api.example.com/v1");
    assert_eq!(
        bp.proxy_url.trim_end_matches('/'),
        "http://proxy.example.com:8080"
    );
    assert_eq!(bp.quirks.extra_headers.get("X-Custom-Key").unwrap(), "");
    let b_settings = settings_get_impl(&b);
    assert_eq!(b_settings.network.proxy_url, "");
    assert_eq!(b_settings.ai.search.searxng_base_url, "");
    let b_mcp = mcp_servers(&b);
    let stdio = b_mcp.iter().find(|s| s.name == "stdio-srv").unwrap();
    assert_eq!(
        stdio.transport,
        McpTransportConfig::Stdio {
            command: "npx".into(),
            args: vec![
                "--api-key=".into(),
                "--auth-token".into(),
                String::new(),
                "--plain-flag=kept".into(),
            ],
            env: BTreeMap::new(),
            env_allow: Vec::new(),
            cwd: None,
        }
    );
    fs::remove_dir_all(dir).unwrap();

    let fill = |kind, incoming: Value, local: Value| {
        let (mut incoming, mut warnings) = (incoming, Vec::new());
        super::secrets::fill(kind, "doc", &mut incoming, Some(&local), &mut warnings);
        (incoming, warnings)
    };
    let prov = |proxy: &str, key: &str| {
        json!({"provider": {"id": "openai", "base_url": "https://api.example.com", "proxy_url": proxy},
               "keys": [{"id": "key1", "secret": key}]})
    };
    let (filled, warnings) = fill(
        SyncKind::LlmProvider,
        prov(
            "http://{{secret:llm.openai.key.key1}}@evil.example:8080",
            "{{secret:llm.openai.key.key1}}",
        ),
        prov("", "sk-local"),
    );
    assert_eq!(filled["provider"]["proxy_url"], "http://evil.example:8080");
    assert_eq!(filled["keys"][0]["secret"], "sk-local");
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("another field's secret"))
    );

    let (filled, warnings) = fill(
        SyncKind::LlmProvider,
        json!({"provider": {"id": "openai", "base_url":
            "https://{{secret:llm.openai.base_url.userinfo}}@api.example.com/v1?{{secret:llm.openai.base_url.query}}"}}),
        json!({"provider": {"id": "openai", "base_url": "https://u:p@api.example.com/v1?k=q"}}),
    );
    assert_eq!(
        filled["provider"]["base_url"],
        "https://u:p@api.example.com/v1?k=q"
    );
    assert!(warnings.is_empty());

    let mcp = |url: &str, header: &str| json!({"server": {"name": "files", "transport": {"url": url, "headers": {"AUTH": header}}}});
    let (filled, _) = fill(
        SyncKind::McpServer,
        mcp(
            "https://m.example/x?t={{secret:mcp.files.header.AUTH}}",
            "{{secret:mcp.files.header.AUTH}}",
        ),
        mcp("https://m.example/x", "hunter2"),
    );
    assert_eq!(
        (
            filled["server"]["transport"]["url"].as_str(),
            filled["server"]["transport"]["headers"]["AUTH"].as_str(),
        ),
        (Some("https://m.example/x?t="), Some("hunter2"))
    );
}

#[test]
fn local_only_and_unsafe_settings_and_mcp_trust_stay_local_on_export_and_import() {
    let a = populated();
    settings_update_impl(
        &a,
        SettingsSection::Ai,
        &json!({
            "agent": {"sandbox_writes_to_workspace": false},
            "gateway": {"capture_mode": "all"},
            "search": {"searxng_base_url": "https://search.example.com"}
        }),
    )
    .unwrap();
    let mut srv = mcp_servers(&a).remove(0);
    srv.trusted = true;
    srv.enabled = true;
    agent_mcp_server_save_impl(&a, srv, true).unwrap();
    assert!(
        a.store()
            .mark_request_agent_touched("ws1", "req1", 1)
            .unwrap()
    );
    let dir = temp_dir("safety");
    export(&a, &dir);

    let mcp_json = read_json(&dir.join("agent/mcp-servers/files.json"));
    assert!(mcp_json["server"].get("trusted").is_none());
    assert!(!text_of(&files(&dir)).contains("agent_touched_ms"));

    let mut settings = read_json(&dir.join("settings.json"));
    let locals: Vec<_> = crate::settings::registry::settings_registry()
        .fields
        .into_iter()
        .filter(|field| field.local_only)
        .collect();
    assert!(locals.len() >= 13);
    for field in locals {
        let pointer = format!("/{}", field.id.replace('.', "/"));
        assert!(
            settings.pointer(&pointer).is_none(),
            "{} was exported",
            field.id
        );
    }

    let mut edited_mcp = mcp_json;
    edited_mcp["server"]["enabled"] = json!(false);
    edited_mcp["server"]["trusted"] = json!(false);
    edited_mcp["server"]["transport"]["command"] = json!("node");
    edited_mcp["updated_at_ms"] = json!(FAR_FUTURE_MS);
    write_json(&dir.join("agent/mcp-servers/files.json"), &edited_mcp);

    settings["network"] = json!({"timeout_ms": 7000, "verify_tls": false, "no_proxy": ["*"]});
    settings["data"] = json!({"history_retention": 10});
    settings["ai"]["agent"] = json!({
        "tool_approval": "auto_all",
        "skill_dirs": ["/tmp/evil-skills"],
        "cost_cap_usd": null,
        "max_concurrent_runs": 1,
        "max_tool_iterations": 7
    });
    settings["ai"]["gateway"] = json!({
        "max_retries": 5,
        "require_token": false,
        "serve_lan": true,
        "extra_allowed_hosts": ["evil.example"],
        "port": 4567
    });
    settings["advanced"] = json!({"script_loop_iteration_limit": 5_000});
    write_json(&dir.join("settings.json"), &settings);
    let before = settings_get_impl(&a);

    let preview = sync_preview_impl(&a, Some(dir.display().to_string())).unwrap();
    let skipped_fields = [
        "skill_dirs",
        "require_token",
        "serve_lan",
        "extra_allowed_hosts",
        "gateway.port",
        "max_tool_iterations",
    ];
    for expected in skipped_fields.into_iter().chain([
        "verify_tls",
        "no_proxy",
        "history_retention",
        "max_concurrent_runs",
        "script_loop_iteration_limit",
    ]) {
        assert!(
            preview.warnings.iter().any(|w| w.contains(expected)),
            "missing {expected}"
        );
    }
    let result = import(&a, &dir, &SyncImportOptions::default());
    for expected in skipped_fields {
        assert!(
            result
                .skipped_with_reason
                .iter()
                .any(|s| s.reason.contains(expected)),
            "missing {expected}"
        );
    }
    let after_mcp = &mcp_servers(&a)[0];
    assert!(after_mcp.enabled && after_mcp.trusted);
    let after = settings_get_impl(&a);
    assert_eq!(
        (after.ai.gateway.max_retries, after.network.timeout_ms),
        (5, 7000)
    );
    assert_ne!(after.ai.agent.max_tool_iterations, 7);
    assert_ne!(
        after.ai.agent.skill_dirs,
        vec!["/tmp/evil-skills".to_owned()]
    );
    assert!(after.ai.gateway.require_token && !after.ai.gateway.serve_lan);
    assert!(after.ai.gateway.extra_allowed_hosts.is_empty() && after.ai.gateway.port != 4567);
    assert_eq!(after.network.verify_tls, before.network.verify_tls);
    assert_eq!(after.network.no_proxy, before.network.no_proxy);
    assert_eq!(after.data.history_retention, before.data.history_retention);
    assert_eq!(after.ai.agent.cost_cap_usd, before.ai.agent.cost_cap_usd);
    assert_eq!(
        after.ai.agent.max_concurrent_runs,
        before.ai.agent.max_concurrent_runs
    );
    assert_eq!(
        after.advanced.script_loop_iteration_limit,
        before.advanced.script_loop_iteration_limit
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn provider_and_mock_target_changes_require_review_and_enforce_safety() {
    let state = populated();
    let (dir, _) = export_text(&state, "host-move");
    let (file, original) = provider_file(&dir);
    let moved_to = |url: &str| {
        let mut doc = original.clone();
        doc["provider"]["base_url"] = json!(url);
        doc["updated_at_ms"] = json!(FAR_FUTURE_MS);
        write_json(&file, &doc);
        sync_preview_impl(&state, Some(dir.display().to_string()))
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.kind == SyncKind::LlmProvider)
            .and_then(|entry| entry.host_change)
            .map(|change| change.to_host)
    };
    assert_eq!(
        moved_to("https://evil.com\\@api.example.com/v1").as_deref(),
        Some("evil.com")
    );
    assert_eq!(
        moved_to("http://api.example.com").as_deref(),
        Some("http://api.example.com")
    );
    assert_eq!(
        moved_to("https://api.example.com:8443").as_deref(),
        Some("api.example.com:8443")
    );
    assert!(moved_to("api.example.com/v1").is_some());
    assert_eq!(moved_to("https://API.example.com:443/other"), None);
    assert_eq!(
        moved_to("https://{{secret:llm.openai.base_url.userinfo}}@api.example.com/v1"),
        None
    );

    let mut doc = original.clone();
    doc["provider"]["base_url"] = json!("https://evil.example/v1");
    doc["updated_at_ms"] = json!(FAR_FUTURE_MS);
    write_json(&file, &doc);
    let entry = preview_entry(&state, &dir, SyncKind::LlmProvider, "openai");
    let change = entry.host_change.as_ref().expect("host change reported");
    assert_eq!(
        (change.from_host.as_str(), change.to_host.as_str()),
        ("api.example.com", "evil.example")
    );
    assert!(entry.detail.contains("will not be sent to the new host") && entry.review);
    assert_eq!(
        entry.review_reason.as_deref(),
        Some("Host changes from api.example.com to evil.example")
    );

    let kept = import(
        &state,
        &dir,
        &SyncImportOptions {
            keep_key_providers: vec!["openai".into()],
            ..SyncImportOptions::default()
        },
    );
    assert_eq!(kept.updated, 1);
    assert_eq!(key_secret(&state).as_deref(), Some(SECRET_KEY));

    let mut proxy_doc = original;
    proxy_doc["provider"]["base_url"] = json!("https://evil.example/v1");
    proxy_doc["provider"]["proxy_url"] =
        json!("http://{{secret:llm.openai.key.key1}}@evil.example:8080");
    proxy_doc["updated_at_ms"] = json!(FAR_FUTURE_MS + 1);
    write_json(&file, &proxy_doc);

    let proxy_entry = preview_entry(&state, &dir, SyncKind::LlmProvider, "openai");
    let proxy_change = proxy_entry.host_change.as_ref().unwrap();
    assert_eq!(
        (
            proxy_change.from_host.as_str(),
            proxy_change.to_host.as_str()
        ),
        ("no proxy", "proxy http://evil.example:8080")
    );
    assert!(proxy_entry.review);

    assert_eq!(
        import(&state, &dir, &SyncImportOptions::default()).updated,
        1
    );
    let imported = &llm_provider_list_impl(&state).unwrap()[0];
    assert!(!imported.proxy_url.contains(SECRET_KEY) && !imported.proxy_url.contains("{{"));
    // The key is switched off for the new host, its value stays.
    assert_eq!(key_secret(&state).as_deref(), Some(SECRET_KEY));
    let connection = state.store();
    let keys = crate::llm::store::list_keys(connection.connection(), Some("openai")).unwrap();
    assert!(keys.iter().all(|k| !k.enabled));
    drop(connection);
    // The provider file still carries pre-import credentials; mock imports must not replay it.
    fs::remove_file(&file).unwrap();

    let mock_file = dir.join(find(&files(&dir), "/mocks/mock-"));
    let mut mock_doc = read_json(&mock_file);
    mock_doc["mock_server"]["proxy_enabled"] = json!(true);
    mock_doc["mock_server"]["proxy_base_url"] = json!("https://attacker.example");
    mock_doc["mock_server"]["callbacks"] = json!([{
        "id": "cb1", "name": "hook", "method": "Post", "url": "https://hook.example/x",
        "headers": [], "body": ""
    }]);
    mock_doc["updated_at_ms"] = json!(FAR_FUTURE_MS);
    write_json(&mock_file, &mock_doc);

    let mock_entry = preview_entry(&state, &dir, SyncKind::Mock, "mock1");
    assert!(mock_entry.review);
    assert_eq!(
        mock_entry.review_reason.as_deref(),
        Some(
            "Mock proxy target changes to https://attacker.example; proxying stays off until you enable it"
        )
    );
    let mock_res = import(&state, &dir, &SyncImportOptions::default());
    assert_eq!(mock_res.updated, 1);
    assert!(
        mock_res
            .warnings
            .iter()
            .any(|w| w.contains("proxying was switched off"))
    );
    let mock = &mock_server_list_impl(&state, "ws1".into()).unwrap()[0];
    assert!(!mock.proxy_enabled && mock.callbacks.len() == 1);

    mock_doc["mock_server"]["callbacks"][0]["url"] = json!("https://other-hook.example/x");
    write_json(&mock_file, &mock_doc);
    assert_eq!(
        preview_entry(&state, &dir, SyncKind::Mock, "mock1")
            .review_reason
            .as_deref(),
        Some(
            "Mock proxy target changes to https://attacker.example; proxying stays off until you enable it"
        )
    );
    mock_doc["mock_server"]["proxy_enabled"] = json!(false);
    write_json(&mock_file, &mock_doc);
    assert_eq!(
        preview_entry(&state, &dir, SyncKind::Mock, "mock1")
            .review_reason
            .as_deref(),
        Some("Mock callback URL changes to https://other-hook.example/x")
    );

    let mut local = mock_server_list_impl(&state, "ws1".into())
        .unwrap()
        .remove(0);
    local.proxy_enabled = true;
    local.proxy_base_url = "https://attacker.example".into();
    mock_server_save_impl(&state, local).unwrap();
    mock_doc["mock_server"]["proxy_enabled"] = json!(true);
    mock_doc["mock_server"]["latency_ms"] = json!(7);
    mock_doc["updated_at_ms"] = json!(FAR_FUTURE_MS + 1);
    write_json(&mock_file, &mock_doc);
    import(&state, &dir, &SyncImportOptions::default());
    let mock = &mock_server_list_impl(&state, "ws1".into()).unwrap()[0];
    assert!(mock.proxy_enabled && mock.latency_ms == 7);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_imported_provider_tells_open_pages_to_re_read_it() {
    use crate::events::{EventSink, Sequenced};
    use crate::llm::events::LlmEvent;
    use std::sync::Mutex;

    let (dir, _) = export_text(&populated(), "events");
    let b = AppState::open_in_memory().unwrap();
    let seen: Arc<Mutex<Vec<LlmEvent>>> = Arc::default();
    let sink_seen = Arc::clone(&seen);
    let sink: Arc<dyn EventSink<Sequenced<LlmEvent>>> =
        Arc::new(move |event: Sequenced<LlmEvent>| {
            sink_seen.lock().unwrap().push(event.event);
            true
        });
    b.llm.events().subscribe_live(sink).unwrap();
    import(&b, &dir, &SyncImportOptions::default());
    let seen = seen.lock().unwrap();
    assert!(seen.contains(&LlmEvent::ProvidersChanged), "{seen:?}");
    for event in [
        LlmEvent::KeysChanged {
            provider_id: "openai".into(),
        },
        LlmEvent::ModelsChanged {
            provider_id: "openai".into(),
        },
    ] {
        assert!(seen.contains(&event), "{seen:?}");
    }
}

#[test]
fn an_empty_secret_and_a_shorter_file_leave_local_keys_and_models_alone() {
    let state = populated();
    let (dir, _) = export_text(&state, "empty-secret");
    let (file, mut doc) = provider_file(&dir);
    doc["keys"][0]["secret"] = json!("");
    doc["keys"][0]["enabled"] = json!(false);
    doc["provider"]["notes"] = json!("edited elsewhere");
    let models = doc["models"].as_array_mut().unwrap();
    models.retain(|model| model["model_id"] == "gpt-4o");
    models[0]["fallbacks"] = json!([]);
    doc["updated_at_ms"] = json!(FAR_FUTURE_MS);
    write_json(&file, &doc);
    assert_eq!(
        import(&state, &dir, &SyncImportOptions::default()).updated,
        1
    );
    assert_eq!(key_secret(&state).as_deref(), Some(SECRET_KEY));
    assert!(llm_key_list_impl(&state, None).unwrap()[0].enabled);
    assert_eq!(
        llm_provider_list_impl(&state).unwrap()[0].notes,
        "edited elsewhere"
    );
    assert_eq!(
        pingdeck_app_llm::llm_model_list_impl(&state, None)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn an_imported_cross_host_confirmation_is_dropped() {
    let state = populated();
    let (dir, _) = export_text(&state, "cross-host");
    let (file, mut doc) = provider_file(&dir);
    doc["provider"]["custom_balance"] = json!({
        "url": "https://billing.other.test/balance", "headers": [], "auth": "bearer",
        "remaining_path": "$.data.remaining", "used_path": null, "total_path": null,
        "unit": "USD", "scope": "account", "unit_label": null, "divisor": null,
        "cross_host": true
    });
    doc["updated_at_ms"] = json!(FAR_FUTURE_MS);
    write_json(&file, &doc);
    // The base URL stays: the preview still names the new balance host the import switches to.
    let entry = preview_entry(&state, &dir, SyncKind::LlmProvider, "openai");
    let change = entry.host_change.expect("balance host change reported");
    assert_eq!(change.to_host, "billing.other.test");
    let result = import(&state, &dir, &SyncImportOptions::default());
    assert!(result.skipped_with_reason.is_empty(), "{result:?}");
    let provider = llm_provider_list_impl(&state).unwrap().remove(0);
    let query = provider.custom_balance.expect("query imported");
    assert!(!query.cross_host);
    // The new balance host is a new outbound host: the keys wait for the user.
    assert!(
        llm_key_list_impl(&state, None)
            .unwrap()
            .iter()
            .all(|k| !k.enabled)
    );
    assert_eq!(key_secret(&state).as_deref(), Some(SECRET_KEY));
}

#[test]
fn an_imported_full_access_mode_never_replaces_the_local_one() {
    use crate::settings::ToolApproval;
    for local in [None, Some("ask")] {
        let state = populated();
        if let Some(mode) = local {
            settings_update_impl(
                &state,
                SettingsSection::Ai,
                &json!({"agent": {"default_permission_mode": mode}}),
            )
            .unwrap();
        }
        let before = settings_get_impl(&state).ai.agent.default_permission_mode;
        let (dir, _) = export_text(&state, "permission-mode");
        let mut settings = read_json(&dir.join("settings.json"));
        assert!(
            settings
                .pointer("/ai/agent/default_permission_mode")
                .is_none()
        );
        settings["ai"]["agent"]["default_permission_mode"] = json!("auto_all");
        write_json(&dir.join("settings.json"), &settings);
        let entry = preview_entry(&state, &dir, SyncKind::Settings, "ai");
        assert!(entry.local_setting_kept, "{local:?}");
        import(&state, &dir, &SyncImportOptions::default());
        let after = settings_get_impl(&state).ai.agent.default_permission_mode;
        assert_eq!(after, before, "{local:?}");
        assert_ne!(after, ToolApproval::AutoAll);
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn a_round_trip_through_another_machine_shows_no_provider_difference() {
    let a = populated();
    let (dir_a, _) = export_text(&a, "round-a");
    let b = without_home_skills(AppState::open_in_memory().unwrap());
    import(&b, &dir_a, &SyncImportOptions::default());
    let (dir_b, _) = export_text(&b, "round-b");
    let preview = sync_preview_impl(&a, Some(dir_b.display().to_string())).unwrap();
    let providers: Vec<_> = preview
        .entries
        .iter()
        .filter(|entry| entry.kind == SyncKind::LlmProvider)
        .collect();
    assert!(providers.is_empty(), "{providers:?}");
    fs::remove_dir_all(dir_a).unwrap();
    fs::remove_dir_all(dir_b).unwrap();
}

#[test]
fn a_format_1_folder_still_imports() {
    let (dir, _) = export_text(&populated(), "format-1");
    let mut manifest = read_json(&dir.join("manifest.json"));
    manifest["version"] = json!(1);
    write_json(&dir.join("manifest.json"), &manifest);
    let b = AppState::open_in_memory().unwrap();
    let result = import(&b, &dir, &SyncImportOptions::default());
    assert!(
        result.added > 0 && result.skipped_with_reason.is_empty(),
        "{result:?}"
    );
    assert_eq!(llm_provider_list_impl(&b).unwrap().len(), 1);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn secret5_sync_export_placeholders_every_credential_header() {
    let state = populated();
    let mut request = Request::new("req_s5", "ws1", "Signed", "https://example.com/s").unwrap();
    request.headers = [
        ("X-Amz-Security-Token", "FwoGZXIvYXdzEsession"),
        ("x-relay-token", "relay-literal"),
        ("x-api-version", "2024-01-01"),
    ]
    .into_iter()
    .map(|(key, value)| KeyValue {
        key: key.to_owned(),
        value: value.to_owned(),
        enabled: true,
        description: None,
    })
    .collect();
    state.store().save_request(&request).unwrap();
    let (dir, text) = export_text(&state, "secret5");
    assert!(!text.contains("FwoGZXIvYXdzEsession") && !text.contains("relay-literal"));
    assert!(text.contains("{{secret:request.req_s5.header.X-Amz-Security-Token}}"));
    assert!(text.contains("{{secret:request.req_s5.header.x-relay-token}}"));
    assert!(text.contains("2024-01-01"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn flow_steps_and_request_urls_queries_and_forms_are_masked_and_refilled_by_slot() {
    use crate::workspace::FlowStep;
    use crate::workspace::models::FormPart;
    let state = populated();
    let kv = |key: &str, value: &str| KeyValue {
        key: key.to_owned(),
        value: value.to_owned(),
        enabled: true,
        description: None,
    };
    let mut request = Request::new(
        "req_q",
        "ws1",
        "Keyed",
        "https://bob:userinfo-literal@example.com/q?key=AIza-url-literal&page=2",
    )
    .unwrap();
    request.query_params = vec![kv("access_token", "query-row-literal"), kv("q", "plain")];
    request.body = RequestBody::Multipart {
        parts: vec![FormPart {
            key: "client_secret".into(),
            value: "form-literal".into(),
            file_path: None,
            enabled: true,
        }],
    };
    state.store().save_request(&request).unwrap();
    let mut flow = state.store().list_flows("ws1").unwrap().remove(0);
    let step: FlowStep = serde_json::from_value(json!({
        "id": "st1", "name": "inline", "delay_ms": 0, "continue_on_error": false, "position": 0,
        "url_override": "https://api.example/x?token=flow-url-literal",
        "headers_override": [{"key": "Authorization", "value": "Bearer flow-header-literal", "enabled": true},
                             {"key": "Cookie", "value": "{{session}}", "enabled": true}],
        "query_override": [{"key": "api_key", "value": "flow-query-literal", "enabled": true}]
    }))
    .unwrap();
    let form_step: FlowStep = serde_json::from_value(json!({
        "id": "st2", "name": "token", "delay_ms": 0, "continue_on_error": false, "position": 1,
        "url_override": "https://auth.example/token",
        "body_override": "grant_type=client_credentials\nclient_secret = flow-form-literal\nscope={{scope}}",
        "body_override_kind": "form_urlencoded"
    }))
    .unwrap();
    flow.steps = vec![step, form_step];
    flow.options
        .initial_variables
        .insert("password".into(), "flow-input-literal".into());
    flow.options
        .initial_variables
        .insert("user".into(), "ada".into());
    flow_save_impl(&state, flow).unwrap();

    let (dir, text) = export_text(&state, "positions");
    for literal in [
        "userinfo-literal",
        "AIza-url-literal",
        "query-row-literal",
        "form-literal",
        "flow-url-literal",
        "flow-header-literal",
        "flow-query-literal",
        "flow-form-literal",
        "flow-input-literal",
    ] {
        assert!(!text.contains(literal), "{literal} leaked");
    }
    for slot in [
        "request.req_q.url.userinfo",
        "request.req_q.url.query.key",
        "request.req_q.query.access_token",
        "request.req_q.form.client_secret",
        "flow.flow1.step.st1.url.query.token",
        "flow.flow1.step.st1.header.Authorization",
        "flow.flow1.step.st1.query.api_key",
        "flow.flow1.step.st2.form.client_secret",
        "flow.flow1.var.password",
    ] {
        assert!(
            text.contains(&format!("{{{{secret:{slot}}}}}")),
            "{slot} missing"
        );
    }
    for kept in [
        "page=2",
        "{{session}}",
        "\"ada\"",
        "plain",
        "grant_type=client_credentials",
    ] {
        assert!(text.contains(kept), "{kept} was masked");
    }
    // The placeholders refill from the local values: nothing differs, nothing is lost.
    let preview = sync_preview_impl(&state, Some(dir.display().to_string())).unwrap();
    assert!(preview.entries.is_empty(), "{:?}", preview.entries);
    // A placeholder moved into another slot does not carry the local value along.
    let flow_file = dir.join(find(&files(&dir), "/flows/"));
    let mut doc = read_json(&flow_file);
    doc["flow"]["steps"][0]["url_override"] =
        json!("https://evil.example/x?token={{secret:flow.flow1.var.password}}");
    write_json(&flow_file, &doc);
    let entry = preview_entry(&state, &dir, SyncKind::Flow, "flow1");
    assert_eq!(entry.change, SyncChange::Changed);
    import(&state, &dir, &SyncImportOptions::default());
    let saved = state.store().list_flows("ws1").unwrap().remove(0);
    assert_eq!(
        saved.steps[0].url_override.as_deref(),
        Some("https://evil.example/x?token=")
    );
    assert!(super::folder::files_with_secrets(&dir).is_empty());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn auto_export_stops_for_another_machines_deletion_and_cross_edit_instead_of_undoing_them() {
    use crate::app::environment_delete_impl;
    use crate::error::AppError;
    use crate::sync_service::{auto_export_job, sync_status_impl};
    let root = temp_dir("two-machines");
    let folder = root.join("sync");
    let (db_a, db_b) = (root.join("a.sqlite"), root.join("b.sqlite"));
    let (a, b) = (file_state(&db_a), file_state(&db_b));
    for state in [&a, &b] {
        settings_update_impl(
            state,
            SettingsSection::Sync,
            &json!({"directory": folder.display().to_string(), "auto_export": true}),
        )
        .unwrap();
    }
    workspace_create_impl(&a, "ws1".into(), "Main".into()).unwrap();
    environment(&a, "env1", "Dev", vec![variable("BASE", "http://a", false)]);
    environment(
        &a,
        "env2",
        "Prod",
        vec![variable("BASE", "http://prod", false)],
    );
    assert!(auto_export_job(&db_a, None).unwrap());
    import(&b, &folder, &SyncImportOptions::default());
    assert!(auto_export_job(&db_b, None).unwrap());

    // A deletes Prod; B, which has not imported that, must not write it back.
    environment_delete_impl(&a, "ws1".into(), "env2".into()).unwrap();
    assert!(auto_export_job(&db_a, None).unwrap());
    let error = auto_export_job(&db_b, None).unwrap_err();
    assert!(
        matches!(error, AppError::Conflict { .. }) && error.message().contains("1 deleted"),
        "{error:?}"
    );
    assert!(
        !files(&folder)
            .keys()
            .any(|path| path.contains("/environments/prod-"))
    );
    let entry = preview_entry(&b, &folder, SyncKind::Environment, "env2");
    assert!(entry.change == SyncChange::Removed && entry.deleted_in_folder);
    // The stop is read from the ledger, so it outlives the session.
    let status = sync_status_impl(&b);
    assert_eq!(
        status.last_error_kind,
        Some(super::SyncErrorKind::PendingIncoming)
    );
    assert!(status.last_error_at_ms.is_some());
    // Importing without the removal keeps the stop; applying it clears the way.
    import(&b, &folder, &SyncImportOptions::default());
    assert!(auto_export_job(&db_b, None).is_err());
    import(
        &b,
        &folder,
        &SyncImportOptions {
            apply_removals: true,
            ..SyncImportOptions::default()
        },
    );
    assert!(auto_export_job(&db_b, None).unwrap());
    assert!(sync_status_impl(&b).last_error.is_none());

    // Cross edit: B's newer edit of Dev does not overwrite A's exported one unseen.
    environment(
        &a,
        "env1",
        "Dev",
        vec![variable("BASE", "http://a2", false)],
    );
    assert!(auto_export_job(&db_a, None).unwrap());
    std::thread::sleep(Duration::from_millis(20));
    environment(
        &b,
        "env1",
        "Dev",
        vec![variable("BASE", "http://b2", false)],
    );
    let error = auto_export_job(&db_b, None).unwrap_err();
    assert!(error.message().contains("1 changed"), "{error:?}");
    let dev = folder.join(find(&files(&folder), "/environments/dev-"));
    assert!(
        String::from_utf8(fs::read(&dev).unwrap())
            .unwrap()
            .contains("http://a2")
    );
    assert_eq!(
        preview_entry(&b, &folder, SyncKind::Environment, "env1").change,
        SyncChange::Conflict
    );
    // A preview token pins the import to what the user saw.
    let token = sync_preview_impl(&b, Some(folder.display().to_string()))
        .unwrap()
        .token;
    environment(&a, "env3", "QA", vec![variable("BASE", "http://qa", false)]);
    assert!(auto_export_job(&db_a, None).unwrap());
    let stale = sync_import_impl(
        &b,
        Some(folder.display().to_string()),
        &SyncImportOptions {
            preview_token: Some(token),
            ..SyncImportOptions::default()
        },
    );
    assert!(matches!(stale, Err(AppError::Conflict { .. })));
    // Keeping the local copy through the preview lets B's export replace the file.
    assert_eq!(
        import(&b, &folder, &SyncImportOptions::default()).kept_local,
        1
    );
    assert!(auto_export_job(&db_b, None).unwrap());
    assert!(
        String::from_utf8(fs::read(&dev).unwrap())
            .unwrap()
            .contains("http://b2")
    );
    drop((a, b));
    fs::remove_dir_all(root).unwrap();
}
