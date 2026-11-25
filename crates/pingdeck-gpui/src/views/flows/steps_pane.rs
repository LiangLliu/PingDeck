//! Ordered step pipeline list for `FlowsView` with reorder, duplicate, partial run, and live step statuses.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_secondary,
    empty_inline, h_row, list_row, meta_pill, method_badge, palette, status_indicator, to_hsla,
    v_col,
};

use super::state::{FlowModal, FlowsView};

pub fn render_steps_pane(view: &FlowsView, cx: &mut Context<FlowsView>) -> AnyElement {
    let p = palette(cx);
    let Some(flow) = &view.draft else {
        return div().into_any_element();
    };

    let mut list = v_col().w_full().p(px(8.0)).gap(px(5.0));
    if flow.steps.is_empty() {
        list = list.child(empty_inline(
            crate::ui::Tr(view.lang)
                .flows()
                .steps_pane_no_steps_in_this_flow(),
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .flows()
                    .steps_pane_add_an_inline_http_step(),
            )),
            None,
            cx,
        ));
    } else {
        let total = flow.steps.len();
        for (idx, step) in flow.steps.iter().enumerate() {
            let sid = step.id.clone();
            let sid_dup = step.id.clone();
            let sid_del = step.id.clone();
            let active = view.selected_step_id.as_deref() == Some(step.id.as_str());

            let linked_req = view.request_of(&step.request_id);
            let method_str = step
                .method_override
                .or_else(|| linked_req.map(|r| r.method))
                .map(|m| format!("{m:?}").to_uppercase())
                .unwrap_or_else(|| "GET".to_owned());
            let target_label = step
                .url_override
                .as_deref()
                .filter(|u| !u.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| linked_req.map(|r| r.url.clone()))
                .unwrap_or_else(|| {
                    crate::ui::Tr(view.lang)
                        .flows()
                        .steps_pane_no_url_set()
                        .to_owned()
                });

            // Live or last run status for this step
            let live = view.live_steps.get(&step.id);
            let res = view.step_result_of(&step.id);
            let (shape, status_pill) = if let Some(s) = live {
                let sh = if s.skipped {
                    StatusShape::Warn
                } else if s.passed {
                    StatusShape::Ok
                } else {
                    StatusShape::Bad
                };
                let pill = if s.skipped {
                    badge(
                        crate::ui::Tr(view.lang).flows().steps_pane_skip(),
                        Tone::Warning,
                        cx,
                    )
                } else if let Some(code) = s.status_code {
                    badge(
                        format!("{code} · {}ms", s.elapsed_ms),
                        if s.passed {
                            Tone::Success
                        } else {
                            Tone::Danger
                        },
                        cx,
                    )
                } else {
                    badge(
                        crate::ui::Tr(view.lang).flows().steps_pane_err(),
                        Tone::Danger,
                        cx,
                    )
                };
                (sh, Some(pill))
            } else if let Some(r) = res {
                let sh = if r.skipped {
                    StatusShape::Warn
                } else if r.passed {
                    StatusShape::Ok
                } else {
                    StatusShape::Bad
                };
                let pill = if r.skipped {
                    badge(
                        crate::ui::Tr(view.lang).flows().steps_pane_skip(),
                        Tone::Warning,
                        cx,
                    )
                } else if let Some(code) = r.status_code {
                    badge(
                        format!("{code} · {}ms", r.elapsed_ms),
                        if r.passed {
                            Tone::Success
                        } else {
                            Tone::Danger
                        },
                        cx,
                    )
                } else {
                    badge(
                        crate::ui::Tr(view.lang).flows().steps_pane_err(),
                        Tone::Danger,
                        cx,
                    )
                };
                (sh, Some(pill))
            } else if view.running {
                (StatusShape::Run, None)
            } else {
                (StatusShape::Idle, None)
            };

            let mut meta_row = h_row()
                .gap(px(4.0))
                .flex_wrap()
                .child(method_badge(&method_str, cx))
                .children(status_pill)
                .children((!step.extractions.is_empty()).then(|| {
                    let names: Vec<String> = step
                        .extractions
                        .iter()
                        .take(2)
                        .map(|e| format!("→ {{{{{}}}}}", e.variable_name))
                        .collect();
                    let suffix = if step.extractions.len() > 2 {
                        format!(" +{}", step.extractions.len() - 2)
                    } else {
                        String::new()
                    };
                    meta_pill(format!("{}{suffix}", names.join(" ")), cx)
                }))
                .children((!step.assertions.is_empty()).then(|| {
                    meta_pill(
                        crate::ui::tr(cx)
                            .flows()
                            .steps_pane_assert(step.assertions.len()),
                        cx,
                    )
                }))
                .children(
                    (!step.condition_expression.trim().is_empty()).then(|| meta_pill("if", cx)),
                );
            let _ = &mut meta_row;

            let mut actions = h_row().gap(px(2.0)).flex_shrink_0();
            if idx > 0 {
                actions = actions.child(
                    btn_icon(
                        SharedString::from(format!("flow-up-{idx}")),
                        UiIcon::ChevronUp,
                    )
                    .tooltip(crate::ui::Tr(view.lang).flows().steps_pane_move_up())
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
                    .tooltip(crate::ui::Tr(view.lang).flows().steps_pane_move_down())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.move_step(idx, idx + 1, cx);
                    })),
                );
            }
            actions = actions
                .child(
                    btn_icon(SharedString::from(format!("flow-run1-{idx}")), UiIcon::Play)
                        .tooltip(
                            crate::ui::Tr(view.lang)
                                .flows()
                                .steps_pane_run_only_this_step(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.run_flow(None, Some(idx), None, cx);
                        })),
                )
                .child(
                    btn_icon(SharedString::from(format!("flow-upto-{idx}")), UiIcon::Zap)
                        .tooltip(
                            crate::ui::Tr(view.lang)
                                .flows()
                                .steps_pane_run_up_to_this_step(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.run_flow(Some(idx + 1), None, None, cx);
                        })),
                )
                .child(
                    btn_icon(SharedString::from(format!("flow-sdup-{idx}")), UiIcon::Copy)
                        .tooltip(crate::ui::Tr(view.lang).flows().insp_duplicate())
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
                    .tooltip(crate::ui::Tr(view.lang).flows().insp_delete())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.remove_step(sid_del.clone(), cx);
                    })),
                );

            list = list.child(
                list_row(
                    SharedString::from(format!("flow-step-{}", step.id)),
                    active,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.snapshot_inputs(cx);
                        this.selected_step_id = Some(sid.clone());
                        this.pending_sync = true;
                        cx.notify();
                    }),
                )
                .items_start()
                .py(px(8.0))
                .child(
                    h_row()
                        .gap(px(5.0))
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
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .truncate()
                                        .child(step.name.clone()),
                                )
                                .child(actions),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(target_label),
                        )
                        .child(meta_row),
                ),
            );
        }
    }

    v_col()
        .w(px(330.0))
        .flex_shrink_0()
        .h_full()
        .bg(to_hsla(p.pane))
        .border_r_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(crate::ui::tr(cx).flows().steps_pane_steps(flow.steps.len())),
                )
                .child(
                    h_row()
                        .gap(px(4.0))
                        .child(
                            btn_secondary(
                                "flow-add-inline",
                                crate::ui::Tr(view.lang).flows().steps_pane_inline(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.add_inline_step(cx))),
                        )
                        .child(
                            btn_ghost(
                                "flow-add-saved",
                                crate::ui::Tr(view.lang).flows().steps_pane_saved(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_modal = Some(FlowModal::AddSavedStep);
                                cx.notify();
                            })),
                        ),
                ),
        )
        .child(
            div()
                .id("flow-steps-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(list),
        )
        .into_any_element()
}
