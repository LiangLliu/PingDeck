//! Modal dialogs for AI Chat: Full Access confirmation, Delete-while-running confirmation,
//! Rename Session, Scope Approval details, Unified Diff Preview, Full Tool Output, and Edit Message.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use pingdeck_desktop::core::agent::model::{AgentSessionPatch, ApprovalDecision, ApprovalScope};
use pingdeck_desktop::core::model::settings::ToolApproval;
use pingdeck_desktop::state::SettingsSection;

use crate::ui::{
    Disableable as _, ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_danger,
    btn_ghost, btn_primary, btn_secondary, h_row, icon_xs, modal_dialog, multiline_input, palette,
    text_input, to_hsla, v_col,
};

use super::ChatView;
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
        ChatModal::ScopeApproval { appr } => {
            let mut body = v_col()
                .gap(px(8.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(appr.summary.clone()),
                )
                .children(appr.rule_label.as_ref().map(|rl| {
                    h_row()
                        .gap(px(6.0))
                        .child(badge(
                            crate::ui::tr(cx).chat().modals_rule(),
                            Tone::Accent,
                            cx,
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .child(rl.clone()),
                        )
                }))
                .children(
                    appr.scope_disabled_reason
                        .as_ref()
                        .map(|r| alert_banner(Tone::Warning, r.clone(), None, cx)),
                );
            let scopes: [(ApprovalScope, &str); 4] = [
                (
                    ApprovalScope::Once,
                    crate::ui::tr(cx).chat().modals_allow_once(),
                ),
                (
                    ApprovalScope::Session,
                    crate::ui::tr(cx).chat().modals_allow_in_this_chat(),
                ),
                (
                    ApprovalScope::WorkspacePattern,
                    crate::ui::tr(cx).chat().modals_allow_in_this_folder(),
                ),
                (
                    ApprovalScope::Tool,
                    crate::ui::tr(cx).chat().modals_allow_tool_always(),
                ),
            ];
            for (sc, lbl) in scopes {
                if !appr.offered_scopes.is_empty() && !appr.offered_scopes.contains(&sc) {
                    continue;
                }
                let rid = appr.run_id.clone();
                let cid = appr.call_id.clone();
                body = body.child(
                    btn_secondary(SharedString::from(format!("chat-sc-{sc:?}")), lbl).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.active_modal = None;
                            this.answer_approval_ext(
                                rid.clone(),
                                cid.clone(),
                                ApprovalDecision::Allow,
                                sc,
                                false,
                                cx,
                            );
                        }),
                    ),
                );
            }
            let footer = btn_ghost("chat-sc-close", crate::ui::tr(cx).common().close())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                }))
                .into_any_element();
            modal_dialog(
                "chat-scope-modal",
                crate::ui::tr(cx).chat().modals_choose_approval_scope(),
                440.0,
                body.into_any_element(),
                footer,
                window,
                cx,
                on_close,
            )
            .into_any_element()
        }
        ChatModal::DiffPreview { path, diff, binary } => {
            let mut lines_col = v_col().w_full().gap(px(1.0)).p(px(10.0));
            if binary {
                lines_col = lines_col.child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(crate::ui::tr(cx).chat().modals_binary_file_changed()),
                );
            } else {
                for line in diff.lines().take(400) {
                    let color = if line.starts_with('+') && !line.starts_with("+++") {
                        p.ok
                    } else if line.starts_with('-') && !line.starts_with("---") {
                        p.bad
                    } else if line.starts_with("@@") {
                        p.accent_ink
                    } else {
                        p.ink_2
                    };
                    lines_col = lines_col.child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(color))
                            .child(line.to_owned()),
                    );
                }
            }
            let body = div()
                .id("chat-diff-scroll")
                .w_full()
                .h(px(380.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.sunken))
                .overflow_y_scrollbar()
                .child(lines_col)
                .into_any_element();
            let footer = btn_primary("chat-diff-ok", crate::ui::tr(cx).common().close())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                }))
                .into_any_element();
            modal_dialog(
                "chat-diff-modal",
                format!("Diff · {path}"),
                680.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
            .into_any_element()
        }
        ChatModal::ToolOutput {
            name,
            content,
            has_more,
            ..
        } => {
            let body = v_col()
                .gap(px(8.0))
                .child(
                    div()
                        .id("chat-tout-scroll")
                        .w_full()
                        .h(px(380.0))
                        .p(px(10.0))
                        .rounded(px(8.0))
                        .bg(to_hsla(p.sunken))
                        .overflow_y_scrollbar()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(content),
                )
                .children(has_more.then(|| {
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(crate::ui::tr(cx).chat().modals_showing_first_32_kb_of())
                }))
                .into_any_element();
            let footer = btn_primary("chat-tout-close", crate::ui::tr(cx).common().close())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                }))
                .into_any_element();
            modal_dialog(
                "chat-tout-modal",
                crate::ui::tr(cx).chat().modals_tool_output(name),
                660.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
            .into_any_element()
        }
        ChatModal::EditMessage { message_id } => {
            let body = v_col()
                .gap(px(6.0))
                .child(
                    div().text_xs().text_color(to_hsla(p.muted)).child(
                        crate::ui::tr(cx)
                            .chat()
                            .modals_edit_message_and_regenerate_from(),
                    ),
                )
                .child(multiline_input(&view.modal_textarea))
                .into_any_element();
            let mid = message_id;
            let footer =
                h_row()
                    .gap(px(8.0))
                    .child(
                        btn_secondary("chat-ed-cancel", crate::ui::tr(cx).common().cancel())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_modal = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        btn_primary("chat-ed-ok", crate::ui::tr(cx).chat().modals_save_run())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm_edit_message(mid.clone(), cx);
                            })),
                    )
                    .into_any_element();
            modal_dialog(
                "chat-edit-msg-modal",
                crate::ui::tr(cx).chat().modals_edit_message(),
                520.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
            .into_any_element()
        }
    }
}
