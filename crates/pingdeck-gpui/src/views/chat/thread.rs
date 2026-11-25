//! Center conversation message stream for AI Chat: user bubbles, assistant prose,
//! collapsible thinking, tool trace cards with full-output modal, per-message actions
//! (Copy / Edit / Retry / Fork / Delete), and Jump-to-latest pill.

use std::collections::HashMap;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, ScrollEvent, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::model::{AgentMessage, AgentRole, AgentToolCallRecord};
use pingdeck_desktop::core::model::hub::HubPart;
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_icon, btn_secondary, h_row,
    icon_xs, kv_chip, markdown_view, palette, status_indicator, to_hsla, v_col,
};

use super::ChatView;
use super::metrics::{format_effort_short, render_message_footer};
use super::reasoning::{
    ReasoningKind, extract_message_reasoning, render_no_reasoning_panel, render_reasoning_block,
    render_reasoning_header_chip, render_sealed_reasoning_card,
};
use super::trace::{render_live_turn, render_message_tool_traces};
use super::types::ChatModal;
use super::welcome::render_welcome_empty;

pub fn render_messages(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let has_live = view.is_run_active()
        || !view.thread.streaming_text.is_empty()
        || !view.thread.streaming_reasoning.is_empty()
        || !view.thread.tool_calls.is_empty();
    let has_pending_user = view.pending_user_text.is_some();

    let mut col = v_col()
        .w_full()
        .max_w(px(1080.0))
        .mx_auto()
        .gap(px(16.0))
        .py(px(16.0))
        .px(px(16.0));

    if view.thread.messages.is_empty() && !has_live && !has_pending_user {
        col = col.child(render_welcome_empty(cx));
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
                    col = col.child(render_user_message(view, msg, cx));
                }
                AgentRole::Assistant => {
                    col = col.child(render_assistant_message(
                        view,
                        msg,
                        &tool_results,
                        &audit_map,
                        cx,
                    ));
                }
                AgentRole::Tool => {}
                AgentRole::Summary => {
                    col = col.child(render_compaction_divider(cx));
                }
            }
        }

        if let Some(pending_text) = &view.pending_user_text {
            col = col.child(render_pending_user_bubble(pending_text, cx));
        }

        if has_live {
            col = col.child(render_live_turn(view, cx));
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

    let max_scroll_y = view.scroll_handle.max_offset().y.as_f32().max(0.0);
    let show_jump =
        max_scroll_y > 40.0 && (!view.stick_to_bottom || view.distance_from_bottom() > 48.0);
    let jump_label = if has_live {
        crate::ui::tr(cx).chat().thread_jump_to_latest()
    } else {
        crate::ui::tr(cx).chat().thread_latest()
    };

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
                .on_scroll(cx.listener(|this, ev: &ScrollEvent, window, cx| {
                    let delta_y = ev.delta.pixel_delta(window.line_height()).y.as_f32();
                    if delta_y > 0.5 {
                        // Scrolling upward toward earlier messages: immediately release follow lock.
                        if this.stick_to_bottom {
                            this.stick_to_bottom = false;
                            cx.notify();
                        }
                    } else if delta_y < -0.5 && !this.stick_to_bottom {
                        let max_y = this.scroll_handle.max_offset().y.as_f32().max(0.0);
                        let cur_y = this.scroll_handle.offset().y.as_f32();
                        let dist_now = (max_y + cur_y).max(0.0);
                        let next_y = (cur_y + delta_y).clamp(-max_y, 0.0);
                        let dist_est = (max_y + next_y).max(0.0);
                        if dist_now <= 24.0 || dist_est <= 16.0 {
                            this.stick_to_bottom = true;
                            cx.notify();
                        }
                    }
                }))
                .child(col),
        )
        .vertical_scrollbar(&view.scroll_handle)
        .children(show_jump.then(|| {
            div().absolute().right(px(18.0)).bottom(px(10.0)).child(
                btn_secondary("chat-jump-bottom", jump_label).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.stick_to_bottom = true;
                        this.scroll_handle.scroll_to_bottom();
                        cx.notify();
                    },
                )),
            )
        }))
}

fn render_pending_user_bubble(text: &str, cx: &gpui_kit::App) -> impl IntoElement {
    let p = palette(cx);
    v_col().w_full().items_end().child(
        div()
            .max_w(px(780.0))
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
                .max_w(px(780.0))
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
                        .tooltip(crate::ui::tr(cx).common().copy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let _ = clipboard::set_text(&txt_cp);
                            this.status_msg =
                                Some((Tone::Success, crate::ui::tr(cx).common().copied().into()));
                            cx.notify();
                        })),
                )
                .children(idle.then(|| {
                    btn_icon(
                        SharedString::from(format!("u-ed-{}", msg.id)),
                        UiIcon::Pencil,
                    )
                    .tooltip(crate::ui::tr(cx).chat().thread_edit_rerun())
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
                    .tooltip(crate::ui::tr(cx).chat().thread_branch_here())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.branch_from_message(mid_br.clone(), cx);
                    })),
                )
                .children(idle.then(|| {
                    btn_icon(
                        SharedString::from(format!("u-dl-{}", msg.id)),
                        UiIcon::Trash,
                    )
                    .tooltip(crate::ui::tr(cx).chat().thread_delete_2())
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
            .children(msg.meta.reasoning_effort.map(|eff| {
                badge(
                    format_effort_short(eff, crate::ui::active_language(cx)),
                    Tone::Accent,
                    cx,
                )
            }))
            .child(render_reasoning_header_chip(
                &msg.id,
                &r_info,
                reason_open,
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
                cx.listener(move |this, _, _, cx| this.toggle_reasoning(k_click.clone(), cx)),
                cx,
            ));
        }
        ReasoningKind::Sealed => {
            body = body.child(render_sealed_reasoning_card(msg, &r_info, reason_open, cx));
        }
        ReasoningKind::NoneReturned => {
            if reason_open {
                let pmeta = view.provider_meta_for_raw(raw_model);
                body = body.child(render_no_reasoning_panel(view, msg, &model, pmeta, cx));
            }
        }
    }

    if !tool_calls.is_empty() {
        body = body.child(render_message_tool_traces(
            view,
            &tool_calls,
            tool_results,
            audit_map,
            cx,
        ));
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
                .tooltip(crate::ui::tr(cx).chat().thread_copy_2())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let _ = clipboard::set_text(&joined_text);
                    this.status_msg = Some((
                        Tone::Success,
                        crate::ui::tr(cx).chat().thread_copied_2().into(),
                    ));
                    cx.notify();
                })),
        )
        .children(idle.then(|| {
            btn_icon(
                SharedString::from(format!("a-rt-{}", msg.id)),
                UiIcon::Refresh,
            )
            .tooltip(crate::ui::tr(cx).chat().thread_regenerate())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.retry_message(mid_retry.clone(), cx);
            }))
        }))
        .child(
            btn_icon(
                SharedString::from(format!("a-br-{}", msg.id)),
                UiIcon::Branch,
            )
            .tooltip(crate::ui::tr(cx).chat().thread_branch_2())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.branch_from_message(mid_br.clone(), cx);
            })),
        )
        .children(idle.then(|| {
            btn_icon(
                SharedString::from(format!("a-dl-{}", msg.id)),
                UiIcon::Trash,
            )
            .tooltip(crate::ui::tr(cx).chat().thread_delete_2())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.delete_message(mid_del.clone(), cx);
            }))
        }));

    body = body.child(actions_row);
    if let Some(footer) = render_message_footer(
        msg,
        view.thread.context_limit_tokens,
        view.thread.cost_currency.as_deref(),
        cx,
    ) {
        body = body.child(footer);
    }

    body
}

fn render_compaction_divider(cx: &gpui_kit::App) -> impl IntoElement {
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
            crate::ui::tr(cx).chat().thread_context_compacted(),
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
