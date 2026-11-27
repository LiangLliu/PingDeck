//! Structured tool-call trace timeline cards, one command row per tool call, and live streaming
//! turn layout for the AI Chat thread stream.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::model::{AgentRole, AgentToolCallRecord, ToolCallStatus};
use spanbay_desktop::core::model::hub::HubPart;
use spanbay_desktop::state::{LiveToolCall, ToolTracePresentation, format_tool_trace};

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, h_row, icon_xs, mono_font_family, palette, status_indicator,
    streaming_markdown_view, to_hsla, v_col,
};

use super::ChatView;
use super::metrics::{extract_inline_thinking, format_duration_ms};
use super::reasoning::render_reasoning_block;
use super::run_topology_subagent::render_live_subagent_group;
use super::trace_detail::{
    localized_tool_verb, render_inline_metric, render_trace_detail, tool_icon,
};

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
    } else if !view.thread.tool_calls.is_empty() || !view.thread.pending_tool_calls.is_empty() {
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
    } else if visible_text.is_empty()
        && view.thread.tool_calls.is_empty()
        && view.thread.pending_tool_calls.is_empty()
    {
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
        let saved_call_ids: HashSet<&str> = view
            .thread
            .messages
            .iter()
            .filter(|m| m.role == AgentRole::Assistant)
            .flat_map(|m| m.parts.iter())
            .filter_map(|pt| match pt {
                HubPart::ToolCall { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();

        let task_calls: Vec<&LiveToolCall> = view
            .thread
            .tool_calls
            .iter()
            .filter(|c| c.name == "task")
            .collect();
        // Skip non-task calls that already belong to a saved assistant message rendered above so
        // steps never appear twice during multi-step runs.
        let regular_calls: Vec<&LiveToolCall> = view
            .thread
            .tool_calls
            .iter()
            .filter(|c| c.name != "task" && !saved_call_ids.contains(c.call_id.as_str()))
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

/// Renders one command row per tool call inside a persisted assistant message's trace card.
pub fn render_message_tool_traces(
    view: &ChatView,
    tool_calls: &[(&str, &str, &str)],
    tool_results: &HashMap<&str, (&[HubPart], bool)>,
    audit_map: &HashMap<&str, &AgentToolCallRecord>,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let run_active = view.is_run_active();
    let any_pending = run_active
        && tool_calls
            .iter()
            .any(|(id, _, _)| view.thread.pending_tool_calls.contains(*id));
    let any_err = tool_calls.iter().any(|(id, _, _)| {
        let pending = run_active && view.thread.pending_tool_calls.contains(*id);
        !pending && tool_results.get(id).is_some_and(|(_, is_err)| *is_err)
    });
    let header_shape = if any_pending {
        StatusShape::Run
    } else if any_err {
        StatusShape::Bad
    } else {
        StatusShape::Ok
    };
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
                .w_full()
                .px(px(10.0))
                .py(px(7.0))
                .gap(px(6.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(status_indicator(header_shape, cx))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(header_label)),
                ),
        );

    let has_workspace = view
        .session
        .as_ref()
        .and_then(|s| s.workspace_dir.as_deref())
        .is_some();

    for &(call_id, name, args) in tool_calls {
        let expanded = view.expanded_traces.contains(call_id);
        let is_pending = run_active && view.thread.pending_tool_calls.contains(call_id);
        let audit = audit_map.get(call_id).copied();
        let live_meta = view.thread.live_tool_meta.get(call_id).copied();
        let result = tool_results.get(call_id).copied();

        // While a call is still executing, pull live streamed shell output if any instead of the
        // crash-recovery `[interrupted before tool finished]` placeholder in `AgentRole::Tool`.
        let out_text = if is_pending {
            view.thread
                .tool_calls
                .iter()
                .find(|c| c.call_id == call_id && !c.output.is_empty())
                .map(|c| c.output.clone())
        } else {
            result.map(|(parts, _)| {
                parts
                    .iter()
                    .filter_map(|pt| match pt {
                        HubPart::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        };

        let shape = if is_pending {
            StatusShape::Run
        } else if let Some(meta) = live_meta {
            match meta.status {
                ToolCallStatus::Ok => StatusShape::Ok,
                ToolCallStatus::Denied | ToolCallStatus::Blocked => StatusShape::Warn,
                _ => StatusShape::Bad,
            }
        } else if result.is_some_and(|(_, e)| e) {
            StatusShape::Bad
        } else {
            StatusShape::Ok
        };
        let is_err = !is_pending && matches!(shape, StatusShape::Bad | StatusShape::Warn);

        let dur = audit
            .and_then(|a| match (a.started_ms, a.ended_ms) {
                (Some(s), Some(e)) if e >= s => Some((e - s) as u64),
                _ => None,
            })
            .or_else(|| live_meta.map(|m| m.duration_ms));
        let exit_code = audit
            .and_then(|a| a.exit_code)
            .or_else(|| live_meta.and_then(|m| m.exit_code));
        let spilled = audit.is_some_and(|a| a.truncated || a.spill_ref.is_some());
        let matched_change = view
            .persisted_changes
            .iter()
            .find(|c| c.call_id == call_id)
            .map(|c| (c.id.clone(), c.path.clone(), c.added, c.removed));
        let live_change_stats = view
            .thread
            .file_changes
            .iter()
            .find(|c| c.call_id == call_id)
            .map(|c| (c.added, c.removed));
        let diff_stats = matched_change
            .as_ref()
            .map(|(_, _, a, r)| (*a, *r))
            .or(live_change_stats);
        let change_ref = matched_change.map(|(id, path, _, _)| (id, path));

        let pres = format_tool_trace(name, args, out_text.as_deref(), is_err, spilled, diff_stats);
        trace_box = trace_box.child(render_trace_row(
            call_id,
            name,
            pres,
            shape,
            is_err,
            dur,
            exit_code,
            expanded,
            spilled,
            has_workspace,
            change_ref,
            cx,
        ));
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
    let is_err = matches!(shape, StatusShape::Bad | StatusShape::Warn);
    let expanded = view.expanded_traces.contains(&call.call_id);
    let has_workspace = view
        .session
        .as_ref()
        .and_then(|s| s.workspace_dir.as_deref())
        .is_some();
    let live_diff = view
        .thread
        .file_changes
        .iter()
        .find(|c| c.call_id == call.call_id)
        .map(|c| (c.added, c.removed));
    let pres = format_tool_trace(
        &call.name,
        &call.arguments,
        (!call.output.is_empty()).then_some(call.output.as_str()),
        is_err,
        false,
        live_diff,
    );
    render_trace_row(
        &call.call_id,
        &call.name,
        pres,
        shape,
        is_err,
        call.duration_ms,
        call.exit_code,
        expanded,
        false,
        has_workspace,
        None,
        cx,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_trace_row(
    call_id: &str,
    tool_name: &str,
    pres: ToolTracePresentation,
    shape: StatusShape,
    is_err: bool,
    duration_ms: Option<u64>,
    exit_code: Option<i32>,
    expanded: bool,
    spilled: bool,
    has_workspace: bool,
    change_ref: Option<(String, String)>,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let verb = localized_tool_verb(tool_name, cx);
    let glyph = tool_icon(tool_name, pres.category);
    let mono = mono_font_family(cx);
    let metric_el = pres.metric.as_ref().map(|m| render_inline_metric(m, cx));

    let cid_toggle = call_id.to_owned();
    let row_header = div()
        .id(SharedString::from(format!("trace-step-{call_id}")))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(6.0))
        .cursor_pointer()
        .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_trace(cid_toggle.clone(), cx)))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(7.0))
                .child(status_indicator(shape, cx))
                .child(div().text_color(to_hsla(p.muted)).child(icon_xs(glyph)))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(verb.clone())),
                )
                .children((!pres.target.is_empty()).then(|| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(mono)
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .truncate()
                        .child(SharedString::from(pres.target.clone()))
                })),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(6.0))
                .children(metric_el)
                .children(
                    exit_code
                        .filter(|c| *c != 0)
                        .map(|c| badge(format!("exit {c}"), Tone::Danger, cx)),
                )
                .children(duration_ms.map(|ms| {
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format_duration_ms(ms)))
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

    let mut row = v_col()
        .w_full()
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(row_header);

    if expanded {
        row = row.child(render_trace_detail(
            call_id,
            tool_name,
            &verb,
            &pres,
            is_err,
            spilled,
            has_workspace,
            change_ref,
            cx,
        ));
    }

    row
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
