//! Running Board (`运行看板`) and Recently Deleted (`最近删除`) sidebar views for AI Chat.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::AgentBoardRow;

use crate::ui::{
    ScrollableElement as _, StatusShape, btn_ghost, empty_inline, h_row, list_row, palette,
    section_heading, status_indicator, to_hsla, v_col,
};

use super::ChatView;

pub fn group_label(
    title: &'static str,
    count: usize,
    shape: Option<StatusShape>,
    cx: &gpui_kit::App,
) -> gpui_kit::Div {
    let p = palette(cx);
    h_row()
        .justify_between()
        .px(px(8.0))
        .pt(px(8.0))
        .pb(px(3.0))
        .child(
            h_row()
                .gap(px(5.0))
                .children(shape.map(|s| status_indicator(s, cx)))
                .child(section_heading(title, cx)),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(SharedString::from(count.to_string())),
        )
}

pub fn render_board_list(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> AnyElement {
    let mut list = v_col().w_full().gap(px(4.0)).p(px(8.0));
    let Some(board) = &view.board else {
        return empty_inline(
            if zh {
                "正在加载运行看板…"
            } else {
                "Loading board…"
            },
            None,
            None,
            cx,
        )
        .into_any_element();
    };
    if !board.needs_you.is_empty() {
        list = list.child(group_label(
            if zh { "待确认" } else { "Needs you" },
            board.needs_you.len(),
            Some(StatusShape::Warn),
            cx,
        ));
        for row in &board.needs_you {
            list = list.child(render_board_row(view, row, StatusShape::Warn, zh, cx));
        }
    }
    if !board.running.is_empty() {
        list = list.child(group_label(
            if zh { "运行中" } else { "Running" },
            board.running.len(),
            Some(StatusShape::Run),
            cx,
        ));
        for row in &board.running {
            list = list.child(render_board_row(view, row, StatusShape::Run, zh, cx));
        }
    }
    if !board.done.is_empty() {
        list = list.child(group_label(
            if zh { "24h 已完成" } else { "Done (24h)" },
            board.done.len(),
            Some(StatusShape::Ok),
            cx,
        ));
        for row in &board.done {
            list = list.child(render_board_row(view, row, StatusShape::Ok, zh, cx));
        }
    }
    if board.needs_you.is_empty() && board.running.is_empty() && board.done.is_empty() {
        list = list.child(empty_inline(
            if zh {
                "当前无活跃或近期运行"
            } else {
                "No active or recent runs"
            },
            None,
            None,
            cx,
        ));
    }
    div()
        .id("chat-board-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(list)
        .into_any_element()
}

fn render_board_row(
    view: &ChatView,
    row: &AgentBoardRow,
    shape: StatusShape,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let p = palette(cx);
    let sid = row.run.session_id.clone();
    let sel = view.thread.session_id == sid;
    let sub = row
        .pending_approval
        .as_ref()
        .map(|a| a.summary.clone())
        .or_else(|| row.run.status_detail.clone())
        .unwrap_or_else(|| view.format_raw_model_label(&row.run.model_label));

    list_row(
        SharedString::from(format!("chat-b-{}", row.run.run_id)),
        sel,
        cx,
        cx.listener(move |this, _, _, cx| this.open_session(sid.clone(), cx)),
    )
    .items_start()
    .py(px(7.0))
    .child(
        v_col()
            .flex_1()
            .min_w_0()
            .gap(px(2.0))
            .child(
                h_row()
                    .gap(px(5.0))
                    .child(status_indicator(shape, cx))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(row.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.faint))
                            .child(format!("{}s", row.awake_ms / 1000)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .truncate()
                    .child(sub),
            )
            .children((row.plan_total > 0).then(|| {
                div()
                    .text_xs()
                    .text_color(to_hsla(p.accent_ink))
                    .child(if zh {
                        format!("计划 {}/{}", row.plan_done, row.plan_total)
                    } else {
                        format!("Plan {}/{}", row.plan_done, row.plan_total)
                    })
            })),
    )
    .into_any_element()
}

pub fn render_trash_list(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> AnyElement {
    let p = palette(cx);
    let mut list = v_col().w_full().gap(px(4.0)).p(px(8.0));
    if view.trash_sessions.is_empty() {
        list = list.child(empty_inline(
            if zh {
                "最近删除为空"
            } else {
                "Trash is empty"
            },
            None,
            None,
            cx,
        ));
    } else {
        for s in &view.trash_sessions {
            let sid = s.id.clone();
            list = list.child(
                h_row()
                    .justify_between()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.surface))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(s.title.clone()),
                    )
                    .child(
                        btn_ghost(
                            SharedString::from(format!("chat-tr-res-{}", s.id)),
                            if zh { "恢复" } else { "Restore" },
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.restore_session(sid.clone(), cx);
                        })),
                    ),
            );
        }
    }
    div()
        .id("chat-trash-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(list)
        .into_any_element()
}
