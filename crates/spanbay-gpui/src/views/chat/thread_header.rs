//! Top header bar for AI Chat: clickable renameable title, model badge, interactive
//! Workspace / Environment / Folder binding popover with Git branch, export, reasoning toggle,
//! pin, and Workflow Monitor modal trigger.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::ui::{
    Anchor, StatusShape, UiIcon, badge, btn_ghost, btn_icon, h_row, icon_xs, menu_divider,
    menu_item, menu_option, palette, pill_btn, popover, popover_header, section_heading,
    status_indicator, text_tooltip, to_hsla, v_col,
};
use crate::views::runs::WorkflowTab;
use spanbay_desktop::core::agent::model::{AgentRunStatus, AgentTodoStatus, FileChangeStatus};

use super::ChatView;
use super::reasoning::message_has_reasoning;
use super::types::ChatModal;

pub fn render_header(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let title = if view.thread.title.trim().is_empty() {
        crate::ui::tr(cx).chat().header_new_chat().to_owned()
    } else {
        view.thread.title.clone()
    };
    let model_label = view
        .session
        .as_ref()
        .and_then(|s| s.target.as_ref())
        .map(|t| view.format_target_label(t))
        .or_else(|| {
            view.available_models
                .first()
                .map(|m| crate::ui::tr(cx).chat().composer_auto(m.display_label()))
        });
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
    let has_workflow = !view.thread.session_id.is_empty()
        && (view.is_run_active()
            || view.thread.active_run.is_some()
            || view.latest_session_run_id.is_some());
    let pending_files: Vec<_> = view
        .persisted_changes
        .iter()
        .filter(|c| {
            matches!(
                c.status,
                FileChangeStatus::Pending | FileChangeStatus::Conflict
            )
        })
        .collect();
    let pending_added: u32 = pending_files.iter().map(|c| c.added).sum();
    let pending_removed: u32 = pending_files.iter().map(|c| c.removed).sum();
    let wf_shape =
        if !view.thread.pending_approvals.is_empty() || !view.thread.open_gates.is_empty() {
            Some(StatusShape::Warn)
        } else if view.is_run_active() {
            Some(StatusShape::Run)
        } else if view.thread.active_run.as_ref().is_some_and(|r| {
            matches!(
                r.status,
                AgentRunStatus::Failed | AgentRunStatus::Interrupted
            )
        }) {
            Some(StatusShape::Bad)
        } else {
            None
        };
    let todo_done = view
        .thread
        .todos
        .iter()
        .filter(|t| t.status == AgentTodoStatus::Completed)
        .count();
    let todo_total = view
        .thread
        .todos
        .iter()
        .filter(|t| t.status != AgentTodoStatus::Cancelled)
        .count();
    let ch = crate::ui::tr(cx).chat();
    let wf_label = if todo_total > 0 && view.is_run_active() {
        format!("{} {todo_done}/{todo_total}", ch.monitor_workflow_btn())
    } else {
        ch.monitor_workflow_btn().to_owned()
    };

    let title_hover_bg = to_hsla(p.hover.composite_over(p.surface));
    let title_row = h_row()
        .w_full()
        .justify_between()
        .gap(px(10.0))
        .child(
            // Non-clickable spacer fills remaining width so clicking empty header whitespace
            // never triggers rename; the clickable trigger inside shrinks tightly to text width.
            h_row().flex_1().min_w_0().child(
                h_row()
                    .id("chat-hdr-title")
                    .group("chat-hdr-title")
                    .max_w_full()
                    .min_w_0()
                    .gap(px(5.0))
                    .px(px(5.0))
                    .ml(px(-5.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .when(can_rename, |el| {
                        el.cursor_pointer()
                            .hover(move |s| s.bg(title_hover_bg))
                            .tooltip(text_tooltip(
                                crate::ui::tr(cx).chat().thread_header_click_to_rename(),
                            ))
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
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(title)),
                    )
                    .children(can_rename.then(|| {
                        div()
                            .flex_shrink_0()
                            .text_color(to_hsla(p.faint))
                            .opacity(0.0)
                            .group_hover("chat-hdr-title", |s| s.opacity(1.0))
                            .child(icon_xs(UiIcon::Pencil))
                    })),
            ),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(4.0))
                .children(
                    view.status_msg
                        .clone()
                        .map(|(tone, msg)| badge(msg, tone, cx)),
                )
                .children(has_any_reasoning.then(|| {
                    btn_icon("chat-hdr-reasoning-all", UiIcon::Brain)
                        .tooltip(if all_reasoning_open {
                            crate::ui::tr(cx)
                                .chat()
                                .thread_header_collapse_all_thinking()
                        } else {
                            crate::ui::tr(cx).chat().thread_header_expand_all_thinking()
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_all_reasoning(cx)))
                }))
                .children((!view.thread.session_id.is_empty()).then(|| {
                    btn_icon("chat-hdr-export", UiIcon::Download)
                        .tooltip(
                            crate::ui::tr(cx)
                                .chat()
                                .thread_header_export_redacted_chat(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.export_chat_redacted(cx)))
                }))
                .child(
                    btn_icon(
                        "chat-hdr-pin",
                        if pinned { UiIcon::PinOff } else { UiIcon::Pin },
                    )
                    .tooltip(if pinned {
                        crate::ui::tr(cx).chat().sidebar_unpin()
                    } else {
                        crate::ui::tr(cx).chat().thread_header_pin_chat()
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_pinned(cx))),
                )
                .children((!pending_files.is_empty()).then(|| {
                    pill_btn(
                        "chat-hdr-changes",
                        format!(
                            "±{} +{pending_added} -{pending_removed}",
                            pending_files.len()
                        ),
                        Some(UiIcon::FileDiff),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_workflow_modal(None, Some(WorkflowTab::Changes), cx);
                    }))
                }))
                .child(
                    h_row()
                        .id("chat-hdr-workflow")
                        .gap(px(5.0))
                        .px(px(8.0))
                        .py(px(3.5))
                        .rounded(px(6.0))
                        .bg(to_hsla(if wf_shape.is_some() {
                            p.accent_soft
                        } else {
                            p.sunken
                        }))
                        .border_1()
                        .border_color(to_hsla(if wf_shape == Some(StatusShape::Warn) {
                            p.warn_line.composite_over(p.accent_soft)
                        } else if wf_shape == Some(StatusShape::Bad) {
                            p.bad_line.composite_over(p.accent_soft)
                        } else if wf_shape.is_some() {
                            p.accent_line.composite_over(p.accent_soft)
                        } else {
                            p.line.composite_over(p.sunken)
                        }))
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(if !has_workflow {
                            p.faint
                        } else if wf_shape.is_some() {
                            p.accent_ink
                        } else {
                            p.ink
                        }))
                        .tooltip(text_tooltip(if has_workflow {
                            ch.monitor_workflow_tooltip()
                        } else {
                            ch.monitor_workflow_empty_tooltip()
                        }))
                        .when(has_workflow, |el| {
                            el.cursor_pointer()
                                .hover(|s| s.border_color(to_hsla(p.accent)))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.open_workflow_modal(None, None, cx);
                                }))
                        })
                        .child(match wf_shape {
                            Some(shape) => status_indicator(shape, cx).into_any_element(),
                            None => icon_xs(UiIcon::Activity).into_any_element(),
                        })
                        .child(SharedString::from(wf_label)),
                ),
        );

    let meta_row = h_row()
        .w_full()
        .gap(px(8.0))
        .flex_wrap()
        .children(model_label.map(|m| {
            h_row()
                .gap(px(5.0))
                .px(px(7.0))
                .py(px(2.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.sunken)))
                .max_w(px(280.0))
                .child(
                    div()
                        .text_color(to_hsla(p.muted))
                        .flex_shrink_0()
                        .child(icon_xs(UiIcon::Cpu)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .truncate()
                        .child(SharedString::from(m)),
                )
        }))
        .child(render_binding_popover(view, cx));

    v_col()
        .w_full()
        .flex_shrink_0()
        .gap(px(5.0))
        .px(px(16.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(title_row)
        .child(meta_row)
}

fn render_binding_popover(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
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
    let bound_ws_id = match &view.session {
        Some(s) => s.spanbay_workspace_id.clone(),
        None => view.active_workspace_id.clone(),
    };
    let bound_env_id = match &view.session {
        Some(s) => s.spanbay_environment_id.clone(),
        None => view.active_environment_id.clone(),
    };
    let ws_name = bound_ws_id
        .as_deref()
        .and_then(|id| {
            view.workspaces
                .iter()
                .find(|w| w.id.as_str() == id)
                .map(|w| w.name.clone())
        })
        .or_else(|| {
            view.session
                .as_ref()
                .and_then(|s| s.spanbay_workspace_name.clone())
        });

    let env_name = bound_env_id.as_deref().and_then(|id| {
        view.environments
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.name.clone())
    });

    let mut parts: Vec<String> = Vec::new();
    match (&folder_name, &git_branch) {
        (Some(f), Some(br)) => parts.push(format!("{f} ({br})")),
        (Some(f), None) => parts.push(f.clone()),
        _ => {}
    }
    if let Some(w) = &ws_name {
        if let Some(e) = &env_name {
            parts.push(format!("{w} · {e}"));
        } else {
            parts.push(w.clone());
        }
    }
    let label = if parts.is_empty() {
        crate::ui::tr(cx)
            .chat()
            .thread_header_bind_folder_workspace()
            .to_owned()
    } else {
        parts.join(" · ")
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
                crate::ui::tr(pop_cx).chat().thread_header_session_context(),
                None,
                pop_cx,
            ));

            let ent_f = chat_ent.clone();
            let pop_f = pop_cx.entity().clone();
            m = m.child(
                v_col()
                    .gap(px(4.0))
                    .child(section_heading(
                        crate::ui::tr(pop_cx).chat().thread_header_local_folder(),
                        pop_cx,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(pp.muted))
                            .truncate()
                            .child(cur_folder.clone().unwrap_or_else(|| {
                                crate::ui::tr(pop_cx)
                                    .chat()
                                    .thread_header_no_folder_bound()
                                    .to_owned()
                            })),
                    )
                    .child(
                        btn_ghost(
                            "chat-bind-pick-folder",
                            crate::ui::tr(pop_cx).chat().thread_header_choose_folder(),
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
                    crate::ui::tr(pop_cx)
                        .chat()
                        .thread_header_spanbay_workspace(),
                    pop_cx,
                ));
                for ws in &workspaces {
                    let wid = ws.id.as_str().to_owned();
                    let sel = bound_ws_id.as_deref() == Some(wid.as_str());
                    let ent = chat_ent.clone();
                    let pop_h = pop_cx.entity().clone();
                    let eid = sel.then(|| bound_env_id.clone()).flatten();
                    m = m.child(
                        menu_option(
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
                        .children(sel.then(|| {
                            div()
                                .text_color(to_hsla(pp.accent))
                                .child(icon_xs(UiIcon::Check))
                        })),
                    );
                }
            }
            if let Some(wid_active) = &bound_ws_id
                && !envs.is_empty()
            {
                m = m.child(section_heading(
                    crate::ui::tr(pop_cx).chat().thread_header_environment(),
                    pop_cx,
                ));
                for env in &envs {
                    let eid = env.id.clone();
                    let sel = bound_env_id.as_deref() == Some(eid.as_str());
                    let ent = chat_ent.clone();
                    let pop_h = pop_cx.entity().clone();
                    let wid = wid_active.clone();
                    m = m.child(
                        menu_option(
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
                        .children(sel.then(|| {
                            div()
                                .text_color(to_hsla(pp.accent))
                                .child(icon_xs(UiIcon::Check))
                        })),
                    );
                }
            }
            let ent_inst = chat_ent.clone();
            let pop_inst = pop_cx.entity().clone();
            m = m.child(menu_divider(pop_cx)).child(menu_item(
                "chat-bind-instructions",
                UiIcon::Sliders,
                crate::ui::tr(pop_cx)
                    .chat()
                    .thread_header_chat_instructions(),
                false,
                pop_cx,
                move |_, w, cx| {
                    ent_inst.update(cx, |this, cx| {
                        this.active_modal = Some(ChatModal::ChatInstructions);
                        cx.notify();
                    });
                    pop_inst.update(cx, |s, cx| s.dismiss(w, cx));
                },
            ));
            m
        })
}
