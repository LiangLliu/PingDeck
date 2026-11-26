//! Collapsible right-hand Details inspector for AI Chat: header + close button + badge counts,
//! `Progress` (todos, sub-agents, background processes w/ Kill), `Changes` (persisted journal w/
//! Diff / Keep / Undo), `Context` (tokens, budget, Compact now), and `Options` (sandbox & rules).

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    div, px,
};
use pingdeck_desktop::core::agent::model::{AgentTodoStatus, FileChangeKind, FileChangeStatus};
use pingdeck_desktop::core::agent::tools::process::ProcessState;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_danger, btn_ghost, btn_icon,
    card, h_row, icon_xs, palette, progress_bar, section_heading, status_indicator,
    tab_bar_underline, tab_item, to_hsla, v_col,
};

use super::inspector_tabs::{render_context_tab, render_options_tab};
use super::run_status::{run_status_meta, subagent_kind_label, subagent_role_label};
use super::run_topology::render_topology;
use super::{ChatView, DetailsTab};

pub fn render_inspector(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let chat_entity = cx.entity().clone();
    let changes_count = if view.persisted_changes.is_empty() {
        view.thread.file_changes.len()
    } else {
        view.persisted_changes.len()
    };
    let changes_lbl = if changes_count > 0 {
        crate::ui::tr(cx).chat().inspector_changes(changes_count)
    } else {
        crate::ui::tr(cx).chat().inspector_changes_2().to_owned()
    };

    let tabs = tab_bar_underline("chat-details-tabs")
        .selected_index(view.details_tab.index())
        .child(tab_item(crate::ui::tr(cx).chat().inspector_progress()))
        .child(tab_item(changes_lbl))
        .child(tab_item(crate::ui::tr(cx).chat().header_ctx_menu()))
        .child(tab_item(crate::ui::tr(cx).chat().inspector_options()))
        .on_click(move |idx, _, cx| {
            let next = DetailsTab::from_index(*idx);
            chat_entity.update(cx, |this, cx| {
                this.details_tab = next;
                cx.notify();
            });
        });

    let body = match view.details_tab {
        DetailsTab::Progress => render_progress_tab(view, cx).into_any_element(),
        DetailsTab::Changes => render_changes_tab(view, cx).into_any_element(),
        DetailsTab::Context => render_context_tab(view, cx).into_any_element(),
        DetailsTab::Options => render_options_tab(view, cx).into_any_element(),
    };

    v_col()
        .size_full()
        .bg(to_hsla(p.pane))
        .child(
            h_row()
                .justify_between()
                .px(px(12.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(crate::ui::tr(cx).chat().inspector_inspector()),
                )
                .child(
                    btn_icon("chat-insp-close", UiIcon::Close).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.details_open = false;
                            cx.notify();
                        },
                    )),
                ),
        )
        .child(
            div()
                .px(px(8.0))
                .pt(px(4.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(tabs),
        )
        .child(
            div()
                .id("chat-insp-body-scroll")
                .flex_1()
                .min_h_0()
                .p(px(12.0))
                .overflow_y_scrollbar()
                .child(body),
        )
}

fn render_progress_tab(view: &ChatView, cx: &mut Context<ChatView>) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let todos = &view.thread.todos;
    let total = todos.len();
    let done = todos
        .iter()
        .filter(|t| t.status == AgentTodoStatus::Completed)
        .count();

    let mut col = v_col().gap(px(12.0));
    let focus_run_id = view
        .session_topology
        .as_ref()
        .map(|t| t.run.run_id.clone())
        .or_else(|| view.thread.active_run.as_ref().map(|r| r.run_id.clone()));

    col = col.child(
        h_row()
            .w_full()
            .justify_between()
            .child(section_heading(ch.inspector_active_run(), cx))
            .child(
                btn_ghost("chat-insp-open-mon", ch.inspector_open_in_monitor())
                    .icon(icon_xs(UiIcon::Activity))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_run_monitor(focus_run_id.clone(), cx);
                    })),
            ),
    );

    if let Some(topo) = &view.session_topology {
        col = col.child(render_topology(view, topo, true, cx));
    } else if let Some(run) = &view.thread.active_run {
        let (shape, tone, status_lbl) =
            run_status_meta(run.status, None, run.status_detail.as_deref(), cx);
        col = col.child(
            card(cx).p(px(10.0)).child(
                v_col()
                    .gap(px(4.0))
                    .child(
                        h_row()
                            .justify_between()
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .child(status_indicator(shape, cx))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .text_color(to_hsla(p.ink))
                                            .child(SharedString::from(
                                                view.format_raw_model_label(&run.model_label),
                                            )),
                                    ),
                            )
                            .child(badge(status_lbl, tone, cx)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(ch.monitor_steps_short(run.iterations))),
                    ),
            ),
        );
    } else {
        col = col.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(ch.inspector_no_runs_yet()),
        );
    }

    if !view.thread.subagents.is_empty() && view.session_topology.is_none() {
        col = col.child(
            div()
                .pt(px(2.0))
                .child(section_heading(ch.tasks_subagents(), cx)),
        );
        for sub in view.thread.subagents.values() {
            let (shape, tone, status_lbl) = run_status_meta(
                sub.summary.status,
                None,
                sub.summary.status_detail.as_deref(),
                cx,
            );
            let title = if sub.description.is_empty() {
                view.format_raw_model_label(&sub.summary.model_label)
            } else {
                sub.description.clone()
            };
            let stop_id = sub.child_run_id.clone();
            col = col.child(
                card(cx).p(px(8.0)).child(
                    v_col()
                        .gap(px(4.0))
                        .child(
                            h_row()
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
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                .text_color(to_hsla(p.ink))
                                                .truncate()
                                                .child(SharedString::from(title)),
                                        ),
                                )
                                .child(badge(status_lbl, tone, cx)),
                        )
                        .child(
                            h_row()
                                .justify_between()
                                .gap(px(4.0))
                                .child(
                                    h_row()
                                        .gap(px(4.0))
                                        .child(badge(
                                            subagent_kind_label(&sub.subagent_kind, cx),
                                            Tone::Neutral,
                                            cx,
                                        ))
                                        .children(sub.summary.role.map(|r| {
                                            badge(subagent_role_label(r, cx), Tone::Accent, cx)
                                        })),
                                )
                                .children((!sub.finished).then(|| {
                                    btn_danger(
                                        SharedString::from(format!(
                                            "insp-sub-stop-{}",
                                            sub.child_run_id
                                        )),
                                        ch.monitor_stop_subagent(),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.stop_run_by_id(stop_id.clone(), cx);
                                        },
                                    ))
                                })),
                        ),
                ),
            );
        }
    }

    col = col.child(
        h_row()
            .justify_between()
            .child(section_heading(ch.inspector_tasks(), cx))
            .children((total > 0).then(|| badge(format!("{done}/{total}"), Tone::Neutral, cx))),
    );

    if total > 0 {
        let pct = (done as f32 / total as f32) * 100.0;
        col = col.child(progress_bar("chat-todos-prog", pct));
        for item in todos {
            let shape = match item.status {
                AgentTodoStatus::Completed => StatusShape::Ok,
                AgentTodoStatus::InProgress => StatusShape::Run,
                AgentTodoStatus::Cancelled => StatusShape::Bad,
                AgentTodoStatus::Pending => StatusShape::Idle,
            };
            col = col.child(
                h_row()
                    .gap(px(8.0))
                    .py(px(3.0))
                    .child(status_indicator(shape, cx))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(if item.status == AgentTodoStatus::Completed {
                                to_hsla(p.muted)
                            } else {
                                to_hsla(p.ink)
                            })
                            .child(SharedString::from(item.content.clone())),
                    ),
            );
        }
    } else {
        col = col.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(ch.inspector_multi_step_task_progress_appears()),
        );
    }

    if !view.processes.is_empty() {
        col = col.child(div().pt(px(4.0)).child(section_heading(
            crate::ui::tr(cx).chat().inspector_background_processes(),
            cx,
        )));
        for proc_item in &view.processes {
            let pid_kill = proc_item.id.clone();
            let running = proc_item.state == ProcessState::Running;
            col = col.child(
                card(cx).p(px(8.0)).child(
                    h_row()
                        .justify_between()
                        .gap(px(6.0))
                        .child(
                            v_col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(to_hsla(p.ink))
                                        .truncate()
                                        .child(proc_item.command.clone()),
                                )
                                .child(div().text_xs().text_color(to_hsla(p.muted)).child(
                                    format!(
                                    "{:?}{}",
                                    proc_item.state,
                                    proc_item
                                        .pid
                                        .map(|pid| format!(" · PID {pid}"))
                                        .unwrap_or_default()
                                ),
                                )),
                        )
                        .children(running.then(|| {
                            btn_danger(
                                SharedString::from(format!("chat-pk-{}", proc_item.id)),
                                crate::ui::tr(cx).chat().inspector_kill(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.kill_process(pid_kill.clone(), cx);
                                },
                            ))
                        })),
                ),
            );
        }
    }

    col
}

fn render_changes_tab(view: &ChatView, cx: &mut Context<ChatView>) -> gpui_kit::Div {
    let p = palette(cx);
    let pending_ids: Vec<String> = view
        .persisted_changes
        .iter()
        .filter(|c| {
            matches!(
                c.status,
                FileChangeStatus::Pending | FileChangeStatus::Conflict
            )
        })
        .map(|c| c.id.clone())
        .collect();

    let mut col = v_col().gap(px(8.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(
                crate::ui::tr(cx).chat().inspector_modified_files(),
                cx,
            ))
            .children((!pending_ids.is_empty()).then(|| {
                let ids_all = pending_ids.clone();
                btn_ghost(
                    "chat-ch-keep-all",
                    crate::ui::tr(cx).chat().changes_keep_all(),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.keep_changes(ids_all.clone(), cx)))
            })),
    );

    if view.persisted_changes.is_empty() && view.thread.file_changes.is_empty() {
        return col.child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                crate::ui::tr(cx)
                    .chat()
                    .inspector_no_file_edits_recorded_in(),
            ),
        );
    }

    for ch in &view.persisted_changes {
        let kind_lbl = match ch.kind {
            FileChangeKind::Create => crate::ui::tr(cx).chat().inspector_new(),
            FileChangeKind::Edit => crate::ui::tr(cx).common().edit(),
        };
        let status_tone = match ch.status {
            FileChangeStatus::Pending => Tone::Warning,
            FileChangeStatus::Kept => Tone::Success,
            FileChangeStatus::Undone => Tone::Neutral,
            FileChangeStatus::Conflict => Tone::Danger,
            FileChangeStatus::Applying => Tone::Accent,
        };
        let cid_diff = ch.id.clone();
        let path_diff = ch.path.clone();
        let cid_keep = ch.id.clone();
        let cid_undo = ch.id.clone();
        let actionable = matches!(
            ch.status,
            FileChangeStatus::Pending | FileChangeStatus::Conflict
        );

        col = col.child(
            card(cx).p(px(8.0)).child(
                v_col()
                    .gap(px(5.0))
                    .child(
                        h_row()
                            .justify_between()
                            .gap(px(6.0))
                            .child(
                                h_row()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(5.0))
                                    .child(icon_xs(UiIcon::FileDiff))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_xs()
                                            .text_color(to_hsla(p.ink))
                                            .truncate()
                                            .child(SharedString::from(ch.path.clone())),
                                    ),
                            )
                            .child(badge(kind_lbl, Tone::Neutral, cx))
                            .child(badge(format!("{:?}", ch.status), status_tone, cx)),
                    )
                    .child(
                        h_row()
                            .justify_between()
                            .child(
                                h_row()
                                    .gap(px(8.0))
                                    .text_xs()
                                    .child(
                                        div()
                                            .text_color(to_hsla(p.ok))
                                            .child(format!("+{}", ch.added)),
                                    )
                                    .child(
                                        div()
                                            .text_color(to_hsla(p.bad))
                                            .child(format!("-{}", ch.removed)),
                                    ),
                            )
                            .child(
                                h_row()
                                    .gap(px(4.0))
                                    .child(
                                        btn_ghost(
                                            SharedString::from(format!("ch-df-{}", ch.id)),
                                            "Diff",
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.open_change_diff(
                                                    cid_diff.clone(),
                                                    path_diff.clone(),
                                                    cx,
                                                );
                                            }),
                                        ),
                                    )
                                    .children(actionable.then(|| {
                                        btn_ghost(
                                            SharedString::from(format!("ch-kp-{}", ch.id)),
                                            crate::ui::tr(cx).chat().changes_keep(),
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.keep_changes(vec![cid_keep.clone()], cx);
                                            }),
                                        )
                                    }))
                                    .children(actionable.then(|| {
                                        btn_ghost(
                                            SharedString::from(format!("ch-ud-{}", ch.id)),
                                            crate::ui::tr(cx).chat().inspector_undo(),
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.undo_change(cid_undo.clone(), cx);
                                            }),
                                        )
                                    })),
                            ),
                    ),
            ),
        );
    }
    col
}
