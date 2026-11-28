//! Left thread-list sidebar for AI Chat: live Run Monitor status entry bar, live search,
//! archived & trash toggles, multi-chat attention/failure status indicators, and quick row actions.

use gpui_kit::{
    AnyElement, Context, FontWeight, HighlightStyle, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText,
    div, px,
};
use spanbay_desktop::core::agent::model::{
    AgentRunStatus, AgentSessionPatch, AgentSessionSummary, SessionSearchHit,
};
use spanbay_desktop::core::app::current_time_ms;
use spanbay_desktop::state::map_run_status;

use crate::ui::{
    Anchor, Disableable as _, ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_ghost,
    btn_icon, btn_primary, btn_secondary, empty_inline, h_row, icon_xs, list_row, master_pane,
    menu_item, palette, pane_header, popover, search_input, status_indicator, status_visual,
    to_hsla, v_col,
};

use crate::views::runs::status::{format_relative_age, format_tokens_compact};

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
    let now_ms = current_time_ms();
    let mut needs_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut failed_unseen_ids: std::collections::HashMap<&str, AgentRunStatus> =
        std::collections::HashMap::new();
    if let Some(b) = &view.board {
        for r in &b.needs_you {
            if matches!(
                r.run.status,
                AgentRunStatus::Failed | AgentRunStatus::Interrupted
            ) {
                match failed_unseen_ids.entry(r.run.session_id.as_str()) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(r.run.status);
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        if r.run.status == AgentRunStatus::Failed {
                            e.insert(AgentRunStatus::Failed);
                        }
                    }
                }
            } else if r.wait.is_some()
                || r.run.status == AgentRunStatus::WaitingApproval
                || (r.approval_timed_out && r.run.status.is_active())
            {
                needs_ids.insert(r.run.session_id.as_str());
            }
        }
    }

    let mut needs_you = Vec::new();
    let mut pinned = Vec::new();
    let mut recent = Vec::new();

    // The core already filtered the page by the query; hits in message bodies come separately.
    for s in &view.sessions {
        if needs_ids.contains(s.id.as_str())
            || ((!view.thread.pending_approvals.is_empty()
                || !view.thread.open_gates.is_empty()
                || view.thread.cost_pause.is_some())
                && s.id == view.thread.session_id)
        {
            needs_you.push(s);
        } else if s.pinned {
            pinned.push(s);
        } else {
            recent.push(s);
        }
    }

    let mut list = v_col().w_full().gap(px(2.0)).p(px(8.0));
    let query_now = view.search_input.read(cx).value().trim().to_owned();
    let ch = crate::ui::tr(cx).chat();
    if !view.search_hits.is_empty() {
        list = list.child(group_label(
            ch.search_matches(),
            Some(view.search_hits.len()),
            None,
            cx,
        ));
        for (ix, hit) in view.search_hits.iter().enumerate() {
            list = list.child(render_search_hit(ix, hit, &query_now, cx));
        }
    }
    for (title, count, shape, items, needs_appr) in [
        (
            ch.sidebar_needs_you(),
            Some(needs_you.len()),
            Some(StatusShape::Warn),
            needs_you,
            true,
        ),
        (ch.sidebar_pinned(), None, None, pinned, false),
        (ch.sidebar_recent(), None, None, recent, false),
    ] {
        if !items.is_empty() {
            list = list.child(group_label(title, count, shape, cx));
            for s in items {
                let failed = failed_unseen_ids.get(s.id.as_str()).copied();
                list = list.child(render_session_row(view, s, now_ms, needs_appr, failed, cx));
            }
        }
    }
    if view.sessions.is_empty() && view.search_hits.is_empty() {
        if !query_now.is_empty() {
            let clear_btn = btn_secondary("chat-search-clear", ch.search_clear())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.search_input
                        .update(cx, |s, cx| s.set_value("", window, cx));
                    this.search_hits.clear();
                    this.refresh_catalog(cx);
                }))
                .into_any_element();
            list = list.child(empty_inline(
                ch.search_empty(&query_now),
                Some(SharedString::from(ch.search_empty_hint())),
                Some(clear_btn),
                cx,
            ));
        } else {
            list = list.child(empty_inline(
                ch.sidebar_no_conversations_yet(),
                Some(SharedString::from(ch.sidebar_click_new_above_to_start())),
                None,
                cx,
            ));
        }
    } else if view.sessions_next_cursor.is_some() {
        let lbl = if view.sessions_loading_more {
            crate::ui::tr(cx).common().loading()
        } else {
            ch.sidebar_load_more()
        };
        list = list.child(
            h_row().w_full().justify_center().pt(px(6.0)).child(
                btn_secondary("chat-sessions-load-more", lbl)
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

/// Splits a core search snippet into plain text and the byte ranges wrapped in `[` `]`,
/// preserving literal `[...]` brackets that do not match `query` or are unclosed.
fn snippet_highlights_for(
    snippet: &str,
    query: Option<&str>,
) -> (String, Vec<std::ops::Range<usize>>) {
    let q = query.map(str::trim).filter(|q| !q.is_empty());
    let q_folded: Option<String> = q.map(|s| s.to_lowercase());
    let has_query_match = q_folded.as_deref().is_some_and(|qf| {
        let lower = snippet.to_lowercase();
        lower.contains(&format!("[{qf}]"))
    });
    let mut text = String::with_capacity(snippet.len());
    let mut ranges = Vec::new();
    let mut rest = snippet;
    while let Some(open_pos) = rest.find('[') {
        text.push_str(&rest[..open_pos]);
        let after_open = &rest[open_pos + 1..];
        if let Some(qf) = q_folded.as_deref().filter(|_| has_query_match) {
            // Check if the prefix of `after_open` matches `query` followed by `]`.
            let mut matched_len = None;
            for (idx, ch) in after_open.char_indices() {
                let end = idx + ch.len_utf8();
                if after_open[..end].to_lowercase() == qf && after_open[end..].starts_with(']') {
                    matched_len = Some(end);
                    break;
                }
                if end > qf.len() * 2 + 4 {
                    break;
                }
            }
            if let Some(mlen) = matched_len {
                let start = text.len();
                text.push_str(&after_open[..mlen]);
                ranges.push(start..text.len());
                rest = &after_open[mlen + 1..];
                continue;
            }
            text.push('[');
            rest = after_open;
            continue;
        }
        match (after_open.find(']'), after_open.find('[')) {
            (Some(close_pos), next_open)
                if close_pos > 0 && next_open.is_none_or(|no| close_pos < no) =>
            {
                let start = text.len();
                text.push_str(&after_open[..close_pos]);
                ranges.push(start..text.len());
                rest = &after_open[close_pos + 1..];
            }
            _ => {
                text.push('[');
                rest = after_open;
            }
        }
    }
    text.push_str(rest);
    (text, ranges)
}

fn render_search_hit(
    ix: usize,
    hit: &SessionSearchHit,
    query: &str,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let (text, ranges) = snippet_highlights_for(&hit.snippet, Some(query));
    let bold = HighlightStyle {
        font_weight: Some(FontWeight::BOLD),
        ..HighlightStyle::default()
    };
    let sid = hit.session_id.clone();
    let mid = hit.message_id.clone();
    v_col()
        .id(SharedString::from(format!("chat-hit-{ix}")))
        .w_full()
        .gap(px(2.0))
        .px(px(8.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .cursor_pointer()
        .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_session_at_message(sid.clone(), mid.clone(), cx);
        }))
        .child(
            h_row()
                .gap(px(6.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(hit.title.clone())),
                )
                .children(hit.in_tool_output.then(|| {
                    badge(
                        crate::ui::tr(cx).chat().search_in_tool_output(),
                        Tone::Neutral,
                        cx,
                    )
                })),
        )
        .child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                StyledText::new(text).with_highlights(ranges.into_iter().map(|r| (r, bold))),
            ),
        )
}

fn render_session_row(
    view: &ChatView,
    s: &AgentSessionSummary,
    now_ms: i64,
    needs_approval: bool,
    failed_unseen: Option<AgentRunStatus>,
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
    let rel = format_relative_age(s.updated_at_ms, now_ms, cx);
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
            let pch = crate::ui::tr(pop_cx).chat();
            let (pin_ico, pin_lbl) = if is_pinned {
                (UiIcon::PinOff, pch.sidebar_unpin())
            } else {
                (UiIcon::Pin, pch.sidebar_pin())
            };
            let arch_lbl = if is_archived {
                pch.pane_menu_unarchive()
            } else {
                pch.pane_menu_archive()
            };
            v_col()
                .w(px(164.0))
                .gap(px(2.0))
                .child(menu_item(
                    "chat-rm-ren",
                    UiIcon::Pencil,
                    pch.sidebar_rename(),
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
                    pin_ico,
                    pin_lbl,
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
                    arch_lbl,
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

    let (diff_added, diff_removed) = if sel && !view.persisted_changes.is_empty() {
        view.persisted_changes
            .iter()
            .filter(|c| {
                matches!(
                    c.status,
                    spanbay_desktop::core::agent::model::FileChangeStatus::Pending
                        | spanbay_desktop::core::agent::model::FileChangeStatus::Conflict
                )
            })
            .fold((0u32, 0u32), |(a, r), c| {
                (a.saturating_add(c.added), r.saturating_add(c.removed))
            })
    } else {
        (s.pending_added, s.pending_removed)
    };
    let has_diff = diff_added > 0 || diff_removed > 0;

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
                failed_unseen.map(|st| status_indicator(status_visual(map_run_status(st)).0, cx))
            })
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
    .children(has_diff.then(|| {
        h_row()
            .flex_shrink_0()
            .gap(px(3.0))
            .text_size(px(10.5))
            .child(
                div()
                    .text_color(to_hsla(p.ok))
                    .child(format!("+{diff_added}")),
            )
            .child(
                div()
                    .text_color(to_hsla(p.bad))
                    .child(format!("−{diff_removed}")),
            )
    }))
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

#[cfg(test)]
mod tests {
    use super::snippet_highlights_for;

    fn snippet_highlights(snippet: &str) -> (String, Vec<std::ops::Range<usize>>) {
        snippet_highlights_for(snippet, None)
    }

    #[test]
    fn snippet_markers_become_ranges() {
        let (text, ranges) = snippet_highlights("…the [gateway] retries [429]");
        assert_eq!(text, "…the gateway retries 429");
        let marked: Vec<&str> = ranges.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(marked, ["gateway", "429"]);
    }

    #[test]
    fn snippet_preserves_normal_and_unclosed_brackets() {
        let (text, ranges) = super::snippet_highlights_for(
            "check arr[0] and [WARN] before [gateway] with [unclosed",
            Some("gateway"),
        );
        assert_eq!(
            text,
            "check arr[0] and [WARN] before gateway with [unclosed"
        );
        let marked: Vec<&str> = ranges.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(marked, ["gateway"]);
    }
}
