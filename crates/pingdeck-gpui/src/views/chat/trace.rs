//! Structured tool-call trace timeline cards, inline diff previews, and collapsible reasoning
//! blocks for the AI Chat thread stream.

use std::collections::HashMap;
use std::time::Instant;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::model::{AgentToolCallRecord, ToolCallStatus};
use pingdeck_desktop::core::model::hub::HubPart;
use pingdeck_desktop::state::LiveToolCall;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_ghost, collapsible_box, diff_lines_preview, h_row,
    icon_xs, palette, status_indicator, streaming_markdown_view, to_hsla, v_col,
};

use super::ChatView;
use super::metrics::{extract_inline_thinking, format_duration_ms};
use super::reasoning::render_reasoning_block;
use super::run_topology_subagent::render_live_subagent_group;

pub fn render_live_turn(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let model_name = view
        .thread
        .active_run
        .as_ref()
        .map(|r| view.format_raw_model_label(&r.model_label))
        .or_else(|| {
            view.session
                .as_ref()
                .and_then(|s| s.target.as_ref())
                .map(|t| view.format_target_label(t))
        })
        .unwrap_or_else(|| "Assistant".to_owned());

    // Combine native `ReasoningDelta` stream with any inline `<think>...</think>` tags in text.
    let (inline_thinking, visible_text) =
        extract_inline_thinking(&view.thread.streaming_text, true);
    let mut live_reasoning = view.thread.streaming_reasoning.clone();
    if !inline_thinking.is_empty() {
        if !live_reasoning.is_empty() {
            live_reasoning.push_str("\n\n");
        }
        live_reasoning.push_str(&inline_thinking.join("\n\n"));
    }

    let effort_explicit = view
        .session
        .as_ref()
        .and_then(|s| s.reasoning_effort)
        .is_some();
    let phase_label = if !visible_text.is_empty() {
        crate::ui::tr(cx).chat().trace_generating_response()
    } else if !view.thread.tool_calls.is_empty() {
        crate::ui::tr(cx).chat().trace_running_tools()
    } else if !live_reasoning.is_empty() {
        crate::ui::tr(cx).chat().composer_thinking()
    } else if view.submitting {
        crate::ui::tr(cx).chat().trace_connecting_to_model()
    } else if effort_explicit {
        crate::ui::tr(cx).chat().composer_thinking()
    } else {
        crate::ui::tr(cx).chat().trace_waiting_for_model()
    };

    let elapsed_lbl = view
        .run_started_at
        .map(|t| format_duration_ms(t.elapsed().as_millis() as u64));

    let mut body = v_col().w_full().gap(px(10.0)).child(
        h_row()
            .gap(px(8.0))
            .flex_wrap()
            .child(
                div()
                    .size(px(22.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.accent_soft))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(status_indicator(StatusShape::Run, cx)),
            )
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink))
                    .child(SharedString::from(model_name)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(phase_label),
            )
            .children(elapsed_lbl.map(|e| {
                div()
                    .text_xs()
                    .text_color(to_hsla(p.faint))
                    .child(SharedString::from(e))
            })),
    );

    if !live_reasoning.is_empty() {
        let key = "live-reasoning".to_owned();
        let open = !view.collapsed_live_reasoning;
        let still_thinking = visible_text.is_empty() && view.thread.tool_calls.is_empty();
        let live_thinking_ms = view.thinking_started_at.map(|started| {
            let end = if still_thinking {
                Instant::now()
            } else {
                view.thinking_ended_at.unwrap_or_else(Instant::now)
            };
            end.saturating_duration_since(started).as_millis() as u64
        });
        let k_click = key.clone();
        body = body.child(render_reasoning_block(
            key,
            open,
            still_thinking,
            live_reasoning,
            live_thinking_ms,
            None,
            cx.listener(move |this, _, _, cx| this.toggle_reasoning(k_click.clone(), cx)),
            cx,
        ));
    } else if visible_text.is_empty() && view.thread.tool_calls.is_empty() {
        let wait_text = if effort_explicit {
            crate::ui::tr(cx)
                .chat()
                .trace_thinking_waiting_for_model_response()
        } else {
            crate::ui::tr(cx).chat().trace_waiting_for_first_token()
        };
        body = body.child(
            h_row()
                .ml(px(30.0))
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(10.0))
                .bg(to_hsla(p.pane))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(status_indicator(StatusShape::Run, cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(wait_text),
                ),
        );
    }

    if !view.thread.tool_calls.is_empty() {
        let task_calls: Vec<&LiveToolCall> = view
            .thread
            .tool_calls
            .iter()
            .filter(|c| c.name == "task")
            .collect();
        let regular_calls: Vec<&LiveToolCall> = view
            .thread
            .tool_calls
            .iter()
            .filter(|c| c.name != "task")
            .collect();

        if !task_calls.is_empty() {
            body = body.child(render_live_subagent_group(view, &task_calls, cx));
        }

        if !regular_calls.is_empty() {
            let mut trace_box = v_col()
                .w_full()
                .rounded(px(10.0))
                .overflow_hidden()
                .bg(to_hsla(p.pane))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)));

            for call in regular_calls {
                trace_box = trace_box.child(render_live_tool_step(view, call, cx));
            }
            body = body.child(trace_box);
        }
    }

    if !visible_text.is_empty() {
        let stream_id = view
            .thread
            .active_run
            .as_ref()
            .map_or(view.thread.session_id.as_str(), |r| r.run_id.as_str());
        body = body.child(div().pl(px(30.0)).child(streaming_markdown_view(
            SharedString::from(format!("chat-live-{stream_id}")),
            visible_text,
        )));
    }

    body
}

/// Renders the collapsible tool-call trace card attached to a persisted assistant message.
pub fn render_message_tool_traces(
    view: &ChatView,
    tool_calls: &[(&str, &str, &str)],
    tool_results: &HashMap<&str, (&[HubPart], bool)>,
    audit_map: &HashMap<&str, &AgentToolCallRecord>,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let any_err = tool_calls
        .iter()
        .any(|(id, _, _)| tool_results.get(id).is_some_and(|(_, is_err)| *is_err));
    let header_label = crate::ui::tr(cx).chat().trace_tool_calls(tool_calls.len());

    let mut trace_box = v_col()
        .w_full()
        .rounded(px(10.0))
        .overflow_hidden()
        .bg(to_hsla(p.pane))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .px(px(10.0))
                .py(px(7.0))
                .gap(px(6.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(status_indicator(
                    if any_err {
                        StatusShape::Bad
                    } else {
                        StatusShape::Ok
                    },
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(header_label)),
                ),
        );

    for &(call_id, name, args) in tool_calls {
        let expanded = view.expanded_traces.contains(call_id);
        let audit = audit_map.get(call_id).copied();
        let result = tool_results.get(call_id).copied();
        let out_text = result.map(|(parts, _)| {
            parts
                .iter()
                .filter_map(|pt| match pt {
                    HubPart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        let is_err = result.is_some_and(|(_, e)| e);
        let dur = audit.and_then(|a| match (a.started_ms, a.ended_ms) {
            (Some(s), Some(e)) if e >= s => Some((e - s) as u64),
            _ => None,
        });
        let cid_click = call_id.to_owned();
        let cid_full = call_id.to_owned();
        let name_full = name.to_owned();
        let display_name = if name == "task" {
            crate::ui::tr(cx).chat().trace_subagent_call()
        } else {
            name
        };
        trace_box = trace_box.child(render_trace_step(
            call_id,
            display_name,
            args,
            out_text.as_deref(),
            if is_err {
                StatusShape::Bad
            } else {
                StatusShape::Ok
            },
            dur,
            audit.and_then(|a| a.exit_code),
            expanded,
            cx.listener(move |this, _, _, cx| this.toggle_trace(cid_click.clone(), cx)),
            cx,
        ));
        if expanded {
            trace_box = trace_box.child(
                h_row().justify_end().px(px(10.0)).pb(px(6.0)).child(
                    btn_ghost(
                        SharedString::from(format!("tout-full-{call_id}")),
                        crate::ui::tr(cx).chat().trace_view_full_output(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_tool_output(cid_full.clone(), name_full.clone(), cx);
                    })),
                ),
            );
        }
    }
    trace_box
}

pub fn render_live_tool_step(
    view: &ChatView,
    call: &LiveToolCall,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let shape = match call.status {
        None => StatusShape::Run,
        Some(ToolCallStatus::Ok) => StatusShape::Ok,
        Some(ToolCallStatus::Denied | ToolCallStatus::Blocked) => StatusShape::Warn,
        Some(_) => StatusShape::Bad,
    };
    let expanded = view.expanded_traces.contains(&call.call_id) || call.status.is_none();
    let cid = call.call_id.clone();
    render_trace_step(
        &call.call_id,
        &call.name,
        &call.arguments,
        (!call.output.is_empty()).then_some(call.output.as_str()),
        shape,
        call.duration_ms,
        call.exit_code,
        expanded,
        cx.listener(move |this, _, _, cx| this.toggle_trace(cid.clone(), cx)),
        cx,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn render_trace_step(
    call_id: &str,
    name: &str,
    args: &str,
    output: Option<&str>,
    shape: StatusShape,
    duration_ms: Option<u64>,
    exit_code: Option<i32>,
    expanded: bool,
    on_toggle: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
    cx: &gpui_kit::App,
) -> impl IntoElement {
    let p = palette(cx);
    let summary = summarize_tool_args(args);
    let glyph = tool_icon(name);

    let mut detail = v_col().gap(px(6.0)).px(px(12.0)).pb(px(10.0)).pt(px(2.0));
    if !args.trim().is_empty() {
        detail = detail.child(
            div()
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(truncate_chars(args.trim(), 500))),
        );
    }
    if let Some(out) = output
        && !out.trim().is_empty()
    {
        if looks_like_diff(out) {
            detail = detail.child(diff_lines_preview(out, 18, cx));
        } else {
            detail = detail.child(
                div()
                    .p(px(8.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.sunken))
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(truncate_chars(out.trim(), 900))),
            );
        }
    }

    let row_header = div()
        .id(SharedString::from(format!("trace-step-{call_id}")))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(6.0))
        .cursor_pointer()
        .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
        .on_click(on_toggle)
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(7.0))
                .child(status_indicator(shape, cx))
                .child(div().text_color(to_hsla(p.muted)).child(icon_xs(glyph)))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(name.to_owned())),
                )
                .children((!summary.is_empty()).then(|| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .truncate()
                        .child(SharedString::from(summary))
                })),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .children(
                    exit_code
                        .filter(|c| *c != 0)
                        .map(|c| badge(format!("exit {c}"), Tone::Danger, cx)),
                )
                .children(duration_ms.map(|ms| {
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format!("{ms}ms")))
                }))
                .child(
                    div()
                        .text_color(to_hsla(p.faint))
                        .child(icon_xs(if expanded {
                            UiIcon::ChevronDown
                        } else {
                            UiIcon::ChevronRight
                        })),
                ),
        );

    v_col()
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            collapsible_box(expanded, SharedString::from(format!("trace-col-{call_id}")))
                .child(row_header)
                .content(detail),
        )
}

fn tool_icon(name: &str) -> UiIcon {
    let lower = name.to_ascii_lowercase();
    if lower.contains("shell") || lower.contains("bash") || lower.contains("exec") {
        UiIcon::SquareTerminal
    } else if lower.contains("edit") || lower.contains("write") || lower.contains("patch") {
        UiIcon::FilePen
    } else if lower.contains("read") || lower.contains("grep") || lower.contains("find") {
        UiIcon::FileText
    } else if lower.contains("web") || lower.contains("fetch") {
        UiIcon::Globe
    } else if lower.contains("request") || lower.contains("http") {
        UiIcon::Send
    } else {
        UiIcon::Wrench
    }
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
                return truncate_chars(s, 80);
            }
        }
    }
    truncate_chars(args.trim(), 70)
}

fn looks_like_diff(text: &str) -> bool {
    text.contains("\n@@ ") || text.starts_with("--- ") || text.contains("\n+++ ")
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}
