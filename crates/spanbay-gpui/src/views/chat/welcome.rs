//! Welcome empty state with clickable prompt starter cards for a fresh AI Chat thread.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::model::{AgentTarget, FileChangeStatus};
use spanbay_desktop::services::clipboard;

use crate::ui::{
    Disableable as _, ScrollableElement as _, Tone, UiIcon, app_icon, btn_danger, btn_ghost,
    btn_icon, btn_primary, btn_secondary, h_row, icon_xs, mono_font_family, mono_font_size,
    palette, to_hsla, v_col,
};

use super::ChatView;
use super::review_actions::GroupedFileChange;
use super::types::{ProviderChatMeta, ReviewToolOutput};

pub fn render_welcome_empty(cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let starters: [(&str, &str, UiIcon); 3] = [
        (
            ch.starter_debug_title(),
            ch.starter_debug_prompt(),
            UiIcon::Send,
        ),
        (
            ch.starter_arch_title(),
            ch.starter_arch_prompt(),
            UiIcon::FolderOpen,
        ),
        (
            ch.starter_tests_title(),
            ch.starter_tests_prompt(),
            UiIcon::ListChecks,
        ),
    ];

    let mut grid = h_row().flex_wrap().gap(px(10.0)).pt(px(8.0));
    for (i, (title, prompt, glyph)) in starters.into_iter().enumerate() {
        grid = grid.child(
            div()
                .id(SharedString::from(format!("chat-starter-{i}")))
                .flex_1()
                .min_w(px(200.0))
                .p(px(12.0))
                .rounded(px(10.0))
                .bg(to_hsla(p.pane))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .cursor_pointer()
                .hover(|s| s.border_color(to_hsla(p.accent_line.composite_over(p.pane))))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_starter(prompt, window, cx);
                }))
                .child(
                    v_col()
                        .gap(px(6.0))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .child(div().text_color(to_hsla(p.accent)).child(icon_xs(glyph)))
                                .child(title),
                        )
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(prompt)),
                ),
        );
    }

    v_col()
        .id("chat-welcome-state")
        .w_full()
        .items_start()
        .pt(px(36.0))
        .gap(px(10.0))
        .child(
            h_row().gap(px(10.0)).child(app_icon(32.0)).child(
                div()
                    .text_base()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink))
                    .child(ch.welcome_how_can_spanbay_help()),
            ),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .max_w(px(680.0))
                .child(ch.welcome_native_selectable_streaming_markdown_with()),
        )
        .child(div().w_full().pt(px(6.0)).child(grid))
}

pub(super) fn context_binding_summary(view: &ChatView, cx: &gpui_kit::App) -> (String, bool) {
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
    let bound_ws_id = view.session.as_ref().map_or_else(
        || view.active_workspace_id.clone(),
        |s| s.spanbay_workspace_id.clone(),
    );
    let bound_env_id = view.session.as_ref().map_or_else(
        || view.active_environment_id.clone(),
        |s| s.spanbay_environment_id.clone(),
    );
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
            .find(|e| Some(e.workspace_id.as_str()) == bound_ws_id.as_deref() && e.id == id)
            .map(|e| e.name.clone())
    });
    let mut parts: Vec<String> = Vec::new();
    if let Some(w) = &ws_name {
        parts.push(
            env_name
                .as_ref()
                .map_or_else(|| w.clone(), |e| format!("{w} · {e}")),
        );
    }
    match (&folder_name, &git_branch) {
        (Some(f), Some(br)) => parts.push(format!("{f} ({br})")),
        (Some(f), None) => parts.push(f.clone()),
        _ => {}
    }
    let has_any = folder_name.is_some() || bound_ws_id.is_some();
    let label = if parts.is_empty() {
        crate::ui::tr(cx)
            .chat()
            .thread_header_bind_folder_workspace()
            .to_owned()
    } else {
        parts.join(" · ")
    };
    (label, has_any)
}

pub(super) fn render_undo_precheck(
    view: &ChatView,
    groups: &[GroupedFileChange],
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let safe_ids: Vec<String> = groups
        .iter()
        .filter(|g| g.status == FileChangeStatus::Pending)
        .flat_map(|g| g.active_ids.iter().cloned())
        .collect();
    let conflict_count = groups
        .iter()
        .filter(|g| g.status == FileChangeStatus::Conflict)
        .count();
    let safe_count = groups
        .iter()
        .filter(|g| g.status == FileChangeStatus::Pending)
        .count();
    let all_ids: Vec<String> = groups
        .iter()
        .filter(|g| {
            matches!(
                g.status,
                FileChangeStatus::Pending | FileChangeStatus::Conflict
            )
        })
        .flat_map(|g| g.change_ids.iter().cloned())
        .collect();
    let untracked_count = view
        .thread
        .notices
        .iter()
        .filter(|n| n.code.as_deref() == Some("untracked_changes"))
        .count();
    v_col()
        .id("chat-rv-undo-precheck")
        .w_full()
        .gap(px(6.0))
        .p(px(10.0))
        .bg(to_hsla(p.warn_soft.composite_over(p.surface)))
        .border_b_1()
        .border_color(to_hsla(p.warn_line.composite_over(p.surface)))
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ink))
                .child(ch.rv_undo_precheck_title()),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.ok))
                .child(ch.rv_undo_safe(safe_count)),
        )
        .children((conflict_count > 0).then(|| {
            div()
                .text_xs()
                .text_color(to_hsla(p.bad))
                .child(ch.rv_undo_conflict(conflict_count))
        }))
        .children((untracked_count > 0).then(|| {
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(ch.rv_undo_ignored(untracked_count))
        }))
        .child(
            h_row()
                .justify_end()
                .gap(px(6.0))
                .child(
                    btn_ghost("chat-rv-undo-cancel", crate::ui::tr(cx).common().cancel()).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.review_undo_precheck = false;
                            cx.notify();
                        }),
                    ),
                )
                .children((conflict_count > 0).then(|| {
                    let ids_force = all_ids.clone();
                    btn_danger(
                        "chat-rv-undo-force",
                        ch.rv_undo_force_all(safe_count + conflict_count),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.undo_changes_with_force(ids_force.clone(), true, cx);
                    }))
                }))
                .child(
                    btn_primary("chat-rv-undo-safe", ch.rv_undo_safe_only(safe_count))
                        .disabled(safe_ids.is_empty())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.undo_changes_with_force(safe_ids.clone(), false, cx);
                        })),
                ),
        )
        .into_any_element()
}

pub(super) fn render_tool_output_panel(
    out: &ReviewToolOutput,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mono = mono_font_family(cx);
    let code_px = (f32::from(mono_font_size(cx)) - 1.0).clamp(11.0, 14.0);
    let content_cp = out.content.clone();
    let cid_more = out.call_id.clone();
    let next_off = out.next_offset;
    let has_more = out.has_more;

    v_col()
        .id("chat-review-panel")
        .size_full()
        .min_w_0()
        .bg(to_hsla(p.surface))
        .border_l_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .w_full()
                .h(px(40.0))
                .flex_shrink_0()
                .justify_between()
                .gap(px(6.0))
                .px(px(12.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(ch.modals_tool_output(&out.name)),
                )
                .child(
                    h_row()
                        .gap(px(4.0))
                        .child(
                            btn_ghost("chat-tout-copy", crate::ui::tr(cx).common().copy())
                                .icon(icon_xs(UiIcon::Copy))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let _ = clipboard::set_text(&content_cp);
                                    this.status_msg = Some((
                                        Tone::Success,
                                        crate::ui::tr(cx).common().copied().into(),
                                    ));
                                    cx.notify();
                                })),
                        )
                        .children(has_more.then(|| {
                            btn_secondary("chat-tout-more", ch.trace_load_more()).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.load_more_tool_output(cid_more.clone(), next_off, cx);
                                }),
                            )
                        }))
                        .child(
                            btn_icon("chat-rv-close", UiIcon::Close)
                                .tooltip(crate::ui::tr(cx).common().close())
                                .on_click(cx.listener(|this, _, _, cx| this.close_review(cx))),
                        ),
                ),
        )
        .child(
            v_col()
                .id("chat-tout-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .p(px(12.0))
                .gap(px(8.0))
                .overflow_y_scrollbar()
                .font_family(mono)
                .text_size(px(code_px))
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(out.content.clone()))
                .children(has_more.then(|| {
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(ch.modals_showing_first_32_kb_of())
                })),
        )
        .into_any_element()
}

impl ChatView {
    #[must_use]
    pub fn format_provider_model(&self, provider_id: &str, model_id: &str) -> String {
        let pname = self
            .provider_meta
            .get(provider_id)
            .map(|m| m.name.as_str())
            .unwrap_or(provider_id);
        format!("{pname} / {model_id}")
    }

    #[must_use]
    pub fn format_target_label(&self, target: &AgentTarget) -> String {
        match target {
            AgentTarget::Pinned {
                provider_id, model, ..
            } => self.format_provider_model(provider_id, model),
            AgentTarget::Named { name } => self.format_raw_model_label(name),
        }
    }

    #[must_use]
    pub fn format_raw_model_label(&self, raw: &str) -> String {
        if let Some((pid, mid)) = raw.split_once('/') {
            self.format_provider_model(pid, mid)
        } else {
            raw.to_owned()
        }
    }

    #[must_use]
    pub fn provider_meta_for_raw(&self, raw: Option<&str>) -> Option<&ProviderChatMeta> {
        if let Some((pid, _)) = raw.and_then(|r| r.split_once('/'))
            && let Some(m) = self.provider_meta.get(pid)
        {
            return Some(m);
        }
        match self.session.as_ref().and_then(|s| s.target.as_ref()) {
            Some(AgentTarget::Pinned { provider_id, .. }) => self.provider_meta.get(provider_id),
            Some(AgentTarget::Named { name }) => name
                .split_once('/')
                .and_then(|(pid, _)| self.provider_meta.get(pid)),
            None => self
                .available_models
                .first()
                .and_then(|m| self.provider_meta.get(&m.provider_id)),
        }
    }

    #[must_use]
    pub fn active_provider_missing_reasoning_quirk(&self) -> bool {
        self.provider_meta_for_raw(None)
            .is_some_and(ProviderChatMeta::missing_reasoning_quirk)
    }
}
