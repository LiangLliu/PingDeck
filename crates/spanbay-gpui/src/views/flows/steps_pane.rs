//! High-density stage-grouped Steps view for `FlowsView`, sharing selection & overlays with
//! the unified Process Graph and Timeline views.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use spanbay_desktop::core::model::items::FlowStepKind;
use spanbay_desktop::state::{FlowGraphLayout, ProcessNodeStatus};

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_icon, empty_inline, h_row,
    list_row, meta_pill, method_badge, palette, section_heading, status_indicator, to_hsla, v_col,
};

use super::state::FlowsView;

pub fn render_steps_view(
    view: &FlowsView,
    layout: &FlowGraphLayout,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let p = palette(cx);
    let fl = crate::ui::Tr(view.lang).flows();
    if layout.nodes.is_empty() {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(empty_inline(
                fl.steps_pane_no_steps_in_this_flow(),
                Some(SharedString::from(fl.steps_pane_add_an_inline_http_step())),
                None,
                cx,
            ))
            .into_any_element();
    }

    let total = layout.nodes.len();
    let max_stage = layout.nodes.iter().map(|n| n.stage).max().unwrap_or(0);
    let mut body = v_col().w_full().gap(px(10.0)).px(px(18.0)).py(px(14.0));

    for stage_idx in 0..=max_stage {
        let stage_nodes: Vec<_> = layout
            .nodes
            .iter()
            .filter(|n| n.stage == stage_idx)
            .collect();
        if stage_nodes.is_empty() {
            continue;
        }
        let mut group = v_col().w_full().gap(px(5.0));
        if layout.has_dag || max_stage > 0 {
            group = group.child(
                h_row()
                    .gap(px(8.0))
                    .child(section_heading(fl.graph_stage_label(stage_idx + 1), cx))
                    .children((stage_nodes.len() >= 2).then(|| {
                        badge(fl.steps_parallel_badge(stage_nodes.len()), Tone::Accent, cx)
                    })),
            );
        }

        for node in stage_nodes {
            let step = &node.step;
            let idx = node.order_index;
            let sid = step.id.clone();
            let sid_dup = step.id.clone();
            let sid_del = step.id.clone();
            let active = view.selected_step_id.as_deref() == Some(step.id.as_str());

            let shape = match node.overlay.status {
                ProcessNodeStatus::Completed => StatusShape::Ok,
                ProcessNodeStatus::Failed | ProcessNodeStatus::Interrupted => StatusShape::Bad,
                ProcessNodeStatus::Running | ProcessNodeStatus::Queued => StatusShape::Run,
                ProcessNodeStatus::Skipped | ProcessNodeStatus::Cancelled => StatusShape::Warn,
                _ => StatusShape::Idle,
            };

            let status_pill: Option<AnyElement> = if node.overlay.skipped {
                Some(badge(fl.steps_pane_skip(), Tone::Warning, cx).into_any_element())
            } else if let Some(code) = node.overlay.status_code {
                let ms = node.overlay.elapsed_ms.unwrap_or(0);
                Some(
                    badge(
                        format!("{code} · {ms}ms"),
                        if node.overlay.passed.unwrap_or(true) {
                            Tone::Success
                        } else {
                            Tone::Danger
                        },
                        cx,
                    )
                    .into_any_element(),
                )
            } else if node.overlay.error.is_some() {
                Some(badge(fl.steps_pane_err(), Tone::Danger, cx).into_any_element())
            } else {
                None
            };

            let kind_badge: AnyElement = match step.step_kind {
                FlowStepKind::Request => method_badge(&node.method, cx).into_any_element(),
                FlowStepKind::Condition => {
                    badge(fl.kind_condition(), Tone::Accent, cx).into_any_element()
                }
                FlowStepKind::Join => badge(fl.kind_join(), Tone::Neutral, cx).into_any_element(),
            };

            let mut meta_row = h_row()
                .gap(px(5.0))
                .flex_wrap()
                .child(kind_badge)
                .children(status_pill)
                .children((!step.extractions.is_empty()).then(|| {
                    let names: Vec<String> = step
                        .extractions
                        .iter()
                        .take(3)
                        .map(|e| format!("→ {{{{{}}}}}", e.variable_name))
                        .collect();
                    let suffix = if step.extractions.len() > 3 {
                        format!(" +{}", step.extractions.len() - 3)
                    } else {
                        String::new()
                    };
                    meta_pill(format!("{}{suffix}", names.join(" ")), cx)
                }))
                .children(
                    (!step.assertions.is_empty())
                        .then(|| meta_pill(fl.steps_pane_assert(step.assertions.len()), cx)),
                )
                .children(
                    (!step.condition_expression.trim().is_empty()).then(|| meta_pill("if", cx)),
                );
            if !step.depends_on.is_empty() {
                meta_row = meta_row.child(meta_pill(fl.join_deps_count(step.depends_on.len()), cx));
            }

            let mut actions = h_row().gap(px(2.0)).flex_shrink_0();
            if idx > 0 {
                actions = actions.child(
                    btn_icon(
                        SharedString::from(format!("flow-up-{idx}")),
                        UiIcon::ChevronUp,
                    )
                    .tooltip(fl.steps_pane_move_up())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.move_step(idx, idx - 1, cx);
                    })),
                );
            }
            if idx + 1 < total {
                actions = actions.child(
                    btn_icon(
                        SharedString::from(format!("flow-dn-{idx}")),
                        UiIcon::ChevronDown,
                    )
                    .tooltip(fl.steps_pane_move_down())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.move_step(idx, idx + 1, cx);
                    })),
                );
            }
            actions = actions
                .child(
                    btn_icon(SharedString::from(format!("flow-run1-{idx}")), UiIcon::Play)
                        .tooltip(fl.steps_pane_run_only_this_step())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.run_flow(None, Some(idx), None, cx);
                        })),
                )
                .child(
                    btn_icon(SharedString::from(format!("flow-upto-{idx}")), UiIcon::Zap)
                        .tooltip(fl.steps_pane_run_up_to_this_step())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.run_flow(Some(idx + 1), None, None, cx);
                        })),
                )
                .child(
                    btn_icon(SharedString::from(format!("flow-sdup-{idx}")), UiIcon::Copy)
                        .tooltip(fl.insp_duplicate())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.duplicate_step(sid_dup.clone(), cx);
                        })),
                )
                .child(
                    btn_icon(
                        SharedString::from(format!("flow-sdel-{idx}")),
                        UiIcon::Trash,
                    )
                    .tooltip(fl.insp_delete())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.remove_step(sid_del.clone(), cx);
                    })),
                );

            let url_sub = match step.step_kind {
                FlowStepKind::Request => {
                    if node.target_url.is_empty() {
                        fl.steps_pane_no_url_set().to_owned()
                    } else {
                        node.target_url.clone()
                    }
                }
                FlowStepKind::Condition => fl.branch_count(step.branches.len()),
                FlowStepKind::Join => fl.join_deps_count(step.depends_on.len()),
            };

            group = group.child(
                list_row(
                    SharedString::from(format!("flow-step-{}", step.id)),
                    active,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.select_step(Some(sid.clone()), cx);
                    }),
                )
                .items_start()
                .px(px(12.0))
                .py(px(9.0))
                .bg(to_hsla(if active { p.accent_soft } else { p.surface }))
                .border_1()
                .border_color(to_hsla(if active {
                    p.accent
                } else {
                    p.line.composite_over(p.surface)
                }))
                .rounded(px(8.0))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .pt(px(2.0))
                        .child(status_indicator(shape, cx))
                        .child(meta_pill(format!("#{}", idx + 1), cx)),
                )
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(3.0))
                        .child(
                            h_row()
                                .w_full()
                                .justify_between()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .truncate()
                                        .child(SharedString::from(step.name.clone())),
                                )
                                .child(actions),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(SharedString::from(url_sub)),
                        )
                        .child(meta_row),
                ),
            );
        }

        body = body.child(group);
    }

    div()
        .id("flow-steps-scroll")
        .size_full()
        .bg(to_hsla(p.sunken))
        .overflow_y_scrollbar()
        .child(body)
        .into_any_element()
}
