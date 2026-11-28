//! Floating top-accented AskUser Question and Plan Approval cards docked above the Chat
//! composer.

use gpui_kit::{
    AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, SharedString, Styled as _, Window, div, px,
};
use spanbay_desktop::core::agent::model::{HumanGateBody, HumanGateDto};
use spanbay_desktop::core::app::agent_plan_approve_impl;
use spanbay_desktop::core::model::settings::ToolApproval;
use spanbay_desktop::state::GraphNodeStatus;

use crate::bridge::spawn_async_into;
use crate::ui::{
    InputEvent, InputState, ScrollableElement as _, Tone, WaitKind, badge, btn_ghost, btn_primary,
    btn_secondary, choice_chip, h_row, markdown_view, palette, status_badge, text_input, to_hsla,
    v_col,
};

use crate::views::runs::graph::render_plan_preview_graph;

use super::ChatView;

impl ChatView {
    pub fn approve_plan(
        &mut self,
        mode: Option<ToolApproval>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        // The approval goes to the model in the interface language, followed by any composer note.
        let note = self.composer.read(cx).value().trim().to_owned();
        let mut text = crate::ui::tr(cx).chat().plan_approved_text().to_owned();
        if !note.is_empty() {
            text.push_str("\n\n");
            text.push_str(&note);
        }
        self.plan_revising = false;
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        self.reset_composer_placeholder(window, cx);
        let sid_check = sid.clone();
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_plan_approve_impl(&state, &sid, Some(text), mode).await },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                match res {
                    Ok(_) => {
                        this.plan_mode = false;
                        this.plan_revising = false;
                        if let Some(m) = mode
                            && let Some(session) = &mut this.session
                        {
                            session.permission_mode = m;
                        }
                        this.thread.open_gates.clear();
                        this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).chat().act_plan_approved().into(),
                        ));
                    }
                    Err(e) => {
                        this.pending_draft_sync = Some(note);
                        this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    /// "Keep planning": the next composer text revises the plan (sent by ↵ while the gate waits).
    pub fn keep_planning(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.plan_revising = true;
        let hint = crate::ui::tr(cx).chat().gates_keep_planning_hint();
        self.composer.update(cx, |c, cx| {
            c.set_placeholder(hint, window, cx);
            c.focus(window, cx);
        });
        cx.notify();
    }

    /// Selects or toggles option `idx` (1-based) on the question gate of `call_id`.
    pub fn toggle_question_choice(
        &mut self,
        call_id: &str,
        idx: u8,
        multi: bool,
        cx: &mut Context<Self>,
    ) {
        let entry = self
            .question_draft_choices
            .entry(call_id.to_owned())
            .or_default();
        if multi {
            if let Some(pos) = entry.iter().position(|&v| v == idx) {
                entry.remove(pos);
            } else {
                entry.push(idx);
            }
        } else if entry.contains(&idx) {
            entry.clear();
        } else {
            entry.clear();
            entry.push(idx);
        }
        cx.notify();
    }

    /// Ensures secondary question cards (`ix > 0`) each have their own [`InputState`] entity and
    /// `PressEnter` / `Change` subscription so multiple open question cards never share one input.
    pub(super) fn sync_question_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_secondary: std::collections::HashSet<String> = self
            .thread
            .open_gates
            .iter()
            .enumerate()
            .filter(|&(ix, (_, g))| ix > 0 && matches!(g.body, HumanGateBody::Question { .. }))
            .map(|(_, (_, g))| g.call_id.clone())
            .collect();
        self.question_other_inputs
            .retain(|cid, _| active_secondary.contains(cid));
        for (ix, (run_id, gate)) in self.thread.open_gates.clone().into_iter().enumerate() {
            if ix == 0 {
                continue;
            }
            let HumanGateBody::Question { multi, .. } = gate.body else {
                continue;
            };
            let cid = gate.call_id;
            if self.question_other_inputs.contains_key(&cid) {
                continue;
            }
            let ph = crate::ui::tr(cx).chat().ph_custom_answer();
            let input: Entity<InputState> =
                cx.new(|cx| InputState::new(window, cx).placeholder(ph));
            let cid_sub = cid.clone();
            let rid_sub = run_id.clone();
            let sub =
                cx.subscribe_in(
                    &input,
                    window,
                    move |this, inp, ev: &InputEvent, w, cx| match ev {
                        InputEvent::PressEnter { .. } => {
                            this.answer_question(rid_sub.clone(), cid_sub.clone(), false, w, cx);
                        }
                        InputEvent::Change if !multi => {
                            if !inp.read(cx).value().trim().is_empty()
                                && let Some(choices) = this.question_draft_choices.get_mut(&cid_sub)
                                && !choices.is_empty()
                            {
                                choices.clear();
                                cx.notify();
                            }
                        }
                        _ => {}
                    },
                );
            self._subscriptions.push(sub);
            self.question_other_inputs.insert(cid, input);
        }
    }

    pub(super) fn reset_composer_placeholder(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ph = crate::ui::tr(cx).chat().ph_composer();
        self.composer
            .update(cx, |c, cx| c.set_placeholder(ph, window, cx));
    }
}

pub fn render_human_gate(
    view: &ChatView,
    run_id: &str,
    gate: &HumanGateDto,
    first: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    match &gate.body {
        HumanGateBody::Question {
            question,
            options,
            multi,
        } => {
            let selected = view
                .question_draft_choices
                .get(&gate.call_id)
                .cloned()
                .unwrap_or_default();
            let mut opts_row = v_col().gap(px(6.0));
            for (i, opt) in options.iter().enumerate() {
                let idx = (i + 1) as u8;
                let active = selected.contains(&idx);
                let cid = gate.call_id.clone();
                let is_multi = *multi;
                opts_row = opts_row.child(
                    h_row()
                        .gap(px(8.0))
                        .child(
                            choice_chip(
                                SharedString::from(format!("q-opt-{}-{idx}", gate.call_id)),
                                format!("{idx}. {}", opt.label),
                                active,
                                cx,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.toggle_question_choice(&cid, idx, is_multi, cx);
                                },
                            )),
                        )
                        .children(opt.detail.as_ref().map(|d| {
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(d.clone())
                        })),
                );
            }
            let r_sub = run_id.to_owned();
            let c_sub = gate.call_id.clone();
            let r_skip = run_id.to_owned();
            let c_skip = gate.call_id.clone();
            let mut card = v_col().w_full();
            if first {
                let r_key = run_id.to_owned();
                let c_key = gate.call_id.clone();
                let opt_len = options.len();
                let is_multi = *multi;
                card = card.track_focus(&view.gate_focus).on_key_down(cx.listener(
                    move |this, ev: &KeyDownEvent, window, cx| {
                        if this.active_modal.is_some() {
                            if ev.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.dismiss_modal_if_open(cx);
                            }
                            return;
                        }
                        let in_other = this
                            .question_other_input
                            .read(cx)
                            .focus_handle(cx)
                            .is_focused(window);
                        if !in_other
                            && !ev.keystroke.modifiers.platform
                            && !ev.keystroke.modifiers.control
                            && !ev.keystroke.modifiers.alt
                            && let [b @ b'1'..=b'9'] = ev.keystroke.key.as_bytes()
                        {
                            let idx = b - b'0';
                            if usize::from(idx) <= opt_len {
                                cx.stop_propagation();
                                this.toggle_question_choice(&c_key, idx, is_multi, cx);
                                return;
                            }
                        }
                        if !in_other && ev.keystroke.key == "enter" {
                            cx.stop_propagation();
                            this.answer_question(r_key.clone(), c_key.clone(), false, window, cx);
                        } else if ev.keystroke.key == "escape" {
                            cx.stop_propagation();
                            if in_other {
                                window.focus(&this.gate_focus, cx);
                                cx.notify();
                            } else {
                                this.answer_question(
                                    r_key.clone(),
                                    c_key.clone(),
                                    true,
                                    window,
                                    cx,
                                );
                            }
                        }
                    },
                ));
            }
            card.child(
                v_col()
                    .p(px(12.0))
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(status_badge(
                                GraphNodeStatus::Waiting,
                                Some(WaitKind::Question),
                                cx,
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .child(ch.gates_agent_question()),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .child(SharedString::from(question.clone())),
                    )
                    .child(opts_row)
                    .child(text_input(if first {
                        &view.question_other_input
                    } else {
                        view.question_other_inputs
                            .get(&gate.call_id)
                            .unwrap_or(&view.question_other_input)
                    }))
                    .child(
                        h_row()
                            .w_full()
                            .justify_between()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.faint))
                                    .child(ch.gates_question_keys()),
                            )
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .child(
                                        btn_secondary(
                                            SharedString::from(format!("q-skip-{}", gate.call_id)),
                                            ch.gate_question_skip(),
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.answer_question(
                                                    r_skip.clone(),
                                                    c_skip.clone(),
                                                    true,
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    )
                                    .child(
                                        btn_primary(
                                            SharedString::from(format!(
                                                "q-submit-{}",
                                                gate.call_id
                                            )),
                                            ch.gates_submit_answer(),
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.answer_question(
                                                    r_sub.clone(),
                                                    c_sub.clone(),
                                                    false,
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            )
        }
        HumanGateBody::Plan {
            markdown,
            title,
            steps,
            tasks,
        } => {
            let has_graph = tasks.len() >= 2;
            let show_md = !has_graph || view.plan_gate_show_markdown.contains(&gate.id);
            let empty_groups = std::collections::HashSet::new();
            let graph_preview =
                has_graph.then(|| render_plan_preview_graph(&gate.id, tasks, &empty_groups, cx));
            let count_badge = match &graph_preview {
                Some((stages, _)) => ch.gates_plan_graph_summary(*stages, tasks.len()),
                None => ch.gates_steps(steps),
            };
            let gid_graph = gate.id.clone();
            let gid_md = gate.id.clone();

            let body_el = if !show_md && let Some((_, graph_el)) = graph_preview {
                graph_el
            } else {
                div()
                    .id(SharedString::from(format!("plan-md-{}", gate.id)))
                    .max_h(px(220.0))
                    .p(px(10.0))
                    .rounded(px(8.0))
                    .bg(to_hsla(p.sunken))
                    .overflow_y_scrollbar()
                    .child(markdown_view(
                        SharedString::from(format!("plan-body-{}", gate.id)),
                        markdown.clone(),
                    ))
                    .into_any_element()
            };

            let mut card = v_col().w_full();
            if first {
                card = card.track_focus(&view.gate_focus).on_key_down(cx.listener(
                    |this, ev: &KeyDownEvent, window, cx| {
                        if this.active_modal.is_some() {
                            if ev.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.dismiss_modal_if_open(cx);
                            }
                            return;
                        }
                        if ev.keystroke.key == "escape" {
                            cx.stop_propagation();
                            this.keep_planning(window, cx);
                        }
                    },
                ));
            }
            card.child(
                v_col()
                    .p(px(12.0))
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .justify_between()
                            .gap(px(8.0))
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .child(status_badge(
                                        GraphNodeStatus::Waiting,
                                        Some(WaitKind::Plan),
                                        cx,
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .text_color(to_hsla(p.ink))
                                            .child(ch.gates_plan_submitted()),
                                    ),
                            )
                            .child(
                                h_row()
                                    .gap(px(5.0))
                                    .children(has_graph.then(|| {
                                        h_row()
                                            .gap(px(4.0))
                                            .child(
                                                choice_chip(
                                                    SharedString::from(format!(
                                                        "plan-tab-g-{}",
                                                        gate.id
                                                    )),
                                                    ch.gates_plan_view_graph(),
                                                    !show_md,
                                                    cx,
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.set_plan_gate_markdown(
                                                        gid_graph.clone(),
                                                        false,
                                                        cx,
                                                    );
                                                })),
                                            )
                                            .child(
                                                choice_chip(
                                                    SharedString::from(format!(
                                                        "plan-tab-m-{}",
                                                        gate.id
                                                    )),
                                                    ch.gates_plan_view_markdown(),
                                                    show_md,
                                                    cx,
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.set_plan_gate_markdown(
                                                        gid_md.clone(),
                                                        true,
                                                        cx,
                                                    );
                                                })),
                                            )
                                    }))
                                    .child(badge(count_badge, Tone::Neutral, cx)),
                            ),
                    )
                    .children(title.as_ref().map(|t| {
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .child(SharedString::from(t.clone()))
                    }))
                    .child(body_el)
                    .children(view.plan_revising.then(|| {
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.accent_ink))
                            .child(ch.gates_revise_waiting())
                    }))
                    .child(
                        h_row()
                            .w_full()
                            .justify_between()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.faint))
                                    .child(ch.gates_plan_keys()),
                            )
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .flex_wrap()
                                    .child(
                                        btn_ghost(
                                            SharedString::from(format!("plan-keep-{}", gate.id)),
                                            ch.gates_keep_planning(),
                                        )
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.keep_planning(window, cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        btn_secondary(
                                            SharedString::from(format!("plan-ask-{}", gate.id)),
                                            ch.gates_approve_ask(),
                                        )
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.approve_plan(
                                                    Some(ToolApproval::Ask),
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    )
                                    .child(
                                        btn_primary(
                                            SharedString::from(format!("plan-go-{}", gate.id)),
                                            ch.gates_approve_execute(),
                                        )
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.approve_plan(
                                                    Some(ToolApproval::AutoReadOnly),
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            )
        }
    }
}
