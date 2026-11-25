//! Top header bar for AI Chat: clickable renameable title, model badge, interactive
//! Workspace / Environment / Folder binding popover with Git branch, export, reasoning toggle,
//! pin, details inspector toggle, and safe delete.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::ui::{
    Anchor, Tone, UiIcon, badge, btn_ghost, btn_icon, h_row, icon_xs, list_row, palette, pill_btn,
    popover, popover_header, section_heading, to_hsla, v_col,
};

use super::ChatView;
use super::reasoning::message_has_reasoning;
use super::types::ChatModal;

pub fn render_header(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let title = if view.thread.title.trim().is_empty() {
        if zh { "新对话" } else { "New chat" }.to_owned()
    } else {
        view.thread.title.clone()
    };
    let model_label = view
        .session
        .as_ref()
        .and_then(|s| s.target.as_ref())
        .map(|t| view.format_target_label(t));
    let pinned = view
        .session
        .as_ref()
        .is_some_and(|s| s.pinned_at_ms.is_some());
    let reasoning_msg_ids: Vec<String> = view
        .thread
        .messages
        .iter()
        .filter(|m| message_has_reasoning(m))
        .map(|m| format!("reason-{}", m.id))
        .collect();
    let has_any_reasoning = !reasoning_msg_ids.is_empty();
    let all_reasoning_open = has_any_reasoning
        && reasoning_msg_ids
            .iter()
            .all(|k| view.expanded_reasoning.contains(k));
    let can_rename = !view.thread.session_id.is_empty();
    let rename_sid = view.thread.session_id.clone();
    let rename_title = title.clone();

    h_row()
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
                .child(
                    div()
                        .id("chat-hdr-title")
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .when(can_rename, |el| {
                            el.cursor_pointer()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.modal_input.update(cx, |s, cx| {
                                        s.set_value(rename_title.clone(), window, cx);
                                    });
                                    this.active_modal = Some(ChatModal::RenameSession {
                                        session_id: rename_sid.clone(),
                                    });
                                    cx.notify();
                                }))
                        })
                        .child(SharedString::from(title)),
                )
                .children(model_label.map(|m| badge(m, Tone::Neutral, cx)))
                .child(render_binding_popover(view, zh, cx)),
        )
        .child(
            h_row()
                .gap(px(4.0))
                .children(
                    view.status_msg
                        .clone()
                        .map(|(tone, msg)| badge(msg, tone, cx)),
                )
                .children(has_any_reasoning.then(|| {
                    btn_icon("chat-hdr-reasoning-all", UiIcon::Brain)
                        .tooltip(if all_reasoning_open {
                            if zh {
                                "收起全部思考过程"
                            } else {
                                "Collapse all thinking"
                            }
                        } else if zh {
                            "展开全部思考过程"
                        } else {
                            "Expand all thinking"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_all_reasoning(cx)))
                }))
                .children((!view.thread.session_id.is_empty()).then(|| {
                    btn_icon("chat-hdr-export", UiIcon::Download)
                        .tooltip(if zh {
                            "导出脱敏对话 Markdown"
                        } else {
                            "Export redacted chat"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.export_chat_redacted(cx)))
                }))
                .child(
                    btn_icon(
                        "chat-hdr-pin",
                        if pinned { UiIcon::PinOff } else { UiIcon::Pin },
                    )
                    .tooltip(if pinned {
                        if zh { "取消置顶" } else { "Unpin" }
                    } else if zh {
                        "置顶对话"
                    } else {
                        "Pin chat"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_pinned(cx))),
                )
                .child(
                    btn_icon(
                        "chat-hdr-details",
                        if view.details_open {
                            UiIcon::PanelRightClose
                        } else {
                            UiIcon::PanelRightOpen
                        },
                    )
                    .tooltip(if zh {
                        "对话详情与变更"
                    } else {
                        "Thread details & changes"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.details_open = !this.details_open;
                        cx.notify();
                    })),
                )
                .child(
                    btn_icon("chat-hdr-del", UiIcon::Trash)
                        .tooltip(if zh { "删除对话" } else { "Delete chat" })
                        .on_click(cx.listener(|this, _, _, cx| this.request_delete_current(cx))),
                ),
        )
}

fn render_binding_popover(
    view: &ChatView,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let folder_name = view
        .session
        .as_ref()
        .and_then(|s| s.workspace_dir.as_deref())
        .and_then(|d| std::path::Path::new(d).file_name())
        .and_then(|n| n.to_str())
        .map(str::to_owned);
    let git_branch = view
        .workspace_info
        .as_ref()
        .and_then(|w| w.git_branch.clone());
    let bound_ws_id = view
        .session
        .as_ref()
        .and_then(|s| s.pingdeck_workspace_id.clone());
    let bound_env_id = view
        .session
        .as_ref()
        .and_then(|s| s.pingdeck_environment_id.clone());
    let ws_name = bound_ws_id.as_deref().and_then(|id| {
        view.workspaces
            .iter()
            .find(|w| w.id.as_str() == id)
            .map(|w| w.name.clone())
    });

    let label = match (&folder_name, &git_branch, &ws_name) {
        (Some(f), Some(br), _) => format!("{f} ({br})"),
        (Some(f), None, _) => f.clone(),
        (None, _, Some(w)) => w.clone(),
        _ => if zh {
            "绑定目录 / 工作区"
        } else {
            "Bind folder / workspace"
        }
        .to_owned(),
    };
    let workspaces = view.workspaces.clone();
    let envs = view.environments.clone();
    let chat_ent = cx.entity().clone();
    let cur_folder = view.session.as_ref().and_then(|s| s.workspace_dir.clone());

    popover("chat-bind-popover")
        .anchor(Anchor::TopLeft)
        .trigger(pill_btn(
            "chat-bind-trigger",
            SharedString::from(label),
            Some(UiIcon::FolderOpen),
            folder_name.is_some() || bound_ws_id.is_some(),
        ))
        .content(move |_, _, pop_cx| {
            let pp = palette(pop_cx);
            let mut m = v_col().w(px(280.0)).gap(px(6.0)).child(popover_header(
                if zh {
                    "上下文绑定"
                } else {
                    "Session Context"
                },
                None,
                pop_cx,
            ));

            let ent_f = chat_ent.clone();
            let pop_f = pop_cx.entity().clone();
            m = m.child(
                v_col()
                    .gap(px(4.0))
                    .child(section_heading(
                        if zh { "本地目录" } else { "Local folder" },
                        pop_cx,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(pp.muted))
                            .truncate()
                            .child(cur_folder.clone().unwrap_or_else(|| {
                                if zh {
                                    "未绑定目录"
                                } else {
                                    "No folder bound"
                                }
                                .to_owned()
                            })),
                    )
                    .child(
                        btn_ghost(
                            "chat-bind-pick-folder",
                            if zh {
                                "选择本地文件夹…"
                            } else {
                                "Choose folder…"
                            },
                        )
                        .icon(icon_xs(UiIcon::FolderOpen))
                        .on_click(move |_, w, cx| {
                            ent_f.update(cx, |this, cx| this.pick_bound_folder(cx));
                            pop_f.update(cx, |s, cx| s.dismiss(w, cx));
                        }),
                    ),
            );

            if !workspaces.is_empty() {
                m = m.child(section_heading(
                    if zh {
                        "PingDeck 工作区"
                    } else {
                        "PingDeck Workspace"
                    },
                    pop_cx,
                ));
                for ws in &workspaces {
                    let wid = ws.id.as_str().to_owned();
                    let sel = bound_ws_id.as_deref() == Some(wid.as_str());
                    let ent = chat_ent.clone();
                    let pop_h = pop_cx.entity().clone();
                    let eid = bound_env_id.clone();
                    m = m.child(
                        list_row(
                            SharedString::from(format!("chat-bws-{wid}")),
                            sel,
                            pop_cx,
                            move |_, w, cx| {
                                ent.update(cx, |this, cx| {
                                    this.bind_workspace_env(wid.clone(), eid.clone(), cx);
                                });
                                pop_h.update(cx, |s, cx| s.dismiss(w, cx));
                            },
                        )
                        .justify_between()
                        .child(div().text_xs().child(ws.name.clone()))
                        .children(sel.then(|| icon_xs(UiIcon::Check))),
                    );
                }
            }
            if let Some(wid_active) = &bound_ws_id
                && !envs.is_empty()
            {
                m = m.child(section_heading(
                    if zh { "绑定环境" } else { "Environment" },
                    pop_cx,
                ));
                for env in &envs {
                    let eid = env.id.clone();
                    let sel = bound_env_id.as_deref() == Some(eid.as_str());
                    let ent = chat_ent.clone();
                    let pop_h = pop_cx.entity().clone();
                    let wid = wid_active.clone();
                    m = m.child(
                        list_row(
                            SharedString::from(format!("chat-benv-{eid}")),
                            sel,
                            pop_cx,
                            move |_, w, cx| {
                                ent.update(cx, |this, cx| {
                                    this.bind_workspace_env(wid.clone(), Some(eid.clone()), cx);
                                });
                                pop_h.update(cx, |s, cx| s.dismiss(w, cx));
                            },
                        )
                        .justify_between()
                        .child(div().text_xs().child(env.name.clone()))
                        .children(sel.then(|| icon_xs(UiIcon::Check))),
                    );
                }
            }
            m
        })
}
