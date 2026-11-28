//! Single-row top header bar for AI Chat: session restore button when sidebar is collapsed,
//! clickable renameable title, unified Workspace / Environment / Folder binding popover with Git
//! branch, inline Review Panel toggle (`±N`), Workflow Monitor trigger, and `⋯` menu with
//! `Compact / Normal / Verbose` density (`⌃O`), pin, export, and instructions.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::ui::{
    Anchor, Selectable, StatusShape, UiIcon, badge, btn_ghost, btn_icon, h_row, icon_xs,
    menu_divider, menu_item, menu_option, palette, pill_btn, popover, popover_header,
    section_heading, status_indicator, status_visual, text_tooltip, to_hsla, v_col,
};
use spanbay_desktop::core::agent::model::{AgentRunStatus, FileChangeStatus};
use spanbay_desktop::state::map_run_status;

use super::ChatView;
use super::reasoning::message_has_reasoning;
use super::types::{ChatModal, TranscriptDensity};
use super::welcome::context_binding_summary;

pub fn render_header(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let title = if view.thread.title.trim().is_empty() {
        ch.header_new_chat().to_owned()
    } else {
        view.thread.title.clone()
    };
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
    let wf_shape = if !view.thread.pending_approvals.is_empty()
        || !view.thread.open_gates.is_empty()
        || view.thread.cost_pause.is_some()
    {
        Some(StatusShape::Warn)
    } else if view
        .thread
        .active_run
        .as_ref()
        .is_some_and(|r| r.status == AgentRunStatus::Queued)
    {
        Some(StatusShape::Idle)
    } else if view.is_run_active() {
        Some(StatusShape::Run)
    } else {
        view.thread
            .active_run
            .as_ref()
            .map(|r| map_run_status(r.status))
            .filter(|s| s.is_warning_or_error())
            .map(|s| status_visual(s).0)
    };
    let parent_link = view
        .session
        .as_ref()
        .and_then(|s| s.parent_session_id.clone())
        .map(|parent_id| {
            let parent_title = view
                .sessions
                .iter()
                .find(|s| s.id == parent_id)
                .map(|s| s.title.trim())
                .filter(|t| !t.is_empty());
            let label = match parent_title {
                Some(t) => ch.header_back_to_parent(t),
                None => ch.header_back_to_parent_bare().to_owned(),
            };
            (parent_id, label)
        });
    let is_child_chat = parent_link.is_some();
    let title_hover_bg = to_hsla(p.hover.composite_over(p.surface));
    let attn_count = view.pending_attention_count();

    let restore_btn = view.sidebar_collapsed.then(|| {
        let label = if attn_count > 0 {
            ch.header_restore_chats_attn(attn_count)
        } else {
            ch.header_restore_chats().to_owned()
        };
        pill_btn("chat-pane-restore", label, None, attn_count > 0)
            .on_click(cx.listener(|this, _, _, cx| this.restore_sidebar(cx)))
    });

    let left_cluster = h_row()
        .flex_1()
        .min_w_0()
        .gap(px(8.0))
        .children(restore_btn)
        .children(parent_link.map(|(parent_id, label)| {
            div()
                .id("chat-hdr-parent")
                .flex_shrink_0()
                .cursor_pointer()
                .text_xs()
                .text_color(to_hsla(p.accent_ink))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_session(parent_id.clone(), cx);
                }))
                .child(SharedString::from(label))
        }))
        .children(
            is_child_chat.then(|| badge(ch.header_child_chat(), crate::ui::Tone::Neutral, cx)),
        )
        .child(
            h_row()
                .id("chat-hdr-title")
                .group("chat-hdr-title")
                .min_w(px(64.0))
                .gap(px(5.0))
                .px(px(5.0))
                .ml(px(-5.0))
                .py(px(2.0))
                .rounded(px(6.0))
                .when(can_rename, |el| {
                    el.cursor_pointer()
                        .hover(move |s| s.bg(title_hover_bg))
                        .tooltip(text_tooltip(ch.thread_header_click_to_rename()))
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
        )
        .child(
            div()
                .flex_shrink_0()
                .child(render_binding_popover(view, cx)),
        );

    let show_changes_btn = !pending_files.is_empty() || view.review_open;
    let right_cluster = h_row()
        .flex_shrink_0()
        .gap(px(5.0))
        .children(
            view.status_msg
                .clone()
                .map(|(tone, msg)| badge(msg, tone, cx)),
        )
        .children(show_changes_btn.then(|| {
            pill_btn(
                "chat-hdr-changes",
                format!(
                    "±{} +{pending_added} −{pending_removed}",
                    pending_files.len()
                ),
                Some(UiIcon::FileDiff),
                view.review_open,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.toggle_review(cx);
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
                .child(SharedString::from(ch.monitor_workflow_btn().to_owned())),
        )
        .child(render_more_menu(
            view,
            pinned,
            has_any_reasoning,
            all_reasoning_open,
            cx,
        ));

    h_row()
        .w_full()
        .h(px(40.0))
        .flex_shrink_0()
        .justify_between()
        .gap(px(10.0))
        .px(px(16.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(left_cluster)
        .child(right_cluster)
}

fn render_more_menu(
    view: &ChatView,
    pinned: bool,
    has_any_reasoning: bool,
    all_reasoning_open: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let cur_density = view.density;
    let has_session = !view.thread.session_id.is_empty();
    let chat_ent = cx.entity().clone();
    popover("chat-hdr-more-popover")
        .anchor(Anchor::TopRight)
        .trigger(
            btn_icon("chat-hdr-more", UiIcon::Ellipsis)
                .tooltip(crate::ui::tr(cx).chat().header_more_actions()),
        )
        .content(move |_, _, pop_cx| {
            let pp = palette(pop_cx);
            let pch = crate::ui::tr(pop_cx).chat();
            let mut seg = h_row()
                .w_full()
                .gap(px(2.0))
                .p(px(2.0))
                .rounded(px(6.0))
                .bg(to_hsla(pp.sunken));
            for (id, mode, label) in [
                (
                    "chat-density-compact",
                    TranscriptDensity::Compact,
                    pch.density_compact(),
                ),
                (
                    "chat-density-normal",
                    TranscriptDensity::Normal,
                    pch.density_normal(),
                ),
                (
                    "chat-density-verbose",
                    TranscriptDensity::Verbose,
                    pch.density_verbose(),
                ),
            ] {
                let (ent, pop_h) = (chat_ent.clone(), pop_cx.entity().clone());
                seg = seg.child(
                    div().flex_1().child(
                        btn_ghost(id, label)
                            .when(cur_density == mode, |b| {
                                b.bg(to_hsla(pp.surface)).text_color(to_hsla(pp.ink))
                            })
                            .on_click(move |_, w, cx| {
                                ent.update(cx, |this, cx| this.set_density(mode, cx));
                                pop_h.update(cx, |s, cx| s.dismiss(w, cx));
                            }),
                    ),
                );
            }
            let mut m = v_col()
                .w(px(236.0))
                .gap(px(4.0))
                .child(section_heading(pch.density_label(), pop_cx))
                .child(seg)
                .child(menu_divider(pop_cx));
            if has_any_reasoning {
                let (ent_r, pop_r) = (chat_ent.clone(), pop_cx.entity().clone());
                let r_lbl = if all_reasoning_open {
                    pch.thread_header_collapse_all_thinking()
                } else {
                    pch.thread_header_expand_all_thinking()
                };
                m = m.child(menu_item(
                    "chat-hdr-reasoning-all",
                    UiIcon::Brain,
                    r_lbl,
                    false,
                    pop_cx,
                    move |_, w, cx| {
                        ent_r.update(cx, |this, cx| this.toggle_all_reasoning(cx));
                        pop_r.update(cx, |s, cx| s.dismiss(w, cx));
                    },
                ));
            }
            let (ent_pin, pop_pin) = (chat_ent.clone(), pop_cx.entity().clone());
            let (pin_ico, pin_lbl) = if pinned {
                (UiIcon::PinOff, pch.sidebar_unpin())
            } else {
                (UiIcon::Pin, pch.thread_header_pin_chat())
            };
            m = m.child(menu_item(
                "chat-hdr-pin",
                pin_ico,
                pin_lbl,
                false,
                pop_cx,
                move |_, w, cx| {
                    ent_pin.update(cx, |this, cx| this.toggle_pinned(cx));
                    pop_pin.update(cx, |s, cx| s.dismiss(w, cx));
                },
            ));
            if has_session {
                let (ent_exp, pop_exp) = (chat_ent.clone(), pop_cx.entity().clone());
                m = m.child(menu_item(
                    "chat-hdr-export",
                    UiIcon::Download,
                    pch.thread_header_export_redacted_chat(),
                    false,
                    pop_cx,
                    move |_, w, cx| {
                        ent_exp.update(cx, |this, cx| this.export_chat_redacted(cx));
                        pop_exp.update(cx, |s, cx| s.dismiss(w, cx));
                    },
                ));
            }
            let (ent_inst, pop_inst) = (chat_ent.clone(), pop_cx.entity().clone());
            m.child(menu_item(
                "chat-hdr-instructions",
                UiIcon::Sliders,
                pch.thread_header_chat_instructions(),
                false,
                pop_cx,
                move |_, w, cx| {
                    ent_inst.update(cx, |this, cx| {
                        this.active_modal = Some(ChatModal::ChatInstructions);
                        cx.notify();
                    });
                    pop_inst.update(cx, |s, cx| s.dismiss(w, cx));
                },
            ))
        })
}

fn render_binding_popover(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let (label, has_any) = context_binding_summary(view, cx);
    let trigger = pill_btn(
        "chat-bind-trigger",
        SharedString::from(label),
        Some(UiIcon::FolderOpen),
        has_any,
    );
    render_binding_popover_with_trigger(view, SharedString::from("chat-bind-popover"), trigger, cx)
}

/// Binding menu (folder + SpanBay workspace + environment + instructions) behind any trigger.
pub(super) fn render_binding_popover_with_trigger<T: Selectable + IntoElement + 'static>(
    view: &ChatView,
    popover_id: SharedString,
    trigger: T,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let bound_ws_id = view.session.as_ref().map_or_else(
        || view.active_workspace_id.clone(),
        |s| s.spanbay_workspace_id.clone(),
    );
    let bound_env_id = view.session.as_ref().map_or_else(
        || view.active_environment_id.clone(),
        |s| s.spanbay_environment_id.clone(),
    );
    let workspaces = view.workspaces.clone();
    let envs: Vec<_> = view
        .environments
        .iter()
        .filter(|e| Some(e.workspace_id.as_str()) == bound_ws_id.as_deref())
        .cloned()
        .collect();
    let (chat_ent, open_ent) = (cx.entity().clone(), cx.entity().clone());
    let cur_folder = view.session.as_ref().and_then(|s| s.workspace_dir.clone());

    popover(popover_id)
        .anchor(Anchor::TopLeft)
        .trigger(trigger)
        .on_open_change(move |open, _, cx| {
            if *open {
                open_ent.update(cx, |this, cx| this.ensure_bound_workspace_envs(cx));
            }
        })
        .content(move |_, _, pop_cx| {
            let pp = palette(pop_cx);
            let pch = crate::ui::tr(pop_cx).chat();
            let (ent_f, pop_f) = (chat_ent.clone(), pop_cx.entity().clone());
            let folder_txt = cur_folder
                .clone()
                .unwrap_or_else(|| pch.thread_header_no_folder_bound().to_owned());
            let mut m = v_col()
                .w(px(280.0))
                .gap(px(6.0))
                .child(popover_header(
                    pch.thread_header_session_context(),
                    None,
                    pop_cx,
                ))
                .child(
                    v_col()
                        .gap(px(4.0))
                        .child(section_heading(pch.thread_header_local_folder(), pop_cx))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(pp.muted))
                                .truncate()
                                .child(folder_txt),
                        )
                        .child(
                            btn_ghost("chat-bind-pick-folder", pch.thread_header_choose_folder())
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
                    let (ent, pop_h) = (chat_ent.clone(), pop_cx.entity().clone());
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
                    let (eid, wid) = (env.id.clone(), wid_active.clone());
                    let sel = bound_env_id.as_deref() == Some(eid.as_str());
                    let (ent, pop_h) = (chat_ent.clone(), pop_cx.entity().clone());
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
