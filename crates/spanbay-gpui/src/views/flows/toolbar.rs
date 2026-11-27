//! Top-bar history playback switcher (`[‹] [#N ▾] [›]`) and unified ProcessView view/layout
//! toolbar (`Steps | Graph | Timeline`, `LR / TB / Wrap`, `Swimlanes`, `Center/Top`, `DataFlow`,
//! `Reset layout`, and zoom) for `FlowsView`.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div, px,
};
use spanbay_desktop::core::model::items::{Flow, FlowLayoutDirectionPref, FlowStepKind};
use spanbay_desktop::state::{FlowGraphLayout, FlowViewMode};

use crate::ui::{
    Anchor, ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_ghost, btn_icon,
    btn_primary, btn_secondary, choice_chip, empty_inline, h_row, icon_xs, kv_line, list_row,
    meta_pill, modal_dialog, palette, popover, select_pill, status_indicator, to_hsla, v_col,
};

use super::state::{FlowModal, FlowsView};

/// Compact history run switcher (`[‹] [#N · PASS ▾] [›]`) matching Agent's WorkflowMonitorModal.
pub fn render_history_switcher(view: &FlowsView, cx: &mut Context<FlowsView>) -> AnyElement {
    let fl = crate::ui::Tr(view.lang).flows();
    if view.past_runs.is_empty() {
        return div().into_any_element();
    }

    let total = view.past_runs.len();
    let active_idx = view
        .active_history_run_id
        .as_deref()
        .and_then(|id| view.past_runs.iter().position(|r| r.run_id == id));
    let can_prev = active_idx.map_or(total > 1, |i| i + 1 < total);
    let can_next = active_idx.is_some_and(|i| i > 0);

    let trigger_label = if let Some(idx) = active_idx {
        let r = &view.past_runs[idx];
        let num = total - idx;
        let st = if r.cancelled {
            fl.row_cancelled()
        } else if r.passed {
            fl.run_panel_passed()
        } else {
            fl.run_panel_failed()
        };
        format!("#{num} · {st} · {}ms", r.elapsed_ms)
    } else if let Some(last) = &view.last_result {
        let st = if last.cancelled {
            fl.row_cancelled()
        } else if last.passed {
            fl.run_panel_passed()
        } else {
            fl.run_panel_failed()
        };
        format!("#{total} · {st} · {}ms", last.elapsed_ms)
    } else {
        fl.run_panel_past_runs(total)
    };

    let runs_pop = view.past_runs.clone();
    let cur_hid = view.active_history_run_id.clone();
    let ent = cx.entity().clone();
    let lang = view.lang;

    let picker = popover("flow-hist-pop")
        .anchor(Anchor::TopRight)
        .trigger(select_pill(
            "flow-hist-trig",
            SharedString::from(trigger_label),
            cur_hid.is_some(),
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let p = palette(pop_cx);
            let ftr = crate::ui::Tr(lang).flows();
            let mut col = v_col().w(px(300.0)).gap(px(3.0)).p(px(6.0));
            if runs_pop.is_empty() {
                return col.child(empty_inline(
                    ftr.run_panel_no_past_runs_recorded(),
                    None,
                    None,
                    pop_cx,
                ));
            }
            for (idx, run) in runs_pop.iter().enumerate() {
                let rid = run.run_id.clone();
                let num = runs_pop.len() - idx;
                let selected = cur_hid.as_deref() == Some(run.run_id.as_str());
                let ent_i = ent.clone();
                let ph = pop_cx.entity().clone();
                let (st_lbl, tone) = if run.cancelled {
                    (ftr.row_cancelled(), Tone::Warning)
                } else if run.passed {
                    (ftr.run_panel_passed(), Tone::Success)
                } else {
                    (ftr.run_panel_failed(), Tone::Danger)
                };
                let sub = run
                    .failed_step_name
                    .as_deref()
                    .map(|n| ftr.hist_failed_at(n))
                    .unwrap_or_else(|| {
                        format!(
                            "{}/{} · {}ms",
                            run.steps_passed, run.steps_total, run.elapsed_ms
                        )
                    });
                col = col.child(
                    list_row(
                        SharedString::from(format!("flow-hrun-{}", run.run_id)),
                        selected,
                        pop_cx,
                        move |_, w, cx| {
                            ent_i.update(cx, |this, cx| {
                                this.select_history_run(Some(rid.clone()), cx);
                            });
                            ph.update(cx, |s, cx| s.dismiss(w, cx));
                        },
                    )
                    .justify_between()
                    .gap(px(6.0))
                    .child(
                        h_row()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.0))
                            .child(meta_pill(format!("#{num}"), pop_cx))
                            .child(badge(st_lbl, tone, pop_cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(SharedString::from(sub)),
                            ),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(10.5))
                            .text_color(to_hsla(p.faint))
                            .child(format!("{}ms", run.elapsed_ms)),
                    ),
                );
            }
            col
        });

    h_row()
        .flex_shrink_0()
        .gap(px(2.0))
        .children(can_prev.then(|| {
            btn_icon("flow-hist-prev", UiIcon::ArrowLeft)
                .tooltip(fl.hist_prev_run())
                .on_click(cx.listener(|this, _, _, cx| this.step_history(1, cx)))
        }))
        .child(picker)
        .children(can_next.then(|| {
            btn_icon("flow-hist-next", UiIcon::ChevronRight)
                .tooltip(fl.hist_next_run())
                .on_click(cx.listener(|this, _, _, cx| this.step_history(-1, cx)))
        }))
        .into_any_element()
}

/// Unified ProcessView secondary toolbar (`Steps | Graph | Timeline`, authoring add buttons,
/// `LR / TB / Wrap`, `Swimlanes`, `Center/Top`, `DataFlow`, `Reset layout`, and zoom).
pub fn render_view_toolbar(
    view: &FlowsView,
    flow: &Flow,
    layout: &FlowGraphLayout,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let p = palette(cx);
    let fl = crate::ui::Tr(view.lang).flows();
    let ch = crate::ui::Tr(view.lang).chat();
    let eff_policy = layout.geometry.policy;
    let pref = &flow.options.layout;
    let scale = view.graph_zoom.clamp(0.75, 1.15);

    let left_group = h_row()
        .gap(px(5.0))
        .flex_wrap()
        .child(
            choice_chip(
                "flow-view-steps",
                ch.monitor_view_steps(),
                view.view_mode == FlowViewMode::Steps,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| this.set_view_mode(FlowViewMode::Steps, cx))),
        )
        .child(
            choice_chip(
                "flow-view-graph",
                ch.monitor_view_graph(),
                view.view_mode == FlowViewMode::Graph,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| this.set_view_mode(FlowViewMode::Graph, cx))),
        )
        .child(
            choice_chip(
                "flow-view-timeline",
                ch.monitor_view_timeline(),
                view.view_mode == FlowViewMode::Timeline,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| this.set_view_mode(FlowViewMode::Timeline, cx))),
        )
        .child(
            div()
                .w(px(1.0))
                .h(px(16.0))
                .mx(px(3.0))
                .bg(to_hsla(p.line.composite_over(p.pane))),
        )
        .child(
            btn_secondary("flow-add-inline", fl.steps_pane_inline())
                .on_click(cx.listener(|this, _, _, cx| this.add_inline_step(cx))),
        )
        .child(
            btn_ghost("flow-add-saved", fl.steps_pane_saved()).on_click(cx.listener(
                |this, _, _, cx| {
                    this.active_modal = Some(FlowModal::AddSavedStep {
                        insert_between: None,
                    });
                    cx.notify();
                },
            )),
        )
        .child(
            btn_ghost("flow-add-cond", fl.toolbar_add_branch())
                .icon(icon_xs(UiIcon::Diamond))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.add_control_step(FlowStepKind::Condition, cx);
                })),
        )
        .child(
            btn_ghost("flow-add-join", fl.toolbar_add_join())
                .icon(icon_xs(UiIcon::Branch))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.add_control_step(FlowStepKind::Join, cx);
                })),
        );

    let graph_tools = (view.view_mode == FlowViewMode::Graph).then(|| {
        let is_lr = matches!(
            eff_policy.direction,
            spanbay_desktop::state::LayoutDirection::LeftToRight
        );
        let is_tb = eff_policy.direction.is_vertical();
        let is_wrap = eff_policy.direction.is_serpentine();
        let align_lbl = if pref.center_align {
            ch.graph_layout_align_center()
        } else {
            ch.graph_layout_align_start()
        };

        h_row()
            .flex_shrink_0()
            .gap(px(4.0))
            .flex_wrap()
            .child(
                choice_chip("flow-dir-lr", ch.graph_layout_lr(), is_lr, cx).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.set_layout_direction(FlowLayoutDirectionPref::LeftToRight, cx);
                    },
                )),
            )
            .child(
                choice_chip("flow-dir-tb", ch.graph_layout_tb(), is_tb, cx).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.set_layout_direction(FlowLayoutDirectionPref::TopToBottom, cx);
                    },
                )),
            )
            .child(
                choice_chip("flow-dir-wrap", ch.graph_layout_wrap(), is_wrap, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.set_layout_direction(FlowLayoutDirectionPref::Serpentine, cx);
                    }),
                ),
            )
            .child(
                choice_chip(
                    "flow-lanes-tog",
                    ch.graph_layout_swimlanes(),
                    pref.swimlanes,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_swimlanes(cx))),
            )
            .child(
                choice_chip("flow-align-tog", align_lbl, pref.center_align, cx)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_center_align(cx))),
            )
            .child(
                choice_chip(
                    "flow-df-tog",
                    fl.toolbar_data_flow(),
                    pref.show_data_flow,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_data_flow(cx))),
            )
            .children(layout.has_pinned.then(|| {
                btn_ghost("flow-layout-reset", ch.graph_layout_reset())
                    .icon(icon_xs(UiIcon::RotateCcw))
                    .on_click(cx.listener(|this, _, _, cx| this.reset_pinned_layout(cx)))
            }))
            .child(zoom_pill("flow-zoom-75", "75%", 0.75, scale, cx))
            .child(zoom_pill("flow-zoom-100", "100%", 1.0, scale, cx))
            .child(zoom_pill("flow-zoom-115", "115%", 1.15, scale, cx))
    });

    h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(10.0))
        .px(px(14.0))
        .py(px(6.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(left_group)
        .children(graph_tools)
        .into_any_element()
}

fn zoom_pill(
    id: &'static str,
    label: &'static str,
    target: f32,
    current: f32,
    cx: &mut Context<FlowsView>,
) -> impl IntoElement {
    choice_chip(id, label, (current - target).abs() < 0.02, cx)
        .on_click(cx.listener(move |this, _, _, cx| this.set_graph_zoom(target, cx)))
}

/// Compact top-bar Variables trigger pill (replaces old bottom run_panel Variables tab).
pub fn render_variables_btn(view: &FlowsView, cx: &mut Context<FlowsView>) -> AnyElement {
    let fl = crate::ui::Tr(view.lang).flows();
    let count = view.active_variables_count();
    let shape = if count > 0 {
        StatusShape::Ok
    } else {
        StatusShape::Idle
    };
    btn_ghost("flow-vars-btn", fl.run_panel_variables(count))
        .child(status_indicator(shape, cx))
        .on_click(cx.listener(|this, _, _, cx| {
            this.active_modal = Some(FlowModal::Variables);
            cx.notify();
        }))
        .into_any_element()
}

pub fn render_variables_modal(
    view: &FlowsView,
    window: &Window,
    cx: &mut Context<FlowsView>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let fl = crate::ui::Tr(view.lang).flows();
    let (vars, secrets): (Vec<(String, String)>, Vec<String>) =
        if let Some(sum) = view.active_past_summary() {
            let v = sum
                .variables
                .as_ref()
                .map(|list| {
                    list.iter()
                        .map(|kv| (kv.key.clone(), kv.value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            (v, sum.withheld_variables.clone())
        } else if let Some(res) = &view.last_result {
            let v = res
                .variables
                .iter()
                .map(|kv| (kv.key.clone(), kv.value.clone()))
                .collect();
            (v, res.secret_variables.clone())
        } else {
            (Vec::new(), Vec::new())
        };

    let body: AnyElement = if vars.is_empty() && secrets.is_empty() {
        empty_inline(
            fl.run_panel_no_variables_captured_in_this(),
            Some(SharedString::from(fl.run_panel_run_the_flow_to_inspect())),
            None,
            cx,
        )
        .into_any_element()
    } else {
        let mut col = v_col().w_full().gap(px(4.0));
        for (k, v) in vars {
            let masked = secrets.iter().any(|s| s == &k);
            col = col.child(kv_line(
                k,
                if masked {
                    "•••••••• (secret)".to_owned()
                } else {
                    v
                },
                180.0,
                cx,
            ));
        }
        div()
            .id("flow-vars-scroll")
            .w_full()
            .max_h(px(340.0))
            .overflow_y_scrollbar()
            .child(col)
            .into_any_element()
    };

    let footer = btn_primary("flow-vars-done", fl.modals_done())
        .on_click(cx.listener(|this, _, _, cx| {
            this.active_modal = None;
            cx.notify();
        }))
        .into_any_element();
    modal_dialog(
        "flow-vars-modal",
        fl.run_panel_variables(view.active_variables_count()),
        500.0,
        body,
        footer,
        window,
        cx,
        on_close,
    )
}
