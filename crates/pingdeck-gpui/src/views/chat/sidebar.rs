//! Left thread-list sidebar for AI Chat: Time / Running Board / Trash switcher,
//! live search, archived toggle, multi-chat attention lanes, and quick row actions.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::agent::model::{AgentSessionPatch, AgentSessionSummary};
use pingdeck_desktop::core::app::current_time_ms;

use crate::ui::{
    Anchor, ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_icon, btn_primary,
    choice_chip, empty_inline, h_row, icon_xs, list_row, master_pane, menu_item, palette,
    pane_header, popover, search_input, status_indicator, to_hsla, v_col,
};

use super::ChatView;
use super::sidebar_board::{group_label, render_board_list, render_trash_list};
use super::types::{ChatModal, ChatSidebarMode};

pub fn render_sidebar(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let attention_n = view.pending_attention_count();

    let mode_strip = h_row()
        .gap(px(4.0))
        .child(
            choice_chip(
                "chat-mode-time",
                if zh { "时间" } else { "Time" },
                view.sidebar_mode == ChatSidebarMode::Time,
                cx,
            )
            .on_click(
                cx.listener(|this, _, _, cx| this.set_sidebar_mode(ChatSidebarMode::Time, cx)),
            ),
        )
        .child(
            choice_chip(
                "chat-mode-board",
                if attention_n > 0 {
                    if zh {
                        format!("运行看板 ({attention_n})")
                    } else {
                        format!("Board ({attention_n})")
                    }
                } else if zh {
                    "运行看板".to_owned()
                } else {
                    "Board".to_owned()
                },
                view.sidebar_mode == ChatSidebarMode::Board,
                cx,
            )
            .on_click(
                cx.listener(|this, _, _, cx| this.set_sidebar_mode(ChatSidebarMode::Board, cx)),
            ),
        )
        .child(
            choice_chip(
                "chat-mode-trash",
                if zh { "最近删除" } else { "Trash" },
                view.sidebar_mode == ChatSidebarMode::Trash,
                cx,
            )
            .on_click(
                cx.listener(|this, _, _, cx| this.set_sidebar_mode(ChatSidebarMode::Trash, cx)),
            ),
        );

    let below = v_col()
        .w_full()
        .gap(px(6.0))
        .child(mode_strip)
        .child(search_input(&view.search_input, cx));

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
                        .on_click(cx.listener(|this, _, _, cx| this.create_session(cx))),
                )
                .into_any_element(),
        ),
        Some(below.into_any_element()),
        cx,
    );

    let list_body = match view.sidebar_mode {
        ChatSidebarMode::Time => render_time_list(view, zh, cx),
        ChatSidebarMode::Board => render_board_list(view, zh, cx),
        ChatSidebarMode::Trash => render_trash_list(view, zh, cx),
    };

    let footer = view.board.as_ref().map(|b| {
        let tok_total = b.tokens_today.input + b.tokens_today.output;
        let cost_txt = if b.cost_today.is_empty() {
            "—".to_owned()
        } else {
            b.cost_today
                .iter()
                .map(|c| format!("{} {:.3}", c.currency, c.cost_micro as f64 / 1_000_000.0))
                .collect::<Vec<_>>()
                .join(" + ")
        };
        h_row()
            .w_full()
            .justify_between()
            .px(px(10.0))
            .py(px(6.0))
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(if zh {
                format!("今日 {tok_total} tok")
            } else {
                format!("Today {tok_total} tok")
            })
            .child(cost_txt)
            .into_any_element()
    });

    master_pane(header, list_body, footer, cx)
}

fn render_time_list(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> AnyElement {
    let query = view.search_input.read(cx).value().trim().to_lowercase();
    let now_ms = current_time_ms();
    let needs_ids: std::collections::HashSet<&str> = view
        .board
        .as_ref()
        .map(|b| {
            b.needs_you
                .iter()
                .map(|r| r.run.session_id.as_str())
                .collect()
        })
        .unwrap_or_default();

    let filtered: Vec<&AgentSessionSummary> = view
        .sessions
        .iter()
        .filter(|s| {
            query.is_empty()
                || s.title.to_lowercase().contains(&query)
                || s.preview.to_lowercase().contains(&query)
        })
        .collect();

    let mut needs_you = Vec::new();
    let mut pinned = Vec::new();
    let mut recent = Vec::new();

    for s in filtered {
        if needs_ids.contains(s.id.as_str())
            || (!view.thread.pending_approvals.is_empty() && s.id == view.thread.session_id)
        {
            needs_you.push(s);
        } else if s.pinned {
            pinned.push(s);
        } else {
            recent.push(s);
        }
    }

    let mut list = v_col().w_full().gap(px(2.0)).p(px(8.0));
    if !needs_you.is_empty() {
        list = list.child(group_label(
            if zh { "待你处理" } else { "Needs you" },
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
                "点击上方「新建」开始协作对话。"
            } else {
                "Click New above to start a conversation."
            })),
            None,
            cx,
        ));
    }
    div()
        .id("chat-time-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(list)
        .into_any_element()
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
    let rel = format_relative_time(s.updated_at_ms, now_ms, zh);
    let chat_ent = cx.entity().clone();
    let sid_m = s.id.clone();
    let title_m = title.clone();
    let is_pinned = s.pinned;
    let is_archived = s.archived;
    let is_running = s.running;

    let menu = popover(SharedString::from(format!("chat-rmenu-{}", s.id)))
        .anchor(Anchor::TopRight)
        .trigger(btn_icon(
            SharedString::from(format!("chat-rmenu-tr-{}", s.id)),
            UiIcon::Ellipsis,
        ))
        .content(move |_, _, pop_cx| {
            let (e1, e2, e3, e4) = (
                chat_ent.clone(),
                chat_ent.clone(),
                chat_ent.clone(),
                chat_ent.clone(),
            );
            let (p1, p2, p3, p4) = (
                pop_cx.entity().clone(),
                pop_cx.entity().clone(),
                pop_cx.entity().clone(),
                pop_cx.entity().clone(),
            );
            let (s1, s2, s3, s4) = (sid_m.clone(), sid_m.clone(), sid_m.clone(), sid_m.clone());
            let (t1, t4) = (title_m.clone(), title_m.clone());
            v_col()
                .w(px(164.0))
                .gap(px(2.0))
                .child(menu_item(
                    "chat-rm-ren",
                    UiIcon::Pencil,
                    if zh { "重命名…" } else { "Rename…" },
                    false,
                    pop_cx,
                    move |_, w, cx| {
                        e1.update(cx, |this, cx| {
                            this.modal_input
                                .update(cx, |st, cx| st.set_value(t1.clone(), w, cx));
                            this.active_modal = Some(ChatModal::RenameSession {
                                session_id: s1.clone(),
                            });
                            cx.notify();
                        });
                        p1.update(cx, |s, cx| s.dismiss(w, cx));
                    },
                ))
                .child(menu_item(
                    "chat-rm-pin",
                    if is_pinned {
                        UiIcon::PinOff
                    } else {
                        UiIcon::Pin
                    },
                    if is_pinned {
                        if zh { "取消置顶" } else { "Unpin" }
                    } else if zh {
                        "置顶"
                    } else {
                        "Pin"
                    },
                    false,
                    pop_cx,
                    move |_, w, cx| {
                        e2.update(cx, |this, cx| {
                            this.patch_session_by_id(
                                s2.clone(),
                                AgentSessionPatch {
                                    pinned: Some(!is_pinned),
                                    ..Default::default()
                                },
                                cx,
                            );
                        });
                        p2.update(cx, |s, cx| s.dismiss(w, cx));
                    },
                ))
                .child(menu_item(
                    "chat-rm-arch",
                    UiIcon::Archive,
                    if is_archived {
                        if zh { "取消归档" } else { "Unarchive" }
                    } else if zh {
                        "归档"
                    } else {
                        "Archive"
                    },
                    false,
                    pop_cx,
                    move |_, w, cx| {
                        e3.update(cx, |this, cx| {
                            this.patch_session_by_id(
                                s3.clone(),
                                AgentSessionPatch {
                                    archived: Some(!is_archived),
                                    ..Default::default()
                                },
                                cx,
                            );
                        });
                        p3.update(cx, |s, cx| s.dismiss(w, cx));
                    },
                ))
                .child(menu_item(
                    "chat-rm-del",
                    UiIcon::Trash,
                    if zh { "删除" } else { "Delete" },
                    true,
                    pop_cx,
                    move |_, w, cx| {
                        e4.update(cx, |this, cx| {
                            if is_running {
                                this.active_modal = Some(ChatModal::DeleteRunningConfirm {
                                    session_id: s4.clone(),
                                    title: t4.clone(),
                                });
                                cx.notify();
                            } else {
                                this.delete_session_by_id(s4.clone(), cx);
                            }
                        });
                        p4.update(cx, |s, cx| s.dismiss(w, cx));
                    },
                ))
        });

    list_row(
        SharedString::from(format!("chat-s-{id}")),
        sel,
        cx,
        cx.listener(move |this, _, _, cx| this.open_session(id.clone(), cx)),
    )
    .items_center()
    .py(px(6.0))
    .gap(px(4.0))
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
                    .flex_shrink_0()
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
    .children(
        s.archived
            .then(|| badge(if zh { "归档" } else { "Arch" }, Tone::Neutral, cx)),
    )
    .child(
        div()
            .flex_shrink_0()
            .text_size(px(11.0))
            .text_color(to_hsla(p.faint))
            .child(SharedString::from(rel)),
    )
    .child(menu)
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
