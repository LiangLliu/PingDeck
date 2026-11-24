//! Left thread-list sidebar for AI Chat: search filter, archived toggle, categorized sections
//! (`Needs you`, `Pinned`, `Recent`), live run indicators, and preview metadata.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::model::AgentSessionSummary;
use pingdeck_desktop::core::app::current_time_ms;

use crate::ui::{
    ScrollableElement as _, StatusShape, UiIcon, btn_icon, btn_primary, empty_inline, h_row,
    icon_xs, list_row, master_pane, palette, pane_header, search_input, section_heading,
    status_indicator, to_hsla, v_col,
};

use super::ChatView;

pub fn render_sidebar(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let query = view.search_input.read(cx).value().trim().to_lowercase();
    let now_ms = current_time_ms();
    let active_needs_attention = !view.thread.pending_approvals.is_empty();

    let filtered: Vec<&AgentSessionSummary> = view
        .sessions
        .iter()
        .filter(|s| {
            if query.is_empty() {
                return true;
            }
            s.title.to_lowercase().contains(&query) || s.preview.to_lowercase().contains(&query)
        })
        .collect();

    let mut needs_you = Vec::new();
    let mut pinned = Vec::new();
    let mut recent = Vec::new();

    for s in filtered {
        if active_needs_attention && s.id == view.thread.session_id {
            needs_you.push(s);
        } else if s.pinned {
            pinned.push(s);
        } else {
            recent.push(s);
        }
    }

    let mut list = v_col()
        .size_full()
        .gap(px(2.0))
        .p(px(8.0))
        .overflow_y_scrollbar();

    if !needs_you.is_empty() {
        list = list.child(group_label(
            if zh { "待处理" } else { "Needs you" },
            needs_you.len(),
            Some(StatusShape::Warn),
            cx,
        ));
        for s in needs_you {
            list = list.child(render_session_row(view, s, now_ms, true, zh, cx));
        }
    }

    if !pinned.is_empty() {
        list = list.child(group_label(
            if zh { "已置顶" } else { "Pinned" },
            pinned.len(),
            None,
            cx,
        ));
        for s in pinned {
            list = list.child(render_session_row(view, s, now_ms, false, zh, cx));
        }
    }

    if !recent.is_empty() {
        list = list.child(group_label(
            if zh { "最近对话" } else { "Recent" },
            recent.len(),
            None,
            cx,
        ));
        for s in recent {
            list = list.child(render_session_row(view, s, now_ms, false, zh, cx));
        }
    }

    if view.sessions.is_empty() {
        list = list.child(empty_inline(
            if zh {
                "还没有对话"
            } else {
                "No conversations yet"
            },
            Some(SharedString::from(if zh {
                "点击上方「新建」开始一段与智能体的协作对话。"
            } else {
                "Click New above to start a conversation with the agent."
            })),
            None,
            cx,
        ));
    }

    let _ = p;
    let header = pane_header(
        if zh { "对话" } else { "Chats" },
        Some(view.sessions.len()),
        Some(
            h_row()
                .gap(px(4.0))
                .child(
                    btn_icon("chat-toggle-arch", UiIcon::Archive)
                        .tooltip(if zh {
                            "显示/隐藏已归档"
                        } else {
                            "Toggle archived"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_archived = !this.show_archived;
                            this.reload_sessions(cx);
                        })),
                )
                .child(
                    btn_primary("chat-new", if zh { "新建" } else { "New" })
                        .icon(icon_xs(UiIcon::MessageSquarePlus))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.create_session(cx);
                        })),
                )
                .into_any_element(),
        ),
        Some(search_input(&view.search_input, cx).into_any_element()),
        cx,
    );

    master_pane(header, list, None, cx)
}

fn group_label(
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

fn render_session_row(
    view: &ChatView,
    s: &AgentSessionSummary,
    now_ms: i64,
    needs_approval: bool,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let id = s.id.clone();
    let sel = view.thread.session_id == s.id;
    let title = if s.title.trim().is_empty() {
        if zh { "新对话" } else { "New chat" }.to_owned()
    } else {
        s.title.clone()
    };
    let subtitle = if !s.preview.trim().is_empty() {
        s.preview.trim().to_owned()
    } else if let Some(target) = &s.target {
        target.label()
    } else if zh {
        "空对话".into()
    } else {
        "Empty thread".into()
    };
    let rel = format_relative_time(s.updated_at_ms, now_ms, zh);

    list_row(
        SharedString::from(format!("chat-s-{id}")),
        sel,
        cx,
        cx.listener(move |this, _, _, cx| this.open_session(id.clone(), cx)),
    )
    .items_start()
    .py(px(7.0))
    .child(
        v_col()
            .flex_1()
            .min_w_0()
            .gap(px(3.0))
            .child(
                h_row()
                    .justify_between()
                    .gap(px(6.0))
                    .child(
                        h_row()
                            .flex_1()
                            .min_w_0()
                            .gap(px(5.0))
                            .children(if needs_approval {
                                Some(status_indicator(StatusShape::Warn, cx))
                            } else if s.running {
                                Some(status_indicator(StatusShape::Run, cx))
                            } else {
                                None
                            })
                            .children(s.pinned.then(|| {
                                div()
                                    .text_color(to_hsla(p.muted))
                                    .child(icon_xs(UiIcon::Pin))
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .font_weight(if sel {
                                        gpui_kit::FontWeight::SEMIBOLD
                                    } else {
                                        gpui_kit::FontWeight::MEDIUM
                                    })
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(title)),
                            ),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(to_hsla(p.faint))
                            .child(SharedString::from(rel)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .truncate()
                    .child(SharedString::from(subtitle)),
            ),
    )
}

fn format_relative_time(ts_ms: i64, now_ms: i64, zh: bool) -> String {
    let diff_s = ((now_ms - ts_ms).max(0)) / 1000;
    if diff_s < 60 {
        if zh { "刚刚".into() } else { "now".into() }
    } else if diff_s < 3600 {
        let m = diff_s / 60;
        if zh {
            format!("{m}分")
        } else {
            format!("{m}m")
        }
    } else if diff_s < 86_400 {
        let h = diff_s / 3600;
        if zh {
            format!("{h}时")
        } else {
            format!("{h}h")
        }
    } else {
        let d = diff_s / 86_400;
        if zh {
            format!("{d}天")
        } else {
            format!("{d}d")
        }
    }
}
