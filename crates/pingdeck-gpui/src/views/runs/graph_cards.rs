//! Card renderers for the GitHub-Actions-style Execution Graph (`执行图`) inside the 90%
//! [`WorkflowMonitorModal`] and read-only Plan preview cards.
//!
//! Supports Tier 1 in-canvas micro-expansion for both `Parallel Group` cards and multi-tool
//! `Subagent` / bound `Planned Task` cards (bounded to `GRAPH_MAX_INLINE_MEMBERS = 6` inline rows
//! with a clean overflow footer row).

use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::state::{
    GRAPH_GROUP_TAB_H, GRAPH_MAX_INLINE_MEMBERS, GRAPH_MEMBER_ROW_H, GRAPH_OVERFLOW_ROW_H,
    GraphNodeKind, GraphNodeMember, PositionedNode,
};

use crate::ui::{Tone, badge, h_row, palette, status_indicator, to_hsla, v_col};

use super::graph::card_border_color;
use super::modal::WorkflowMonitorModal;
use super::status::{
    format_elapsed_short, format_model_short, graph_kind_label, graph_status_visual,
    subagent_role_label,
};

/// Interactive graph card rendered inside [`WorkflowMonitorModal`].
pub(super) fn render_modal_graph_card(
    view: &WorkflowMonitorModal,
    pnode: &PositionedNode,
    scale: f32,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let node = &pnode.node;
    if node.group_tab.is_some() && !node.members.is_empty() {
        render_group_card(view, pnode, scale, cx)
    } else {
        render_single_card(view, pnode, scale, cx)
    }
}

/// Static read-only graph card rendered inside the Chat `Plan` approval gate preview.
pub(super) fn render_preview_graph_card(
    pnode: &PositionedNode,
    scale: f32,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let node = &pnode.node;
    let (shape, tone) = graph_status_visual(node.status);
    let border = card_border_color(false, tone, &p);

    if let Some(tab_text) = &node.group_tab
        && !node.members.is_empty()
    {
        let (done, total) = node.member_progress();
        let tab_h = GRAPH_GROUP_TAB_H * scale;
        let body_h = (pnode.h - GRAPH_GROUP_TAB_H) * scale;
        return v_col()
            .absolute()
            .left(px(pnode.x * scale))
            .top(px(pnode.y * scale))
            .w(px(pnode.w * scale))
            .h(px(pnode.h * scale))
            .items_start()
            .child(
                h_row()
                    .h(px(tab_h))
                    .max_w(px((pnode.w * 0.78) * scale))
                    .px(px(8.0 * scale))
                    .bg(to_hsla(p.pane))
                    .border_t_1()
                    .border_l_1()
                    .border_r_1()
                    .border_color(border)
                    .rounded_t(px(6.0 * scale))
                    .child(
                        div()
                            .text_size(px((10.0 * scale).clamp(9.0, 11.0)))
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(tab_text.clone())),
                    ),
            )
            .child(
                v_col()
                    .w_full()
                    .h(px(body_h))
                    .px(px(9.0 * scale))
                    .py(px(7.0 * scale))
                    .gap(px(4.0 * scale))
                    .bg(to_hsla(p.surface))
                    .border_1()
                    .border_color(border)
                    .rounded_b(px(8.0 * scale))
                    .rounded_tr(px(8.0 * scale))
                    .overflow_hidden()
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(status_indicator(shape, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px((11.5 * scale).clamp(9.5, 12.5)))
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(node.title.clone())),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(ch.graph_group_progress(done, total))),
                    ),
            )
            .into_any_element();
    }

    v_col()
        .absolute()
        .left(px(pnode.x * scale))
        .top(px(pnode.y * scale))
        .w(px(pnode.w * scale))
        .h(px(pnode.h * scale))
        .px(px(9.0 * scale))
        .py(px(7.0 * scale))
        .gap(px(4.0 * scale))
        .rounded(px(8.0 * scale))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(border)
        .overflow_hidden()
        .child(
            h_row()
                .gap(px(6.0))
                .child(status_indicator(shape, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px((11.5 * scale).clamp(9.5, 12.5)))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(SharedString::from(node.title.clone())),
                ),
        )
        .child(
            h_row()
                .gap(px(4.0))
                .child(badge(graph_kind_label(node.kind, cx), Tone::Neutral, cx))
                .children(
                    node.role
                        .map(|r| badge(subagent_role_label(r, cx), Tone::Accent, cx)),
                ),
        )
        .into_any_element()
}

fn render_single_card(
    view: &WorkflowMonitorModal,
    pnode: &PositionedNode,
    scale: f32,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let node = &pnode.node;
    let selected = view.selected_node_id.as_deref() == Some(node.id.as_str());
    let (shape, tone) = graph_status_visual(node.status);
    let border = card_border_color(selected, tone, &p);
    let hover_border = to_hsla(p.accent_line.composite_over(p.surface));
    let bg = if selected {
        to_hsla(p.accent_soft.composite_over(p.surface))
    } else {
        to_hsla(p.surface)
    };
    let nid = node.id.clone();

    let mut body = v_col()
        .id(SharedString::from(format!("wf-gnode-{}", node.id)))
        .absolute()
        .left(px(pnode.x * scale))
        .top(px(pnode.y * scale))
        .w(px(pnode.w * scale))
        .h(px(pnode.h * scale))
        .px(px(10.0 * scale))
        .py(px(8.0 * scale))
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
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.select_node(Some(nid.clone()), cx);
        }))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(6.0))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(6.0))
                        .child(status_indicator(shape, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px((12.0 * scale).clamp(10.0, 13.0)))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(node.title.clone())),
                        ),
                )
                .children(node.elapsed_ms.map(|ms| {
                    div()
                        .flex_shrink_0()
                        .text_size(px((10.5 * scale).clamp(9.0, 11.5)))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format_elapsed_short(ms)))
                })),
        )
        .child(
            h_row()
                .w_full()
                .gap(px(5.0))
                .overflow_hidden()
                .children(
                    (node.kind != GraphNodeKind::Main)
                        .then(|| badge(graph_kind_label(node.kind, cx), Tone::Neutral, cx)),
                )
                .children(
                    node.role
                        .map(|r| badge(subagent_role_label(r, cx), Tone::Accent, cx)),
                )
                .children(
                    (node.attempt > 0)
                        .then(|| badge(ch.graph_attempt_badge(node.attempt), Tone::Warning, cx)),
                )
                .children(
                    node.resumed
                        .then(|| badge(ch.monitor_resumed(), Tone::Neutral, cx)),
                )
                .children(
                    node.exit_code
                        .filter(|c| *c != 0)
                        .map(|c| badge(format!("exit {c}"), Tone::Danger, cx)),
                )
                .children(node.subtitle.as_ref().map(|sub| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px((10.5 * scale).clamp(9.0, 11.5)))
                        .text_color(to_hsla(p.muted))
                        .truncate()
                        .child(SharedString::from(format_model_short(sub)))
                })),
        );

    if let Some(wait_txt) = &node.waiting_summary {
        body = body.child(alert_pill(wait_txt, true, cx));
    } else if let Some(err) = &node.error {
        body = body.child(alert_pill(err, false, cx));
    }
    body.into_any_element()
}

fn render_group_card(
    view: &WorkflowMonitorModal,
    pnode: &PositionedNode,
    scale: f32,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let node = &pnode.node;
    let selected = view.selected_node_id.as_deref() == Some(node.id.as_str());
    let (shape, tone) = graph_status_visual(node.status);
    let border = card_border_color(selected, tone, &p);
    let hover_border = to_hsla(p.accent_line.composite_over(p.surface));
    let bg = if selected {
        to_hsla(p.accent_soft.composite_over(p.surface))
    } else {
        to_hsla(p.surface)
    };
    let (done, total) = node.member_progress();
    let nid_sel = node.id.clone();
    let nid_toggle = node.id.clone();
    let nid_more = node.id.clone();
    let tab_text = node.group_tab.clone().unwrap_or_default();
    let tab_h = GRAPH_GROUP_TAB_H * scale;
    let body_h = (pnode.h - GRAPH_GROUP_TAB_H) * scale;

    let mut card_body = v_col()
        .id(SharedString::from(format!("wf-ggroup-{}", node.id)))
        .w_full()
        .h(px(body_h))
        .bg(bg)
        .border_1()
        .border_color(border)
        .rounded_b(px(8.0 * scale))
        .rounded_tr(px(8.0 * scale))
        .overflow_hidden()
        .cursor_pointer()
        .hover(move |s| {
            if selected {
                s
            } else {
                s.border_color(hover_border)
            }
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.select_node(Some(nid_sel.clone()), cx);
        }))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(6.0))
                .px(px(10.0 * scale))
                .pt(px(8.0 * scale))
                .pb(px(4.0 * scale))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(6.0))
                        .child(status_indicator(shape, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px((12.0 * scale).clamp(10.0, 13.0)))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(if node.kind == GraphNodeKind::Group {
                                    ch.graph_group_progress(done, total)
                                } else {
                                    node.title.clone()
                                })),
                        ),
                )
                .children(node.elapsed_ms.map(|ms| {
                    div()
                        .flex_shrink_0()
                        .text_size(px((10.5 * scale).clamp(9.0, 11.5)))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format_elapsed_short(ms)))
                })),
        );

    if let Some(wait_txt) = &node.waiting_summary {
        card_body = card_body.child(
            div()
                .mx(px(8.0 * scale))
                .mb(px(3.0))
                .child(alert_pill(wait_txt, true, cx)),
        );
    } else if let Some(err) = &node.error {
        card_body = card_body.child(
            div()
                .mx(px(8.0 * scale))
                .mb(px(3.0))
                .child(alert_pill(err, false, cx)),
        );
    }

    if node.expanded {
        let collapse_row = h_row()
            .w_full()
            .justify_between()
            .px(px(10.0 * scale))
            .pb(px(4.0 * scale))
            .child(
                div()
                    .text_size(px((10.5 * scale).clamp(9.0, 11.5)))
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(ch.graph_group_progress(done, total))),
            )
            .child(
                div()
                    .id(SharedString::from(format!("wf-gcol-{}", node.id)))
                    .text_size(px((10.5 * scale).clamp(9.0, 11.5)))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(to_hsla(p.accent_ink))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_group(nid_toggle.clone(), cx);
                    }))
                    .child(ch.graph_group_collapse()),
            );
        card_body = card_body.child(collapse_row);

        let mut list = v_col()
            .w_full()
            .border_t_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)));
        for member in node.members.iter().take(GRAPH_MAX_INLINE_MEMBERS) {
            list = list.child(render_member_row(view, member, scale, cx));
        }
        let hidden = node.members.len().saturating_sub(GRAPH_MAX_INLINE_MEMBERS);
        if hidden > 0 {
            list = list.child(
                h_row()
                    .id(SharedString::from(format!("wf-gmore-{}", node.id)))
                    .w_full()
                    .h(px(GRAPH_OVERFLOW_ROW_H * scale))
                    .px(px(10.0 * scale))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.select_node(Some(nid_more.clone()), cx);
                    }))
                    .child(
                        div()
                            .text_size(px((10.0 * scale).clamp(9.0, 11.0)))
                            .text_color(to_hsla(p.accent_ink))
                            .truncate()
                            .child(SharedString::from(ch.graph_more_members(hidden))),
                    ),
            );
        }
        card_body = card_body.child(list);
    } else {
        let footer = h_row()
            .w_full()
            .justify_between()
            .px(px(10.0 * scale))
            .pt(px(2.0 * scale))
            .child(
                div()
                    .id(SharedString::from(format!("wf-gexp-{}", node.id)))
                    .text_size(px((11.0 * scale).clamp(9.5, 12.0)))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(to_hsla(p.muted))
                    .hover(|s| s.text_color(to_hsla(p.accent_ink)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_group(nid_toggle.clone(), cx);
                    }))
                    .child(SharedString::from(ch.graph_group_show_all(total))),
            )
            .children((node.kind != GraphNodeKind::Group).then(|| {
                div()
                    .text_size(px((10.5 * scale).clamp(9.0, 11.5)))
                    .text_color(to_hsla(p.faint))
                    .child(SharedString::from(ch.graph_group_progress(done, total)))
            }));
        card_body = card_body.child(footer);
    }

    let tab_flap = h_row()
        .h(px(tab_h))
        .max_w(px((pnode.w * 0.78) * scale))
        .px(px(9.0 * scale))
        .gap(px(5.0))
        .bg(to_hsla(p.pane))
        .border_t_1()
        .border_l_1()
        .border_r_1()
        .border_color(border)
        .rounded_t(px(6.0 * scale))
        .child(
            div()
                .text_size(px((10.5 * scale).clamp(9.0, 11.5)))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.muted))
                .truncate()
                .child(SharedString::from(tab_text)),
        );

    v_col()
        .absolute()
        .left(px(pnode.x * scale))
        .top(px(pnode.y * scale))
        .w(px(pnode.w * scale))
        .h(px(pnode.h * scale))
        .items_start()
        .child(tab_flap)
        .child(card_body)
        .into_any_element()
}

fn render_member_row(
    view: &WorkflowMonitorModal,
    member: &GraphNodeMember,
    scale: f32,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let selected = view.selected_node_id.as_deref() == Some(member.id.as_str());
    let (shape, _) = graph_status_visual(member.status);
    let mid = member.id.clone();
    let row_bg = if selected {
        to_hsla(p.accent_soft)
    } else {
        to_hsla(p.surface)
    };
    let hover_bg = to_hsla(p.hover.composite_over(p.surface));

    h_row()
        .id(SharedString::from(format!("wf-gmem-{}", member.id)))
        .w_full()
        .h(px(GRAPH_MEMBER_ROW_H * scale))
        .justify_between()
        .gap(px(6.0))
        .px(px(10.0 * scale))
        .bg(row_bg)
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .cursor_pointer()
        .hover(move |s| if selected { s } else { s.bg(hover_bg) })
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.select_node(Some(mid.clone()), cx);
        }))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(6.0))
                .child(status_indicator(shape, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px((11.0 * scale).clamp(9.5, 12.0)))
                        .text_color(to_hsla(p.ink_2))
                        .truncate()
                        .child(SharedString::from(member.title.clone())),
                )
                .children(
                    member
                        .exit_code
                        .filter(|c| *c != 0)
                        .map(|c| badge(format!("exit {c}"), Tone::Danger, cx)),
                ),
        )
        .children(member.elapsed_ms.map(|ms| {
            div()
                .flex_shrink_0()
                .text_size(px((10.5 * scale).clamp(9.0, 11.5)))
                .text_color(to_hsla(p.faint))
                .child(SharedString::from(format_elapsed_short(ms)))
        }))
        .into_any_element()
}

fn alert_pill(text: &str, warn: bool, cx: &App) -> gpui_kit::Div {
    let p = palette(cx);
    div()
        .w_full()
        .px(px(6.0))
        .py(px(2.0))
        .rounded(px(4.0))
        .bg(to_hsla(if warn { p.warn_soft } else { p.bad_soft }))
        .text_size(px(10.0))
        .text_color(to_hsla(if warn { p.warn } else { p.bad }))
        .truncate()
        .child(SharedString::from(text.to_owned()))
}
