//! Unified Todo progress strip docked above the composer (`docs/design.md` §3.3, `.overhaul/chat-redesign-spec.md` §4 & §8.1).
//!
//! Single speaking location for todos across the entire chat UI (`todo_write` calls are silenced
//! in the thread stream): one summary bar ("任务 3/7 · 当前：…") with failed-task badge, active
//! step/elapsed counter on the right, and trailing chevron, expanding to one status row per task.

use std::time::Instant;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::model::AgentTodoStatus;
use spanbay_desktop::state::{GraphNodeStatus, map_todo_status};

use crate::ui::{
    Tone, UiIcon, badge, h_row, icon_xs, mono_font_family, palette, status_indicator, status_row,
    status_visual, to_hsla, v_col,
};

use super::ChatView;

pub fn render_todo_panel(view: &ChatView, cx: &mut Context<ChatView>) -> Option<impl IntoElement> {
    let todos = &view.thread.todos;
    if todos.is_empty() {
        return None;
    }
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let run_active = view.is_run_active();
    let done = todos
        .iter()
        .filter(|t| t.status == AgentTodoStatus::Completed)
        .count();
    let total = todos
        .iter()
        .filter(|t| t.status != AgentTodoStatus::Cancelled)
        .count();
    let failed_count = todos
        .iter()
        .filter(|t| t.status == AgentTodoStatus::Failed)
        .count();

    let mut summary = ch.todos_summary(done, total);
    if run_active
        && let Some(current) = todos
            .iter()
            .find(|t| t.status == AgentTodoStatus::InProgress)
    {
        summary.push_str(" · ");
        summary.push_str(&ch.todos_current(&current.content));
    }
    let overall = if todos
        .iter()
        .any(|t| t.status == AgentTodoStatus::InProgress)
    {
        if run_active {
            GraphNodeStatus::Running
        } else {
            GraphNodeStatus::Interrupted
        }
    } else if failed_count > 0 {
        GraphNodeStatus::Failed
    } else if todos.iter().any(|t| t.status == AgentTodoStatus::Blocked) {
        GraphNodeStatus::Blocked
    } else if total == 0 {
        GraphNodeStatus::Cancelled
    } else if done == total {
        GraphNodeStatus::Completed
    } else {
        GraphNodeStatus::Pending
    };

    let step_elapsed = run_active.then(|| {
        let steps = view
            .thread
            .active_run
            .as_ref()
            .map_or(1, |r| r.iterations.max(1));
        let secs = view
            .run_started_at
            .map_or(0, |s| Instant::now().saturating_duration_since(s).as_secs());
        ch.composer_step_elapsed(steps, secs)
    });

    let open = view.todos_open;
    let mono = mono_font_family(cx);
    let mut panel = v_col().w_full().child(
        h_row()
            .id("chat-todos-toggle")
            .w_full()
            .justify_between()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(5.0))
            .cursor_pointer()
            .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
            .on_click(cx.listener(|this, _, _, cx| {
                this.todos_open = !this.todos_open;
                cx.notify();
            }))
            .child(
                h_row()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.0))
                    .child(status_indicator(status_visual(overall).0, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink_2))
                            .child(SharedString::from(summary)),
                    )
                    .children(
                        (failed_count > 0)
                            .then(|| badge(ch.trace_failed_count(failed_count), Tone::Danger, cx)),
                    ),
            )
            .child(
                h_row()
                    .flex_shrink_0()
                    .gap(px(6.0))
                    .children(step_elapsed.map(|se| {
                        div()
                            .font_family(mono)
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.faint))
                            .child(SharedString::from(se))
                    }))
                    .child(div().text_color(to_hsla(p.faint)).child(icon_xs(if open {
                        UiIcon::ChevronDown
                    } else {
                        UiIcon::ChevronRight
                    }))),
            ),
    );

    if open {
        let mut rows = v_col()
            .w_full()
            .pb(px(4.0))
            .border_t_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)));
        for (ix, todo) in todos.iter().enumerate() {
            let row_status = if todo.status == AgentTodoStatus::InProgress && !run_active {
                GraphNodeStatus::Interrupted
            } else {
                map_todo_status(todo.status)
            };
            rows = rows.child(status_row(
                SharedString::from(format!("chat-todo-{ix}")),
                row_status,
                todo.content.clone(),
                None,
                None,
                cx,
            ));
            if let Some(err) = todo
                .error
                .as_ref()
                .filter(|_| todo.status == AgentTodoStatus::Failed)
            {
                rows = rows.child(
                    div()
                        .pl(px(30.0))
                        .pb(px(2.0))
                        .text_xs()
                        .text_color(to_hsla(p.bad))
                        .child(SharedString::from(err.clone())),
                );
            }
        }
        panel = panel.child(rows);
    }
    Some(panel)
}
