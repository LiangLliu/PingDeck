//! Bottom floating dock for AI Chat: open gates (AskUser / Plan), pending approval cards,
//! cost pause, editable queued turns bar, single working/todo strip, attachment/skill/comment
//! chips, and slimmed 5-control composer.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    SharedString, Styled as _, Window, div, px,
};
use spanbay_desktop::core::agent::attachments::attachment_parts;
use spanbay_desktop::core::app::agent_queue_update_impl;

use crate::bridge::spawn_blocking_into;
use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, badge, borderless_multiline, btn_danger,
    btn_ghost, btn_icon, btn_primary, h_row, icon_xs, palette, round_icon_btn, to_hsla, v_col,
};

use super::ChatView;
use super::approval_card::{approval_tone, render_approval_gate, render_approval_result};
use super::composer_popovers::{render_model_popover, render_perm_popover, render_skills_popover};
use super::context_meter::render_context_meter;
use super::gates::render_human_gate;
use super::gates_extra::render_cost_pause;
use super::todos::render_todo_panel;

impl ChatView {
    pub fn begin_edit_queued_turn(
        &mut self,
        queue_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.thread.queued_turns.iter().find(|q| q.id == queue_id) else {
            return;
        };
        let text: String = item
            .parts
            .iter()
            .filter_map(|part| match part {
                spanbay_desktop::core::model::hub::HubPart::Text { text, .. } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.editing_queue_id = Some(queue_id);
        self.selected_skills.clone_from(&item.skills);
        self.composer
            .update(cx, |c, cx| c.set_value(text, window, cx));
        cx.notify();
    }

    pub fn cancel_edit_queued_turn(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_queue_id.take().is_some() {
            self.composer
                .update(cx, |c, cx| c.set_value("", window, cx));
            cx.notify();
        }
    }

    pub fn save_edited_queued_turn(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(qid) = self.editing_queue_id.take() else {
            return;
        };
        let sid = self.thread.session_id.clone();
        let text = self.composer.read(cx).value().trim().to_owned();
        if sid.is_empty() || (text.is_empty() && self.pending_attachments.is_empty()) {
            cx.notify();
            return;
        }
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        let skills = std::mem::take(&mut self.selected_skills);
        let atts = std::mem::take(&mut self.pending_attachments);
        let parts = attachment_parts(&atts, &text);
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_queue_update_impl(state, &sid, &qid, parts, skills, &[]),
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                match res {
                    Ok(q) => this.thread.queued_turns = q,
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
        cx.notify();
    }
}

pub fn render_dock(
    view: &ChatView,
    stack_max_h: Pixels,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let running = view.is_run_active();

    let mut stack: Vec<(Option<Tone>, AnyElement)> = Vec::new();
    for (ix, (run_id, gate)) in view.thread.open_gates.iter().enumerate() {
        let row = render_human_gate(view, run_id, gate, ix == 0, cx).into_any_element();
        stack.push((Some(Tone::Accent), row));
    }
    for (ix, appr) in view.thread.pending_approvals.iter().enumerate() {
        let row = render_approval_gate(view, appr, ix == 0, cx).into_any_element();
        stack.push((Some(approval_tone(appr)), row));
    }
    if view.thread.pending_approvals.is_empty()
        && let Some(notice) = &view.last_approval_notice
    {
        stack.push((None, render_approval_result(notice, cx).into_any_element()));
    }
    if let Some(pause) = &view.thread.cost_pause {
        stack.push((
            Some(Tone::Warning),
            render_cost_pause(pause, cx).into_any_element(),
        ));
    }
    if !view.thread.queued_turns.is_empty() {
        stack.push((None, render_editable_queue_bar(view, cx).into_any_element()));
    }
    if let Some(todos) = render_todo_panel(view, cx) {
        stack.push((None, todos.into_any_element()));
    }

    // One tray above the input: rows share the card border, split by 1px dividers, scroll past the cap.
    // Gate-like rows get a 3px tone bar + soft tint (the tray tint paints the rounded corners).
    let tray_line = to_hsla(p.line.composite_over(p.pane));
    let tray = (!stack.is_empty()).then(|| {
        let mut rows = v_col().w_full();
        for (ix, (tone, row)) in stack.into_iter().enumerate() {
            let mut cell = div().w_full().child(row);
            if let Some(tone) = tone {
                let (bar, soft) = match tone {
                    Tone::Danger => (p.bad, p.bad_soft),
                    Tone::Warning => (p.warn, p.warn_soft),
                    _ => (p.accent, p.accent_soft),
                };
                cell = cell
                    .border_l(px(3.0))
                    .border_color(to_hsla(bar))
                    .bg(to_hsla(soft));
                if ix == 0 {
                    cell = cell.rounded_t(px(13.0));
                }
            }
            let mut outer = div().w_full().child(cell);
            if ix > 0 {
                outer = outer.border_t_1().border_color(tray_line);
            }
            rows = rows.child(outer);
        }
        div()
            .id("chat-dock-stack")
            .w_full()
            .max_h(stack_max_h)
            .rounded_t(px(13.0))
            .bg(to_hsla(p.pane))
            .border_b_1()
            .border_color(tray_line)
            .overflow_y_scrollbar()
            .child(rows)
    });

    let perm_mode = view
        .session
        .as_ref()
        .map(|s| s.permission_mode)
        .unwrap_or_default();
    let (active_model_label, no_model_configured) =
        match view.session.as_ref().and_then(|s| s.target.as_ref()) {
            Some(t) => (view.format_target_label(t), false),
            None => match view.available_models.first() {
                Some(m) => (ch.composer_auto(m.display_label()), false),
                None => (ch.composer_no_model().into(), true),
            },
        };

    let mut composer_card = v_col()
        .w_full()
        .px(px(12.0))
        .pt(px(8.0))
        .pb(px(8.0))
        .gap(px(6.0));

    if let Some(editing_id) = &view.editing_queue_id
        && let Some(pos) = view
            .thread
            .queued_turns
            .iter()
            .position(|q| &q.id == editing_id)
    {
        composer_card = composer_card.child(
            h_row()
                .id("chat-queue-edit-banner")
                .w_full()
                .justify_between()
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.accent_soft))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.accent_ink))
                        .child(ch.queue_editing_banner(pos + 1)),
                )
                .child(
                    h_row()
                        .gap(px(4.0))
                        .child(
                            btn_ghost("chat-q-edit-cancel", crate::ui::tr(cx).common().cancel())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.cancel_edit_queued_turn(window, cx);
                                })),
                        )
                        .child(
                            btn_primary("chat-q-edit-save", ch.queue_save_edit()).on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.save_edited_queued_turn(window, cx);
                                }),
                            ),
                        ),
                ),
        );
    }

    if !view.pending_attachments.is_empty()
        || !view.selected_skills.is_empty()
        || !view.diff_comments.is_empty()
    {
        let mut chips = h_row().gap(px(4.0)).flex_wrap();
        for att in &view.pending_attachments {
            let aid = att.id.clone();
            chips = chips.child(
                h_row()
                    .gap(px(4.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.sunken))
                    .child(icon_xs(UiIcon::FileText))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .child(att.name.clone()),
                    )
                    .child(
                        btn_icon(
                            SharedString::from(format!("chat-att-rm-{}", att.id)),
                            UiIcon::Close,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.pending_attachments.retain(|a| a.id != aid);
                            cx.notify();
                        })),
                    ),
            );
        }
        for sk in &view.selected_skills {
            let sname = sk.clone();
            chips = chips.child(
                h_row()
                    .gap(px(4.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.accent_soft))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.accent_ink))
                            .child(format!("/{sk}")),
                    )
                    .child(
                        btn_icon(
                            SharedString::from(format!("chat-sk-rm-{sk}")),
                            UiIcon::Close,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected_skills.retain(|s| s != &sname);
                            cx.notify();
                        })),
                    ),
            );
        }
        for (ix, dc) in view.diff_comments.iter().enumerate() {
            chips = chips.child(
                h_row()
                    .gap(px(4.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.accent_soft))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.accent_ink))
                            .child(format!("@{}:{} · {}", dc.path, dc.line, dc.comment)),
                    )
                    .child(
                        btn_icon(
                            SharedString::from(format!("chat-dc-rm-{ix}")),
                            UiIcon::Close,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if ix < this.diff_comments.len() {
                                this.diff_comments.remove(ix);
                                cx.notify();
                            }
                        })),
                    ),
            );
        }
        composer_card = composer_card.child(chips);
    }

    composer_card = composer_card
        .child(borderless_multiline(&view.composer))
        .child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .pt(px(4.0))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(
                    h_row()
                        .gap(px(4.0))
                        .flex_wrap()
                        .child(
                            btn_icon("chat-attach-btn", UiIcon::Plus)
                                .tooltip(ch.composer_attach_files())
                                .on_click(cx.listener(|this, _, _, cx| this.pick_attachments(cx))),
                        )
                        .child(render_skills_popover(view, cx))
                        .child(render_perm_popover(view, perm_mode, cx)),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(render_context_meter(view, cx))
                        .child(render_model_popover(
                            view,
                            active_model_label,
                            no_model_configured,
                            cx,
                        ))
                        .child(if running {
                            h_row()
                                .gap(px(4.0))
                                .child(
                                    btn_ghost(
                                        "chat-queue-send",
                                        crate::ui::tr(cx).common().wire_queue(),
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.send_message(window, cx);
                                        },
                                    )),
                                )
                                .child(
                                    btn_ghost("chat-steer", ch.gates_extra_steer_now())
                                        .tooltip(ch.composer_steer_run_now_enter())
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.steer_active_run(window, cx);
                                        })),
                                )
                                .child(
                                    btn_danger("chat-status-stop", ch.card_stop())
                                        .icon(icon_xs(UiIcon::Stop))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.stop_active_run(cx);
                                        })),
                                )
                                .into_any_element()
                        } else {
                            round_icon_btn("chat-send", UiIcon::ArrowUp, Tone::Accent)
                                .tooltip(ch.composer_send_enter())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.send_message(window, cx);
                                }))
                                .into_any_element()
                        }),
                ),
        );

    let card = v_col()
        .w_full()
        .rounded(px(14.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
        .shadow_sm()
        .children(tray)
        .child(composer_card);
    super::chat_column(v_col().pt(px(6.0)).pb(px(10.0)).child(card)).flex_none()
}

fn render_editable_queue_bar(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mut box_el = v_col()
        .id("chat-queued-bar")
        .w_full()
        .p(px(8.0))
        .gap(px(5.0))
        .child(if view.thread.queue_paused {
            let actions = h_row()
                .gap(px(4.0))
                .child(
                    btn_primary("chat-q-resume", ch.queue_resume_send())
                        .on_click(cx.listener(|this, _, _, cx| this.queue_resume(cx))),
                )
                .child(
                    btn_ghost("chat-q-clear", ch.queue_clear())
                        .on_click(cx.listener(|this, _, _, cx| this.queue_clear(cx))),
                );
            alert_banner(
                Tone::Warning,
                ch.queue_paused_line(view.thread.queued_turns.len()),
                Some(actions.into_any_element()),
                cx,
            )
        } else {
            h_row()
                .justify_between()
                .child(badge(
                    ch.gates_extra_queued(view.thread.queued_turns.len()),
                    Tone::Accent,
                    cx,
                ))
                .child(
                    btn_ghost("chat-q-clear", ch.queue_clear())
                        .on_click(cx.listener(|this, _, _, cx| this.queue_clear(cx))),
                )
        });

    for (i, q) in view.thread.queued_turns.iter().enumerate() {
        let qid_st = q.id.clone();
        let qid_ed = q.id.clone();
        let qid_rm = q.id.clone();
        let preview: String = q
            .parts
            .iter()
            .filter_map(|part| match part {
                spanbay_desktop::core::model::hub::HubPart::Text { text, .. } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        box_el = box_el.child(
            h_row()
                .gap(px(6.0))
                .px(px(6.0))
                .py(px(4.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(format!("#{} {preview}", i + 1)),
                )
                .children((!view.thread.queue_paused).then(|| {
                    btn_ghost(
                        SharedString::from(format!("chat-q-steer-{}", q.id)),
                        ch.gates_extra_steer_now(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.queue_steer(qid_st.clone(), cx);
                    }))
                }))
                .child(
                    btn_ghost(
                        SharedString::from(format!("chat-q-edit-{}", q.id)),
                        ch.queue_edit(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.begin_edit_queued_turn(qid_ed.clone(), window, cx);
                    })),
                )
                .child(
                    btn_ghost(SharedString::from(format!("chat-q-rm-{}", q.id)), "")
                        .icon(icon_xs(UiIcon::Close))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.queue_remove(qid_rm.clone(), cx);
                        })),
                ),
        );
    }
    box_el
}
