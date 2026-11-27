//! Heterogeneous Process Graph step cards for `FlowsView`:
//! - `FlowStepKind::Request`: HTTP/AI card with method badge, URL summary, extraction/assertion
//!   pills, live/past status badge, quick run/fork actions, and unpin button
//! - `FlowStepKind::Condition`: multi-exit decision card listing branch labels & matched path
//! - `FlowStepKind::Join`: compact parallel fan-in barrier card

use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::model::items::FlowStepKind;
use spanbay_desktop::state::{FlowGraphNode, ProcessNodeStatus};

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, h_row, icon_xs, meta_pill, method_badge, palette,
    status_indicator, text_tooltip, to_hsla, v_col,
};

use super::state::FlowsView;

pub(super) fn render_flow_graph_card(
    view: &FlowsView,
    node: &FlowGraphNode,
    scale: f32,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let p = palette(cx);
    let fl = crate::ui::Tr(view.lang).flows();
    let step = &node.step;
    let selected = view.selected_step_id.as_deref() == Some(step.id.as_str());
    let (shape, tone) = status_visual(node.overlay.status);
    let border = if selected {
        to_hsla(p.accent)
    } else {
        match tone {
            Tone::Danger => to_hsla(p.bad_line.composite_over(p.surface)),
            Tone::Warning => to_hsla(p.warn_line.composite_over(p.surface)),
            Tone::Success => to_hsla(p.ok_line.composite_over(p.surface)),
            Tone::Accent => to_hsla(p.accent_line.composite_over(p.surface)),
            Tone::Neutral => to_hsla(p.line_strong.composite_over(p.surface)),
        }
    };
    let hover_border = to_hsla(p.accent_line.composite_over(p.surface));
    let bg = if selected {
        to_hsla(p.accent_soft.composite_over(p.surface))
    } else {
        to_hsla(p.surface)
    };

    let sid_drag = step.id.clone();
    let sid_sel = step.id.clone();
    let pos = (node.x, node.y);
    let size = (node.w, node.h);

    let mut card = v_col()
        .id(SharedString::from(format!("flow-gnode-{}", step.id)))
        .absolute()
        .left(px(node.x * scale))
        .top(px(node.y * scale))
        .w(px(node.w * scale))
        .h(px(node.h * scale))
        .px(px(10.0 * scale))
        .py(px(7.0 * scale))
        .gap(px(4.0 * scale))
        .rounded(px(8.0 * scale))
        .bg(bg)
        .border_1()
        .border_color(border)
        .overflow_hidden()
        .cursor_pointer()
        .hover(move |s| {
            if selected {
                s
            } else {
                s.border_color(hover_border)
            }
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.begin_step_drag(sid_drag.clone(), pos, size, scale, ev);
            }),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            if this.drag_suppress_click {
                this.drag_suppress_click = false;
                return;
            }
            this.select_step(Some(sid_sel.clone()), cx);
        }));

    // Row 1: status dot + ordinal + title + optional pin + timing/status pill.
    let status_chip = status_pill(node, view.lang, cx);
    let title_row = h_row()
        .w_full()
        .justify_between()
        .gap(px(4.0))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(5.0))
                .child(status_indicator(shape, cx))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px((10.0 * scale).clamp(8.5, 11.0)))
                        .text_color(to_hsla(p.faint))
                        .child(format!("#{}", node.order_index + 1)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px((12.0 * scale).clamp(10.0, 13.0)))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(SharedString::from(step.name.clone())),
                ),
        )
        .children(node.pinned.then(|| unpin_btn(&step.id, scale, cx)))
        .children(status_chip);
    card = card.child(title_row);

    // Kind-specific body rows.
    match step.step_kind {
        FlowStepKind::Request => {
            let url_lbl = if node.target_url.is_empty() {
                fl.steps_pane_no_url_set().to_owned()
            } else {
                node.target_url.clone()
            };
            card = card.child(
                h_row()
                    .w_full()
                    .gap(px(5.0))
                    .child(method_badge(&node.method, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px((10.5 * scale).clamp(9.0, 11.5)))
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(url_lbl)),
                    ),
            );

            let mut meta = h_row().w_full().justify_between().gap(px(4.0));
            let mut pills = h_row().flex_1().min_w_0().gap(px(4.0)).overflow_hidden();
            if !node.produces_vars.is_empty() {
                let first = &node.produces_vars[0].0;
                let extra = node.produces_vars.len().saturating_sub(1);
                let txt = if extra > 0 {
                    format!("→ {{{{{first}}}}} +{extra}")
                } else {
                    format!("→ {{{{{first}}}}}")
                };
                pills = pills.child(meta_pill(txt, cx));
            }
            if !step.assertions.is_empty() {
                pills = pills.child(meta_pill(fl.steps_pane_assert(step.assertions.len()), cx));
            }
            if !step.condition_expression.trim().is_empty() {
                pills = pills.child(meta_pill("if", cx));
            }
            if step.retries > 0 {
                pills = pills.child(meta_pill(format!("×{}", step.retries), cx));
            }
            meta = meta.child(pills);

            // Compact hover/selected quick actions: run single step, fork parallel sibling.
            let ord = node.order_index;
            let sid_fork = step.id.clone();
            meta = meta.child(
                h_row()
                    .flex_shrink_0()
                    .gap(px(2.0))
                    .child(quick_icon_btn(
                        format!("flow-gc-run-{ord}"),
                        UiIcon::Play,
                        fl.steps_pane_run_only_this_step(),
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.run_flow(None, Some(ord), None, cx);
                        }),
                        cx,
                    ))
                    .child(quick_icon_btn(
                        format!("flow-gc-fork-{ord}"),
                        UiIcon::Branch,
                        fl.graph_add_parallel(),
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.add_parallel_sibling(&sid_fork, cx);
                        }),
                        cx,
                    )),
            );
            card = card.child(meta);
        }
        FlowStepKind::Condition => {
            let mut br_col = v_col().w_full().gap(px(2.0 * scale));
            br_col = br_col.child(
                h_row()
                    .gap(px(4.0))
                    .child(badge(fl.kind_condition(), Tone::Accent, cx))
                    .child(meta_pill(fl.branch_count(step.branches.len()), cx)),
            );
            for br in step.branches.iter().take(3) {
                let matched = node.overlay.matched_branch_id.as_deref() == Some(br.id.as_str());
                let expr_hint = if br.expression.trim().is_empty() {
                    fl.branch_default_else().to_owned()
                } else {
                    br.expression.clone()
                };
                br_col = br_col.child(
                    h_row().w_full().justify_between().gap(px(4.0)).child(
                        div()
                            .text_size(px((10.0 * scale).clamp(8.5, 11.0)))
                            .font_weight(if matched {
                                gpui_kit::FontWeight::SEMIBOLD
                            } else {
                                gpui_kit::FontWeight::NORMAL
                            })
                            .text_color(to_hsla(if matched { p.ok } else { p.ink_2 }))
                            .truncate()
                            .child(SharedString::from(format!("{} · {expr_hint}", br.label))),
                    ),
                );
            }
            card = card.child(br_col);
        }
        FlowStepKind::Join => {
            card = card.child(
                h_row()
                    .gap(px(5.0))
                    .child(badge(fl.kind_join(), Tone::Neutral, cx))
                    .child(
                        div()
                            .text_size(px((10.5 * scale).clamp(9.0, 11.5)))
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(
                                fl.join_deps_count(step.depends_on.len()),
                            )),
                    ),
            );
        }
    }

    if let Some(err) = &node.overlay.error {
        card = card.child(
            div()
                .w_full()
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .bg(to_hsla(p.bad_soft))
                .text_size(px(10.0))
                .text_color(to_hsla(p.bad))
                .truncate()
                .child(SharedString::from(err.clone())),
        );
    }

    card.into_any_element()
}

fn unpin_btn(step_id: &str, scale: f32, cx: &mut Context<FlowsView>) -> AnyElement {
    let p = palette(cx);
    let fl = crate::ui::tr(cx).flows();
    let sid = step_id.to_owned();
    div()
        .id(SharedString::from(format!("flow-gunpin-{step_id}")))
        .flex_shrink_0()
        .p(px(2.0 * scale))
        .rounded(px(4.0))
        .text_color(to_hsla(p.accent_ink))
        .hover(|s| s.bg(to_hsla(p.accent_soft)))
        .cursor_pointer()
        .tooltip(text_tooltip(fl.graph_unpin_step()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.unpin_step(&sid, cx);
        }))
        .child(icon_xs(UiIcon::Pin))
        .into_any_element()
}

fn quick_icon_btn(
    id: String,
    glyph: UiIcon,
    tip: &'static str,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    div()
        .id(SharedString::from(id))
        .p(px(2.5))
        .rounded(px(4.0))
        .text_color(to_hsla(p.muted))
        .hover(|s| s.bg(to_hsla(p.hover)).text_color(to_hsla(p.ink)))
        .cursor_pointer()
        .tooltip(text_tooltip(tip))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(on_click)
        .child(icon_xs(glyph))
        .into_any_element()
}

fn status_visual(status: ProcessNodeStatus) -> (StatusShape, Tone) {
    match status {
        ProcessNodeStatus::Running | ProcessNodeStatus::Queued => (StatusShape::Run, Tone::Accent),
        ProcessNodeStatus::Waiting => (StatusShape::Warn, Tone::Warning),
        ProcessNodeStatus::Completed => (StatusShape::Ok, Tone::Success),
        ProcessNodeStatus::Failed | ProcessNodeStatus::Interrupted => {
            (StatusShape::Bad, Tone::Danger)
        }
        ProcessNodeStatus::Skipped | ProcessNodeStatus::Cancelled => {
            (StatusShape::Warn, Tone::Warning)
        }
        ProcessNodeStatus::Idle | ProcessNodeStatus::Pending => (StatusShape::Idle, Tone::Neutral),
    }
}

fn status_pill(
    node: &FlowGraphNode,
    lang: spanbay_desktop::core::model::settings::Language,
    cx: &App,
) -> Option<AnyElement> {
    let fl = crate::ui::Tr(lang).flows();
    if node.overlay.skipped {
        return Some(badge(fl.steps_pane_skip(), Tone::Warning, cx).into_any_element());
    }
    if let Some(code) = node.overlay.status_code {
        let ms = node.overlay.elapsed_ms.unwrap_or(0);
        let tone = if node.overlay.passed.unwrap_or(true) {
            Tone::Success
        } else {
            Tone::Danger
        };
        return Some(badge(format!("{code} · {ms}ms"), tone, cx).into_any_element());
    }
    if let Some(ms) = node.overlay.elapsed_ms {
        let tone = match node.overlay.passed {
            Some(true) => Tone::Success,
            Some(false) => Tone::Danger,
            None => Tone::Neutral,
        };
        return Some(badge(format!("{ms}ms"), tone, cx).into_any_element());
    }
    None
}
