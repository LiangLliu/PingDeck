//! Shared sidebar section heading (`group_label`) and Recently Deleted (`最近删除`) list.
//!
//! The Run Monitor itself moved to the full-canvas center view (`run_monitor.rs`).

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::model::AgentRunStatus;

use crate::ui::{
    Disableable as _, ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_ghost,
    btn_secondary, empty_inline, h_row, icon_xs, palette, section_heading, status_indicator,
    to_hsla, v_col,
};

use super::ChatView;
use super::types::ChatSidebarMode;

/// Compact Run Monitor status entry bar pinned at the top of the Chat sidebar.
pub fn render_monitor_entry(
    view: &ChatView,
    active: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let (running_n, max_slots, waiting_n, unseen_fail_n, recent_n) =
        view.board.as_ref().map_or((0, 3, 0, 0, 0), |b| {
            let run_n = b.running.len();
            let max_s = usize::from(b.limits.max_concurrent_runs.max(1));
            let wait_n = b
                .needs_you
                .iter()
                .filter(|r| r.run.status.is_active() || r.approval_timed_out)
                .count();
            let fail_n = b
                .needs_you
                .iter()
                .filter(|r| {
                    matches!(
                        r.run.status,
                        AgentRunStatus::Failed | AgentRunStatus::Interrupted
                    )
                })
                .count();
            (
                run_n,
                max_s,
                wait_n,
                fail_n,
                b.done.len() + b.needs_you.len(),
            )
        });

    let lead_shape = if waiting_n > 0 {
        Some(StatusShape::Warn)
    } else if unseen_fail_n > 0 {
        Some(StatusShape::Bad)
    } else if running_n > 0 {
        Some(StatusShape::Run)
    } else {
        None
    };

    let bg = if active {
        to_hsla(p.accent_soft)
    } else {
        to_hsla(p.surface)
    };
    let border = if active {
        to_hsla(p.accent_line.composite_over(p.accent_soft))
    } else if waiting_n > 0 {
        to_hsla(p.warn_line.composite_over(p.surface))
    } else if unseen_fail_n > 0 {
        to_hsla(p.bad_line.composite_over(p.surface))
    } else {
        to_hsla(p.line.composite_over(p.surface))
    };

    let mut pills = h_row().gap(px(4.0)).flex_wrap();
    if running_n > 0 {
        pills = pills.child(badge(
            ch.sidebar_monitor_live(running_n, max_slots),
            Tone::Accent,
            cx,
        ));
    }
    if waiting_n > 0 {
        pills = pills.child(badge(ch.sidebar_monitor_attn(waiting_n), Tone::Warning, cx));
    }
    if unseen_fail_n > 0 {
        pills = pills.child(badge(
            ch.sidebar_monitor_fail(unseen_fail_n),
            Tone::Danger,
            cx,
        ));
    }
    if running_n == 0 && waiting_n == 0 && unseen_fail_n == 0 {
        pills = pills.child(
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(ch.sidebar_monitor_idle(recent_n))),
        );
    }

    div()
        .id("chat-open-monitor")
        .w_full()
        .px(px(9.0))
        .py(px(7.0))
        .rounded(px(8.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .cursor_pointer()
        .hover(|s| {
            if active {
                s
            } else {
                s.bg(to_hsla(p.hover.composite_over(p.surface)))
            }
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            if active {
                this.set_sidebar_mode(ChatSidebarMode::Time, cx);
            } else {
                this.open_run_monitor(None, cx);
            }
        }))
        .child(
            v_col()
                .w_full()
                .gap(px(4.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .text_color(to_hsla(if active {
                                            p.accent
                                        } else {
                                            p.ink_2
                                        }))
                                        .child(icon_xs(UiIcon::Activity)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(if active {
                                            p.accent_ink
                                        } else {
                                            p.ink
                                        }))
                                        .child(ch.monitor_title()),
                                ),
                        )
                        .children(lead_shape.map(|s| status_indicator(s, cx))),
                )
                .child(pills),
        )
}

pub fn group_label(
    title: &'static str,
    count: Option<usize>,
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
        .children(count.map(|c| {
            div()
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(SharedString::from(c.to_string()))
        }))
}

pub fn render_trash_list(view: &ChatView, cx: &mut Context<ChatView>) -> AnyElement {
    let p = palette(cx);
    let mut list = v_col().w_full().gap(px(4.0)).p(px(8.0));
    if view.trash_sessions.is_empty() {
        list = list.child(empty_inline(
            crate::ui::tr(cx).chat().sidebar_board_trash_is_empty(),
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
                            crate::ui::tr(cx).chat().pane_deleted_restore(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.restore_session(sid.clone(), cx);
                        })),
                    ),
            );
        }
        if view.trash_next_cursor.is_some() {
            list = list.child(
                h_row().w_full().justify_center().pt(px(6.0)).child(
                    btn_secondary(
                        "chat-trash-load-more",
                        if view.trash_loading_more {
                            crate::ui::tr(cx).common().loading()
                        } else {
                            crate::ui::tr(cx).chat().sidebar_board_load_more_trash()
                        },
                    )
                    .disabled(view.trash_loading_more)
                    .on_click(cx.listener(|this, _, _, cx| this.load_more_trash(cx))),
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
