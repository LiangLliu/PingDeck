//! Structured tool-call trace timeline cards, inline diff previews, and collapsible reasoning
//! blocks for the AI Chat thread stream.

use std::time::Instant;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::model::ToolCallStatus;
use pingdeck_desktop::state::LiveToolCall;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, collapsible_box, diff_lines_preview, h_row, icon_xs, palette,
    status_indicator, streaming_markdown_view, to_hsla, v_col,
};

use super::ChatView;
use super::metrics::{extract_inline_thinking, format_duration_ms};
use super::reasoning::render_reasoning_block;

pub fn render_live_turn(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> impl IntoElement {
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
        if zh {
            "正在生成回复…"
        } else {
            "Generating response…"
        }
    } else if !view.thread.tool_calls.is_empty() {
        if zh {
            "正在执行工具…"
        } else {
            "Running tools…"
        }
    } else if !live_reasoning.is_empty() {
        if zh {
            "正在深度思考…"
        } else {
            "Thinking…"
        }
    } else if view.submitting {
        if zh {
            "正在连接模型…"
        } else {
            "Connecting to model…"
        }
    } else if effort_explicit {
        if zh {
            "正在深度思考…"
        } else {
            "Thinking…"
        }
    } else if zh {
        "等待模型响应…"
    } else {
        "Waiting for model…"
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
            zh,
            cx.listener(move |this, _, _, cx| this.toggle_reasoning(k_click.clone(), cx)),
            cx,
        ));
    } else if visible_text.is_empty() && view.thread.tool_calls.is_empty() {
        let wait_text = if effort_explicit {
            if zh {
                "正在深度思考，等待模型响应…"
            } else {
                "Thinking — waiting for model response…"
            }
        } else if zh {
            "等待模型首字响应…"
        } else {
            "Waiting for first token…"
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
        let mut trace_box = v_col()
            .w_full()
            .rounded(px(10.0))
            .overflow_hidden()
            .bg(to_hsla(p.pane))
            .border_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)));

        for call in &view.thread.tool_calls {
            trace_box = trace_box.child(render_live_tool_step(view, call, cx));
        }
        body = body.child(trace_box);
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

fn summarize_tool_args(args: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(args)
        && let Some(obj) = val.as_object()
    {
        for key in [
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
