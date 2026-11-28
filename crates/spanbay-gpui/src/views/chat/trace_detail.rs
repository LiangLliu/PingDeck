//! Expanded detail renderer, inline metrics, localized action verbs, and icons for Chat tool
//! trace rows (`trace.rs`).

use crate::ui::{
    Tone, UiIcon, badge, btn_ghost, btn_icon, btn_secondary, code_lines_preview,
    diff_lines_preview, h_row, icon_xs, kv_chip, mono_font_family, mono_font_size, palette,
    to_hsla, v_col,
};
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use spanbay_desktop::core::agent::model::{AgentToolCallRecord, ApprovalScope, OutputStream};
use spanbay_desktop::core::model::hub::HubPart;
use spanbay_desktop::services::clipboard;
use spanbay_desktop::state::{
    ApprovalResolution, SettingsSection, ToolCategory, ToolInlineMetric, ToolPreviewBody,
    ToolTracePresentation, format_human_bytes, format_mcp_tool_name,
};

use super::ChatView;
use super::thread_header::render_binding_popover_with_trigger;
use super::types::ChatHandoff;

/// Localised sentence for one of the 6 core coded tool errors (delta §2.14).
pub(super) fn localized_tool_error(code: &str, verb: &str, cx: &gpui_kit::App) -> Option<String> {
    let ch = crate::ui::tr(cx).chat();
    match code {
        "not_bound" => Some(ch.tool_error_not_bound(verb)),
        "workspace_deleted" => Some(ch.tool_error_workspace_deleted(verb)),
        "tool_unavailable" => Some(ch.tool_error_tool_unavailable(verb)),
        "invalid_arguments" => Some(ch.tool_error_invalid_arguments(verb)),
        "timeout" => Some(ch.tool_error_timeout(verb)),
        "outside_folder" => Some(ch.tool_error_outside_folder(verb)),
        _ => None,
    }
}

/// Follow-up button for actionable coded tool errors (`not_bound`, `workspace_deleted`, `tool_unavailable`).
pub(super) fn render_coded_error_action(
    view: &ChatView,
    call_id: &str,
    code: &str,
    cx: &mut Context<ChatView>,
) -> Option<AnyElement> {
    let ch = crate::ui::tr(cx).chat();
    match code {
        "not_bound" | "workspace_deleted" => Some(
            render_binding_popover_with_trigger(
                view,
                SharedString::from(format!("t-bind-pop-{call_id}")),
                btn_secondary(
                    SharedString::from(format!("t-bind-{call_id}")),
                    ch.tool_error_bind_workspace(),
                ),
                cx,
            )
            .into_any_element(),
        ),
        "tool_unavailable" => Some(
            btn_secondary(
                SharedString::from(format!("t-tools-set-{call_id}")),
                ch.tool_error_open_tool_settings(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.pending_handoff
                    .set(Some(ChatHandoff::OpenSettings(SettingsSection::Tools)));
                cx.notify();
            }))
            .into_any_element(),
        ),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_trace_detail(
    call_id: &str,
    tool_name: &str,
    verb: &str,
    pres: &ToolTracePresentation,
    tone: Tone,
    spilled: bool,
    has_workspace: bool,
    change_ref: Option<(String, String)>,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mono = mono_font_family(cx);
    let code_px = (f32::from(mono_font_size(cx)) - 1.5).clamp(10.5, 13.0);
    let is_err = tone == Tone::Danger;

    let (card_bg, card_border) = match tone {
        Tone::Danger => (
            to_hsla(p.bad_soft.composite_over(p.pane)),
            to_hsla(p.bad_line.composite_over(p.pane)),
        ),
        Tone::Warning => (
            to_hsla(p.warn_soft.composite_over(p.pane)),
            to_hsla(p.warn_line.composite_over(p.pane)),
        ),
        _ => (to_hsla(p.sunken), to_hsla(p.line.composite_over(p.sunken))),
    };
    let file_has_more = matches!(
        pres.preview,
        ToolPreviewBody::CodeLines {
            file_has_more: true,
            ..
        }
    );
    let has_left_meta = !pres.meta_chips.is_empty() || pres.external_isolated || file_has_more;
    let has_right_actions =
        pres.file_path.is_some() || change_ref.is_some() || pres.can_preview_full;
    let has_body = pres.command_line.is_some()
        || pres.raw_args_display.is_some()
        || !matches!(pres.preview, ToolPreviewBody::Empty);

    // One unified, full-width sunken card with an integrated micro-header bar and crisp body.
    let mut card = v_col()
        .w_full()
        .rounded(px(7.0))
        .overflow_hidden()
        .bg(card_bg)
        .border_1()
        .border_color(card_border);

    if has_left_meta || has_right_actions {
        let mut left = h_row()
            .flex_1()
            .min_w_0()
            .gap(px(5.0))
            .flex_wrap()
            .items_center();
        for (k, v) in &pres.meta_chips {
            left = left.child(kv_chip(k.clone(), v.clone(), cx));
        }
        if pres.external_isolated {
            left = left.child(badge(ch.trace_external_isolated(), Tone::Neutral, cx));
        }
        if file_has_more {
            left = left.child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.muted))
                    .child(ch.trace_file_has_more()),
            );
        }

        let mut right = h_row().flex_shrink_0().gap(px(2.0)).items_center();
        if let Some(path) = pres.file_path.clone() {
            let path_cp = path.clone();
            right = right.child(
                btn_ghost(
                    SharedString::from(format!("t-cp-path-{call_id}")),
                    ch.trace_copy_path(),
                )
                .icon(icon_xs(UiIcon::Copy))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let _ = clipboard::set_text(&path_cp);
                    this.status_msg = Some((
                        Tone::Success,
                        crate::ui::tr(cx).chat().trace_copied_path().into(),
                    ));
                    cx.notify();
                })),
            );
            if has_workspace {
                let path_open = path;
                right = right.child(
                    btn_ghost(
                        SharedString::from(format!("t-open-{call_id}")),
                        ch.trace_open_external(),
                    )
                    .icon(icon_xs(UiIcon::ExternalLink))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_workspace_file_external(path_open.clone(), cx);
                    })),
                );
            }
        }
        if let Some((fc_id, fc_path)) = change_ref {
            right = right.child(
                btn_ghost(
                    SharedString::from(format!("t-diff-{call_id}")),
                    ch.trace_view_diff(),
                )
                .icon(icon_xs(UiIcon::FileDiff))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let _ = &fc_id;
                    this.open_review(Some(fc_path.clone()), cx);
                })),
            );
        }
        if pres.can_preview_full {
            let cid_full = call_id.to_owned();
            let tname_full = tool_name.to_owned();
            let modal_title = if pres.target.is_empty() {
                verb.to_owned()
            } else {
                format!("{verb} · {}", pres.target)
            };
            let inline_full = pres.cleaned_full_output.clone();
            let btn_lbl = if pres.category == ToolCategory::FileRead {
                ch.trace_preview_full()
            } else {
                ch.trace_view_full_output()
            };
            right = right.child(
                btn_ghost(SharedString::from(format!("tout-full-{call_id}")), btn_lbl)
                    .icon(icon_xs(UiIcon::Eye))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_tool_output(
                            cid_full.clone(),
                            tname_full.clone(),
                            modal_title.clone(),
                            inline_full.clone(),
                            spilled,
                            cx,
                        );
                    })),
            );
        }

        let mut header_bar = h_row()
            .w_full()
            .justify_between()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(4.0))
            .bg(to_hsla(p.surface))
            .child(left)
            .child(right);
        if has_body {
            header_bar = header_bar.border_b_1().border_color(card_border);
        }
        card = card.child(header_bar);
    }

    // Shell command strip (`$ <command>`) with 1-click copy.
    if let Some(cmd) = &pres.command_line {
        let cmd_cp = cmd.clone();
        let mut cmd_bar = h_row()
            .w_full()
            .justify_between()
            .gap(px(6.0))
            .px(px(10.0))
            .py(px(6.0))
            .bg(to_hsla(p.surface))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family(mono.clone())
                    .text_size(px(code_px))
                    .text_color(to_hsla(p.ink))
                    .child(SharedString::from(format!("$ {cmd}"))),
            )
            .child(
                btn_icon(
                    SharedString::from(format!("t-cmd-cp-{call_id}")),
                    UiIcon::Copy,
                )
                .tooltip(crate::ui::tr(cx).common().copy())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let _ = clipboard::set_text(&cmd_cp);
                    this.status_msg =
                        Some((Tone::Success, crate::ui::tr(cx).common().copied().into()));
                    cx.notify();
                })),
            );
        if !matches!(pres.preview, ToolPreviewBody::Empty) {
            cmd_bar = cmd_bar.border_b_1().border_color(card_border);
        }
        card = card.child(cmd_bar);
    }

    // Raw JSON arguments only for MCP / unknown tools whose parameters aren't self-evident.
    if let Some(raw_args) = &pres.raw_args_display {
        let mut args_el = div()
            .w_full()
            .px(px(10.0))
            .py(px(7.0))
            .font_family(mono)
            .text_size(px(code_px))
            .text_color(to_hsla(p.ink_2))
            .child(SharedString::from(raw_args.clone()));
        if !matches!(pres.preview, ToolPreviewBody::Empty) {
            args_el = args_el.border_b_1().border_color(card_border);
        }
        card = card.child(args_el);
    }

    match &pres.preview {
        ToolPreviewBody::Empty => {}
        ToolPreviewBody::CodeLines {
            lines,
            omitted_above,
            omitted_below,
            ..
        } => {
            let above_lbl = (*omitted_above > 0).then(|| ch.trace_earlier_lines(*omitted_above));
            let below_lbl = (*omitted_below > 0).then(|| ch.trace_more_lines(*omitted_below));
            card = card.child(code_lines_preview(lines, above_lbl, below_lbl, is_err, cx));
        }
        ToolPreviewBody::Diff { text } => {
            card = card.child(diff_lines_preview(text, 18, cx));
        }
        ToolPreviewBody::PlainText { text } => {
            let fg = match tone {
                Tone::Danger => to_hsla(p.bad),
                Tone::Warning => to_hsla(p.warn),
                _ => to_hsla(p.ink_2),
            };
            card = card.child(
                div()
                    .w_full()
                    .px(px(10.0))
                    .py(px(8.0))
                    .text_xs()
                    .text_color(fg)
                    .child(SharedString::from(text.clone())),
            );
        }
    }

    let mut wrapper = v_col().w_full().px(px(12.0)).pb(px(10.0)).pt(px(2.0));
    if has_left_meta || has_right_actions || has_body {
        wrapper = wrapper.child(card);
    }
    wrapper
}

pub(super) fn render_inline_metric(metric: &ToolInlineMetric, cx: &gpui_kit::App) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    match metric {
        ToolInlineMetric::DiffStats { added, removed } => h_row()
            .gap(px(4.0))
            .text_xs()
            .child(div().text_color(to_hsla(p.ok)).child(format!("+{added}")))
            .child(
                div()
                    .text_color(to_hsla(p.bad))
                    .child(format!("-{removed}")),
            )
            .into_any_element(),
        ToolInlineMetric::HttpStatus(code) => {
            let tone = if (200..300).contains(code) {
                Tone::Success
            } else if *code >= 500 {
                Tone::Danger
            } else {
                Tone::Warning
            };
            badge(code.to_string(), tone, cx).into_any_element()
        }
        other => {
            let label = match other {
                ToolInlineMetric::LineRange { start, end } => ch.trace_lines_range(*start, *end),
                ToolInlineMetric::Lines(n) => ch.trace_line_count(*n),
                ToolInlineMetric::Matches(n) => ch.trace_match_count(*n),
                ToolInlineMetric::Files(n) => ch.trace_file_count(*n),
                ToolInlineMetric::Entries(n) => ch.trace_entry_count(*n),
                ToolInlineMetric::Bytes(b) => format_human_bytes(*b),
                ToolInlineMetric::DiffStats { .. } | ToolInlineMetric::HttpStatus(_) => {
                    String::new()
                }
            };
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(label))
                .into_any_element()
        }
    }
}

pub(super) fn localized_tool_verb(name: &str, cx: &gpui_kit::App) -> String {
    let ch = crate::ui::tr(cx).chat();
    match name {
        "read_file" => ch.trace_verb_read_file().to_owned(),
        "edit_file" => ch.trace_verb_edit_file().to_owned(),
        "write_file" => ch.trace_verb_write_file().to_owned(),
        "delete_file" => ch.trace_verb_delete_file().to_owned(),
        "move_file" => ch.trace_verb_move_file().to_owned(),
        "list_dir" => ch.trace_verb_list_dir().to_owned(),
        "glob" => ch.trace_verb_glob().to_owned(),
        "grep" => ch.trace_verb_grep().to_owned(),
        "shell" => ch.trace_verb_shell().to_owned(),
        "shell_output" => ch.trace_verb_shell_output().to_owned(),
        "shell_kill" => ch.trace_verb_shell_kill().to_owned(),
        "web_search" => ch.trace_verb_web_search().to_owned(),
        "fetch_url" => ch.trace_verb_fetch_url().to_owned(),
        "send_request" => ch.trace_verb_send_request().to_owned(),
        "get_request" => ch.trace_verb_get_request().to_owned(),
        "list_requests" => ch.trace_verb_list_requests().to_owned(),
        "create_request" => ch.trace_verb_create_request().to_owned(),
        "get_trace" => ch.trace_verb_get_trace().to_owned(),
        "list_environments" => ch.trace_verb_list_environments().to_owned(),
        "run_probe" => ch.trace_verb_run_probe().to_owned(),
        "query_gateway_log" => ch.trace_verb_query_gateway_log().to_owned(),
        "read_attachment" => ch.trace_verb_read_attachment().to_owned(),
        "read_skill" => ch.trace_verb_read_skill().to_owned(),
        "read_output" => ch.trace_verb_read_output().to_owned(),
        "todo_write" => ch.trace_verb_todo_write().to_owned(),
        "ask_user" => ch.trace_verb_ask_user().to_owned(),
        "submit_plan" => ch.trace_verb_submit_plan().to_owned(),
        "task" => ch.trace_subagent_call().to_owned(),
        other => format_mcp_tool_name(other).unwrap_or_else(|| other.to_owned()),
    }
}

/// Renders `(OutputStream, String)` segments in arrival order with `stdout │` / `stderr │` prefixes.
pub(super) fn render_interleaved_streams(
    streams: &[(OutputStream, String)],
    cx: &gpui_kit::App,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mono = mono_font_family(cx);
    let mut col = v_col()
        .mx(px(12.0))
        .mb(px(6.0))
        .px(px(8.0))
        .py(px(6.0))
        .gap(px(2.0))
        .rounded(px(6.0))
        .bg(to_hsla(p.sunken))
        .font_family(mono)
        .text_xs();
    for (stream, chunk) in streams {
        let (prefix, fg) = match stream {
            OutputStream::Stdout => (ch.trace_stream_stdout(), to_hsla(p.ink_2)),
            OutputStream::Stderr => (ch.trace_stream_stderr(), to_hsla(p.warn)),
        };
        for line in chunk.lines() {
            col = col.child(
                h_row()
                    .items_start()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(to_hsla(p.faint))
                            .child(prefix),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(fg)
                            .child(SharedString::from(line.to_owned())),
                    ),
            );
        }
    }
    col
}

pub(super) fn extract_denied_reason(
    parts: &[HubPart],
    streams: &[(OutputStream, String)],
) -> (Option<String>, Option<String>) {
    let mut body_lines = Vec::new();
    let mut nudge_lines = Vec::new();
    let push_text = |raw: &str, body: &mut Vec<String>, nudge: &mut Vec<String>| {
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("[System notice]") {
                nudge.push(rest.trim().to_owned());
            } else if nudge.is_empty() {
                body.push(trimmed.to_owned());
            } else {
                nudge.push(trimmed.to_owned());
            }
        }
    };
    if !parts.is_empty() {
        for pt in parts {
            if let HubPart::Text { text, .. } = pt {
                push_text(text, &mut body_lines, &mut nudge_lines);
            }
        }
    } else {
        for (_, chunk) in streams {
            push_text(chunk, &mut body_lines, &mut nudge_lines);
        }
    }
    let body = body_lines.join("\n");
    let reason = if let Some(note) = body.strip_prefix("The user denied this call:") {
        let note = note.trim();
        (!note.is_empty()).then(|| note.to_owned())
    } else if body.is_empty() || body == "The user denied this call." {
        None
    } else {
        Some(body)
    };
    let nudge = (!nudge_lines.is_empty()).then(|| nudge_lines.join(" "));
    (reason, nudge)
}

pub(super) fn format_tech_details(
    exit_code: Option<i32>,
    audit: Option<&AgentToolCallRecord>,
    resolution: Option<ApprovalResolution>,
    raw_result_first_line: Option<&str>,
) -> Option<String> {
    let mut kv = Vec::new();
    if let Some(code) = exit_code {
        kv.push(format!("exit_code={code}"));
    }
    if let Some(sb) = audit.and_then(|a| a.sandbox.as_deref()) {
        kv.push(format!("sandbox={sb}"));
    }
    if let Some(by) = audit.and_then(|a| a.decided_by.as_deref()) {
        kv.push(format!("decided_by={by}"));
    }
    if let Some(res) = resolution {
        let sc = match res.scope {
            ApprovalScope::Once => "once",
            ApprovalScope::Session => "session",
            ApprovalScope::WorkspacePattern => "folder",
            ApprovalScope::Tool => "tool",
        };
        kv.push(format!("scope={sc}"));
    }
    let mut lines = Vec::new();
    if let Some(raw) = raw_result_first_line {
        lines.push(raw.to_owned());
    }
    if !kv.is_empty() {
        lines.push(kv.join(" · "));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

pub(super) fn summarize_tool_args(args: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(args)
        && let Some(obj) = val.as_object()
    {
        for key in [
            "description",
            "cmd",
            "command",
            "path",
            "file_path",
            "url",
            "query",
            "pattern",
        ] {
            if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
                let mut chars = s.chars();
                let prefix: String = chars.by_ref().take(80).collect();
                return if chars.next().is_some() {
                    format!("{prefix}…")
                } else {
                    prefix
                };
            }
        }
    }
    let trimmed = args.trim();
    let mut chars = trimmed.chars();
    let prefix: String = chars.by_ref().take(70).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}
