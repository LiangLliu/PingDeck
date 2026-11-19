//! The `read_output` tool: the full output of an earlier call of this chat, read back from the
//! file the runner kept when it cut the result.
//!
//! Single responsibility: page through, or search, a kept output. The handle is the call id; the
//! chat must have made the call and the core must have kept its output
//! (`agent_service::tool_output`), so another chat's handle is refused. Class `Internal`: it never
//! asks and Plan mode offers it. Its own result is never kept on disk, and every page is redacted
//! again before the model sees it.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::secret_guard::expand_needles;
use crate::agent::tool_output_files::{READ_OUTPUT, truncation_marker};
use crate::agent::tools::shell_meta::valid_call_id;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolRegistry, ToolSource,
    ToolSpec,
};
use crate::agent_service::agent_tool_output_read_impl;
use crate::app::AppState;
use crate::error::AppError;
use crate::secrets::redact_secrets;

/// Most matches one `grep` returns.
const MAX_MATCHES: usize = 200;
/// Lines of context around each match.
const CONTEXT_LINES: usize = 2;
/// Room left for the marker line after a page.
const MARKER_ROOM: usize = 200;
/// Bytes read per page from the file.
const PAGE: u32 = 64 * 1024;

/// Registers `read_output`.
pub fn register(registry: &mut ToolRegistry) {
    registry.register(Arc::new(ReadOutputTool::new()));
}

/// `read_output {handle, offset?, limit?, grep?}`.
pub struct ReadOutputTool {
    spec: ToolSpec,
}

impl ReadOutputTool {
    /// The tool with its static spec.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: ToolSpec {
                name: READ_OUTPUT.to_owned(),
                description: "Reads the rest of a tool result that was cut. Use the handle and offset from the `[output truncated: …]` line; `grep` searches the whole output instead (regex, up to 200 matches with 2 lines of context).".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "handle": {"type": "string", "description": "The handle from the truncation line"},
                        "offset": {"type": "integer", "minimum": 0, "description": "Byte offset to read from (default 0)"},
                        "limit": {"type": "integer", "minimum": 1, "description": "Bytes to read (default and most: what fits in one result)"},
                        "grep": {"type": "string", "description": "A regular expression; returns the matching lines with context instead of a page"}
                    },
                    "required": ["handle"]
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

impl Default for ReadOutputTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct Args {
    handle: String,
    offset: Option<u64>,
    limit: Option<u64>,
    grep: Option<String>,
}

fn parse(arguments: &Value) -> Result<Args, AppError> {
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| AppError::validation(format!("invalid arguments: {error}")))?;
    if !valid_call_id(args.handle.trim()) {
        return Err(AppError::validation("handle is not a valid output handle"));
    }
    Ok(args)
}

impl AgentTool for ReadOutputTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        parse(arguments)?;
        Ok(CallInspection {
            read_only: true,
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args = match parse(&arguments) {
                Ok(args) => args,
                Err(error) => return ToolOutput::error(error.message()),
            };
            let state = crate::agent_tools::app_state(ctx).clone();
            let (session, cap) = (ctx.session_id.to_owned(), ctx.result_cap);
            let read = tokio::task::spawn_blocking(move || read(&state, &session, &args, cap));
            match read.await {
                // Kept output is another tool's result (a page, a response, a command's
                // output): it reaches the model as external data, like the rest of it did.
                Ok(Ok(text)) => ToolOutput::text(text).external(),
                Ok(Err(error)) => ToolOutput::error(error.message()),
                Err(error) => ToolOutput::error(format!("reading the output failed: {error}")),
            }
        })
    }
}

/// A page of the kept output, or its matching lines, at most about `cap` bytes.
///
/// # Errors
/// What [`agent_tool_output_read_impl`] refuses (not this chat's call, nothing kept, cleared), or
/// an invalid `grep` pattern.
fn read(state: &AppState, session_id: &str, args: &Args, cap: usize) -> Result<String, AppError> {
    let handle = args.handle.trim();
    let room = cap.saturating_sub(MARKER_ROOM).max(1);
    let limit = args
        .limit
        .map_or(room, |limit| usize::try_from(limit).unwrap_or(usize::MAX))
        .clamp(1, room);
    let Some(pattern) = args.grep.as_deref() else {
        return page(state, session_id, handle, args.offset.unwrap_or(0), limit);
    };
    let regex = regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map_err(|error| AppError::validation(format!("grep is not a valid regex: {error}")))?;
    let mut text = String::new();
    let mut offset = 0;
    loop {
        let chunk = agent_tool_output_read_impl(state, session_id, handle, offset, PAGE)?;
        text.push_str(&chunk.text);
        if chunk.eof || chunk.next_offset <= offset {
            break;
        }
        offset = chunk.next_offset;
    }
    let found = grep_lines(&text, &regex, limit);
    let needles = expand_needles(state.secrets().active_redaction_needles());
    Ok(redact_secrets(&found, &needles))
}

/// A page of kept output `handle` from `offset` that fits a result of `cap` bytes (`fetch_url`
/// with `offset`).
///
/// # Errors
/// As [`agent_tool_output_read_impl`].
pub(crate) fn page_of(
    state: &AppState,
    session_id: &str,
    handle: &str,
    offset: u64,
    cap: usize,
) -> Result<String, AppError> {
    page(
        state,
        session_id,
        handle,
        offset,
        cap.saturating_sub(MARKER_ROOM).max(1),
    )
}

/// Up to `limit` bytes from `offset`, ending with a marker when more follows.
fn page(
    state: &AppState,
    session_id: &str,
    handle: &str,
    offset: u64,
    limit: usize,
) -> Result<String, AppError> {
    let mut text = String::new();
    let mut at = offset;
    let (mut next, mut total, mut eof);
    loop {
        let want = u32::try_from(limit - text.len()).unwrap_or(PAGE).min(PAGE);
        let chunk = agent_tool_output_read_impl(state, session_id, handle, at, want)?;
        text.push_str(&chunk.text);
        (next, total, eof) = (chunk.next_offset, chunk.total, chunk.eof);
        if eof || next <= at || text.len() >= limit {
            break;
        }
        at = next;
    }
    if !eof {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&truncation_marker(handle, next, total));
    }
    Ok(text)
}

/// The lines of `text` that match, numbered, with [`CONTEXT_LINES`] around each and `--`
/// between groups; at most [`MAX_MATCHES`] matches and about `limit` bytes.
fn grep_lines(text: &str, regex: &regex::Regex, limit: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let hits: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| regex.is_match(line))
        .map(|(index, _)| index)
        .take(MAX_MATCHES + 1)
        .collect();
    if hits.is_empty() {
        return "No line matches.".to_owned();
    }
    let shown = &hits[..hits.len().min(MAX_MATCHES)];
    let mut out = String::new();
    let mut printed_to: Option<usize> = None;
    for &hit in shown {
        let from = hit.saturating_sub(CONTEXT_LINES);
        let to = (hit + CONTEXT_LINES).min(lines.len() - 1);
        let start = match printed_to {
            Some(end) if end >= to => continue,
            Some(end) if from <= end + 1 => end + 1,
            Some(_) => {
                out.push_str("--\n");
                from
            }
            None => from,
        };
        for (index, line) in lines.iter().enumerate().take(to + 1).skip(start) {
            let mark = if shown.binary_search(&index).is_ok() {
                ':'
            } else {
                '-'
            };
            let row = format!("{}{mark} {line}\n", index + 1);
            if out.len() + row.len() > limit {
                out.push_str("[more matches were left out; narrow the pattern]");
                return out;
            }
            out.push_str(&row);
        }
        printed_to = Some(to);
    }
    if hits.len() > MAX_MATCHES {
        out.push_str(&format!("[only the first {MAX_MATCHES} matches are shown]"));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::agent::model::{AgentSessionInput, AgentToolCallRecord};
    use crate::agent::tool_output_files::{SpillCaps, write_spill};
    use crate::agent_service::agent_session_create_impl;

    fn kept(session_id: &str, call_id: &str) -> AgentToolCallRecord {
        AgentToolCallRecord {
            session_id: session_id.to_owned(),
            call_id: call_id.to_owned(),
            run_id: "r".to_owned(),
            message_id: None,
            name: "shell".to_owned(),
            class: "exec".to_owned(),
            state: "output-available".to_owned(),
            args_json: "{}".to_owned(),
            risk: "medium".to_owned(),
            decided_by: None,
            decision: None,
            decided_ms: None,
            started_ms: None,
            ended_ms: None,
            exit_code: None,
            sandbox: None,
            truncated: true,
            output_bytes: 0,
            spill_ref: Some(call_id.to_owned()),
            requested_ms: None,
        }
    }

    fn args(handle: &str, offset: Option<u64>, grep: Option<&str>) -> Args {
        Args {
            handle: handle.to_owned(),
            offset,
            limit: None,
            grep: grep.map(str::to_owned),
        }
    }

    // A handle reads the rest of this chat's kept output, page by page, redacted again on
    // the way out; another chat's handle is refused; an evicted file says it was cleared.
    #[test]
    fn a_handle_reads_the_rest_redacted_and_only_in_its_own_chat() {
        let dir = std::env::temp_dir().join(format!("pd-read-output-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state = AppState::open(dir.join("pingdeck.sqlite3")).unwrap();
        state.on_start().unwrap();
        let chat = |title: &str| {
            let input = AgentSessionInput {
                title: Some(title.to_owned()),
                ..AgentSessionInput::default()
            };
            agent_session_create_impl(&state, input).unwrap().id
        };
        let (owner, stranger) = (chat("owner"), chat("stranger"));
        let root = state.data_dirs().unwrap().root.join("tool-output");
        // Written before the value was known to be a secret: the read redacts it.
        let full = format!(
            "{}\nAuthorization: Bearer abcdefghijklmnop123456\nend\n",
            "a".repeat(150)
        );
        write_spill(&root, &owner, "call-1", &full, SpillCaps::DEFAULT, &[]).unwrap();
        state
            .store()
            .upsert_tool_call(&kept(&owner, "call-1"))
            .unwrap();

        let cap = MARKER_ROOM + 100;
        let first = read(&state, &owner, &args("call-1", None, None), cap).unwrap();
        let marker = truncation_marker("call-1", 100, full.len() as u64);
        assert_eq!(first, format!("{}\n{marker}", "a".repeat(100)));
        let rest = read(&state, &owner, &args("call-1", Some(100), None), 4096).unwrap();
        assert!(
            rest.starts_with(&"a".repeat(50)) && rest.ends_with("end\n"),
            "{rest}"
        );
        assert!(!rest.contains("abcdefghijklmnop123456"), "{rest}");
        let found = read(&state, &owner, &args("call-1", None, Some("^end$")), 4096).unwrap();
        assert!(
            found.contains("3: end") && !found.contains("abcdefghijklmnop123456"),
            "{found}"
        );

        let refused = read(&state, &stranger, &args("call-1", None, None), 4096).unwrap_err();
        assert!(matches!(refused, AppError::NotFound { .. }), "{refused:?}");
        crate::agent::tool_output_files::purge(&root, Some(&owner));
        let cleared = read(&state, &owner, &args("call-1", None, None), 4096).unwrap_err();
        assert_eq!(cleared.message(), crate::agent::tool_output_files::CLEARED);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
