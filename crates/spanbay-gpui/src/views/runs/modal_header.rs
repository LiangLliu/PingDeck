//! Header bar with Session Run History switcher (`[‹] [Run #N ▾] [›]`) and quick actions for the
//! 90% viewport [`WorkflowMonitorModal`].

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use spanbay_desktop::core::agent::dto::SessionRunHistoryItem;
use spanbay_desktop::core::agent::model::AgentRunStatus;

use crate::ui::{
    Anchor, Disableable as _, ScrollableElement as _, Tone, UiIcon, badge, btn_danger, btn_ghost,
    btn_icon, btn_secondary, choice_chip, h_row, icon_xs, list_row, palette, pill_btn, popover,
    popover_header, status_indicator, to_hsla, v_col,
};

use super::modal::WorkflowMonitorModal;
use super::status::{format_elapsed_short, format_model_short, format_relative_age};
use super::types::HistoryPickerFilter;
use crate::ui::run_status_meta;

pub fn render_modal_header(
    view: &WorkflowMonitorModal,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    let title = view
        .topology
        .as_ref()
        .map(|t| {
            if t.title.trim().is_empty() {
                ch.header_new_chat().to_owned()
            } else {
                t.title.clone()
            }
        })
        .unwrap_or_else(|| ch.monitor_title().to_owned());

    let active_run_id = view
        .run_id
        .as_deref()
        .or_else(|| view.topology.as_ref().map(|t| t.run.run_id.as_str()));
    let current_idx = active_run_id.and_then(|rid| {
        view.session_history
            .iter()
            .position(|item| item.run.run_id == rid)
    });
    let total_runs = view.session_history.len();
    let can_prev = current_idx.is_some_and(|i| i + 1 < total_runs);
    let can_next = current_idx.is_some_and(|i| i > 0);
    let is_active = view
        .topology
        .as_ref()
        .is_some_and(|t| t.run.status.is_active());
    let rid_stop = view.topology.as_ref().map(|t| t.run.run_id.clone());
    let rid_logs = view.topology.as_ref().map(|t| t.run.run_id.clone());
    let sid_chat = view.session_id.clone();

    let history_controls = (!view.session_history.is_empty()).then(|| {
        h_row()
            .gap(px(3.0))
            .child(
                btn_icon("wf-hist-prev", UiIcon::ArrowLeft)
                    .tooltip(ch.monitor_prev_run())
                    .disabled(!can_prev)
                    .on_click(cx.listener(|this, _, _, cx| this.step_history(1, cx))),
            )
            .child(render_history_popover(view, current_idx, cx))
            .child(
                btn_icon("wf-hist-next", UiIcon::ChevronRight)
                    .tooltip(ch.monitor_next_run())
                    .disabled(!can_next)
                    .on_click(cx.listener(|this, _, _, cx| this.step_history(-1, cx))),
            )
    });

    h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(12.0))
        .px(px(16.0))
        .py(px(10.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(10.0))
                .child(
                    h_row()
                        .flex_shrink_0()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_color(to_hsla(p.accent))
                                .child(icon_xs(UiIcon::Activity)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.muted))
                                .child(ch.monitor_title()),
                        ),
                )
                .child(
                    div()
                        .min_w_0()
                        .max_w(px(320.0))
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(SharedString::from(title)),
                )
                .children(history_controls),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(6.0))
                .children(
                    view.status_msg
                        .clone()
                        .map(|(tone, msg)| badge(msg, tone, cx)),
                )
                .children(rid_logs.map(|rid| {
                    btn_ghost("wf-hdr-logs", ch.monitor_view_logs())
                        .icon(icon_xs(UiIcon::Usage))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_usage_and_close(rid.clone(), cx);
                        }))
                }))
                .children((!sid_chat.is_empty()).then(|| {
                    btn_secondary("wf-hdr-chat", ch.monitor_open_chat())
                        .icon(icon_xs(UiIcon::ExternalLink))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_chat_and_close(sid_chat.clone(), cx);
                        }))
                }))
                .children(is_active.then_some(rid_stop).flatten().map(|rid| {
                    btn_danger("wf-hdr-stop", ch.card_stop()).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.stop_run_by_id(rid.clone(), cx);
                        },
                    ))
                }))
                .child(
                    btn_icon("wf-hdr-refresh", UiIcon::Refresh)
                        .tooltip(ch.monitor_refresh())
                        .on_click(cx.listener(|this, _, _, cx| this.reload_all(cx))),
                )
                .child(
                    btn_icon("wf-hdr-close", UiIcon::Close)
                        .tooltip(ch.monitor_close())
                        .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                ),
        )
        .into_any_element()
}

fn render_history_popover(
    view: &WorkflowMonitorModal,
    current_idx: Option<usize>,
    cx: &mut Context<WorkflowMonitorModal>,
) -> impl IntoElement {
    let ch = crate::ui::tr(cx).chat();
    let now_ms = spanbay_desktop::core::app::current_time_ms();
    let total = view.session_history.len();
    let trigger_label = match current_idx.and_then(|i| view.session_history.get(i)) {
        Some(item) => {
            let num = total.saturating_sub(current_idx.unwrap_or(0));
            let (_, _, status_lbl) =
                run_status_meta(item.run.status, None, item.run.status_detail.as_deref(), cx);
            let age = format_relative_age(item.run.started_at_ms, now_ms, cx);
            format!(
                "{} · {} · {} · {status_lbl}",
                ch.monitor_history_label(num),
                age,
                format_elapsed_short(item.awake_ms)
            )
        }
        None => ch.monitor_history_heading(total),
    };

    let items = view.session_history.clone();
    let active_rid = view.run_id.clone();
    let active_filter = view.history_filter;
    let ent = cx.entity().clone();

    popover("wf-history-popover")
        .anchor(Anchor::TopLeft)
        .trigger(pill_btn(
            "wf-history-trigger",
            trigger_label,
            Some(UiIcon::ChevronDown),
            false,
        ))
        .content(move |_, _, pop_cx| {
            let pp = palette(pop_cx);
            let pch = crate::ui::tr(pop_cx).chat();
            let ent_all = ent.clone();
            let ent_cpx = ent.clone();
            let ent_fail = ent.clone();

            let filter_bar = h_row()
                .w_full()
                .gap(px(4.0))
                .px(px(6.0))
                .pb(px(4.0))
                .child(
                    choice_chip(
                        "wf-hf-all",
                        pch.monitor_history_all(),
                        active_filter == HistoryPickerFilter::All,
                        pop_cx,
                    )
                    .on_click(move |_, _, cx| {
                        ent_all.update(cx, |this, cx| {
                            this.history_filter = HistoryPickerFilter::All;
                            cx.notify();
                        });
                    }),
                )
                .child(
                    choice_chip(
                        "wf-hf-cpx",
                        pch.monitor_history_complex(),
                        active_filter == HistoryPickerFilter::Complex,
                        pop_cx,
                    )
                    .on_click(move |_, _, cx| {
                        ent_cpx.update(cx, |this, cx| {
                            this.history_filter = HistoryPickerFilter::Complex;
                            cx.notify();
                        });
                    }),
                )
                .child(
                    choice_chip(
                        "wf-hf-fail",
                        pch.monitor_history_failed(),
                        active_filter == HistoryPickerFilter::Failed,
                        pop_cx,
                    )
                    .on_click(move |_, _, cx| {
                        ent_fail.update(cx, |this, cx| {
                            this.history_filter = HistoryPickerFilter::Failed;
                            cx.notify();
                        });
                    }),
                );

            let filtered: Vec<(usize, &SessionRunHistoryItem)> = items
                .iter()
                .enumerate()
                .filter(|(_, item)| match active_filter {
                    HistoryPickerFilter::All => true,
                    HistoryPickerFilter::Complex => {
                        item.has_parallel || item.has_task_graph || item.tool_count > 0
                    }
                    HistoryPickerFilter::Failed => item.run.status == AgentRunStatus::Failed,
                })
                .collect();

            let mut list = v_col().w_full().gap(px(3.0));
            if filtered.is_empty() {
                list = list.child(
                    div()
                        .p(px(10.0))
                        .text_xs()
                        .text_color(to_hsla(pp.muted))
                        .child(pch.monitor_history_empty()),
                );
            } else {
                for (orig_idx, item) in filtered {
                    let num = items.len().saturating_sub(orig_idx);
                    let selected = active_rid.as_deref() == Some(item.run.run_id.as_str());
                    let (shape, tone, status_lbl) = run_status_meta(
                        item.run.status,
                        None,
                        item.run.status_detail.as_deref(),
                        pop_cx,
                    );
                    let age = format_relative_age(item.run.started_at_ms, now_ms, pop_cx);
                    let rid_pick = item.run.run_id.clone();
                    let ent_pick = ent.clone();
                    let pop_h = pop_cx.entity().clone();
                    let prompt_line = item
                        .prompt_preview
                        .clone()
                        .unwrap_or_else(|| format_model_short(&item.run.model_label));

                    list = list.child(
                        list_row(
                            SharedString::from(format!("wf-hpick-{}", item.run.run_id)),
                            selected,
                            pop_cx,
                            move |_, w, cx| {
                                ent_pick.update(cx, |this, cx| {
                                    this.select_history_run(rid_pick.clone(), cx);
                                });
                                pop_h.update(cx, |s, cx| s.dismiss(w, cx));
                            },
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
                                            h_row()
                                                .gap(px(5.0))
                                                .child(status_indicator(shape, pop_cx))
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                        .text_color(to_hsla(pp.ink))
                                                        .child(SharedString::from(
                                                            pch.monitor_history_label(num),
                                                        )),
                                                )
                                                .child(badge(status_lbl, tone, pop_cx))
                                                .children(item.has_task_graph.then(|| {
                                                    badge(
                                                        pch.gates_plan_view_graph(),
                                                        Tone::Accent,
                                                        pop_cx,
                                                    )
                                                }))
                                                .children((item.subagent_count > 0).then(|| {
                                                    badge(
                                                        pch.monitor_sub_badge(item.subagent_count),
                                                        Tone::Neutral,
                                                        pop_cx,
                                                    )
                                                })),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(10.5))
                                                .text_color(to_hsla(pp.faint))
                                                .child(SharedString::from(format!(
                                                    "{} · {age}",
                                                    format_elapsed_short(item.awake_ms)
                                                ))),
                                        ),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .text_xs()
                                        .text_color(to_hsla(pp.muted))
                                        .truncate()
                                        .child(SharedString::from(prompt_line)),
                                ),
                        ),
                    );
                }
            }

            v_col()
                .w(px(420.0))
                .gap(px(4.0))
                .child(popover_header(
                    pch.monitor_history_heading(items.len()),
                    None,
                    pop_cx,
                ))
                .child(filter_bar)
                .child(
                    div()
                        .id("wf-hist-pop-scroll")
                        .w_full()
                        .max_h(px(360.0))
                        .overflow_y_scrollbar()
                        .child(list),
                )
        })
}
