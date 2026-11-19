//! The `read_attachment` tool: a text attachment of this chat read in ranges.
//!
//! Single responsibility: let the model read an attachment too large to be sent inline (over
//! 100 KB). It needs no Folder, reads only attachments linked to the current chat's messages and
//! never anything outside the attachment store. The first request names the size and line count;
//! output is numbered lines like `read_file`. Class `Internal`: it never asks for approval.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::attachments::count_lines;
use crate::agent::model::{AgentAttachment, AttachmentKind};
use crate::agent::tools::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolRegistry, ToolSource,
    ToolSpec,
};
use crate::app::AppState;
use crate::error::AppError;

/// Name of the tool.
pub const READ_ATTACHMENT: &str = "read_attachment";

/// Lines returned when the model gives no `limit`, and the most one call returns.
const MAX_LINES: usize = 2000;
/// Most bytes of text one call returns.
const MAX_BYTES: usize = 256 * 1024;

/// Registers `read_attachment`.
pub fn register(registry: &mut ToolRegistry) {
    registry.register(Arc::new(ReadAttachmentTool::new()));
}

/// `read_attachment {attachment, offset?, limit?}`.
pub struct ReadAttachmentTool {
    spec: ToolSpec,
}

impl ReadAttachmentTool {
    /// The tool with its static spec.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: ToolSpec {
                name: READ_ATTACHMENT.to_owned(),
                description: "Reads a text attachment of this chat in line ranges (for files too large to be shown in the message). The first call also reports the size and line count.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "attachment": {"type": "string", "description": "The attachment's name or id"},
                        "offset": {"type": "integer", "minimum": 1, "description": "First line to read (default 1)"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LINES, "description": "Lines to read (default and most 2000)"}
                    },
                    "required": ["attachment"]
                }),
                read_only: true,
                destructive: false,
                source: ToolSource::Builtin,
                class: ToolClass::Internal,
                parallel_safe: true,
            },
        }
    }
}

impl Default for ReadAttachmentTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct Args {
    attachment: String,
    offset: Option<u64>,
    limit: Option<u64>,
}

/// The attachment of the chat that `wanted` names: an id, else a unique name.
fn pick(candidates: Vec<AgentAttachment>, wanted: &str) -> Result<AgentAttachment, AppError> {
    let wanted = wanted.trim();
    if let Some(found) = candidates.iter().find(|a| a.id == wanted) {
        return Ok(found.clone());
    }
    let mut named: Vec<&AgentAttachment> = candidates.iter().filter(|a| a.name == wanted).collect();
    match named.len() {
        0 => Err(AppError::not_found(format!(
            "this chat has no attachment {wanted}"
        ))),
        1 => Ok(named.remove(0).clone()),
        _ => Err(AppError::validation(format!(
            "several attachments are named {wanted}; use one of the ids: {}",
            named
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn read(state: &AppState, session_id: &str, args: &Args) -> Result<ToolOutput, AppError> {
    let candidates = state
        .store()
        .list_session_attachments(session_id)
        .map_err(|error| AppError::database(error.to_string()))?;
    let attachment = pick(candidates, &args.attachment)?;
    if attachment.removed {
        return Err(AppError::validation(format!(
            "{} was removed from the store",
            attachment.name
        )));
    }
    if attachment.kind != AttachmentKind::Text {
        return Err(AppError::validation(format!(
            "{} is not a text attachment",
            attachment.name
        )));
    }
    let bytes = crate::agent_service::store_for(state)?.read(&attachment.sha256)?;
    let text = String::from_utf8_lossy(&bytes);
    let total = usize::try_from(count_lines(&bytes)).unwrap_or(usize::MAX);
    let offset = usize::try_from(args.offset.unwrap_or(1).max(1)).unwrap_or(usize::MAX);
    let limit = usize::try_from(args.limit.unwrap_or(MAX_LINES as u64))
        .unwrap_or(MAX_LINES)
        .clamp(1, MAX_LINES);
    let mut out = String::new();
    if offset == 1 {
        out.push_str(&format!(
            "[{}: {total} lines, {} bytes]\n",
            attachment.name, attachment.size_bytes
        ));
    }
    let mut shown = 0;
    let mut next = offset;
    for (index, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        if out.len() + line.len() > MAX_BYTES {
            break;
        }
        out.push_str(&format!("{}\t{line}\n", index + 1));
        shown += 1;
        next = index + 2;
    }
    if offset > total {
        out.push_str(&format!("[the attachment has {total} lines]\n"));
    } else if next <= total {
        out.push_str(&format!(
            "[showing lines {offset} to {}; continue with offset {next}]\n",
            next - 1
        ));
    } else if shown > 0 {
        out.push_str(&format!("[end of {}]\n", attachment.name));
    }
    Ok(ToolOutput::text(out))
}

impl AgentTool for ReadAttachmentTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        serde_json::from_value::<Args>(arguments.clone())
            .map_err(|error| AppError::validation(format!("invalid arguments: {error}")))?;
        Ok(CallInspection {
            read_only: true,
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        let state = crate::agent_tools::app_state(ctx).clone();
        let session_id = ctx.session_id.to_owned();
        Box::pin(async move {
            let args: Args = match serde_json::from_value(arguments) {
                Ok(args) => args,
                Err(error) => return ToolOutput::error(format!("invalid arguments: {error}")),
            };
            match tokio::task::spawn_blocking(move || read(&state, &session_id, &args)).await {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => ToolOutput::error(error.message()),
                Err(error) => ToolOutput::error(format!("the attachment read failed: {error}")),
            }
        })
    }
}
