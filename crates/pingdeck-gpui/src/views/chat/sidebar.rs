//! Left thread-list sidebar for AI Chat: live Run Monitor status entry bar, live search,
//! archived & trash toggles, multi-chat attention/failure status indicators, and quick row actions.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::agent::model::{
    AgentRunStatus, AgentSessionPatch, AgentSessionSummary,
};
use pingdeck_desktop::core::app::current_time_ms;

use crate::ui::{
    Anchor, Disableable as _, ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_ghost,
    btn_icon, btn_primary, btn_secondary, empty_inline, h_row, icon_xs, list_row, master_pane,
    menu_item, palette, pane_header, popover, search_input, status_indicator, to_hsla, v_col,
};

use crate::views::runs::status::format_tokens_compact;

use super::ChatView;
use super::sidebar_board::{group_label, render_trash_list};
use super::types::{ChatModal, ChatSidebarMode};

pub fn render_sidebar(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let in_trash = view.sidebar_mode == ChatSidebarMode::Trash;

    let below = if in_trash {
        h_row()
            .w_full()
            .justify_between()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink_2))
                    .child(ch.sidebar_trash()),
            )
            .child(
                btn_ghost("chat-back-chats", ch.sidebar_back_to_chats())
                    .icon(icon_xs(UiIcon::ArrowLeft))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_sidebar_mode(ChatSidebarMode::Time, cx);
                    })),
            )
            .into_any_element()
    } else {
        search_input(&view.search_input, cx).into_any_element()
    };

    let header = pane_header(
        ch.pane_label(),
        Some(
            h_row()
                .gap(px(4.0))
                .child(
                    btn_icon("chat-toggle-arch", UiIcon::Archive)
                        .tooltip(ch.sidebar_toggle_archived())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_archived = !this.show_archived;
                            this.reload_sessions(cx);
                        })),
                )
                .child(
                    btn_icon("chat-toggle-trash", UiIcon::Trash)
                        .tooltip(ch.sidebar_trash())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let next = if in_trash {
                                ChatSidebarMode::Time
                            } else {
                                ChatSidebarMode::Trash
                            };
                            this.set_sidebar_mode(next, cx);
                        })),
                )
                .child(
                    btn_primary("chat-new", ch.inspector_new())
                        .icon(icon_xs(UiIcon::MessageSquarePlus))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sidebar_mode = ChatSidebarMode::Time;
                            this.create_session(cx);
                        })),
                )
                .into_any_element(),
        ),
        Some(below),
        cx,
    );

    let list_body = if in_trash {
        render_trash_list(view, cx)
    } else {
        render_time_list(view, cx)
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
            .child(ch.sidebar_today_tok(format_tokens_compact(tok_total)))
            .child(cost_txt)
            .into_any_element()
    });

    master_pane(header, list_body, footer, cx)
}

fn render_time_list(view: &ChatView, cx: &mut Context<ChatView>) -> AnyElement {
    let query = view.search_input.read(cx).value().trim().to_lowercase();
    let now_ms = current_time_ms();
    let mut needs_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut failed_unseen_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    if let Some(b) = &view.board {
        for r in &b.needs_you {
            if matches!(
                r.run.status,
                AgentRunStatus::Failed | AgentRunStatus::Interrupted
            ) {
                failed_unseen_ids.insert(r.run.session_id.as_str());
            } else {
                needs_ids.insert(r.run.session_id.as_str());
            }
        }
    }

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
            crate::ui::tr(cx).chat().sidebar_needs_you(),
            Some(needs_you.len()),
            Some(StatusShape::Warn),
            cx,
        ));
        for s in needs_you {
            let failed = failed_unseen_ids.contains(s.id.as_str());
            list = list.child(render_session_row(view, s, now_ms, true, failed, cx));
        }
    }
    if !pinned.is_empty() {
        list = list.child(group_label(
            crate::ui::tr(cx).chat().sidebar_pinned(),
            None,
            None,
            cx,
        ));
        for s in pinned {
            let failed = failed_unseen_ids.contains(s.id.as_str());
            list = list.child(render_session_row(view, s, now_ms, false, failed, cx));
        }
    }
    if !recent.is_empty() {
        list = list.child(group_label(
            crate::ui::tr(cx).chat().sidebar_recent(),
            None,
            None,
            cx,
        ));
        for s in recent {
            let failed = failed_unseen_ids.contains(s.id.as_str());
            list = list.child(render_session_row(view, s, now_ms, false, failed, cx));
        }
    }
    if view.sessions.is_empty() {
        list = list.child(empty_inline(
            crate::ui::tr(cx).chat().sidebar_no_conversations_yet(),
            Some(SharedString::from(
                crate::ui::tr(cx).chat().sidebar_click_new_above_to_start(),
            )),
            None,
            cx,
        ));
    } else if view.sessions_next_cursor.is_some() {
        list = list.child(
            h_row().w_full().justify_center().pt(px(6.0)).child(
                btn_secondary(
                    "chat-sessions-load-more",
                    if view.sessions_loading_more {
                        crate::ui::tr(cx).common().loading()
                    } else {
                        crate::ui::tr(cx).chat().sidebar_load_more()
                    },
                )
                .disabled(view.sessions_loading_more)
                .on_click(cx.listener(|this, _, _, cx| this.load_more_sessions(cx))),
            ),
        );
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
    failed_unseen: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let id = s.id.clone();
    let sel = view.thread.session_id == s.id;
    let title = if s.title.trim().is_empty() {
        crate::ui::tr(cx).chat().header_new_chat().to_owned()
    } else {
        s.title.clone()
    };
    let rel = format_relative_time(s.updated_at_ms, now_ms, crate::ui::active_language(cx));
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
                    crate::ui::tr(pop_cx).chat().sidebar_rename(),
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
                        crate::ui::tr(pop_cx).chat().sidebar_unpin()
                    } else {
                        crate::ui::tr(pop_cx).chat().sidebar_pin()
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
                        crate::ui::tr(pop_cx).chat().pane_menu_unarchive()
                    } else {
                        crate::ui::tr(pop_cx).chat().pane_menu_archive()
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
                    crate::ui::tr(pop_cx).common().delete(),
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
            } else if failed_unseen {
                Some(status_indicator(StatusShape::Bad, cx))
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
                    .text_size(px(13.0))
                    .font_weight(if sel {
                        gpui_kit::FontWeight::SEMIBOLD
                    } else {
                        gpui_kit::FontWeight::MEDIUM
                    })
                    .text_color(to_hsla(if sel { p.ink } else { p.ink_2 }))
                    .truncate()
                    .child(SharedString::from(title)),
            ),
    )
    .children(
        s.archived
            .then(|| badge(crate::ui::tr(cx).chat().sidebar_arch(), Tone::Neutral, cx)),
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

fn format_relative_time(
    ts_ms: i64,
    now_ms: i64,
    lang: pingdeck_desktop::core::model::settings::Language,
) -> String {
    let diff_s = ((now_ms - ts_ms).max(0)) / 1000;
    if diff_s < 60 {
        crate::ui::Tr(lang).chat().sidebar_now().into()
    } else if diff_s < 3600 {
        let m = diff_s / 60;
        crate::ui::Tr(lang).chat().sidebar_m(m)
    } else if diff_s < 86_400 {
        let h = diff_s / 3600;
        crate::ui::Tr(lang).chat().sidebar_h(h)
    } else {
        let d = diff_s / 86_400;
        crate::ui::Tr(lang).chat().sidebar_d(d)
    }
}
