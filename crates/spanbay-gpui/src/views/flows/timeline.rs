//! Millisecond waterfall Timeline (`时间轴`) view for `FlowsView`.
//!
//! Displays stage-grouped execution timing bars using the shared Layer 3
//! [`process_segmented_waterfall_bar`] primitive so HTTP requests render crisp `DNS / TLS / TTFB /
//! Body` segmented bars and parallel DAG waves show true concurrent overlap.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::model::items::FlowStepKind;
use spanbay_desktop::state::{FlowGraphLayout, ProcessEdgeTone, ProcessNodeStatus};

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, badge, empty_inline, h_row, meta_pill, method_badge,
    palette, process_segmented_waterfall_bar, section_heading, status_indicator, to_hsla, v_col,
};

use super::state::{FlowStepTab, FlowsView};

pub fn render_flow_timeline(
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
                None,
                None,
                cx,
            ))
            .into_any_element();
    }

    // Compute per-node `(start_ms, duration_ms)` using explicit `started_offset_ms` when present,
    // falling back to cumulative sequential offsets so older runs still plot cleanly.
    let mut spans: Vec<(u64, u64)> = Vec::with_capacity(layout.nodes.len());
    let mut cursor_ms = 0u64;
    let mut total_ms = 1u64;
    for node in &layout.nodes {
        let dur = node.overlay.elapsed_ms.unwrap_or(0);
        let start = node.overlay.started_offset_ms.unwrap_or(cursor_ms);
        cursor_ms = start.saturating_add(dur);
        total_ms = total_ms.max(cursor_ms);
        spans.push((start, dur));
    }
    let has_any_timing = layout.nodes.iter().any(|n| n.overlay.elapsed_ms.is_some());
    let track_w = 240.0f32;

    let mut body = v_col()
        .w_full()
        .gap(px(8.0))
        .px(px(18.0))
        .py(px(14.0))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(12.0))
                .child(
                    h_row()
                        .gap(px(8.0))
                        .child(section_heading(fl.timeline_heading(), cx))
                        .child(meta_pill("DNS · TLS · TTFB · Body", cx)),
                )
                .child(
                    h_row()
                        .w(px(track_w + 68.0))
                        .justify_between()
                        .text_size(px(10.5))
                        .text_color(to_hsla(p.faint))
                        .child("0 ms")
                        .child(format!("{} ms", total_ms / 2))
                        .child(format!("{total_ms} ms")),
                ),
        );

    if !has_any_timing {
        body = body.child(empty_inline(
            fl.run_panel_no_flow_run_yet(),
            Some(SharedString::from(fl.run_panel_click_run_flow_above_to())),
            None,
            cx,
        ));
    }

    for (idx, node) in layout.nodes.iter().enumerate() {
        let (start_ms, dur_ms) = spans[idx];
        let start_ratio = if total_ms > 0 {
            start_ms as f32 / total_ms as f32
        } else {
            0.0
        };
        let width_ratio = if total_ms > 0 {
            dur_ms as f32 / total_ms as f32
        } else {
            0.0
        };
        let selected = view.selected_step_id.as_deref() == Some(node.step.id.as_str());
        let (shape, fallback_tone) = match node.overlay.status {
            ProcessNodeStatus::Completed => (StatusShape::Ok, ProcessEdgeTone::Completed),
            ProcessNodeStatus::Failed | ProcessNodeStatus::Interrupted => {
                (StatusShape::Bad, ProcessEdgeTone::Danger)
            }
            ProcessNodeStatus::Running | ProcessNodeStatus::Queued => {
                (StatusShape::Run, ProcessEdgeTone::Active)
            }
            ProcessNodeStatus::Skipped | ProcessNodeStatus::Cancelled => {
                (StatusShape::Warn, ProcessEdgeTone::Warning)
            }
            _ => (StatusShape::Idle, ProcessEdgeTone::Neutral),
        };
        let sid = node.step.id.clone();

        let kind_badge: AnyElement = match node.step.step_kind {
            FlowStepKind::Request => method_badge(&node.method, cx).into_any_element(),
            FlowStepKind::Condition => {
                badge(fl.kind_condition(), Tone::Accent, cx).into_any_element()
            }
            FlowStepKind::Join => badge(fl.kind_join(), Tone::Neutral, cx).into_any_element(),
        };

        let status_tag: Option<AnyElement> = if node.overlay.skipped {
            Some(badge(fl.steps_pane_skip(), Tone::Warning, cx).into_any_element())
        } else {
            node.overlay.status_code.map(|code| {
                badge(
                    code.to_string(),
                    if node.overlay.passed.unwrap_or(true) {
                        Tone::Success
                    } else {
                        Tone::Danger
                    },
                    cx,
                )
                .into_any_element()
            })
        };

        body = body.child(
            h_row()
                .id(SharedString::from(format!("flow-tl-row-{}", node.step.id)))
                .w_full()
                .justify_between()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .bg(to_hsla(if selected { p.accent_soft } else { p.surface }))
                .border_1()
                .border_color(to_hsla(if selected {
                    p.accent
                } else {
                    p.line.composite_over(p.surface)
                }))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_step_id = Some(sid.clone());
                    this.inspector_open = true;
                    this.step_tab = FlowStepTab::Output;
                    this.pending_sync = true;
                    cx.notify();
                }))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.0))
                        .child(status_indicator(shape, cx))
                        .child(meta_pill(format!("S{}", node.stage + 1), cx))
                        .child(kind_badge)
                        .child(
                            v_col()
                                .flex_1()
                                .min_w_0()
                                .gap(px(1.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .truncate()
                                        .child(SharedString::from(node.step.name.clone())),
                                )
                                .children(node.overlay.error.as_ref().map(|e| {
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(to_hsla(p.bad))
                                        .truncate()
                                        .child(SharedString::from(e.clone()))
                                })),
                        )
                        .children(status_tag),
                )
                .child(
                    h_row()
                        .flex_shrink_0()
                        .gap(px(10.0))
                        .child(process_segmented_waterfall_bar(
                            start_ratio,
                            width_ratio,
                            track_w,
                            &node.overlay.phases,
                            fallback_tone,
                            cx,
                        ))
                        .child(
                            div()
                                .w(px(58.0))
                                .text_right()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(if node.overlay.elapsed_ms.is_some() {
                                    format!("{dur_ms} ms")
                                } else {
                                    "—".to_owned()
                                }),
                        ),
                ),
        );
    }

    div()
        .id("flow-timeline-scroll")
        .size_full()
        .bg(to_hsla(p.sunken))
        .overflow_y_scrollbar()
        .child(body)
        .into_any_element()
}
