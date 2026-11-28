//! Right-side inline Changes & Unified Diff Review Panel (`⌘⇧D`) view rendering for AI Chat.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::model::FileChangeStatus;
use spanbay_desktop::services::clipboard;

use crate::ui::{
    Disableable as _, ScrollableElement as _, StatusShape, Tone, UiIcon, alert_banner, badge,
    btn_danger, btn_ghost, btn_icon, btn_primary, btn_secondary, empty_inline, h_row,
    mono_font_family, mono_font_size, palette, status_indicator, text_input, to_hsla, v_col,
};
use crate::views::runs::status::file_change_status_label;

use super::ChatView;
use super::review_actions::{DiffLineKind, GroupedFileChange};
use super::types::{ReviewDiffMode, ReviewScope};
use super::welcome::{render_tool_output_panel, render_undo_precheck};

pub fn render_review_panel(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    if let Some(out) = &view.review_tool_output {
        return render_tool_output_panel(out, cx).into_any_element();
    }
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let groups = view.grouped_review_files();
    let pending_all_ids: Vec<String> = view
        .scoped_changes()
        .into_iter()
        .filter(|c| {
            matches!(
                c.status,
                FileChangeStatus::Pending | FileChangeStatus::Conflict
            )
        })
        .map(|c| c.id.clone())
        .collect();
    let mut scope_seg = h_row()
        .gap(px(2.0))
        .p(px(2.0))
        .rounded(px(6.0))
        .bg(to_hsla(p.sunken));
    for (id, sc, lbl) in [
        ("chat-rv-scope-turn", ReviewScope::Turn, ch.rv_scope_turn()),
        ("chat-rv-scope-all", ReviewScope::All, ch.rv_scope_all()),
    ] {
        scope_seg = scope_seg.child(
            btn_ghost(id, lbl)
                .when(view.review_scope == sc, |b| {
                    b.bg(to_hsla(p.surface)).text_color(to_hsla(p.ink))
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_review_scope(sc, cx))),
        );
    }

    let keep_ids = pending_all_ids.clone();
    let header = h_row()
        .w_full()
        .h(px(40.0))
        .flex_shrink_0()
        .justify_between()
        .gap(px(6.0))
        .px(px(12.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .bg(to_hsla(p.surface))
        .child(
            h_row()
                .gap(px(6.0))
                .min_w_0()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(ch.rv_title()),
                )
                .child(badge(groups.len().to_string(), Tone::Neutral, cx))
                .child(scope_seg),
        )
        .child(
            h_row()
                .gap(px(4.0))
                .flex_shrink_0()
                .children((!pending_all_ids.is_empty()).then(|| {
                    btn_ghost("chat-rv-keep-all", ch.changes_keep_all()).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.keep_changes(keep_ids.clone(), cx);
                        },
                    ))
                }))
                .children((!pending_all_ids.is_empty()).then(|| {
                    btn_ghost("chat-rv-undo-all", ch.rv_undo_all()).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.open_undo_precheck(cx);
                        },
                    ))
                }))
                .child(
                    btn_icon("chat-rv-close", UiIcon::Close)
                        .tooltip(crate::ui::tr(cx).common().close())
                        .on_click(cx.listener(|this, _, _, cx| this.close_review(cx))),
                ),
        );

    if groups.is_empty() {
        return v_col()
            .id("chat-review-panel")
            .size_full()
            .min_w_0()
            .bg(to_hsla(p.pane))
            .border_l_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(empty_inline(
                        ch.inspector_no_file_edits_recorded_in(),
                        None,
                        None,
                        cx,
                    )),
            )
            .into_any_element();
    }

    let selected_group = view
        .review_selected_path
        .as_ref()
        .and_then(|path| groups.iter().find(|g| &g.path == path))
        .unwrap_or(&groups[0]);

    let mut file_list = v_col()
        .w_full()
        .max_h(px(148.0))
        .overflow_y_scrollbar()
        .id("chat-rv-files")
        .p(px(6.0))
        .gap(px(2.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .bg(to_hsla(p.pane));

    for (ix, g) in groups.iter().enumerate() {
        let is_sel = g.path == selected_group.path;
        let path_click = g.path.clone();
        let (shape, tone) = match g.status {
            FileChangeStatus::Kept => (StatusShape::Ok, Tone::Success),
            FileChangeStatus::Undone => (StatusShape::Stopped, Tone::Neutral),
            FileChangeStatus::Conflict => (StatusShape::Bad, Tone::Danger),
            _ => (StatusShape::Warn, Tone::Warning),
        };
        file_list = file_list.child(
            h_row()
                .id(SharedString::from(format!("chat-rv-file-{ix}")))
                .w_full()
                .justify_between()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(4.5))
                .rounded(px(6.0))
                .cursor_pointer()
                .bg(to_hsla(if is_sel { p.accent_soft } else { p.pane }))
                .border_1()
                .border_color(to_hsla(if is_sel {
                    p.accent_line.composite_over(p.accent_soft)
                } else {
                    p.line.composite_over(p.pane)
                }))
                .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_review_file(path_click.clone(), cx);
                }))
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
                                .font_family(mono_font_family(cx))
                                .text_xs()
                                .font_weight(if is_sel {
                                    gpui_kit::FontWeight::SEMIBOLD
                                } else {
                                    gpui_kit::FontWeight::MEDIUM
                                })
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(g.path.clone())),
                        ),
                )
                .child(
                    h_row()
                        .flex_shrink_0()
                        .gap(px(5.0))
                        .child(badge(file_change_status_label(g.status, cx), tone, cx))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.ok))
                                .child(format!("+{}", g.added)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.bad))
                                .child(format!("−{}", g.removed)),
                        ),
                ),
        );
    }

    let edit_count = selected_group.change_ids.len();
    let file_keep_ids = selected_group.active_ids.clone();
    let file_undo_ids = selected_group.change_ids.clone();
    let is_conflict = selected_group.status == FileChangeStatus::Conflict;
    let actionable = matches!(
        selected_group.status,
        FileChangeStatus::Pending | FileChangeStatus::Conflict
    );
    let path_copy = selected_group.path.clone();
    let path_ext = selected_group.path.clone();
    let has_ws_dir = view
        .session
        .as_ref()
        .is_some_and(|s| s.workspace_dir.is_some());

    let toolbar = h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(6.0))
        .px(px(10.0))
        .py(px(6.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .bg(to_hsla(p.surface))
        .child(
            h_row()
                .gap(px(2.0))
                .p(px(2.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .child(
                    btn_ghost("chat-rv-mode-net", ch.rv_net_diff())
                        .when(view.review_diff_mode == ReviewDiffMode::Net, |b| {
                            b.bg(to_hsla(p.surface)).text_color(to_hsla(p.ink))
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_review_diff_mode(ReviewDiffMode::Net, cx);
                        })),
                )
                .child(
                    btn_ghost("chat-rv-mode-edit", ch.rv_per_edit_count(edit_count))
                        .when(view.review_diff_mode == ReviewDiffMode::PerEdit, |b| {
                            b.bg(to_hsla(p.surface)).text_color(to_hsla(p.ink))
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_review_diff_mode(ReviewDiffMode::PerEdit, cx);
                        })),
                ),
        )
        .child(
            h_row()
                .gap(px(4.0))
                .child(
                    btn_icon("chat-rv-copy-path", UiIcon::Copy)
                        .tooltip(ch.trace_copy_path())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let _ = clipboard::set_text(&path_copy);
                            this.status_msg =
                                Some((Tone::Success, ch.trace_copied_path().to_owned()));
                            cx.notify();
                        })),
                )
                .children(has_ws_dir.then(|| {
                    btn_icon("chat-rv-open-ext", UiIcon::ExternalLink)
                        .tooltip(ch.trace_open_external())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_workspace_file_external(path_ext.clone(), cx);
                        }))
                }))
                .child(
                    btn_secondary("chat-rv-keep-file", ch.changes_keep())
                        .disabled(!actionable)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.keep_changes(file_keep_ids.clone(), cx);
                        })),
                )
                .child(
                    btn_danger(
                        "chat-rv-undo-file",
                        if is_conflict {
                            crate::ui::tr(cx).common().change_force_undo()
                        } else {
                            ch.inspector_undo()
                        },
                    )
                    .disabled(selected_group.status == FileChangeStatus::Undone)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.undo_changes_with_force(file_undo_ids.clone(), is_conflict, cx);
                    })),
                ),
        );

    let precheck_card = view
        .review_undo_precheck
        .then(|| render_undo_precheck(view, &groups, cx));
    let diff_area = render_diff_body(view, selected_group, cx);

    v_col()
        .id("chat-review-panel")
        .size_full()
        .min_w_0()
        .bg(to_hsla(p.surface))
        .border_l_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(header)
        .children(precheck_card)
        .child(file_list)
        .child(toolbar)
        .child(diff_area)
        .into_any_element()
}

fn render_diff_body(
    view: &ChatView,
    selected: &GroupedFileChange,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mono = mono_font_family(cx);
    let code_px = (f32::from(mono_font_size(cx)) - 1.5).clamp(10.5, 13.0);
    let active_change_id = selected
        .active_ids
        .first()
        .or_else(|| selected.change_ids.first())
        .cloned();
    let can_revert_hunk = view.can_revert_hunk_in_current_view(&selected.path);

    let mut scroll = v_col()
        .id("chat-rv-diff-scroll")
        .flex_1()
        .min_h_0()
        .w_full()
        .p(px(10.0))
        .gap(px(10.0))
        .overflow_y_scrollbar();

    if selected.status == FileChangeStatus::Conflict {
        scroll = scroll.child(alert_banner(
            Tone::Warning,
            crate::ui::tr(cx).common().change_conflict_hint(),
            None,
            cx,
        ));
    }
    if let Some((_, err)) = &view.review_diff_error {
        return scroll
            .child(alert_banner(Tone::Danger, err.clone(), None, cx))
            .into_any_element();
    }
    let Some(cached) = &view.review_diff_cache else {
        return scroll
            .child(empty_inline(
                crate::ui::tr(cx).common().loading(),
                None,
                None,
                cx,
            ))
            .into_any_element();
    };
    if cached.binary {
        return scroll
            .child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(ch.modals_binary_file_changed()),
            )
            .into_any_element();
    }

    for (hunk_ix, hunk) in cached.hunks.iter().enumerate() {
        let mut hunk_card = v_col()
            .w_full()
            .rounded(px(8.0))
            .overflow_hidden()
            .border_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)))
            .font_family(mono.clone());

        let cid_hunk = active_change_id.clone().unwrap_or_default();
        let hunk_head = h_row()
            .w_full()
            .justify_between()
            .px(px(10.0))
            .py(px(4.0))
            .bg(to_hsla(p.pane))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.accent_ink))
                    .truncate()
                    .child(SharedString::from(hunk.header.clone())),
            )
            .children(can_revert_hunk.then(|| {
                btn_ghost(
                    SharedString::from(format!("chat-rv-revert-hunk-{hunk_ix}")),
                    format!("{} ↺", ch.rv_revert_hunk()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.revert_change_hunk(cid_hunk.clone(), hunk_ix, cx);
                }))
            }));
        hunk_card = hunk_card.child(hunk_head);

        for (line_ix, line) in hunk.lines.iter().enumerate() {
            let (bg, fg) = match line.kind {
                DiffLineKind::Add => (
                    to_hsla(p.diff_add_bg.composite_over(p.surface)),
                    to_hsla(p.diff_add),
                ),
                DiffLineKind::Del => (
                    to_hsla(p.diff_del_bg.composite_over(p.surface)),
                    to_hsla(p.diff_del),
                ),
                DiffLineKind::Context => (to_hsla(p.surface), to_hsla(p.ink_2)),
            };
            let path_for_line = selected.path.clone();
            let ln = line.line_no;
            let is_commenting = view
                .review_comment_target
                .as_ref()
                .is_some_and(|(cp, cl)| cp == &selected.path && *cl == ln);

            hunk_card = hunk_card.child(
                h_row()
                    .id(SharedString::from(format!(
                        "chat-rv-line-{hunk_ix}-{line_ix}"
                    )))
                    .w_full()
                    .bg(bg)
                    .px(px(8.0))
                    .py(px(1.5))
                    .gap(px(8.0))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.review_comment_target = Some((path_for_line.clone(), ln));
                        this.review_comment_input
                            .update(cx, |inp, cx| inp.set_value("", window, cx));
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(38.0))
                            .flex_shrink_0()
                            .text_right()
                            .text_size(px(code_px - 0.5))
                            .text_color(to_hsla(p.faint))
                            .child(format!("+ {ln}")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(code_px))
                            .text_color(fg)
                            .truncate()
                            .child(SharedString::from(line.text.clone())),
                    ),
            );
            if is_commenting {
                hunk_card = hunk_card.child(
                    h_row()
                        .w_full()
                        .gap(px(6.0))
                        .p(px(8.0))
                        .bg(to_hsla(p.pane))
                        .border_t_1()
                        .border_color(to_hsla(p.line.composite_over(p.pane)))
                        .child(div().flex_1().child(text_input(&view.review_comment_input)))
                        .child(
                            btn_ghost(
                                "chat-rv-comment-cancel",
                                crate::ui::tr(cx).common().cancel(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.review_comment_target = None;
                                cx.notify();
                            })),
                        )
                        .child(
                            btn_primary("chat-rv-comment-save", ch.rv_comment_add()).on_click(
                                cx.listener(|this, _, _, cx| this.submit_review_comment(cx)),
                            ),
                        ),
                );
            }
        }
        scroll = scroll.child(hunk_card);
    }
    if cached.omitted > 0 {
        scroll = scroll.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(ch.trace_more_lines(cached.omitted)),
        );
    }
    scroll.into_any_element()
}
