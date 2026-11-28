//! Full-width 2-pane File Changes & Unified Diff review tab (`变更`) inside the 90% viewport
//! [`WorkflowMonitorModal`].
//!
//! Replaces the cramped 316px Chat right-hand Details › Changes drawer with a spacious side-by-side
//! file list + inline Unified Diff viewer and single-file / bulk Keep & Undo controls.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::model::{FileChangeKind, FileChangeStatus};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_danger, btn_ghost, btn_primary,
    diff_lines_preview, empty_inline, h_row, icon_xs, palette, section_heading, to_hsla, v_col,
};

use super::modal::WorkflowMonitorModal;
use super::status::file_change_status_label;
pub fn render_changes_tab(
    view: &WorkflowMonitorModal,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    if view.changes.is_empty() {
        return div()
            .size_full()
            .bg(to_hsla(p.surface))
            .flex()
            .items_center()
            .justify_center()
            .child(empty_inline(
                ch.inspector_no_file_edits_recorded_in(),
                None,
                None,
                cx,
            ))
            .into_any_element();
    }

    let pending_ids: Vec<String> = view
        .changes
        .iter()
        .filter(|c| {
            matches!(
                c.status,
                FileChangeStatus::Pending | FileChangeStatus::Conflict
            )
        })
        .map(|c| c.id.clone())
        .collect();

    let mut file_rows = v_col().w_full().gap(px(5.0)).p(px(10.0));
    for fc in &view.changes {
        let selected = view.selected_change_id.as_deref() == Some(fc.id.as_str());
        let kind_lbl = match fc.kind {
            FileChangeKind::Create => ch.inspector_new(),
            FileChangeKind::Edit => crate::ui::tr(cx).common().edit(),
        };
        let status_tone = match fc.status {
            FileChangeStatus::Pending => Tone::Warning,
            FileChangeStatus::Kept => Tone::Success,
            FileChangeStatus::Undone => Tone::Neutral,
            FileChangeStatus::Conflict => Tone::Danger,
            FileChangeStatus::Applying => Tone::Accent,
        };
        let cid_pick = fc.id.clone();
        let bg = if selected {
            to_hsla(p.accent_soft)
        } else {
            to_hsla(p.surface)
        };
        let border = if selected {
            to_hsla(p.accent)
        } else {
            to_hsla(p.line.composite_over(p.surface))
        };

        file_rows = file_rows.child(
            v_col()
                .id(SharedString::from(format!("wf-chg-row-{}", fc.id)))
                .w_full()
                .gap(px(4.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(7.0))
                .bg(bg)
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_change(cid_pick.clone(), cx);
                }))
                .child(
                    h_row()
                        .w_full()
                        .gap(px(6.0))
                        .child(icon_xs(UiIcon::FileDiff))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(fc.path.clone())),
                        ),
                )
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .gap(px(6.0))
                        .child(
                            h_row()
                                .gap(px(4.0))
                                .child(badge(kind_lbl, Tone::Neutral, cx))
                                .child(badge(
                                    file_change_status_label(fc.status, cx),
                                    status_tone,
                                    cx,
                                )),
                        )
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .text_xs()
                                .child(
                                    div()
                                        .text_color(to_hsla(p.ok))
                                        .child(format!("+{}", fc.added)),
                                )
                                .child(
                                    div()
                                        .text_color(to_hsla(p.bad))
                                        .child(format!("-{}", fc.removed)),
                                ),
                        ),
                ),
        );
    }

    let left_pane = v_col()
        .w(px(300.0))
        .flex_shrink_0()
        .h_full()
        .bg(to_hsla(p.pane))
        .border_r_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .w_full()
                .flex_shrink_0()
                .justify_between()
                .px(px(12.0))
                .py(px(9.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(section_heading(ch.inspector_modified_files(), cx))
                .children((!pending_ids.is_empty()).then(|| {
                    let ids_all = pending_ids.clone();
                    btn_ghost("wf-chg-keep-all", ch.changes_keep_all()).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.keep_changes(ids_all.clone(), cx);
                        },
                    ))
                })),
        )
        .child(
            div()
                .id("wf-chg-list-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(file_rows),
        );

    let selected_change = view
        .selected_change_id
        .as_deref()
        .and_then(|id| view.changes.iter().find(|c| c.id == id))
        .or_else(|| view.changes.first());

    let right_pane: AnyElement = if let Some(fc) = selected_change {
        let actionable = matches!(
            fc.status,
            FileChangeStatus::Pending | FileChangeStatus::Conflict
        );
        let is_conflict = fc.status == FileChangeStatus::Conflict;
        let cid_keep = fc.id.clone();
        let cid_undo = fc.id.clone();
        let diff_or_binary = |diff: &str, binary: bool| -> AnyElement {
            if binary {
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(ch.modals_binary_file_changed())
                    .into_any_element()
            } else {
                diff_lines_preview(diff, 600, cx).into_any_element()
            }
        };
        let diff_body: AnyElement =
            match (&view.selected_change_diff, &view.selected_change_diff_err) {
                (Some((did, diff_txt, binary)), _) if did == &fc.id => {
                    let mut body = v_col()
                        .id("wf-chg-diff-scroll")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .gap(px(10.0))
                        .p(px(14.0))
                        .overflow_scrollbar();
                    if is_conflict {
                        body = body.child(alert_banner(
                            Tone::Warning,
                            crate::ui::tr(cx).common().change_conflict_hint(),
                            None,
                            cx,
                        ));
                    }
                    // Several edits of this file: the net change first, the edit itself below.
                    if let Some((net_txt, net_binary)) = &view.selected_net_diff {
                        let edits = view
                            .changes
                            .iter()
                            .filter(|c| {
                                c.session_id == fc.session_id
                                    && c.path == fc.path
                                    && c.folder == fc.folder
                                    && c.status != FileChangeStatus::Undone
                            })
                            .count();
                        body = body
                            .child(section_heading(ch.changes_net_change(edits), cx))
                            .child(diff_or_binary(net_txt, *net_binary))
                            .child(section_heading(ch.changes_per_edit(), cx));
                    }
                    body.child(diff_or_binary(diff_txt, *binary))
                        .into_any_element()
                }
                (_, Some((eid, err_msg))) if eid == &fc.id => v_col()
                    .flex_1()
                    .w_full()
                    .gap(px(10.0))
                    .p(px(14.0))
                    .children(is_conflict.then(|| {
                        alert_banner(
                            Tone::Warning,
                            crate::ui::tr(cx).common().change_conflict_hint(),
                            None,
                            cx,
                        )
                    }))
                    .child(alert_banner(Tone::Danger, err_msg.clone(), None, cx))
                    .into_any_element(),
                _ => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(empty_inline(
                        crate::ui::tr(cx).common().loading(),
                        None,
                        None,
                        cx,
                    ))
                    .into_any_element(),
            };

        v_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(to_hsla(p.surface))
            .child(
                h_row()
                    .w_full()
                    .flex_shrink_0()
                    .justify_between()
                    .gap(px(10.0))
                    .px(px(16.0))
                    .py(px(10.0))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)))
                    .child(
                        h_row()
                            .flex_1()
                            .min_w_0()
                            .gap(px(8.0))
                            .child(icon_xs(UiIcon::FileDiff))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(fc.path.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.ok))
                                    .child(format!("+{}", fc.added)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.bad))
                                    .child(format!("-{}", fc.removed)),
                            ),
                    )
                    .child(
                        h_row()
                            .flex_shrink_0()
                            .gap(px(6.0))
                            .children(actionable.then(|| {
                                btn_primary("wf-chg-keep", ch.changes_keep()).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.keep_changes(vec![cid_keep.clone()], cx);
                                    },
                                ))
                            }))
                            .children(actionable.then(|| {
                                let undo_lbl = if is_conflict {
                                    crate::ui::tr(cx).common().change_force_undo()
                                } else {
                                    ch.inspector_undo()
                                };
                                btn_danger("wf-chg-undo", undo_lbl).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.undo_change(cid_undo.clone(), is_conflict, cx);
                                    },
                                ))
                            })),
                    ),
            )
            .child(diff_body)
            .into_any_element()
    } else {
        div().flex_1().into_any_element()
    };

    h_row()
        .size_full()
        .items_stretch()
        .child(left_pane)
        .child(right_pane)
        .into_any_element()
}
