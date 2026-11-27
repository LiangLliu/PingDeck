//! Expanded detail renderer, inline metrics, localized action verbs, and icons for Chat tool
//! trace rows (`trace.rs`).

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use spanbay_desktop::services::clipboard;
use spanbay_desktop::state::{
    ToolCategory, ToolInlineMetric, ToolPreviewBody, ToolTracePresentation, format_human_bytes,
    format_mcp_tool_name,
};

use crate::ui::{
    Tone, UiIcon, badge, btn_ghost, btn_icon, code_lines_preview, diff_lines_preview, h_row,
    icon_xs, kv_chip, mono_font_family, mono_font_size, palette, to_hsla, v_col,
};

use super::ChatView;

#[allow(clippy::too_many_arguments)]
pub(super) fn render_trace_detail(
    call_id: &str,
    tool_name: &str,
    verb: &str,
    pres: &ToolTracePresentation,
    is_err: bool,
    spilled: bool,
    has_workspace: bool,
    change_ref: Option<(String, String)>,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mono = mono_font_family(cx);
    let code_px = (f32::from(mono_font_size(cx)) - 1.5).clamp(10.5, 13.0);

    let card_bg = if is_err {
        to_hsla(p.bad_soft.composite_over(p.pane))
    } else {
        to_hsla(p.sunken)
    };
    let card_border = if is_err {
        to_hsla(p.bad_line.composite_over(p.pane))
    } else {
        to_hsla(p.line.composite_over(p.sunken))
    };

    let file_has_more = matches!(
        pres.preview,
        ToolPreviewBody::CodeLines {
            file_has_more: true,
            ..
        }
    );
    let has_left_meta = !pres.meta_chips.is_empty()
        || pres.external_isolated
        || pres.error_code.is_some()
        || file_has_more;
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
        if let Some(code) = &pres.error_code {
            left = left.child(badge(code.clone(), Tone::Danger, cx));
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
                    this.open_change_diff(fc_id.clone(), fc_path.clone(), cx);
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
            let fg = if is_err {
                to_hsla(p.bad)
            } else {
                to_hsla(p.ink_2)
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

pub(super) fn tool_icon(name: &str, category: ToolCategory) -> UiIcon {
    match name {
        "grep" => return UiIcon::Search,
        "glob" => return UiIcon::FolderOpen,
        "list_dir" => return UiIcon::Folder,
        "todo_write" => return UiIcon::ListChecks,
        _ => {}
    }
    match category {
        ToolCategory::FileRead | ToolCategory::OutputRead => UiIcon::FileText,
        ToolCategory::FileSearch => UiIcon::Search,
        ToolCategory::FileEdit => UiIcon::FilePen,
        ToolCategory::Shell => UiIcon::SquareTerminal,
        ToolCategory::SpanBayApi => UiIcon::Send,
        ToolCategory::Web => UiIcon::Globe,
        ToolCategory::Subagent => UiIcon::Bot,
        ToolCategory::Planning => UiIcon::ListTodo,
        ToolCategory::Mcp | ToolCategory::Other => UiIcon::Wrench,
    }
}
