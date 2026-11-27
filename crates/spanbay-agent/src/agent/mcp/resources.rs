//! A server's resources as one read tool for the model.
//!
//! Single responsibility: [`parse_resources`] caps a `resources/list` answer (count, sizes,
//! hidden characters made visible) and [`McpResourceTool`] offers it to the run loop as
//! `mcp__<server>__resources`: without a `uri` it lists the resources, with one it reads it.
//! Resources are read-only by the protocol, but only a trusted server is believed: an untrusted
//! server's read asks once, every time, like its tools. The text is external data.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::adapter::{mcp_policy_key, mcp_tool_name, sanitized};
use super::{CONNECT_TIMEOUT, McpTransportConfig};
use crate::agent::model::AgentMcpServer;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolSource, ToolSpec,
};
use crate::agent::untrusted::sanitize_untrusted;
use spanbay_base::error::AppError;
use spanbay_llm::llm::hub::HubPart;

/// Resources kept from one listing.
const MAX_RESOURCES: usize = 200;
/// Longest URI kept; a longer one drops the entry.
const MAX_URI_BYTES: usize = 2 * 1024;
/// Longest name or description kept, in characters.
const MAX_LABEL_CHARS: usize = 300;
/// The server-side name the resource tool's policy key uses.
const RESOURCE_TOOL: &str = "resources";
/// Time one listing or read may take.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// One entry of `resources/list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpResource {
    /// `uri`.
    pub uri: String,
    /// `name` (or `title`), hidden characters made visible.
    pub name: String,
    /// `description`, hidden characters made visible; empty when absent.
    pub description: String,
    /// `mimeType`, when given.
    pub mime_type: Option<String>,
}

fn label(value: Option<&Value>) -> String {
    let text = value.and_then(Value::as_str).unwrap_or_default();
    sanitize_untrusted(text)
        .chars()
        .take(MAX_LABEL_CHARS)
        .collect()
}

/// The usable entries of a `resources/list` answer: those with a URI of at most 2 KiB, at most
/// [`MAX_RESOURCES`], in the server's order.
pub(super) fn parse_resources(items: &[Value]) -> Vec<McpResource> {
    items
        .iter()
        .filter_map(|item| {
            let uri = item.get("uri").and_then(Value::as_str)?;
            (uri.len() <= MAX_URI_BYTES && !uri.trim().is_empty()).then(|| McpResource {
                uri: sanitize_untrusted(uri).into_owned(),
                name: label(item.get("title").or_else(|| item.get("name"))),
                description: label(item.get("description")),
                mime_type: item
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .map(|mime| mime.chars().take(100).collect()),
            })
        })
        .take(MAX_RESOURCES)
        .collect()
}

/// The listing the model reads: one line per resource.
fn listing(resources: &[McpResource]) -> String {
    if resources.is_empty() {
        return "The server lists no resources.".to_owned();
    }
    let mut out = format!(
        "{} resource{} (read one by passing its uri):\n",
        resources.len(),
        if resources.len() == 1 { "" } else { "s" }
    );
    for resource in resources {
        out.push_str(&format!("- {}", resource.uri));
        if !resource.name.is_empty() {
            out.push_str(&format!(" — {}", resource.name));
        }
        if let Some(mime) = &resource.mime_type {
            out.push_str(&format!(" ({mime})"));
        }
        if !resource.description.is_empty() {
            out.push_str(&format!(": {}", resource.description));
        }
        out.push('\n');
    }
    out
}

/// The resources of one MCP server, callable by the run loop.
pub struct McpResourceTool {
    spec: ToolSpec,
    server_id: String,
    server_name: String,
    transport: McpTransportConfig,
    /// The server is not trusted: every read asks once.
    once_only: bool,
}

impl McpResourceTool {
    /// The resource tool of `server`; `transport` is its resolved config.
    #[must_use]
    pub fn new(server: &AgentMcpServer, transport: McpTransportConfig) -> Arc<Self> {
        let trusted = server.trusted;
        Arc::new(Self {
            spec: ToolSpec {
                name: mcp_tool_name(&server.name, RESOURCE_TOOL),
                description: format!(
                    "Lists the resources (files, records, documents) the MCP server {} offers, or reads one: call it without `uri` to list them, with a `uri` from the list to read it.",
                    sanitize_untrusted(&server.name)
                ),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "uri": { "type": "string", "description": "The resource to read; leave it out to list the resources." }
                    }
                }),
                read_only: trusted,
                destructive: false,
                source: ToolSource::Mcp {
                    server_id: server.id.clone(),
                },
                class: if trusted {
                    ToolClass::Read
                } else {
                    ToolClass::Network
                },
                parallel_safe: trusted,
            },
            server_id: server.id.clone(),
            server_name: server.name.clone(),
            transport,
            once_only: !trusted,
        })
    }
}

impl AgentTool for McpResourceTool {
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
                "the resource tool's arguments must be a JSON object",
            ));
        }
        if arguments
            .get("uri")
            .is_some_and(|uri| !uri.is_string() && !uri.is_null())
        {
            return Err(AppError::validation("`uri` must be a string"));
        }
        let always_ask = self
            .once_only
            .then(|| format!("the MCP server {} is not trusted", self.server_name));
        Ok(CallInspection {
            read_only: self.spec.read_only,
            once_only: self.once_only,
            ask_code: always_ask.is_some().then_some("mcp_untrusted"),
            always_ask,
            arg_subject: arguments
                .get("uri")
                .and_then(Value::as_str)
                .map(str::to_owned),
            policy_key: Some(mcp_policy_key(&self.server_id, RESOURCE_TOOL)),
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let Some(agent) = ctx.agent else {
                return ToolOutput::error("MCP servers are not available in this run");
            };
            let proxy = Some(ctx.settings.network.proxy_url.trim())
                .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case("direct"));
            let connection = match agent
                .mcp
                .connection(&self.server_id, &self.transport, proxy, CONNECT_TIMEOUT)
                .await
            {
                Ok(connection) => connection,
                Err(error) => return ToolOutput::error(error.message()),
            };
            let uri = arguments
                .get("uri")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|uri| !uri.is_empty());
            let parts = match uri {
                None => connection
                    .list_resources(READ_TIMEOUT)
                    .await
                    .map(|resources| vec![HubPart::text(listing(&resources))]),
                Some(uri) => connection
                    .read_resource(uri, READ_TIMEOUT, Some(ctx.cancel))
                    .await
                    .map(|result| result.parts),
            };
            match parts {
                Ok(parts) => ToolOutput {
                    parts: if parts.is_empty() {
                        vec![HubPart::text("(empty resource)")]
                    } else {
                        sanitized(parts)
                    },
                    is_error: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listings_are_capped_and_hidden_characters_shown() {
        let mut items = vec![
            json!({"uri": "file:///a.md", "name": "A\u{202e}gpj", "mimeType": "text/markdown", "description": "doc"}),
            json!({"name": "no uri"}),
            json!({"uri": "x".repeat(MAX_URI_BYTES + 1)}),
        ];
        items.extend((0..MAX_RESOURCES + 5).map(|n| json!({"uri": format!("db://row/{n}")})));
        let parsed = parse_resources(&items);
        assert_eq!(parsed.len(), MAX_RESOURCES);
        assert_eq!(parsed[0].name, "A⟦U+202E⟧gpj");
        assert_eq!(parsed[1].uri, "db://row/0");
        let text = listing(&parsed[..1]);
        assert!(text.contains("- file:///a.md — A⟦U+202E⟧gpj (text/markdown): doc"));
        assert_eq!(listing(&[]), "The server lists no resources.");
    }
}
