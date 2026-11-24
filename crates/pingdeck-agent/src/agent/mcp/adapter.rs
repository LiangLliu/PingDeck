//! One MCP server tool as an [`AgentTool`].
//!
//! Single responsibility: give a server's tool a model-safe function name, classify a call from
//! the tool's annotations (believed only for trusted servers) and forward the call over the
//! shared [`McpManager`](super::McpManager) connection with the server's timeout (restarted by
//! each progress notification) and the run's cancel token. Which servers and tools a run gets is decided by the caller.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::Value;

use super::{CALL_TIMEOUT, CONNECT_TIMEOUT, McpTool, McpTransportConfig};
use crate::agent::model::AgentMcpServer;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolSource, ToolSpec,
};
use crate::agent::untrusted::sanitize_untrusted;
use pingdeck_base::error::AppError;
use pingdeck_llm::llm::hub::HubPart;

/// Longest function name a model API accepts.
const MAX_TOOL_NAME: usize = 64;
/// Characters of a too long name kept before its `_<8 hex>` hash suffix.
const HASHED_NAME_KEEP: usize = MAX_TOOL_NAME - 9;

fn clean_name_part(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// What a server name becomes inside a tool function name. Two names with the same key (`my.srv`,
/// `my_srv`) would share one tool-name prefix, so a saved server must have a key of its own.
#[must_use]
pub fn mcp_name_key(server_name: &str) -> String {
    clean_name_part(server_name)
}

/// 32-bit FNV-1a; stable across runs and platforms, unlike `DefaultHasher`.
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |hash, &byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

/// The function name of an MCP server's tool: `mcp__<server name>__<tool>`. A name over 64
/// characters keeps its head and ends in a hash of the whole name, so two long names never collide
/// by truncation.
#[must_use]
pub fn mcp_tool_name(server_name: &str, tool: &str) -> String {
    let name = format!(
        "mcp__{}__{}",
        clean_name_part(server_name),
        clean_name_part(tool)
    );
    if name.len() <= MAX_TOOL_NAME {
        return name;
    }
    format!(
        "{}_{:08x}",
        &name[..HASHED_NAME_KEEP],
        fnv1a(name.as_bytes())
    )
}

/// A prefix shared by every tool name of `server_name`, even the hashed ones.
#[must_use]
pub fn mcp_server_prefix(server_name: &str) -> String {
    let prefix = format!("mcp__{}__", clean_name_part(server_name));
    prefix[..prefix.len().min(HASHED_NAME_KEEP)].to_owned()
}

/// The per-tool policy key of `tool` on server `server_id`: `mcp:<server id>:<tool>`. It survives
/// a rename of the server, unlike the function name the model sees.
#[must_use]
pub fn mcp_policy_key(server_id: &str, tool: &str) -> String {
    format!("mcp:{server_id}:{tool}")
}

/// The `(read_only, destructive)` hints to act on, for the permission engine and the tool picker
/// alike: only a trusted server's own hints count, every other tool is treated as one that may
/// change things.
#[must_use]
pub fn effective_hints(server: &AgentMcpServer, tool: &McpTool) -> (bool, bool) {
    if server.trusted {
        (tool.read_only, tool.destructive)
    } else {
        (false, true)
    }
}

/// A tool of one MCP server, callable by the run loop.
pub struct McpToolAdapter {
    spec: ToolSpec,
    /// The server's id (the connection and approval key).
    server_id: String,
    /// The server-side tool name.
    tool: String,
    /// The resolved transport (secrets substituted); never printed.
    transport: McpTransportConfig,
    /// The server is not trusted: its calls are approved once, never by a stored rule or a chat
    /// grant, and they ask in every mode, Full access included.
    once_only: bool,
    /// The server's display name, for the approval card's reason.
    server_name: String,
    /// The tool's definition changed since the user pinned it on this trusted server: it is
    /// treated as untrusted until reviewed.
    drifted: bool,
}

impl McpToolAdapter {
    /// Adapts `tool` of `server`; `transport` is the server's config after
    /// [`resolve_transport`](super::resolve_transport). Only a trusted server's read-only and
    /// destructive hints count; every tool of another server may change things.
    #[must_use]
    pub fn new(server: &AgentMcpServer, transport: McpTransportConfig, tool: McpTool) -> Arc<Self> {
        Self::pinned(server, transport, tool, false)
    }

    /// [`Self::new`] for a tool of a trusted server whose definition no longer matches its pin
    /// (`drifted`): it gets no hints and asks every time, like an untrusted server's tool.
    #[must_use]
    pub fn pinned(
        server: &AgentMcpServer,
        transport: McpTransportConfig,
        tool: McpTool,
        drifted: bool,
    ) -> Arc<Self> {
        let trusted = server.trusted && !drifted;
        let (read_only, destructive) = if trusted {
            effective_hints(server, &tool)
        } else {
            (false, true)
        };
        Arc::new(Self {
            spec: ToolSpec {
                name: mcp_tool_name(&server.name, &tool.name),
                description: tool.description,
                input_schema: tool.input_schema,
                read_only,
                destructive,
                source: ToolSource::Mcp {
                    server_id: server.id.clone(),
                },
                // A trusted read-only tool reads like a file tool; any other can reach out or
                // change something.
                class: if read_only {
                    ToolClass::Read
                } else {
                    ToolClass::Network
                },
                parallel_safe: read_only,
            },
            server_id: server.id.clone(),
            tool: tool.name,
            transport,
            once_only: !trusted,
            server_name: server.name.clone(),
            drifted,
        })
    }

    /// The server-side tool name (what `tools/call` receives).
    #[must_use]
    pub fn tool_name(&self) -> &str {
        &self.tool
    }
}

/// `parts` with the hidden characters of their text made visible (`sanitize_untrusted`).
pub(super) fn sanitized(parts: Vec<HubPart>) -> Vec<HubPart> {
    parts
        .into_iter()
        .map(|part| match part {
            HubPart::Text {
                text,
                signature,
                origin,
                citations,
            } => HubPart::Text {
                text: sanitize_untrusted(&text).into_owned(),
                signature,
                origin,
                citations,
            },
            other => other,
        })
        .collect()
}

impl AgentTool for McpToolAdapter {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        if !arguments.is_object() && !arguments.is_null() {
            return Err(AppError::validation(
                "MCP tool arguments must be a JSON object",
            ));
        }
        // An untrusted server's call is a forced Ask: Full access only skips what is trusted.
        let always_ask = self.once_only.then(|| {
            if self.drifted {
                format!(
                    "the definition of this tool of the MCP server {} changed since it was \
                     reviewed",
                    self.server_name
                )
            } else {
                format!("the MCP server {} is not trusted", self.server_name)
            }
        });
        Ok(CallInspection {
            read_only: self.spec.read_only,
            destructive: self.spec.destructive,
            once_only: self.once_only,
            ask_code: always_ask.is_some().then_some("mcp_untrusted"),
            always_ask,
            policy_key: Some(mcp_policy_key(&self.server_id, &self.tool)),
            ..CallInspection::default()
        })
    }

    #[allow(clippy::expect_used)] // the run always supplies the runtime: the None arm cannot happen and a tool error would hide the bug
    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let connection = match ctx
                .agent
                .expect("the MCP adapter is only called from a run with a runtime")
                .mcp
                .connection(
                    &self.server_id,
                    &self.transport,
                    Some(ctx.settings.network.proxy_url.trim())
                        .filter(|proxy| !proxy.is_empty() && !proxy.eq_ignore_ascii_case("direct")),
                    CONNECT_TIMEOUT,
                )
                .await
            {
                Ok(connection) => connection,
                Err(error) => return ToolOutput::error(error.message()),
            };
            // The server's own timeout when the user set one (`ai.agent.mcp_call_timeout_s`).
            let timeout = ctx
                .settings
                .ai
                .agent
                .mcp_call_timeout_s
                .get(&self.server_id)
                .map_or(CALL_TIMEOUT, |secs| Duration::from_secs(u64::from(*secs)));
            match connection
                .call_tool(&self.tool, arguments, timeout, Some(ctx.cancel))
                .await
            {
                Ok(result) => ToolOutput {
                    parts: if result.parts.is_empty() {
                        vec![HubPart::text("(no output)")]
                    } else {
                        sanitized(result.parts)
                    },
                    is_error: result.is_error,
                    meta: None,
                    external: true,
                    cap: None,
                    exit_code: None,
                },
                Err(error) => ToolOutput::error(error.message()),
            }
        })
    }
}
