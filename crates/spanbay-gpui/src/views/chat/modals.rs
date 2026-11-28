//! Modal dialogs for AI Chat: Full Access confirmation, Delete-while-running confirmation,
//! Rename Session, Scope Approval details, Unified Diff Preview, Full Tool Output, and Edit Message.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use spanbay_desktop::core::agent::model::AgentSessionPatch;
use spanbay_desktop::core::model::settings::ToolApproval;
use spanbay_desktop::state::SettingsSection;

use crate::ui::{
    Disableable as _, Tone, UiIcon, alert_banner, btn_danger, btn_ghost, btn_primary,
    btn_secondary, h_row, icon_xs, modal_dialog, multiline_input, palette, text_input, to_hsla,
    v_col,
};

use super::ChatView;
use super::gates_extra::render_scope_modal;
use super::media::render_image_viewer_modal;
use super::types::{ChatHandoff, ChatModal};

pub fn render_active_modal(
    view: &ChatView,
    modal: ChatModal,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let p = palette(cx);
    let entity = cx.entity().clone();
    let on_close = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        entity.update(cx, |this, cx| {
            this.active_modal = None;
            cx.notify();
        });
    };

    match modal {
        ChatModal::FullAccessConfirm { acknowledged } => {
            let body = v_col()
                .gap(px(10.0))
                .child(alert_banner(
                    Tone::Warning,
                    crate::ui::tr(cx)
                        .chat()
                        .modals_full_access_auto_runs_edits(),
                    None,
                    cx,
                ))
                .child(
                    div().text_xs().text_color(to_hsla(p.ink_2)).child(
                        crate::ui::tr(cx)
                            .chat()
                            .modals_hard_blocks_block_rules_forced(),
                    ),
                )
                .child(
                    h_row()
                        .id("chat-fa-ack")
                        .gap(px(8.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(ChatModal::FullAccessConfirm { acknowledged }) =
                                &mut this.active_modal
                            {
                                *acknowledged = !*acknowledged;
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .size(px(16.0))
                                .rounded(px(4.0))
                                .border_1()
                                .border_color(to_hsla(if acknowledged {
                                    p.accent
                                } else {
                                    p.line_strong
                                }))
                                .bg(to_hsla(if acknowledged { p.accent } else { p.surface }))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(to_hsla(p.surface))
                                .children(acknowledged.then(|| icon_xs(UiIcon::Check))),
                        )
                        .child(
                            div().text_xs().text_color(to_hsla(p.ink)).child(
                                crate::ui::tr(cx)
                                    .chat()
                                    .modals_i_understand_enable_full_access(),
                            ),
                        ),
                )
                .child(
                    btn_ghost(
                        "chat-fa-eval-link",
                        crate::ui::tr(cx)
                            .chat()
                            .modals_view_6_layer_evaluation_order(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = None;
                        this.pending_handoff.set(Some(ChatHandoff::OpenSettings(
                            SettingsSection::Permissions,
                        )));
                        cx.notify();
                    })),
                )
                .into_any_element();

            let mut ok_btn = btn_danger(
                "chat-fa-confirm",
                crate::ui::tr(cx).chat().modals_enable_full_access(),
            );
            if acknowledged {
                ok_btn = ok_btn.on_click(cx.listener(|this, _, _, cx| {
                    this.active_modal = None;
                    this.set_permission_mode(ToolApproval::AutoAll, cx);
                    cx.notify();
                }));
            } else {
                ok_btn = ok_btn.disabled(true);
            }
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("chat-fa-cancel", crate::ui::tr(cx).common().cancel()).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(ok_btn)
                .into_any_element();

            modal_dialog(
                "chat-fa-modal",
                crate::ui::tr(cx).chat().modals_enable_full_access_2(),
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
            .into_any_element()
        }
        ChatModal::DeleteRunningConfirm { session_id, title } => {
            let body = div()
                .text_sm()
                .text_color(to_hsla(p.ink))
                .child(
                    crate::ui::tr(cx)
                        .chat()
                        .modals_chat_has_an_active_run(title),
                )
                .into_any_element();
            let sid_del = session_id;
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("chat-del-cancel", crate::ui::tr(cx).common().cancel()).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_danger(
                        "chat-del-confirm",
                        crate::ui::tr(cx).chat().modals_stop_delete(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active_modal = None;
                        this.delete_session_by_id(sid_del.clone(), cx);
                    })),
                )
                .into_any_element();
            modal_dialog(
                "chat-del-running-modal",
                crate::ui::tr(cx).chat().modals_delete_running_chat(),
                440.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
            .into_any_element()
        }
        ChatModal::RenameSession { session_id } => {
            let body = v_col()
                .gap(px(6.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(crate::ui::tr(cx).chat().pane_row_rename_label()),
                )
                .child(text_input(&view.modal_input))
                .into_any_element();
            let sid = session_id;
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("chat-rn-cancel", crate::ui::tr(cx).common().cancel()).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary("chat-rn-save", crate::ui::tr(cx).common().save()).on_click(
                        cx.listener(move |this, _, _, cx| {
                            let val = this.modal_input.read(cx).value().trim().to_owned();
                            this.active_modal = None;
                            if !val.is_empty() {
                                this.patch_session_by_id(
                                    sid.clone(),
                                    AgentSessionPatch {
                                        title: Some(val),
                                        ..Default::default()
                                    },
                                    cx,
                                );
                            }
                            cx.notify();
                        }),
                    ),
                )
                .into_any_element();
            modal_dialog(
                "chat-rename-modal",
                crate::ui::tr(cx).chat().modals_rename_chat(),
                420.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
            .into_any_element()
        }
        ChatModal::ScopeApproval { appr, selected } => {
            render_scope_modal(&appr, selected, window, cx, on_close)
        }
        ChatModal::EditMessage { message_id } => {
            let ch = crate::ui::tr(cx).chat();
            let has_changes = view.turn_has_active_changes(&message_id);
            let body = v_col()
                .gap(px(6.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(ch.modals_edit_message_and_regenerate_from()),
                )
                .child(multiline_input(&view.modal_textarea))
                .into_any_element();
            let mid = message_id.clone();
            let mid_undo = message_id;
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("chat-ed-cancel", crate::ui::tr(cx).common().cancel()).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(if has_changes {
                    btn_secondary("chat-ed-ok", ch.modals_save_run())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_edit_message_with_undo(mid.clone(), false, cx);
                        }))
                        .into_any_element()
                } else {
                    btn_primary("chat-ed-ok", ch.modals_save_run())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_edit_message_with_undo(mid.clone(), false, cx);
                        }))
                        .into_any_element()
                })
                .children(has_changes.then(|| {
                    btn_primary("chat-ed-undo-ok", ch.graph_action_undo_retry()).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.confirm_edit_message_with_undo(mid_undo.clone(), true, cx);
                        }),
                    )
                }))
                .into_any_element();
            modal_dialog(
                "chat-edit-msg-modal",
                ch.modals_edit_message(),
                520.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
            .into_any_element()
        }
        ChatModal::ChatInstructions => {
            let body = v_col()
                .gap(px(8.0))
                .child(multiline_input(&view.instructions_input))
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("chat-inst-cancel", crate::ui::tr(cx).common().cancel())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        })),
                )
                .child(
                    btn_primary(
                        "chat-inst-save",
                        crate::ui::tr(cx).chat().inspector_tabs_save(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save_instructions(cx);
                        this.active_modal = None;
                        cx.notify();
                    })),
                )
                .into_any_element();
            modal_dialog(
                "chat-instructions-modal",
                crate::ui::tr(cx).chat().inspector_tabs_chat_instructions(),
                520.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
            .into_any_element()
        }
        ChatModal::ImageViewer {
            key,
            title,
            meta,
            dims,
            actual,
            source,
        } => render_image_viewer_modal(
            view, &key, title, meta, dims, actual, source, window, cx, on_close,
        ),
    }
}
