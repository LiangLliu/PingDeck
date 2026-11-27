//! Pure-Rust presentation formatter for AI agent tool calls and results.
//!
//! Decouples the LLM wire protocol (`<external_data>` envelopes, `{"error":{"code":"..."}}`
//! prefixes, raw JSON arguments, `{line:>6}\t` numbering, `[Showing lines ...]` nudges) from the
//! human-facing UI so every tool call renders as one clean command line by default and expands
//! into a structured, line-bounded monospace snippet or diff on demand.

use serde_json::Value;
use spanbay_core::agent::untrusted::{strip_coded_error_prefix, strip_external_envelope};

/// Maximum number of whole lines rendered inline when expanding a tool call.
pub const INLINE_PREVIEW_MAX_LINES: usize = 14;
/// Maximum characters rendered on a single inline preview line before ellipsizing.
const INLINE_PREVIEW_MAX_LINE_CHARS: usize = 240;
/// Maximum characters for a compact raw-arguments block (MCP / unknown tools only).
const RAW_ARGS_MAX_CHARS: usize = 600;
/// Maximum characters for plain-text inline fallback bodies.
const PLAIN_TEXT_MAX_CHARS: usize = 1_200;

/// High-level functional family of a tool call used to pick icons, labels, and detail layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    FileRead,
    FileSearch,
    FileEdit,
    Shell,
    SpanBayApi,
    Web,
    Subagent,
    Planning,
    OutputRead,
    Mcp,
    Other,
}

/// Compact inline metric rendered on the collapsed one-line tool row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolInlineMetric {
    LineRange { start: usize, end: usize },
    Lines(usize),
    Matches(usize),
    Files(usize),
    Entries(usize),
    HttpStatus(u16),
    DiffStats { added: u32, removed: u32 },
    Bytes(u64),
}

/// One monospace preview line with an optional source line number for the left gutter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodePreviewLine {
    pub line_no: Option<usize>,
    pub text: String,
}

/// Structured detail body rendered inside an expanded tool call card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPreviewBody {
    Empty,
    /// Monospace line-bounded snippet (head for file/search tools, tail for shell output).
    CodeLines {
        lines: Vec<CodePreviewLine>,
        omitted_above: usize,
        omitted_below: usize,
        /// True when `read_file` itself reported additional unread lines in the file.
        file_has_more: bool,
    },
    /// Unified diff block.
    Diff {
        text: String,
    },
    /// Short human-readable confirmation or error text.
    PlainText {
        text: String,
    },
}

/// Complete render-ready presentation of one tool call step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTracePresentation {
    pub category: ToolCategory,
    /// Primary target shown next to the action verb (file path, command, query, URL, etc.).
    pub target: String,
    /// Optional structured result metric shown inline on the collapsed row.
    pub metric: Option<ToolInlineMetric>,
    /// Workspace file or directory path for Copy Path / Open Externally actions.
    pub file_path: Option<String>,
    /// Full shell command line shown inside expanded `shell` details.
    pub command_line: Option<String>,
    /// Compact key-value metadata chips shown in expanded detail (cwd, glob, slice, etc.).
    pub meta_chips: Vec<(String, String)>,
    /// Formatted raw JSON arguments (populated only for MCP / unknown tools).
    pub raw_args_display: Option<String>,
    /// True when the stored model payload was wrapped in an `<external_data>` fence.
    pub external_isolated: bool,
    /// Machine error code stripped from `{"error":{"code":"..."}}`, if present.
    pub error_code: Option<String>,
    /// Cleaned full output text (without `<external_data>` or machine error header), ready for
    /// the full-output preview modal or clipboard copy.
    pub cleaned_full_output: Option<String>,
    /// Structured inline preview shown when the step is expanded.
    pub preview: ToolPreviewBody,
    /// True when the full-output modal offers more content than the inline preview (either
    /// because the inline preview omitted lines/characters or because the core spilled to disk).
    pub can_preview_full: bool,
}

/// Classifies a tool name into its display [`ToolCategory`].
#[must_use]
pub fn classify_tool(name: &str) -> ToolCategory {
    if name.starts_with("mcp__") {
        return ToolCategory::Mcp;
    }
    match name {
        "read_file" | "read_attachment" | "read_skill" => ToolCategory::FileRead,
        "grep" | "glob" | "list_dir" => ToolCategory::FileSearch,
        "edit_file" | "write_file" | "delete_file" | "move_file" => ToolCategory::FileEdit,
        "shell" | "shell_output" | "shell_kill" => ToolCategory::Shell,
        "send_request" | "get_request" | "list_requests" | "create_request" | "get_trace"
        | "list_environments" | "run_probe" | "query_gateway_log" => ToolCategory::SpanBayApi,
        "web_search" | "fetch_url" => ToolCategory::Web,
        "task" => ToolCategory::Subagent,
        "todo_write" | "ask_user" | "submit_plan" => ToolCategory::Planning,
        "read_output" => ToolCategory::OutputRead,
        _ => ToolCategory::Other,
    }
}

/// Formats an `mcp__<server>__<tool>` wire name as `<server> / <tool>`.
#[must_use]
pub fn format_mcp_tool_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some(format!("{server} / {tool}"))
}

/// Builds the complete presentation model for one tool call row and its expanded detail.
#[must_use]
pub fn format_tool_trace(
    name: &str,
    args_json: &str,
    raw_output: Option<&str>,
    is_error: bool,
    spilled_to_disk: bool,
    file_diff_stats: Option<(u32, u32)>,
) -> ToolTracePresentation {
    let category = classify_tool(name);
    let parsed_args = serde_json::from_str::<Value>(args_json).ok();
    let args_obj = parsed_args.as_ref().and_then(Value::as_object);

    let ExtractedArgs {
        target,
        file_path,
        command_line,
        mut meta_chips,
    } = extract_arg_presentation(name, category, args_json, args_obj);

    let raw_args_display = match category {
        ToolCategory::Mcp | ToolCategory::Other => {
            format_compact_json_args(args_json, parsed_args.as_ref())
        }
        _ => None,
    };

    let Some(raw) = raw_output.filter(|s| !s.trim().is_empty()) else {
        let metric =
            file_diff_stats.map(|(added, removed)| ToolInlineMetric::DiffStats { added, removed });
        return ToolTracePresentation {
            category,
            target,
            metric,
            file_path,
            command_line,
            meta_chips,
            raw_args_display,
            external_isolated: false,
            error_code: None,
            cleaned_full_output: None,
            preview: ToolPreviewBody::Empty,
            can_preview_full: spilled_to_disk,
        };
    };

    let (unwrapped, external_isolated) = strip_external_envelope(raw);
    let (error_code_opt, clean_slice) = strip_coded_error_prefix(unwrapped.as_ref());
    let error_code = error_code_opt.map(str::to_owned);
    let clean_trimmed = clean_slice.trim_matches(['\r', '\n']);

    if is_error {
        let (preview, truncated_inline) = build_plain_or_tail_error(clean_trimmed, category);
        return ToolTracePresentation {
            category,
            target,
            metric: None,
            file_path,
            command_line,
            meta_chips,
            raw_args_display,
            external_isolated,
            error_code,
            cleaned_full_output: Some(clean_trimmed.to_owned()),
            preview,
            can_preview_full: spilled_to_disk || truncated_inline,
        };
    }

    if looks_like_diff(clean_trimmed) {
        let total_lines = clean_trimmed.lines().count();
        let metric =
            file_diff_stats.map(|(added, removed)| ToolInlineMetric::DiffStats { added, removed });
        return ToolTracePresentation {
            category,
            target,
            metric,
            file_path,
            command_line,
            meta_chips,
            raw_args_display,
            external_isolated,
            error_code,
            cleaned_full_output: Some(clean_trimmed.to_owned()),
            preview: ToolPreviewBody::Diff {
                text: clean_trimmed.to_owned(),
            },
            can_preview_full: spilled_to_disk || total_lines > 18,
        };
    }

    match category {
        ToolCategory::FileRead => {
            let parsed = parse_numbered_file_output(clean_trimmed);
            let metric = parsed.line_range_metric();
            let raw_bytes = parsed.plain_full.len() as u64;
            if raw_bytes > 0 {
                meta_chips.push(("size".to_owned(), format_human_bytes(raw_bytes)));
            }
            let (preview, truncated_inline) = build_head_code_preview(
                &parsed.lines,
                parsed.file_has_more,
                INLINE_PREVIEW_MAX_LINES,
            );
            ToolTracePresentation {
                category,
                target,
                metric,
                file_path,
                command_line,
                meta_chips,
                raw_args_display,
                external_isolated,
                error_code,
                cleaned_full_output: Some(parsed.plain_full),
                preview,
                can_preview_full: spilled_to_disk || truncated_inline,
            }
        }
        ToolCategory::FileSearch => {
            let (metric, display_lines) = parse_search_or_list_output(name, clean_trimmed);
            let (preview, truncated_inline) =
                build_head_code_preview(&display_lines, false, INLINE_PREVIEW_MAX_LINES);
            ToolTracePresentation {
                category,
                target,
                metric,
                file_path,
                command_line,
                meta_chips,
                raw_args_display,
                external_isolated,
                error_code,
                cleaned_full_output: Some(clean_trimmed.to_owned()),
                preview,
                can_preview_full: spilled_to_disk || truncated_inline,
            }
        }
        ToolCategory::FileEdit => {
            let metric = file_diff_stats
                .map(|(added, removed)| ToolInlineMetric::DiffStats { added, removed });
            let (preview, truncated_inline) = build_plain_text_preview(clean_trimmed);
            ToolTracePresentation {
                category,
                target,
                metric,
                file_path,
                command_line,
                meta_chips,
                raw_args_display,
                external_isolated,
                error_code,
                cleaned_full_output: Some(clean_trimmed.to_owned()),
                preview,
                can_preview_full: spilled_to_disk || truncated_inline,
            }
        }
        ToolCategory::Shell => {
            let lines: Vec<CodePreviewLine> = clean_trimmed
                .lines()
                .map(|l| CodePreviewLine {
                    line_no: None,
                    text: l.to_owned(),
                })
                .collect();
            let metric = (!lines.is_empty()).then_some(ToolInlineMetric::Lines(lines.len()));
            let (preview, truncated_inline) =
                build_tail_code_preview(&lines, INLINE_PREVIEW_MAX_LINES);
            ToolTracePresentation {
                category,
                target,
                metric,
                file_path,
                command_line,
                meta_chips,
                raw_args_display,
                external_isolated,
                error_code,
                cleaned_full_output: Some(clean_trimmed.to_owned()),
                preview,
                can_preview_full: spilled_to_disk || truncated_inline,
            }
        }
        ToolCategory::SpanBayApi => {
            let (formatted, status_metric) = format_api_output(clean_trimmed);
            let lines: Vec<CodePreviewLine> = formatted
                .lines()
                .map(|l| CodePreviewLine {
                    line_no: None,
                    text: l.to_owned(),
                })
                .collect();
            let (preview, truncated_inline) =
                build_head_code_preview(&lines, false, INLINE_PREVIEW_MAX_LINES);
            ToolTracePresentation {
                category,
                target,
                metric: status_metric,
                file_path,
                command_line,
                meta_chips,
                raw_args_display,
                external_isolated,
                error_code,
                cleaned_full_output: Some(formatted),
                preview,
                can_preview_full: spilled_to_disk || truncated_inline,
            }
        }
        ToolCategory::Web | ToolCategory::OutputRead | ToolCategory::Mcp | ToolCategory::Other => {
            let formatted = maybe_pretty_json(clean_trimmed);
            let lines: Vec<CodePreviewLine> = formatted
                .lines()
                .map(|l| CodePreviewLine {
                    line_no: None,
                    text: l.to_owned(),
                })
                .collect();
            let metric = match category {
                ToolCategory::Web => Some(ToolInlineMetric::Bytes(clean_trimmed.len() as u64)),
                _ => (!lines.is_empty()).then_some(ToolInlineMetric::Lines(lines.len())),
            };
            let (preview, truncated_inline) =
                build_head_code_preview(&lines, false, INLINE_PREVIEW_MAX_LINES);
            ToolTracePresentation {
                category,
                target,
                metric,
                file_path,
                command_line,
                meta_chips,
                raw_args_display,
                external_isolated,
                error_code,
                cleaned_full_output: Some(formatted),
                preview,
                can_preview_full: spilled_to_disk || truncated_inline,
            }
        }
        ToolCategory::Subagent | ToolCategory::Planning => {
            if name == "todo_write"
                && let Some((todo_metric, todo_lines, todo_full)) = parse_todo_write_args(args_obj)
            {
                let (preview, truncated_inline) =
                    build_head_code_preview(&todo_lines, false, INLINE_PREVIEW_MAX_LINES);
                return ToolTracePresentation {
                    category,
                    target,
                    metric: Some(todo_metric),
                    file_path,
                    command_line,
                    meta_chips,
                    raw_args_display,
                    external_isolated,
                    error_code,
                    cleaned_full_output: Some(todo_full),
                    preview,
                    can_preview_full: spilled_to_disk || truncated_inline,
                };
            }
            let (preview, truncated_inline) = build_plain_text_preview(clean_trimmed);
            ToolTracePresentation {
                category,
                target,
                metric: None,
                file_path,
                command_line,
                meta_chips,
                raw_args_display,
                external_isolated,
                error_code,
                cleaned_full_output: Some(clean_trimmed.to_owned()),
                preview,
                can_preview_full: spilled_to_disk || truncated_inline,
            }
        }
    }
}

/// Cleans raw spilled or stored tool output for the full-output viewer modal (stripping internal
/// `<external_data>` tags and machine error prefix, and normalizing `read_file` line-number tabs).
#[must_use]
pub fn clean_tool_output_for_modal(name: &str, raw: &str) -> String {
    let (unwrapped, _) = strip_external_envelope(raw);
    let (_, slice) = strip_coded_error_prefix(unwrapped.as_ref());
    let trimmed = slice.trim_matches(['\r', '\n']);
    if classify_tool(name) == ToolCategory::FileRead {
        parse_numbered_file_output(trimmed).plain_full
    } else {
        maybe_pretty_json(trimmed)
    }
}

/// Formats a byte count compactly (`512 B`, `2.4 KB`, `1.2 MB`).
#[must_use]
pub fn format_human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

struct ExtractedArgs {
    target: String,
    file_path: Option<String>,
    command_line: Option<String>,
    meta_chips: Vec<(String, String)>,
}

impl ExtractedArgs {
    fn simple(target: String, meta_chips: Vec<(String, String)>) -> Self {
        Self {
            target,
            file_path: None,
            command_line: None,
            meta_chips,
        }
    }
}

fn extract_arg_presentation(
    name: &str,
    category: ToolCategory,
    args_json: &str,
    obj: Option<&serde_json::Map<String, Value>>,
) -> ExtractedArgs {
    let mut chips = Vec::new();
    let get_str = |key: &str| obj.and_then(|m| m.get(key)).and_then(Value::as_str);
    let get_u64 = |key: &str| obj.and_then(|m| m.get(key)).and_then(Value::as_u64);

    match name {
        "read_file" => {
            let path = get_str("path")
                .or_else(|| get_str("file_path"))
                .unwrap_or("");
            if let Some(off) = get_u64("offset") {
                if let Some(lim) = get_u64("limit") {
                    chips.push((
                        "range".to_owned(),
                        format!("{off}–{}", off.saturating_add(lim).saturating_sub(1)),
                    ));
                } else {
                    chips.push(("offset".to_owned(), off.to_string()));
                }
            } else if let Some(lim) = get_u64("limit") {
                chips.push(("limit".to_owned(), lim.to_string()));
            }
            return ExtractedArgs {
                target: truncate_chars(path, 88),
                file_path: (!path.is_empty()).then(|| path.to_owned()),
                command_line: None,
                meta_chips: chips,
            };
        }
        "read_attachment" => {
            let att = get_str("attachment")
                .or_else(|| get_str("name"))
                .unwrap_or("");
            return ExtractedArgs::simple(truncate_chars(att, 88), chips);
        }
        "read_skill" => {
            let skill = get_str("name").unwrap_or("");
            let sub = get_str("path");
            let label = match sub {
                Some(p) if !p.is_empty() => format!("{skill} ({p})"),
                _ => skill.to_owned(),
            };
            return ExtractedArgs::simple(truncate_chars(&label, 88), chips);
        }
        "edit_file" | "write_file" => {
            let path = get_str("path")
                .or_else(|| get_str("file_path"))
                .unwrap_or("");
            return ExtractedArgs {
                target: truncate_chars(path, 88),
                file_path: (!path.is_empty()).then(|| path.to_owned()),
                command_line: None,
                meta_chips: chips,
            };
        }
        "delete_file" => {
            let path = get_str("path").unwrap_or("");
            return ExtractedArgs::simple(truncate_chars(path, 88), chips);
        }
        "move_file" => {
            let from = get_str("from").unwrap_or("");
            let to = get_str("to").unwrap_or("");
            let label = if !from.is_empty() && !to.is_empty() {
                format!("{from} → {to}")
            } else {
                to.to_owned()
            };
            return ExtractedArgs {
                target: truncate_chars(&label, 88),
                file_path: (!to.is_empty()).then(|| to.to_owned()),
                command_line: None,
                meta_chips: chips,
            };
        }
        "list_dir" => {
            let path = get_str("path").unwrap_or(".");
            return ExtractedArgs {
                target: truncate_chars(path, 88),
                file_path: Some(path.to_owned()),
                command_line: None,
                meta_chips: chips,
            };
        }
        "glob" => {
            let pattern = get_str("pattern").unwrap_or("");
            if let Some(p) = get_str("path").filter(|p| !p.is_empty() && *p != ".") {
                chips.push(("dir".to_owned(), p.to_owned()));
            }
            return ExtractedArgs::simple(truncate_chars(pattern, 88), chips);
        }
        "grep" => {
            let pattern = get_str("pattern").unwrap_or("");
            let dir = get_str("path").filter(|p| !p.is_empty() && *p != ".");
            let target = match dir {
                Some(d) => format!("\"{pattern}\" ({d})"),
                None if !pattern.is_empty() => format!("\"{pattern}\""),
                None => String::new(),
            };
            if let Some(g) = get_str("glob").filter(|g| !g.is_empty()) {
                chips.push(("glob".to_owned(), g.to_owned()));
            }
            if obj
                .and_then(|m| m.get("case_insensitive"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                chips.push(("flags".to_owned(), "-i".to_owned()));
            }
            return ExtractedArgs::simple(truncate_chars(&target, 88), chips);
        }
        "shell" => {
            let cmd = get_str("command")
                .or_else(|| get_str("cmd"))
                .unwrap_or("")
                .trim();
            let first_line = cmd.lines().next().unwrap_or("");
            if let Some(cwd) = get_str("cwd").filter(|c| !c.is_empty() && *c != ".") {
                chips.push(("cwd".to_owned(), cwd.to_owned()));
            }
            if obj
                .and_then(|m| m.get("background"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                chips.push(("mode".to_owned(), "background".to_owned()));
            }
            return ExtractedArgs {
                target: truncate_chars(first_line, 88),
                file_path: None,
                command_line: (!cmd.is_empty()).then(|| cmd.to_owned()),
                meta_chips: chips,
            };
        }
        "shell_output" | "shell_kill" => {
            let pid = get_str("process_id")
                .or_else(|| get_str("id"))
                .unwrap_or("");
            return ExtractedArgs::simple(truncate_chars(pid, 88), chips);
        }
        "send_request" | "create_request" => {
            let method = get_str("method").unwrap_or("").to_ascii_uppercase();
            let url = get_str("url")
                .or_else(|| get_str("name"))
                .or_else(|| get_str("request_id"))
                .unwrap_or("");
            let label = if !method.is_empty() && !url.is_empty() {
                format!("{method} {url}")
            } else {
                url.to_owned()
            };
            if let Some(env) = get_str("environment").filter(|e| !e.is_empty()) {
                chips.push(("env".to_owned(), env.to_owned()));
            }
            return ExtractedArgs::simple(truncate_chars(&label, 88), chips);
        }
        "web_search" => {
            let q = get_str("query").unwrap_or("");
            let target = if q.is_empty() {
                String::new()
            } else {
                format!("\"{q}\"")
            };
            return ExtractedArgs::simple(truncate_chars(&target, 88), chips);
        }
        "fetch_url" => {
            let url = get_str("url").unwrap_or("");
            return ExtractedArgs::simple(truncate_chars(url, 88), chips);
        }
        "read_output" => {
            let handle = get_str("handle").unwrap_or("");
            if let Some(g) = get_str("grep").filter(|g| !g.is_empty()) {
                chips.push(("grep".to_owned(), g.to_owned()));
            }
            return ExtractedArgs::simple(truncate_chars(handle, 88), chips);
        }
        "todo_write" => {
            let active = obj
                .and_then(|m| m.get("todos"))
                .and_then(Value::as_array)
                .and_then(|arr| {
                    arr.iter()
                        .find(|t| t.get("status").and_then(Value::as_str) == Some("in_progress"))
                        .or_else(|| arr.first())
                        .and_then(|t| t.get("content").and_then(Value::as_str))
                })
                .unwrap_or("");
            return ExtractedArgs::simple(truncate_chars(active, 88), chips);
        }
        _ => {}
    }

    // Generic fallback for remaining SpanBay tools, MCP tools, and custom tools.
    if let Some(m) = obj {
        for key in [
            "description",
            "question",
            "title",
            "name",
            "request_id",
            "history_id",
            "provider_id",
            "url",
            "query",
            "pattern",
            "path",
            "file_path",
            "cmd",
            "command",
        ] {
            if let Some(s) = m.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                return ExtractedArgs::simple(truncate_chars(s, 88), chips);
            }
        }
    }
    let fallback = match category {
        ToolCategory::Mcp | ToolCategory::Other => {
            let trimmed = args_json.trim();
            if trimmed == "{}" {
                String::new()
            } else {
                truncate_chars(trimmed, 72)
            }
        }
        _ => String::new(),
    };
    ExtractedArgs::simple(fallback, chips)
}

fn parse_todo_write_args(
    obj: Option<&serde_json::Map<String, Value>>,
) -> Option<(ToolInlineMetric, Vec<CodePreviewLine>, String)> {
    let arr = obj?.get("todos")?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(arr.len());
    for item in arr {
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        let content = item.get("content").and_then(Value::as_str).unwrap_or("");
        let summary = item
            .get("summary")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        let glyph = match status {
            "completed" => "✓",
            "in_progress" => "●",
            "failed" | "cancelled" => "✕",
            "blocked" => "⊘",
            _ => "○",
        };
        let text = match summary {
            Some(s) => format!("{glyph} {content} — {s}"),
            None => format!("{glyph} {content}"),
        };
        lines.push(CodePreviewLine {
            line_no: None,
            text,
        });
    }
    let full = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Some((ToolInlineMetric::Entries(arr.len()), lines, full))
}

fn format_compact_json_args(args_json: &str, parsed: Option<&Value>) -> Option<String> {
    let trimmed = args_json.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return None;
    }
    let text = match parsed {
        Some(v) => serde_json::to_string_pretty(v).unwrap_or_else(|_| trimmed.to_owned()),
        None => trimmed.to_owned(),
    };
    Some(truncate_chars(&text, RAW_ARGS_MAX_CHARS))
}

struct ParsedFileOutput {
    lines: Vec<CodePreviewLine>,
    plain_full: String,
    file_has_more: bool,
}

impl ParsedFileOutput {
    fn line_range_metric(&self) -> Option<ToolInlineMetric> {
        let first = self.lines.first().and_then(|l| l.line_no);
        let last = self.lines.last().and_then(|l| l.line_no);
        match (first, last) {
            (Some(start), Some(end)) if end >= start => {
                Some(ToolInlineMetric::LineRange { start, end })
            }
            _ if !self.lines.is_empty() => Some(ToolInlineMetric::Lines(self.lines.len())),
            _ => None,
        }
    }
}

fn parse_numbered_file_output(clean: &str) -> ParsedFileOutput {
    let raw_lines: Vec<&str> = clean.lines().collect();
    let mut end_idx = raw_lines.len();
    let mut file_has_more = false;

    // Strip the LLM-only continuation hint appended by `fs/ops.rs::read_text`.
    while end_idx > 0 && raw_lines[end_idx - 1].trim().is_empty() {
        end_idx -= 1;
    }
    if end_idx > 0 && is_read_file_more_marker(raw_lines[end_idx - 1]) {
        file_has_more = true;
        end_idx -= 1;
        while end_idx > 0 && raw_lines[end_idx - 1].trim().is_empty() {
            end_idx -= 1;
        }
    }
    let slice = &raw_lines[..end_idx];

    let mut parsed_lines = Vec::with_capacity(slice.len());
    let mut numbered_hits = 0usize;
    for line in slice {
        if let Some((num, rest)) = parse_numbered_line(line) {
            numbered_hits += 1;
            parsed_lines.push(CodePreviewLine {
                line_no: Some(num),
                text: rest.to_owned(),
            });
        } else {
            parsed_lines.push(CodePreviewLine {
                line_no: None,
                text: (*line).to_owned(),
            });
        }
    }

    // Only treat as numbered when lines actually came from `{line_no:>6}\t{text}` formatting.
    let is_numbered = !slice.is_empty() && numbered_hits * 2 >= slice.len();
    if !is_numbered {
        parsed_lines = slice
            .iter()
            .map(|l| CodePreviewLine {
                line_no: None,
                text: (*l).to_owned(),
            })
            .collect();
    }

    let plain_full = parsed_lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    ParsedFileOutput {
        lines: parsed_lines,
        plain_full,
        file_has_more,
    }
}

fn is_read_file_more_marker(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("[Showing lines ") && t.ends_with(']')
}

fn parse_numbered_line(line: &str) -> Option<(usize, &str)> {
    let (lhs, rhs) = line.split_once('\t')?;
    let trimmed = lhs.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let num = trimmed.parse::<usize>().ok()?;
    Some((num, rhs))
}

fn parse_search_or_list_output(
    name: &str,
    clean: &str,
) -> (Option<ToolInlineMetric>, Vec<CodePreviewLine>) {
    let mut raw_lines: Vec<&str> = clean.lines().collect();
    // Exclude trailing bracketed budget notes like `[Showing the first 100 of ...]` from counts.
    let count_lines: Vec<&str> = raw_lines
        .iter()
        .copied()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !(t.starts_with('[') && t.ends_with(']'))
        })
        .collect();

    match name {
        "grep" => {
            if clean.starts_with("No matches") {
                return (
                    Some(ToolInlineMetric::Matches(0)),
                    vec![CodePreviewLine {
                        line_no: None,
                        text: clean.to_owned(),
                    }],
                );
            }
            let lines = raw_lines
                .into_iter()
                .map(|l| CodePreviewLine {
                    line_no: None,
                    text: l.to_owned(),
                })
                .collect();
            (Some(ToolInlineMetric::Matches(count_lines.len())), lines)
        }
        "glob" => {
            if clean.starts_with("No files match") {
                return (
                    Some(ToolInlineMetric::Files(0)),
                    vec![CodePreviewLine {
                        line_no: None,
                        text: clean.to_owned(),
                    }],
                );
            }
            let lines = raw_lines
                .into_iter()
                .map(|l| CodePreviewLine {
                    line_no: None,
                    text: l.to_owned(),
                })
                .collect();
            (Some(ToolInlineMetric::Files(count_lines.len())), lines)
        }
        "list_dir" => {
            // `list_dir` starts with `{rel}/ ({total} entries)` or `{rel}/ is empty`.
            if clean.ends_with(" is empty") {
                return (
                    Some(ToolInlineMetric::Entries(0)),
                    vec![CodePreviewLine {
                        line_no: None,
                        text: clean.to_owned(),
                    }],
                );
            }
            let header_entries = raw_lines
                .first()
                .and_then(|first| parse_list_dir_header_count(first));
            if header_entries.is_some() && raw_lines.len() > 1 {
                raw_lines.remove(0);
            }
            let count = header_entries.unwrap_or(count_lines.len());
            let lines = raw_lines
                .into_iter()
                .map(|l| CodePreviewLine {
                    line_no: None,
                    text: l.to_owned(),
                })
                .collect();
            (Some(ToolInlineMetric::Entries(count)), lines)
        }
        _ => {
            let lines = raw_lines
                .into_iter()
                .map(|l| CodePreviewLine {
                    line_no: None,
                    text: l.to_owned(),
                })
                .collect();
            (Some(ToolInlineMetric::Lines(count_lines.len())), lines)
        }
    }
}

fn parse_list_dir_header_count(first_line: &str) -> Option<usize> {
    let (_, after_paren) = first_line.rsplit_once('(')?;
    let count_str = after_paren.strip_suffix(" entries)")?;
    count_str.trim().parse::<usize>().ok()
}

fn format_api_output(clean: &str) -> (String, Option<ToolInlineMetric>) {
    if let Ok(val) = serde_json::from_str::<Value>(clean) {
        let status = val
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|n| u16::try_from(n).ok())
            .filter(|code| (100..=599).contains(code))
            .map(ToolInlineMetric::HttpStatus);
        let pretty = serde_json::to_string_pretty(&val).unwrap_or_else(|_| clean.to_owned());
        return (pretty, status);
    }
    (clean.to_owned(), None)
}

fn maybe_pretty_json(clean: &str) -> String {
    let trimmed = clean.trim();
    let looks_json = (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'));
    if looks_json
        && let Ok(val) = serde_json::from_str::<Value>(trimmed)
        && let Ok(pretty) = serde_json::to_string_pretty(&val)
    {
        return pretty;
    }
    clean.to_owned()
}

fn build_head_code_preview(
    lines: &[CodePreviewLine],
    file_has_more: bool,
    max_lines: usize,
) -> (ToolPreviewBody, bool) {
    if lines.is_empty() {
        return (ToolPreviewBody::Empty, file_has_more);
    }
    let omitted_below = lines.len().saturating_sub(max_lines);
    let mut clipped_any = false;
    let shown: Vec<CodePreviewLine> = lines
        .iter()
        .take(max_lines)
        .map(|l| {
            let (text, clipped) = clip_line_chars(&l.text, INLINE_PREVIEW_MAX_LINE_CHARS);
            clipped_any |= clipped;
            CodePreviewLine {
                line_no: l.line_no,
                text,
            }
        })
        .collect();
    (
        ToolPreviewBody::CodeLines {
            lines: shown,
            omitted_above: 0,
            omitted_below,
            file_has_more,
        },
        omitted_below > 0 || clipped_any,
    )
}

fn build_tail_code_preview(lines: &[CodePreviewLine], max_lines: usize) -> (ToolPreviewBody, bool) {
    if lines.is_empty() {
        return (ToolPreviewBody::Empty, false);
    }
    let omitted_above = lines.len().saturating_sub(max_lines);
    let mut clipped_any = false;
    let shown: Vec<CodePreviewLine> = lines
        .iter()
        .skip(omitted_above)
        .map(|l| {
            let (text, clipped) = clip_line_chars(&l.text, INLINE_PREVIEW_MAX_LINE_CHARS);
            clipped_any |= clipped;
            CodePreviewLine {
                line_no: l.line_no,
                text,
            }
        })
        .collect();
    (
        ToolPreviewBody::CodeLines {
            lines: shown,
            omitted_above,
            omitted_below: 0,
            file_has_more: false,
        },
        omitted_above > 0 || clipped_any,
    )
}

fn build_plain_or_tail_error(clean: &str, category: ToolCategory) -> (ToolPreviewBody, bool) {
    if category == ToolCategory::Shell || clean.lines().count() > 6 {
        let lines: Vec<CodePreviewLine> = clean
            .lines()
            .map(|l| CodePreviewLine {
                line_no: None,
                text: l.to_owned(),
            })
            .collect();
        return build_tail_code_preview(&lines, INLINE_PREVIEW_MAX_LINES);
    }
    build_plain_text_preview(clean)
}

fn build_plain_text_preview(clean: &str) -> (ToolPreviewBody, bool) {
    if clean.is_empty() {
        return (ToolPreviewBody::Empty, false);
    }
    let (text, clipped) = clip_line_chars(clean, PLAIN_TEXT_MAX_CHARS);
    (ToolPreviewBody::PlainText { text }, clipped)
}

fn looks_like_diff(text: &str) -> bool {
    text.contains("\n@@ ") || text.starts_with("--- ") || text.contains("\n+++ ")
}

fn clip_line_chars(s: &str, max_chars: usize) -> (String, bool) {
    let mut chars = s.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        (format!("{prefix}…"), true)
    } else {
        (prefix, false)
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    clip_line_chars(s, max_chars).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use spanbay_core::agent::untrusted::envelope_tags;

    #[test]
    fn read_file_strips_envelope_omits_json_args_and_truncates_on_whole_lines() {
        let (open, close) = envelope_tags("read_file");
        let mut body = String::new();
        for i in 1..=25 {
            body.push_str(&format!("{i:>6}\tline_{i} = \"value\"\n"));
        }
        body.push_str("\n[Showing lines 1-25. Call read_file again with offset=26 to continue.]");
        let raw = format!("{open}\n{body}\n{close}");

        let pres = format_tool_trace(
            "read_file",
            r#"{"path":"Cargo.toml"}"#,
            Some(&raw),
            false,
            false,
            None,
        );
        assert_eq!(pres.category, ToolCategory::FileRead);
        assert_eq!(pres.target, "Cargo.toml");
        assert_eq!(pres.file_path.as_deref(), Some("Cargo.toml"));
        assert!(pres.raw_args_display.is_none());
        assert!(pres.external_isolated);
        assert_eq!(
            pres.metric,
            Some(ToolInlineMetric::LineRange { start: 1, end: 25 })
        );
        assert!(pres.can_preview_full);

        let full = pres.cleaned_full_output.as_deref().unwrap();
        assert!(!full.contains("<external_data"));
        assert!(!full.contains("Call read_file again"));
        assert!(full.starts_with("line_1 = \"value\""));
        assert_eq!(full.lines().count(), 25);

        match pres.preview {
            ToolPreviewBody::CodeLines {
                lines,
                omitted_above,
                omitted_below,
                file_has_more,
            } => {
                assert_eq!(lines.len(), INLINE_PREVIEW_MAX_LINES);
                assert_eq!(lines[0].line_no, Some(1));
                assert_eq!(lines[0].text, "line_1 = \"value\"");
                assert_eq!(lines[13].line_no, Some(14));
                assert_eq!(omitted_above, 0);
                assert_eq!(omitted_below, 11);
                assert!(file_has_more);
            }
            other => panic!("expected CodeLines, got {other:?}"),
        }
    }

    #[test]
    fn short_read_file_does_not_enable_preview_full_button() {
        let raw = "     1\t[package]\n     2\tname = \"tiny\"\n";
        let pres = format_tool_trace(
            "read_file",
            r#"{"path":"Cargo.toml"}"#,
            Some(raw),
            false,
            false,
            None,
        );
        assert!(!pres.can_preview_full);
        assert_eq!(
            pres.metric,
            Some(ToolInlineMetric::LineRange { start: 1, end: 2 })
        );
    }

    #[test]
    fn shell_shows_tail_lines_and_preserves_command_line() {
        let mut out = String::new();
        for i in 1..=30 {
            out.push_str(&format!("test case_{i} ... ok\n"));
        }
        out.push_str("error: assertion failed\n");
        let pres = format_tool_trace(
            "shell",
            r#"{"command":"cargo test -p spanbay-core","cwd":"crates"}"#,
            Some(&out),
            false,
            false,
            None,
        );
        assert_eq!(pres.category, ToolCategory::Shell);
        assert_eq!(pres.target, "cargo test -p spanbay-core");
        assert_eq!(
            pres.command_line.as_deref(),
            Some("cargo test -p spanbay-core")
        );
        assert_eq!(
            pres.meta_chips,
            vec![("cwd".to_owned(), "crates".to_owned())]
        );
        assert!(pres.can_preview_full);
        match pres.preview {
            ToolPreviewBody::CodeLines {
                lines,
                omitted_above,
                omitted_below,
                ..
            } => {
                assert_eq!(lines.len(), INLINE_PREVIEW_MAX_LINES);
                assert_eq!(omitted_above, 17);
                assert_eq!(omitted_below, 0);
                assert_eq!(lines.last().unwrap().text, "error: assertion failed");
            }
            other => panic!("expected tail CodeLines, got {other:?}"),
        }
    }

    #[test]
    fn coded_error_strips_json_prefix_and_preserves_code() {
        let raw = "{\"error\":{\"code\":\"outside_folder\"}}\npath is outside the workspace";
        let pres = format_tool_trace(
            "read_file",
            r#"{"path":"../etc/passwd"}"#,
            Some(raw),
            true,
            false,
            None,
        );
        assert_eq!(pres.error_code.as_deref(), Some("outside_folder"));
        assert_eq!(
            pres.cleaned_full_output.as_deref(),
            Some("path is outside the workspace")
        );
    }

    #[test]
    fn grep_and_mcp_extract_expected_metrics_and_args() {
        let grep_pres = format_tool_trace(
            "grep",
            r#"{"pattern":"retry","path":"crates/","glob":"*.rs"}"#,
            Some("a.rs:1:retry()\nb.rs:2:retry()\n"),
            false,
            false,
            None,
        );
        assert_eq!(grep_pres.target, "\"retry\" (crates/)");
        assert_eq!(grep_pres.metric, Some(ToolInlineMetric::Matches(2)));
        assert_eq!(
            grep_pres.meta_chips,
            vec![("glob".to_owned(), "*.rs".to_owned())]
        );
        assert!(grep_pres.raw_args_display.is_none());

        assert_eq!(
            format_mcp_tool_name("mcp__github__list_issues").as_deref(),
            Some("github / list_issues")
        );
        let mcp_pres = format_tool_trace(
            "mcp__github__list_issues",
            r#"{"repo":"spanbay","state":"open"}"#,
            Some(r#"{"count":3}"#),
            false,
            false,
            None,
        );
        assert_eq!(mcp_pres.category, ToolCategory::Mcp);
        assert!(mcp_pres.raw_args_display.is_some());
    }

    #[test]
    fn todo_write_extracts_active_task_and_renders_checklist() {
        let args = r#"{"todos":[
            {"id":"a","content":"Inspect layout","status":"completed"},
            {"id":"b","content":"Fix reducer","status":"in_progress"},
            {"id":"c","content":"Run tests","status":"pending"}
        ]}"#;
        let pres = format_tool_trace(
            "todo_write",
            args,
            Some("Task list updated: 3 items."),
            false,
            false,
            None,
        );
        assert_eq!(pres.target, "Fix reducer");
        assert_eq!(pres.metric, Some(ToolInlineMetric::Entries(3)));
        match pres.preview {
            ToolPreviewBody::CodeLines { lines, .. } => {
                assert_eq!(lines.len(), 3);
                assert_eq!(lines[0].text, "✓ Inspect layout");
                assert_eq!(lines[1].text, "● Fix reducer");
                assert_eq!(lines[2].text, "○ Run tests");
            }
            other => panic!("expected CodeLines checklist, got {other:?}"),
        }
    }
}
