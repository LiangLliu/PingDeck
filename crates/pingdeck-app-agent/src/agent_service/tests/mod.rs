//! Agent use-case tests against real stores and temp folders. The loop itself is covered by
//! `agent/runner/tests.rs` over fakes; `run.rs` here drives whole runs through `AppState` against
//! a fake upstream.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use crate::agent::mcp::McpTransportConfig;
use crate::agent::model::{
    AgentExportFormat, AgentMcpServer, AgentMessage, AgentMessageMeta, AgentRole, AgentSession,
    AgentSessionInput, AgentSessionPatch, AgentTarget,
};
use crate::app::{AppState, settings_update_impl};
use crate::error::AppError;
use crate::llm::hub::HubPart;
use crate::settings::SettingsSection;

use super::{
    AgentPageRequest, agent_mcp_server_delete_impl, agent_mcp_server_list_impl,
    agent_mcp_server_save_impl, agent_session_create_impl, agent_session_delete_impl,
    agent_session_export_impl, agent_session_get_impl, agent_session_list_impl,
    agent_session_update_impl, agent_skill_import_impl, agent_skill_list_impl,
    agent_skill_remove_impl, agent_skill_set_enabled_impl,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "pingdeck-agent-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn state() -> AppState {
    AppState::open_in_memory().expect("state opens")
}

fn message(session_id: &str, id: &str, role: AgentRole, text: &str, at: i64) -> AgentMessage {
    AgentMessage {
        id: id.to_owned(),
        session_id: session_id.to_owned(),
        seq: 0,
        role,
        parts: vec![HubPart::text(text)],
        usage: None,
        model: None,
        error: None,
        run_id: None,
        parent_id: None,
        branch_index: 0,
        partial: false,
        cost_micro_usd: 0,
        cost_currency: None,
        meta: AgentMessageMeta::default(),
        branch_siblings: Vec::new(),
        attachments: Vec::new(),
        created_at_ms: at,
    }
}

fn session(state: &AppState, title: &str) -> AgentSession {
    agent_session_create_impl(
        state,
        AgentSessionInput {
            title: Some(title.to_owned()),
            ..AgentSessionInput::default()
        },
    )
    .expect("session creates")
}

fn append(state: &AppState, message: &AgentMessage) -> AgentMessage {
    state
        .store()
        .append_agent_message(message)
        .expect("message appends")
}

fn stdio_server(name: &str) -> AgentMcpServer {
    AgentMcpServer {
        id: String::new(),
        name: name.to_owned(),
        transport: McpTransportConfig::Stdio {
            command: "some-mcp-server".to_owned(),
            args: vec!["--flag".to_owned()],
            env: BTreeMap::new(),
            env_allow: Vec::new(),
            cwd: None,
        },
        enabled: true,
        trusted: false,
        updated_at_ms: 0,
    }
}

mod ask_user;
mod attachments;
mod changes;
mod config;
mod plan;
mod process_output;
mod queue;
mod roles;
mod run;
mod sessions;
mod streaming;
mod tasks;
mod unbound;
mod upstream;
