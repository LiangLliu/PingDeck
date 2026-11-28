//! Subagent lane and tool-step waterfall rows rendered inside the Timeline (`时间轴`) tab of
//! [`WorkflowMonitorModal`]. Row heads are the shared `status_row`; the waterfall bar, badges and
//! actions ride in its trailing slot.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, px,
};
use spanbay_desktop::core::agent::dto::{TopologySubagent, TopologyToolStep};
use spanbay_desktop::state::{map_run_status, map_tool_status};

use crate::ui::{
    Tone, WaitKind, badge, btn_danger, exit_code_badge, h_row, palette, status_row_wait,
    status_visual, to_hsla, v_col, wait_kind,
};

use super::modal::WorkflowMonitorModal;
use super::status::{
    compute_bar_span, format_elapsed_short, subagent_kind_label, subagent_role_label, waterfall_bar,
};

pub(super) struct TimelineAxis {
    pub run_start_ms: i64,
    pub run_total_ms: i64,
    pub snapshot_ms: i64,
    pub track_w: f32,
}

pub(super) fn render_subagent_lane(
    view: &WorkflowMonitorModal,
    sub: &TopologySubagent,
    axis: &TimelineAxis,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let node_id = format!("sub:{}", sub.run.run_id);
    let selected = view.is_subagent_selected(&sub.run.run_id);
    let status = map_run_status(sub.run.status);
    let (shape, _) = status_visual(status);
    let (left, width) = compute_bar_span(
        axis.run_start_ms,
        axis.run_total_ms,
        Some(sub.run.started_at_ms),
        sub.run.finished_at_ms,
        axis.snapshot_ms,
    );
    let nid_click = node_id.clone();
    let stop_id = sub.run.run_id.clone();

    let trailing = h_row()
        .gap(px(6.0))
        .child(badge(
            subagent_kind_label(&sub.subagent_kind, cx),
            Tone::Neutral,
            cx,
        ))
        .children(
            sub.run
                .role
                .map(|r| badge(subagent_role_label(r, cx), Tone::Neutral, cx)),
        )
        .child(waterfall_bar(left, width, axis.track_w, shape, cx))
        .children(sub.run.status.is_active().then(|| {
            btn_danger(
                SharedString::from(format!("wf-tl-stop-{}", sub.run.run_id)),
                ch.monitor_stop_subagent(),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.stop_run_by_id(stop_id.clone(), cx);
            }))
        }));

    let mut box_col = v_col()
        .id(SharedString::from(format!("wf-tl-sub-{}", sub.run.run_id)))
        .w_full()
        .rounded(px(8.0))
        .bg(to_hsla(if selected { p.accent_soft } else { p.surface }))
        .border_1()
        .border_color(to_hsla(if selected {
            p.accent
        } else {
            p.line.composite_over(p.surface)
        }))
        .p(px(4.0))
        .gap(px(3.0))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select_node(Some(nid_click.clone()), cx);
        }))
        .child(status_row_wait(
            SharedString::from(format!("wf-tl-sub-head-{}", sub.run.run_id)),
            status,
            Some(wait_kind(None, sub.run.status_detail.as_deref())),
            sub.description.clone(),
            Some(SharedString::from(format_elapsed_short(sub.awake_ms))),
            Some(trailing.into_any_element()),
            cx,
        ));

    if !sub.tool_calls.is_empty() {
        let mut child_rows = v_col().w_full().pl(px(14.0)).gap(px(3.0));
        for step in &sub.tool_calls {
            child_rows = child_rows.child(render_tool_row(view, step, axis, cx));
        }
        box_col = box_col.child(child_rows);
    }
    box_col.into_any_element()
}

pub(super) fn render_tool_row(
    view: &WorkflowMonitorModal,
    step: &TopologyToolStep,
    axis: &TimelineAxis,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let node_id = format!("tool:{}", step.call_id);
    let selected = view.selected_node_id.as_deref() == Some(node_id.as_str());
    let status = map_tool_status(&step.status);
    let (shape, _) = status_visual(status);
    let (left, width) = compute_bar_span(
        axis.run_start_ms,
        axis.run_total_ms,
        step.started_ms,
        step.ended_ms,
        axis.snapshot_ms,
    );
    let nid_click = node_id.clone();
    let title = if step.summary.is_empty() {
        step.name.clone()
    } else {
        format!("{} · {}", step.name, step.summary)
    };
    let trailing = h_row()
        .gap(px(6.0))
        .children(
            step.exit_code
                .filter(|c| *c != 0)
                .map(|c| exit_code_badge(c, cx)),
        )
        .child(waterfall_bar(left, width, axis.track_w, shape, cx));
    let wait = match step.name.as_str() {
        "submit_plan" => WaitKind::Plan,
        "ask_user" => WaitKind::Question,
        _ => WaitKind::Approval,
    };

    status_row_wait(
        SharedString::from(format!("wf-tl-t-{}", step.call_id)),
        status,
        Some(wait),
        title,
        Some(SharedString::from(
            step.duration_ms
                .map_or_else(|| "—".to_owned(), format_elapsed_short),
        )),
        Some(trailing.into_any_element()),
        cx,
    )
    .bg(to_hsla(if selected { p.accent_soft } else { p.surface }))
    .border_1()
    .border_color(to_hsla(if selected {
        p.accent
    } else {
        p.line.composite_over(p.surface)
    }))
    .cursor_pointer()
    .on_click(cx.listener(move |this, _, _, cx| {
        cx.stop_propagation();
        this.select_node(Some(nid_click.clone()), cx);
    }))
    .into_any_element()
}
