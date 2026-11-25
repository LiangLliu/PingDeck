//! Center conversation message stream for AI Chat: user bubbles, assistant prose,
//! collapsible thinking, tool trace cards with full-output modal, per-message actions
//! (Copy / Edit / Retry / Fork / Delete), and Jump-to-latest pill.

use std::collections::HashMap;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::model::{AgentMessage, AgentRole, AgentToolCallRecord};
use pingdeck_desktop::core::model::hub::HubPart;
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_secondary,
    h_row, icon_xs, kv_chip, markdown_view, palette, status_indicator, to_hsla, v_col,
};

use super::ChatView;
use super::metrics::{format_duration_ms, format_effort_short, render_message_footer};
use super::reasoning::{
    ReasoningKind, extract_message_reasoning, render_no_reasoning_panel, render_reasoning_block,
    render_reasoning_header_chip, render_sealed_reasoning_card,
};
use super::trace::{render_live_turn, render_trace_step};
use super::types::ChatModal;
use super::welcome::render_welcome_empty;

pub fn render_messages(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let has_live = view.is_run_active()
        || !view.thread.streaming_text.is_empty()
        || !view.thread.streaming_reasoning.is_empty()
        || !view.thread.tool_calls.is_empty();
    let has_pending_user = view.pending_user_text.is_some();

    let mut col = v_col()
        .w_full()
        .max_w(px(760.0))
        .mx_auto()
        .gap(px(18.0))
        .py(px(20.0))
        .px(px(20.0));

    if view.thread.messages.is_empty() && !has_live && !has_pending_user {
        col = col.child(render_welcome_empty(zh, cx));
    } else {
        let mut tool_results: HashMap<&str, (&[HubPart], bool)> = HashMap::new();
        for msg in &view.thread.messages {
            for part in &msg.parts {
                if let HubPart::ToolResult {
                    call_id,
                    content,
                    is_error,
                    ..
                } = part
                {
                    tool_results.insert(call_id.as_str(), (content.as_slice(), *is_error));
                }
            }
        }
        let audit_map: HashMap<&str, &AgentToolCallRecord> = view
            .audit_tool_calls
            .iter()
            .map(|r| (r.call_id.as_str(), r))
            .collect();

        for msg in &view.thread.messages {
            match msg.role {
                AgentRole::User => {
                    col = col.child(render_user_message(view, msg, zh, cx));
                }
                AgentRole::Assistant => {
                    col = col.child(render_assistant_message(
                        view,
                        msg,
                        &tool_results,
                        &audit_map,
                        zh,
                        cx,
                    ));
                }
                AgentRole::Tool => {}
                AgentRole::Summary => {
                    col = col.child(render_compaction_divider(zh, cx));
                }
            }
        }

        if let Some(pending_text) = &view.pending_user_text {
            col = col.child(render_pending_user_bubble(pending_text, cx));
        }

        if has_live {
            col = col.child(render_live_turn(view, zh, cx));
        }
    }

    if let Some(err) = &view.thread.last_error {
        col = col.child(
            h_row()
                .gap(px(8.0))
                .p(px(12.0))
                .rounded(px(10.0))
                .bg(to_hsla(p.bad_soft))
                .border_1()
                .border_color(to_hsla(p.bad_line.composite_over(p.bad_soft)))
                .child(status_indicator(StatusShape::Bad, cx))
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(to_hsla(p.bad))
                        .child(SharedString::from(err.clone())),
                ),
        );
    }

    let show_jump = view.thread.messages.len() >= 6;

    div()
        .relative()
        .flex_1()
        .min_h_0()
        .w_full()
        .child(
            div()
                .id("chat-messages-scroll")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&view.scroll_handle)
                .child(col),
        )
        .vertical_scrollbar(&view.scroll_handle)
        .children(show_jump.then(|| {
            div().absolute().right(px(20.0)).bottom(px(10.0)).child(
                btn_secondary(
                    "chat-jump-bottom",
                    if zh { "回到底部 ↓" } else { "Latest ↓" },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.scroll_handle.scroll_to_bottom();
                    cx.notify();
                })),
            )
        }))
}

fn render_pending_user_bubble(text: &str, cx: &gpui_kit::App) -> impl IntoElement {
    let p = palette(cx);
    v_col().w_full().items_end().child(
        div()
            .max_w(px(580.0))
            .px(px(14.0))
            .py(px(10.0))
            .rounded(px(16.0))
            .bg(to_hsla(p.bubble_user))
            .border_1()
            .border_color(to_hsla(p.line.composite_over(p.bubble_user)))
            .child(markdown_view("chat-u-pending", text.to_owned())),
    )
}

fn render_user_message(
    view: &ChatView,
    msg: &AgentMessage,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let text = extract_text_parts(&msg.parts);
    let txt_cp = text.clone();
    let txt_ed = text.clone();
    let mid_ed = msg.id.clone();
    let mid_br = msg.id.clone();
    let mid_del = msg.id.clone();
    let idle = !view.is_run_active();

    v_col()
        .w_full()
        .items_end()
        .gap(px(4.0))
        .child(
            div()
                .max_w(px(580.0))
                .px(px(14.0))
                .py(px(10.0))
                .rounded(px(16.0))
                .bg(to_hsla(p.bubble_user))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.bubble_user)))
                .child(markdown_view(
                    SharedString::from(format!("chat-u-{}", msg.id)),
                    text,
                )),
        )
        .children((!msg.attachments.is_empty()).then(|| {
            let mut row = h_row().gap(px(4.0)).flex_wrap().justify_end();
            for att in &msg.attachments {
                row = row.child(kv_chip("file", att.name.clone(), cx));
            }
            row
        }))
        .child(
            h_row()
                .gap(px(2.0))
                .child(
                    btn_icon(SharedString::from(format!("u-cp-{}", msg.id)), UiIcon::Copy)
                        .tooltip(if zh { "复制" } else { "Copy" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let _ = clipboard::set_text(&txt_cp);
                            this.status_msg =
                                Some((Tone::Success, if zh { "已复制" } else { "Copied" }.into()));
                            cx.notify();
                        })),
                )
                .children(idle.then(|| {
                    btn_icon(
                        SharedString::from(format!("u-ed-{}", msg.id)),
                        UiIcon::Pencil,
                    )
                    .tooltip(if zh {
                        "编辑并重发"
                    } else {
                        "Edit & rerun"
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.modal_textarea.update(cx, |s, cx| {
                            s.set_value(txt_ed.clone(), window, cx);
                        });
                        this.active_modal = Some(ChatModal::EditMessage {
                            message_id: mid_ed.clone(),
                        });
                        cx.notify();
                    }))
                }))
                .child(
                    btn_icon(
                        SharedString::from(format!("u-br-{}", msg.id)),
                        UiIcon::Branch,
                    )
                    .tooltip(if zh { "从此处分支" } else { "Branch here" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.branch_from_message(mid_br.clone(), cx);
                    })),
                )
                .children(idle.then(|| {
                    btn_icon(
                        SharedString::from(format!("u-dl-{}", msg.id)),
                        UiIcon::Trash,
                    )
                    .tooltip(if zh { "删除消息" } else { "Delete" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_message(mid_del.clone(), cx);
                    }))
                })),
        )
}

fn render_assistant_message(
    view: &ChatView,
    msg: &AgentMessage,
    tool_results: &HashMap<&str, (&[HubPart], bool)>,
    audit_map: &HashMap<&str, &AgentToolCallRecord>,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let raw_model = msg.meta.served_model.as_deref().or(msg.model.as_deref());
    let model = raw_model
        .map(|raw| view.format_raw_model_label(raw))
        .unwrap_or_else(|| "Assistant".to_owned());

    let recorded_dur = view.thinking_durations.get(&msg.id).copied();
    let (r_info, text_chunks) = extract_message_reasoning(msg, recorded_dur);
    let tool_calls: Vec<(&str, &str, &str)> = msg
        .parts
        .iter()
        .filter_map(|part| match part {
            HubPart::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some((id.as_str(), name.as_str(), arguments.as_str())),
            _ => None,
        })
        .collect();

    let header_timing = match (msg.meta.ttft_ms, msg.meta.stream_ms) {
        (Some(ttft), Some(stream)) => Some(format!(
            "{} (TTFT {})",
            format_duration_ms(ttft.saturating_add(stream)),
            format_duration_ms(ttft),
        )),
        (Some(ttft), None) => Some(format!("TTFT {}", format_duration_ms(ttft))),
        (None, Some(stream)) => Some(format_duration_ms(stream)),
        (None, None) => None,
    };

    let r_key = format!("reason-{}", msg.id);
    let reason_open = view.expanded_reasoning.contains(&r_key);

    let mut body = v_col().w_full().gap(px(10.0)).child(
        h_row()
            .gap(px(8.0))
            .flex_wrap()
            .child(
                div()
                    .size(px(22.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.accent_soft))
                    .text_color(to_hsla(p.accent_ink))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon_xs(UiIcon::Sparkles)),
            )
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink))
                    .child(SharedString::from(model.clone())),
            )
            .children(
                msg.meta
                    .reasoning_effort
                    .map(|eff| badge(format_effort_short(eff, zh), Tone::Accent, cx)),
            )
            .children(header_timing.map(|t| {
                div()
                    .text_xs()
                    .text_color(to_hsla(p.faint))
                    .child(SharedString::from(t))
            }))
            .child(render_reasoning_header_chip(
                &msg.id,
                &r_info,
                reason_open,
                zh,
                cx,
            )),
    );

    match r_info.kind {
        ReasoningKind::Plaintext => {
            let k_click = r_key.clone();
            body = body.child(render_reasoning_block(
                r_key,
                reason_open,
                false,
                r_info.text,
                r_info.duration_ms,
                r_info.reasoning_tokens,
                zh,
                cx.listener(move |this, _, _, cx| this.toggle_reasoning(k_click.clone(), cx)),
                cx,
            ));
        }
        ReasoningKind::Sealed => {
            body = body.child(render_sealed_reasoning_card(
                msg,
                &r_info,
                reason_open,
                zh,
                cx,
            ));
        }
        ReasoningKind::NoneReturned => {
            if reason_open {
                let pmeta = view.provider_meta_for_raw(raw_model);
                body = body.child(render_no_reasoning_panel(view, msg, &model, pmeta, zh, cx));
            }
        }
    }

    if !tool_calls.is_empty() {
        let any_err = tool_calls
            .iter()
            .any(|(id, _, _)| tool_results.get(id).is_some_and(|(_, is_err)| *is_err));
        let header_label = if zh {
            format!("执行了 {} 个工具调用", tool_calls.len())
        } else if tool_calls.len() == 1 {
            "1 tool call".into()
        } else {
            format!("{} tool calls", tool_calls.len())
        };

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

        for (call_id, name, args) in tool_calls {
            let expanded = view.expanded_traces.contains(call_id);
            let audit = audit_map.get(call_id).copied();
            let result = tool_results.get(call_id).copied();
            let out_text = result.map(|(parts, _)| extract_text_parts(parts));
            let is_err = result.is_some_and(|(_, e)| e);
            let dur = audit.and_then(|a| match (a.started_ms, a.ended_ms) {
                (Some(s), Some(e)) if e >= s => Some((e - s) as u64),
                _ => None,
            });
            let cid_click = call_id.to_owned();
            let cid_full = call_id.to_owned();
            let name_full = name.to_owned();
            trace_box = trace_box.child(render_trace_step(
                call_id,
                name,
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
                            if zh {
                                "查看完整输出"
                            } else {
                                "View full output"
                            },
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_tool_output(cid_full.clone(), name_full.clone(), cx);
                        })),
                    ),
                );
            }
        }
        body = body.child(trace_box);
    }

    let joined_text = text_chunks.join("\n");
    if !joined_text.is_empty() {
        body = body.child(div().pl(px(30.0)).child(markdown_view(
            SharedString::from(format!("chat-a-{}", msg.id)),
            joined_text.clone(),
        )));
    }

    if let Some(err) = &msg.error {
        body = body.child(
            h_row()
                .ml(px(30.0))
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(7.0))
                .bg(to_hsla(p.bad_soft))
                .text_xs()
                .text_color(to_hsla(p.bad))
                .child(status_indicator(StatusShape::Bad, cx))
                .child(SharedString::from(err.clone())),
        );
    }

    let mid_retry = msg.id.clone();
    let mid_br = msg.id.clone();
    let mid_del = msg.id.clone();
    let idle = !view.is_run_active();
    let actions_row = h_row()
        .pl(px(30.0))
        .gap(px(4.0))
        .child(
            btn_icon(SharedString::from(format!("a-cp-{}", msg.id)), UiIcon::Copy)
                .tooltip(if zh { "复制回复" } else { "Copy" })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let _ = clipboard::set_text(&joined_text);
                    this.status_msg = Some((
                        Tone::Success,
                        if zh { "已复制回复" } else { "Copied" }.into(),
                    ));
                    cx.notify();
                })),
        )
        .children(idle.then(|| {
            btn_icon(
                SharedString::from(format!("a-rt-{}", msg.id)),
                UiIcon::Refresh,
            )
            .tooltip(if zh { "重新生成" } else { "Regenerate" })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.retry_message(mid_retry.clone(), cx);
            }))
        }))
        .child(
            btn_icon(
                SharedString::from(format!("a-br-{}", msg.id)),
                UiIcon::Branch,
            )
            .tooltip(if zh { "从此处分支" } else { "Branch" })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.branch_from_message(mid_br.clone(), cx);
            })),
        )
        .children(idle.then(|| {
            btn_icon(
                SharedString::from(format!("a-dl-{}", msg.id)),
                UiIcon::Trash,
            )
            .tooltip(if zh { "删除消息" } else { "Delete" })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.delete_message(mid_del.clone(), cx);
            }))
        }));

    body = body.child(actions_row);
    if let Some(footer) = render_message_footer(msg, view.thread.cost_currency.as_deref(), zh, cx) {
        body = body.child(footer);
    }

    body
}

fn render_compaction_divider(zh: bool, cx: &gpui_kit::App) -> impl IntoElement {
    let p = palette(cx);
    h_row()
        .gap(px(10.0))
        .py(px(4.0))
        .child(
            div()
                .flex_1()
                .h(px(1.0))
                .bg(to_hsla(p.line.composite_over(p.surface))),
        )
        .child(badge(
            if zh {
                "已压缩上下文摘要"
            } else {
                "Context compacted"
            },
            Tone::Neutral,
            cx,
        ))
        .child(
            div()
                .flex_1()
                .h(px(1.0))
                .bg(to_hsla(p.line.composite_over(p.surface))),
        )
}

fn extract_text_parts(parts: &[HubPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            HubPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
