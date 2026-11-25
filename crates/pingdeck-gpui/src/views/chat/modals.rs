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
    zh: bool,
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
                    if zh {
                        "完全权限模式会自动执行文件修改与常规 Shell 命令而不再逐条询问。"
                    } else {
                        "Full access auto-runs edits and regular shell commands without asking."
                    },
                    None,
                    cx,
                ))
                .child(div().text_xs().text_color(to_hsla(p.ink_2)).child(if zh {
                    "硬阻止列表、用户禁止规则、强制询问边界与 OS 沙箱仍会保持开启，不会被关闭。"
                } else {
                    "Hard blocks, Block rules, forced-ask gates, and the OS sandbox stay enabled."
                }))
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
                        .child(div().text_xs().text_color(to_hsla(p.ink)).child(if zh {
                            "我已了解风险，仅为当前对话开启完全权限"
                        } else {
                            "I understand — enable Full access for this chat"
                        })),
                )
                .child(
                    btn_ghost(
                        "chat-fa-eval-link",
                        if zh {
                            "查看 6 层权限评估顺序 ↗"
                        } else {
                            "View 6-layer evaluation order ↗"
                        },
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
                if zh {
                    "开启完全权限"
                } else {
                    "Enable Full access"
                },
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
                    btn_secondary("chat-fa-cancel", if zh { "取消" } else { "Cancel" }).on_click(
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
                if zh {
                    "确认开启完全权限"
                } else {
                    "Enable Full Access?"
                },
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
                .child(if zh {
                    format!("对话「{title}」仍有任务正在运行。删除会先中断运行并移入最近删除，确认继续吗？")
                } else {
                    format!("Chat \"{title}\" has an active run. Deleting stops the run and moves it to Trash.")
                })
                .into_any_element();
            let sid_del = session_id;
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("chat-del-cancel", if zh { "取消" } else { "Cancel" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_danger(
                        "chat-del-confirm",
                        if zh {
                            "停止并删除"
                        } else {
                            "Stop & Delete"
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active_modal = None;
                        this.delete_session_by_id(sid_del.clone(), cx);
                    })),
                )
                .into_any_element();
            modal_dialog(
                "chat-del-running-modal",
                if zh {
                    "删除运行中的对话"
                } else {
                    "Delete Running Chat?"
                },
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
                .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                    "对话标题"
                } else {
                    "Chat title"
                }))
                .child(text_input(&view.modal_input))
                .into_any_element();
            let sid = session_id;
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("chat-rn-cancel", if zh { "取消" } else { "Cancel" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary("chat-rn-save", if zh { "保存" } else { "Save" }).on_click(
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
                if zh { "重命名对话" } else { "Rename Chat" },
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
                            if zh { "匹配规则" } else { "Rule" },
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
                    if zh {
                        "仅允许本次调用"
                    } else {
                        "Allow once"
                    },
                ),
                (
                    ApprovalScope::Session,
                    if zh {
                        "本对话内允许"
                    } else {
                        "Allow in this chat"
                    },
                ),
                (
                    ApprovalScope::WorkspacePattern,
                    if zh {
                        "本目录内记住此规则"
                    } else {
                        "Allow in this folder"
                    },
                ),
                (
                    ApprovalScope::Tool,
                    if zh {
                        "全局始终允许此工具"
                    } else {
                        "Allow tool always"
                    },
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
            let footer = btn_ghost("chat-sc-close", if zh { "关闭" } else { "Close" })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                }))
                .into_any_element();
            modal_dialog(
                "chat-scope-modal",
                if zh {
                    "选择允许范围"
                } else {
                    "Choose Approval Scope"
                },
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
                lines_col =
                    lines_col.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                        "二进制文件已修改，无法以文本 Diff 预览。"
                    } else {
                        "Binary file changed."
                    }));
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
            let footer = btn_primary("chat-diff-ok", if zh { "关闭" } else { "Close" })
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
                    div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                        "仅展示前 32 KB 输出"
                    } else {
                        "Showing first 32 KB of output"
                    })
                }))
                .into_any_element();
            let footer = btn_primary("chat-tout-close", if zh { "关闭" } else { "Close" })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                }))
                .into_any_element();
            modal_dialog(
                "chat-tout-modal",
                if zh {
                    format!("工具输出 · {name}")
                } else {
                    format!("Tool Output · {name}")
                },
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
                .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                    "编辑该条消息并从此处重新生成回复"
                } else {
                    "Edit message and regenerate from this turn"
                }))
                .child(multiline_input(&view.modal_textarea))
                .into_any_element();
            let mid = message_id;
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("chat-ed-cancel", if zh { "取消" } else { "Cancel" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary(
                        "chat-ed-ok",
                        if zh {
                            "保存并重新生成"
                        } else {
                            "Save & Run"
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_edit_message(mid.clone(), cx);
                    })),
                )
                .into_any_element();
            modal_dialog(
                "chat-edit-msg-modal",
                if zh { "编辑消息" } else { "Edit Message" },
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
