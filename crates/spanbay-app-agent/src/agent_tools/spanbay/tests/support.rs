//! Fixtures of the SpanBay tool tests: a world with two workspaces, a collection, saved requests
//! and two environments (one of them guarded) whose `base_url` points at a fake upstream on
//! 127.0.0.1, a chat bound to the first workspace, and helpers to inspect and call a tool.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use crate::agent::model::{AgentSession, AgentTarget};
use crate::agent::tools::{AgentTool, CallInspection, TestParts, ToolContext, ToolOutput};
use crate::app::AppState;
use crate::cancel::CancelToken;
use crate::error::AppError;
use crate::llm::hub::HubUsage;
use crate::llm::probe::test_support::{Fake, Reply, Seen, read_request, spawn_fake};
use crate::llm::store::{
    LogWrite, ModelSource, Provider, ProviderModel, RequestLogEntry, insert_log,
};
use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint, Quirks};
use crate::settings::{Settings, ToolApproval};
use crate::workspace::models::{Collection, Folder, KeyValue, Request};
use crate::workspace::storage::SpanBayBinding;
use crate::workspace::{Environment, EnvironmentVariable, Workspace};
use spanbay_app_llm::{
    SaveProviderKeyInput, llm_key_save_impl, llm_model_save_impl, llm_provider_save_impl,
};

/// The chat the tools run in.
pub(super) const SESSION: &str = "sess_a";
/// The workspace the chat is bound to.
pub(super) const WS_A: &str = "ws_a";
/// The other workspace, which the chat must never reach.
pub(super) const WS_B: &str = "ws_b";
/// The value of the secret variable `token` in both environments.
pub(super) const TOKEN: &str = "tok_live_9f8e7d6c5b4a";

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A world: state over a database file of its own, a fake upstream and a context's parts.
pub(super) struct World {
    pub state: AppState,
    pub fake: Fake,
    pub settings: Settings,
    pub cancel: CancelToken,
    pub parts: TestParts,
}

impl World {
    /// A world whose fake upstream answers every request with `handler`.
    pub(super) async fn with(handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Self {
        let fake = spawn_fake(handler).await;
        Self::seeded(fake)
    }

    /// A world whose upstream echoes `{"ok": true}` as JSON.
    pub(super) async fn new() -> Self {
        Self::with(|_| Reply::json(200, &json!({"ok": true}))).await
    }

    /// A world over an upstream the test already started.
    pub(super) fn seeded(fake: Fake) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "pd-wp10-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path: PathBuf = dir.join("spanbay.sqlite3");
        let state = AppState::open(path).expect("state");
        let world = Self {
            state,
            fake,
            settings: Settings::default(),
            cancel: CancelToken::new(),
            parts: TestParts::default(),
        };
        world.seed();
        world
    }

    /// `http://127.0.0.1:<port>`, the base URL of both environments.
    pub(super) fn base_url(&self) -> String {
        format!("http://{}", self.fake.addr)
    }

    /// The `host:port` the environments declare.
    pub(super) fn host(&self) -> String {
        self.fake.addr.to_string()
    }

    fn seed(&self) {
        let store = self.state.store();
        for (id, name) in [(WS_A, "Billing platform"), (WS_B, "Other workspace")] {
            store
                .save_workspace(&Workspace::new(id, name).unwrap())
                .unwrap();
        }
        store
            .save_collection(&Collection::new("col_a", WS_A, "Billing API").unwrap())
            .unwrap();
        store
            .save_folder(&Folder::new("fld_inv", "col_a", "Invoices").unwrap())
            .unwrap();
        let mut invoices =
            Request::new("req_inv", WS_A, "GET /invoices", "{{base_url}}/invoices").unwrap();
        invoices.collection_id =
            Some(crate::workspace::models::CollectionId::new("col_a").unwrap());
        invoices.folder_id = Some(crate::workspace::models::FolderId::new("fld_inv").unwrap());
        invoices.headers = vec![KeyValue {
            key: "Authorization".to_owned(),
            value: "Bearer {{token}}".to_owned(),
            enabled: true,
            description: None,
        }];
        store.save_request(&invoices).unwrap();
        store
            .save_collection(&Collection::new("col_b", WS_B, "Other API").unwrap())
            .unwrap();
        let mut other =
            Request::new("req_other", WS_B, "GET /secret", "https://other.test/x").unwrap();
        other.collection_id = Some(crate::workspace::models::CollectionId::new("col_b").unwrap());
        store.save_request(&other).unwrap();
        for (id, name, guarded) in [
            ("env_stg", "Staging", false),
            ("env_prod", "Production", true),
        ] {
            let mut environment = Environment::new(id, WS_A, name).unwrap();
            environment.confirm_before_send = guarded;
            environment.variables = vec![
                EnvironmentVariable::plain("base_url", self.base_url()).unwrap(),
                EnvironmentVariable::masked("token", TOKEN).unwrap(),
            ];
            store.save_environment(&environment).unwrap();
        }
        store.save_agent_session(&session(SESSION)).unwrap();
        store
            .set_session_spanbay_binding(
                SESSION,
                Some(&SpanBayBinding {
                    workspace_id: WS_A.to_owned(),
                    environment_id: Some("env_stg".to_owned()),
                }),
            )
            .unwrap();
    }

    /// A context of the bound chat, run `run`.
    pub(super) fn ctx_in<'a>(&'a self, session: &'a str, run: &'a str) -> ToolContext<'a> {
        let mut ctx = ToolContext::for_test(
            Some(&self.state as &(dyn std::any::Any + Send + Sync)),
            &self.settings,
            &self.cancel,
            &self.parts,
        );
        ctx.session_id = session;
        ctx.run_id = run;
        ctx
    }

    /// A context of the bound chat.
    pub(super) fn ctx(&self) -> ToolContext<'_> {
        self.ctx_in(SESSION, "run_1")
    }

    /// The history rows of workspace A, newest first.
    pub(super) fn history(&self) -> Vec<crate::workspace::models::ExecutionHistoryEntry> {
        self.state
            .store()
            .list_execution_history(WS_A, 100)
            .unwrap()
    }
}

/// A saved chat session row.
pub(super) fn session(id: &str) -> AgentSession {
    AgentSession {
        id: id.to_owned(),
        title: format!("Session {id}"),
        target: Some(AgentTarget::Pinned {
            provider_id: "p".to_owned(),
            key_id: Some("k".to_owned()),
            model: "m".to_owned(),
        }),
        workspace_dir: None,
        permission_mode: ToolApproval::AutoReadOnly,
        reasoning_effort: None,
        disabled_tools: Vec::new(),
        disabled_skills: Vec::new(),
        web_enabled: false,
        title_locked: false,
        head_message_id: None,
        pinned_at_ms: None,
        archived_at_ms: None,
        deleted_at_ms: None,
        forked_from: None,
        forked_from_message_id: None,
        parent_session_id: None,
        parent_run_id: None,
        todos: Vec::new(),
        total_usage: HubUsage::default(),
        total_cost_micro_usd: 0,
        last_context_tokens: 0,
        spanbay_workspace_id: None,
        spanbay_environment_id: None,
        spanbay_workspace_name: None,
        notice: None,
        instructions: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

/// Inspects `args` with `tool` in the bound chat.
pub(super) fn inspect(
    tool: &dyn AgentTool,
    world: &World,
    args: Value,
) -> Result<CallInspection, AppError> {
    tool.inspect(&world.ctx(), &args)
}

/// The reason a call is a terminal hard block (`blocked`, no card), if it is one.
pub(super) fn blocked(tool: &dyn AgentTool, world: &World, args: Value) -> Option<String> {
    inspect(tool, world, args).expect("a valid call").hard_block
}

/// The hard-block reason of a call that must be refused, as the error it stands for.
pub(super) fn refused(tool: &dyn AgentTool, world: &World, args: Value) -> AppError {
    AppError::validation(blocked(tool, world, args).expect("the call must be hard-blocked"))
}

/// Calls `tool` with `args` in the bound chat.
pub(super) async fn call(tool: &dyn AgentTool, world: &World, args: Value) -> ToolOutput {
    let ctx = world.ctx();
    tool.call(&ctx, args).await
}

/// The text of a result.
pub(super) fn text(output: &ToolOutput) -> String {
    output.text_content()
}

/// The JSON a successful result carries.
pub(super) fn json_of(output: &ToolOutput) -> Value {
    assert!(!output.is_error, "unexpected error: {}", text(output));
    serde_json::from_str(&text(output)).expect("a JSON result")
}

/// An upstream that answers with raw bytes (custom headers, binary bodies); every request it
/// reads is recorded.
pub(super) async fn raw_server(response: Vec<u8>) -> (SocketAddr, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let response = response.clone();
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                if let Some(request) = read_request(&mut socket).await {
                    log.lock().unwrap().push(request);
                }
                let _ = socket.write_all(&response).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (addr, seen)
}

/// An HTTP/1.1 response with `headers` and `body`.
pub(super) fn http_response(headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut head = String::from("HTTP/1.1 200 OK\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    ));
    let mut out = head.into_bytes();
    out.extend_from_slice(body);
    out
}

/// A provider `id` that speaks OpenAI chat to `base_url`, with one enabled key and models.
pub(super) fn add_provider(state: &AppState, id: &str, base_url: &str, models: &[&str]) {
    llm_provider_save_impl(
        state,
        Provider {
            id: id.to_owned(),
            name: format!("Relay {id}"),
            preset_id: None,
            category: "custom".to_owned(),
            base_url: base_url.to_owned(),
            auth: AuthStyle::Bearer,
            endpoints: vec![ProviderEndpoint {
                protocol: Protocol::OpenaiChat,
                path_prefix: "/v1".to_owned(),
                auth: None,
                url_mode: Default::default(),
                enabled: true,
            }],
            models_endpoint: Some("/models".to_owned()),
            quirks: Quirks::default(),
            enabled: true,
            ..Default::default()
        },
    )
    .expect("provider");
    llm_key_save_impl(
        state,
        SaveProviderKeyInput {
            id: format!("k-{id}"),
            provider_id: id.to_owned(),
            name: "k".to_owned(),
            secret: Some(format!("sk-{id}-secret-0123456789")),
            enabled: true,
            position: 0,
            api_user_id: None,
            management_token: None,
            notes: None,
            group_name: None,
            protocols: None,
        },
    )
    .expect("key");
    for model in models {
        llm_model_save_impl(
            state,
            ProviderModel {
                provider_id: id.to_owned(),
                model_id: (*model).to_owned(),
                source: ModelSource::Manual,
                enabled: true,
                ..ProviderModel::default()
            },
        )
        .expect("model");
    }
}

/// Stores one gateway log row (a successful request now, unless `edit` changes it).
pub(super) fn log_row(world: &World, edit: impl FnOnce(&mut RequestLogEntry)) {
    let mut entry = RequestLogEntry {
        ts_ms: crate::app::current_time_ms(),
        protocol_in: "openai_chat".to_owned(),
        requested_model: "gpt-x".to_owned(),
        status: 200,
        outcome: "success".to_owned(),
        ..RequestLogEntry::default()
    };
    edit(&mut entry);
    insert_log(
        world.state.store().connection(),
        &LogWrite {
            entry: &entry,
            capture: None,
            rollup: None,
        },
    )
    .expect("log row");
}
