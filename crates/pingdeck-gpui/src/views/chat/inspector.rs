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
use super::{ChatView, DetailsTab};

pub fn render_inspector(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let chat_entity = cx.entity().clone();
    let changes_count = if view.persisted_changes.is_empty() {
        view.thread.file_changes.len()
    } else {
        view.persisted_changes.len()
    };
    let changes_lbl = if changes_count > 0 {
        if zh {
            format!("变更 ({changes_count})")
        } else {
            format!("Changes ({changes_count})")
        }
    } else if zh {
        "变更".to_owned()
    } else {
        "Changes".to_owned()
    };

    let tabs = tab_bar_underline("chat-details-tabs")
        .selected_index(view.details_tab.index())
        .child(tab_item(if zh { "进度" } else { "Progress" }))
        .child(tab_item(changes_lbl))
        .child(tab_item(if zh { "上下文" } else { "Context" }))
        .child(tab_item(if zh { "选项" } else { "Options" }))
        .on_click(move |idx, _, cx| {
            let next = DetailsTab::from_index(*idx);
            chat_entity.update(cx, |this, cx| {
                this.details_tab = next;
                cx.notify();
            });
        });

    let body = match view.details_tab {
        DetailsTab::Progress => render_progress_tab(view, zh, cx).into_any_element(),
        DetailsTab::Changes => render_changes_tab(view, zh, cx).into_any_element(),
        DetailsTab::Context => render_context_tab(view, zh, cx).into_any_element(),
        DetailsTab::Options => render_options_tab(view, zh, cx).into_any_element(),
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
                        .child(if zh { "对话检视器" } else { "Inspector" }),
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

fn render_progress_tab(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> gpui_kit::Div {
    let p = palette(cx);
    let todos = &view.thread.todos;
    let total = todos.len();
    let done = todos
        .iter()
        .filter(|t| t.status == AgentTodoStatus::Completed)
        .count();

    let mut col = v_col().gap(px(12.0));

    if let Some(run) = &view.thread.active_run {
        col =
            col.child(
                card(cx).p(px(10.0)).child(
                    v_col()
                        .gap(px(4.0))
                        .child(
                            h_row()
                                .justify_between()
                                .child(section_heading(
                                    if zh { "当前运行" } else { "Active run" },
                                    cx,
                                ))
                                .child(badge(format!("{:?}", run.status), Tone::Accent, cx)),
                        )
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(
                            SharedString::from(format!(
                                "{} · {} step(s)",
                                view.format_raw_model_label(&run.model_label),
                                run.iterations
                            )),
                        )),
                ),
            );
    }

    col = col.child(
        h_row()
            .justify_between()
            .child(section_heading(if zh { "任务清单" } else { "Tasks" }, cx))
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
        col = col.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "智能体制定多步计划后，任务进度会显示在这里。"
        } else {
            "Multi-step task progress appears here once the agent creates a plan."
        }));
    }

    if !view.thread.subagents.is_empty() {
        col = col.child(div().pt(px(4.0)).child(section_heading(
            if zh { "子智能体" } else { "Sub-agents" },
            cx,
        )));
        for sub in view.thread.subagents.values() {
            col = col.child(
                card(cx).p(px(8.0)).child(
                    h_row()
                        .justify_between()
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(status_indicator(
                                    if sub.finished {
                                        StatusShape::Ok
                                    } else {
                                        StatusShape::Run
                                    },
                                    cx,
                                ))
                                .child(div().text_xs().text_color(to_hsla(p.ink)).child(
                                    SharedString::from(
                                        view.format_raw_model_label(&sub.summary.model_label),
                                    ),
                                )),
                        )
                        .child(badge(
                            format!("{} tools", sub.active_tools),
                            Tone::Neutral,
                            cx,
                        )),
                ),
            );
        }
    }

    if !view.processes.is_empty() {
        col = col.child(div().pt(px(4.0)).child(section_heading(
            if zh {
                "后台进程"
            } else {
                "Background Processes"
            },
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
                                if zh { "终止" } else { "Kill" },
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

fn render_changes_tab(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> gpui_kit::Div {
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
                if zh { "文件改动" } else { "Modified files" },
                cx,
            ))
            .children((!pending_ids.is_empty()).then(|| {
                let ids_all = pending_ids.clone();
                btn_ghost("chat-ch-keep-all", if zh { "全部保留" } else { "Keep all" }).on_click(
                    cx.listener(move |this, _, _, cx| this.keep_changes(ids_all.clone(), cx)),
                )
            })),
    );

    if view.persisted_changes.is_empty() && view.thread.file_changes.is_empty() {
        return col.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "本对话暂无文件改动。"
        } else {
            "No file edits recorded in this chat."
        }));
    }

    for ch in &view.persisted_changes {
        let kind_lbl = match ch.kind {
            FileChangeKind::Create => {
                if zh {
                    "新建"
                } else {
                    "New"
                }
            }
            FileChangeKind::Edit => {
                if zh {
                    "修改"
                } else {
                    "Edit"
                }
            }
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
                                            if zh { "保留" } else { "Keep" },
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
                                            if zh { "撤回" } else { "Undo" },
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
