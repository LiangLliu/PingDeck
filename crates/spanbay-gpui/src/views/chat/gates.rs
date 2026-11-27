//! Floating top-accented Approval, AskUser Question, Plan Approval, Cost Pause, and Queued
//! Turns cards docked above the Chat composer.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    div, px,
};
use spanbay_desktop::core::agent::model::{
    ApprovalDecision, ApprovalScope, HumanGateBody, HumanGateDto,
};
use spanbay_desktop::core::model::settings::ToolApproval;
use spanbay_desktop::state::LiveApproval;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, badge, btn_danger, btn_ghost, btn_primary,
    btn_secondary, choice_chip, h_row, markdown_view, palette, status_indicator, text_input,
    to_hsla, top_accent_card, v_col,
};

use crate::views::runs::graph::render_plan_preview_graph;

use super::ChatView;
use super::types::ChatModal;

pub fn render_approval_gate(appr: &LiveApproval, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let tone = if appr.destructive {
        Tone::Danger
    } else {
        Tone::Warning
    };
    let run_once = appr.run_id.clone();
    let call_once = appr.call_id.clone();
    let run_net = appr.run_id.clone();
    let call_net = appr.call_id.clone();
    let run_sess = appr.run_id.clone();
    let call_sess = appr.call_id.clone();
    let run_deny = appr.run_id.clone();
    let call_deny = appr.call_id.clone();
    let appr_modal = appr.clone();

    let can_session =
        appr.offered_scopes.is_empty() || appr.offered_scopes.contains(&ApprovalScope::Session);
    let has_more_scopes = appr.offered_scopes.len() > 2
        || appr
            .offered_scopes
            .contains(&ApprovalScope::WorkspacePattern)
        || appr.offered_scopes.contains(&ApprovalScope::Tool);

    let preview = appr
        .full_text
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(appr.arguments.as_str());
    let reason_line = appr
        .why
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(appr.reason.as_str());

    top_accent_card(tone, cx).child(
        v_col()
            .p(px(12.0))
            .gap(px(8.0))
            .child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .gap(px(7.0))
                            .child(status_indicator(
                                if appr.destructive {
                                    StatusShape::Bad
                                } else {
                                    StatusShape::Warn
                                },
                                cx,
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(appr.summary.clone())),
                            ),
                    )
                    .child(
                        h_row()
                            .gap(px(4.0))
                            .child(badge(appr.name.clone(), Tone::Neutral, cx))
                            .child(badge(appr.risk.clone(), tone, cx))
                            .children(
                                appr.sandbox_label
                                    .as_ref()
                                    .map(|sb| badge(sb.clone(), Tone::Neutral, cx)),
                            ),
                    ),
            )
            .children((!preview.trim().is_empty()).then(|| {
                div()
                    .px(px(9.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.sunken))
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(preview.trim().to_owned()))
            }))
            .children(appr.scope_disabled_reason.as_ref().map(|msg| {
                div()
                    .text_xs()
                    .text_color(to_hsla(p.warn))
                    .child(SharedString::from(msg.clone()))
            }))
            .child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .flex_wrap()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(120.0))
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(reason_line.to_owned())),
                    )
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .flex_wrap()
                            .child(
                                btn_danger(
                                    SharedString::from(format!("appr-deny-{call_deny}")),
                                    crate::ui::tr(cx).chat().approval_deny(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.answer_approval_ext(
                                            run_deny.clone(),
                                            call_deny.clone(),
                                            ApprovalDecision::Deny,
                                            ApprovalScope::Once,
                                            false,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .children(can_session.then(|| {
                                btn_secondary(
                                    SharedString::from(format!("appr-sess-{call_sess}")),
                                    crate::ui::tr(cx).chat().gates_allow_in_chat(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.answer_approval_ext(
                                            run_sess.clone(),
                                            call_sess.clone(),
                                            ApprovalDecision::Allow,
                                            ApprovalScope::Session,
                                            false,
                                            cx,
                                        );
                                    },
                                ))
                            }))
                            .children(has_more_scopes.then(|| {
                                let cid_m = appr_modal.call_id.clone();
                                btn_ghost(
                                    SharedString::from(format!("appr-scope-{cid_m}")),
                                    crate::ui::tr(cx).chat().gates_scope(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.active_modal = Some(ChatModal::ScopeApproval {
                                            appr: Box::new(appr_modal.clone()),
                                        });
                                        cx.notify();
                                    },
                                ))
                            }))
                            .children(appr.network.then(|| {
                                btn_secondary(
                                    SharedString::from(format!("appr-net-{call_net}")),
                                    crate::ui::tr(cx).chat().gates_allow_net(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.answer_approval_ext(
                                            run_net.clone(),
                                            call_net.clone(),
                                            ApprovalDecision::Allow,
                                            ApprovalScope::Once,
                                            true,
                                            cx,
                                        );
                                    },
                                ))
                            }))
                            .child(
                                btn_primary(
                                    SharedString::from(format!("appr-once-{call_once}")),
                                    crate::ui::tr(cx).chat().approval_allow_once(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.answer_approval_ext(
                                            run_once.clone(),
                                            call_once.clone(),
                                            ApprovalDecision::Allow,
                                            ApprovalScope::Once,
                                            false,
                                            cx,
                                        );
                                    },
                                )),
                            ),
                    ),
            ),
    )
}

pub fn render_human_gate(
    view: &ChatView,
    run_id: &str,
    gate: &HumanGateDto,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
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
                                    let entry =
                                        this.question_draft_choices.entry(cid.clone()).or_default();
                                    if is_multi {
                                        if let Some(pos) = entry.iter().position(|&v| v == idx) {
                                            entry.remove(pos);
                                        } else {
                                            entry.push(idx);
                                        }
                                    } else {
                                        entry.clear();
                                        entry.push(idx);
                                    }
                                    cx.notify();
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

            top_accent_card(Tone::Accent, cx).child(
                v_col()
                    .p(px(12.0))
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(badge(
                                crate::ui::tr(cx).chat().gates_agent_question(),
                                Tone::Accent,
                                cx,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(question.clone())),
                            ),
                    )
                    .child(opts_row)
                    .child(text_input(&view.question_other_input))
                    .child(
                        h_row()
                            .justify_end()
                            .gap(px(6.0))
                            .child(
                                btn_ghost(
                                    SharedString::from(format!("q-skip-{}", gate.call_id)),
                                    crate::ui::tr(cx).chat().gate_question_skip(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.answer_question(
                                            r_skip.clone(),
                                            c_skip.clone(),
                                            true,
                                            window,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .child(
                                btn_primary(
                                    SharedString::from(format!("q-submit-{}", gate.call_id)),
                                    crate::ui::tr(cx).chat().gates_submit_answer(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.answer_question(
                                            r_sub.clone(),
                                            c_sub.clone(),
                                            false,
                                            window,
                                            cx,
                                        );
                                    },
                                )),
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
            let hdr_title = title.clone().unwrap_or_else(|| {
                crate::ui::tr(cx)
                    .chat()
                    .gates_plan_ready_for_approval()
                    .to_owned()
            });
            let ch = crate::ui::tr(cx).chat();
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

            top_accent_card(Tone::Accent, cx).child(
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
                                    .child(badge(ch.composer_plan(), Tone::Accent, cx))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .text_color(to_hsla(p.ink))
                                            .child(hdr_title),
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
                    .child(body_el)
                    .child(
                        h_row()
                            .justify_end()
                            .gap(px(6.0))
                            .flex_wrap()
                            .child(
                                btn_ghost(
                                    SharedString::from(format!("plan-keep-{}", gate.id)),
                                    crate::ui::tr(cx).chat().gates_keep_planning(),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.status_msg = Some((
                                            Tone::Accent,
                                            crate::ui::tr(cx)
                                                .chat()
                                                .gates_keep_planning_hint()
                                                .into(),
                                        ));
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                btn_secondary(
                                    SharedString::from(format!("plan-ask-{}", gate.id)),
                                    crate::ui::tr(cx).chat().gates_approve_ask(),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.approve_plan(Some(ToolApproval::Ask), cx);
                                    },
                                )),
                            )
                            .child(
                                btn_primary(
                                    SharedString::from(format!("plan-go-{}", gate.id)),
                                    crate::ui::tr(cx).chat().gates_approve_execute(),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.approve_plan(Some(ToolApproval::AutoReadOnly), cx);
                                    },
                                )),
                            ),
                    ),
            )
        }
    }
}
