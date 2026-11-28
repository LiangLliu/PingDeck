//! Cost Limit Pause card and Queued Turns bar docked above the Chat composer.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Div, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use spanbay_desktop::core::agent::model::{ApprovalDecision, ApprovalScope};
use spanbay_desktop::state::{GraphNodeStatus, LiveApproval, LiveCostPause};

use crate::ui::{
    Disableable as _, Tone, WaitKind, alert_banner, btn_primary, btn_secondary, h_row,
    modal_dialog, mono_font_family, palette, progress_bar, status_badge, to_hsla, v_col,
};

use super::ChatView;
use super::approval_card::render_preview;
use super::metrics::{format_money, format_tokens_int};
use super::types::{ChatModal, LastApprovalNotice};
pub fn render_cost_pause(pause: &LiveCostPause, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let rid_go = pause.run_id.clone();
    let rid_stop = pause.run_id.clone();
    let timeout_m = (crate::bridge::runtime(cx)
        .state()
        .settings()
        .ai
        .agent
        .approval_timeout_s
        / 60)
        .max(1);
    let mono = mono_font_family(cx);
    let amount = |micro: i64| {
        if pause.is_tokens {
            format_tokens_int(u64::try_from(micro).unwrap_or(0))
        } else {
            format_money(micro, &pause.currency)
        }
    };
    let desc = if pause.is_tokens {
        ch.cost_tokens_used(amount(pause.cost_micro), amount(pause.cap_micro))
    } else {
        format!("{} / {}", amount(pause.cost_micro), amount(pause.cap_micro))
    };
    // Where the money went, what continuing means, and what the cap cannot see.
    let mut details: Vec<gpui_kit::Div> = Vec::new();
    for share in &pause.breakdown {
        let part = match share.part.as_str() {
            "main" => ch.cost_part_main(),
            "subagent" => ch.cost_part_subagent(),
            "compaction" => ch.cost_part_compaction(),
            _ => continue,
        };
        let pct = if pause.cost_micro > 0 {
            ((share.micro.max(0) as f64 / pause.cost_micro as f64) * 100.0).clamp(0.0, 100.0) as f32
        } else {
            0.0
        };
        details.push(
            h_row()
                .w_full()
                .gap(px(8.0))
                .text_xs()
                .child(
                    div()
                        .w(px(84.0))
                        .flex_shrink_0()
                        .text_color(to_hsla(p.muted))
                        .child(part),
                )
                .child(div().flex_1().min_w_0().child(progress_bar(
                    SharedString::from(format!("cost-share-{}", share.part)),
                    pct,
                )))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(mono.clone())
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(amount(share.micro))),
                ),
        );
    }
    if let Some(next) = pause.next_cap_micro {
        details.push(
            div()
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(ch.cost_next_cap(amount(next))),
        );
    }
    if !pause.unpriced_models.is_empty() {
        details.push(
            div()
                .text_xs()
                .text_color(to_hsla(p.warn))
                .child(ch.cost_unpriced_models(
                    pause.unpriced_models.len(),
                    pause.unpriced_models.join(ch.cost_list_sep()),
                )),
        );
    }
    v_col().w_full().child(
        v_col()
            .p(px(12.0))
            .gap(px(8.0))
            .child(
                h_row()
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(status_badge(
                                GraphNodeStatus::Waiting,
                                Some(WaitKind::Cost),
                                cx,
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .child(ch.gates_extra_run_paused_at_budget_limit()),
                            ),
                    )
                    .child(
                        div()
                            .font_family(mono)
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .child(SharedString::from(desc)),
                    ),
            )
            .children(details)
            .child(
                h_row()
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.faint))
                            .child(ch.cost_note(timeout_m)),
                    )
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(
                                btn_secondary("cost-stop", ch.gates_extra_stop_run()).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.continue_cost_pause(rid_stop.clone(), false, cx);
                                    }),
                                ),
                            )
                            .child(
                                btn_primary("cost-continue", ch.gates_extra_continue()).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.continue_cost_pause(rid_go.clone(), true, cx);
                                    }),
                                ),
                            ),
                    ),
            ),
    )
}

/// Radio scope-picker modal opened from an approval card's "Scope…" button.
pub(super) fn render_scope_modal(
    appr: &LiveApproval,
    selected: ApprovalScope,
    window: &Window,
    cx: &mut Context<ChatView>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let card_info = appr.request.card.as_deref();
    let rule = card_info
        .and_then(|c| c.rule_label.as_deref())
        .filter(|r| !r.is_empty())
        .unwrap_or(&appr.name);
    let disabled_reason = card_info.and_then(|c| c.scope_disabled_reason.as_deref());
    let offered = appr.offered_scopes();
    let scopes: [(ApprovalScope, String, &'static str); 4] = [
        (
            ApprovalScope::Once,
            ch.modals_allow_once().to_owned(),
            ch.modals_allow_once_sub(),
        ),
        (
            ApprovalScope::Session,
            ch.modals_allow_in_this_chat(rule),
            ch.modals_allow_in_this_chat_sub(),
        ),
        (
            ApprovalScope::WorkspacePattern,
            ch.modals_allow_in_this_folder(rule),
            ch.modals_allow_in_this_folder_sub(),
        ),
        (
            ApprovalScope::Tool,
            ch.modals_allow_tool_always().to_owned(),
            ch.modals_allow_tool_always_sub(),
        ),
    ];
    let mut options_col = v_col().w_full().gap(px(6.0));
    for (sc, title, default_sub) in scopes {
        let allowed = offered.contains(&sc);
        let active = allowed && selected == sc;
        let sub = if !allowed
            && matches!(sc, ApprovalScope::Session | ApprovalScope::WorkspacePattern)
            && let Some(reason) = disabled_reason
        {
            reason
        } else {
            default_sub
        };
        let radio = div()
            .size(px(14.0))
            .rounded(px(999.0))
            .border_2()
            .border_color(to_hsla(if active { p.accent } else { p.line_strong }))
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .children(active.then(|| div().size(px(6.0)).rounded(px(999.0)).bg(to_hsla(p.accent))));
        let mut row = h_row()
            .id(SharedString::from(format!("chat-sc-{sc:?}")))
            .w_full()
            .items_start()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(to_hsla(if active {
                p.accent_line.composite_over(p.accent_soft)
            } else {
                p.line.composite_over(p.surface)
            }))
            .bg(to_hsla(if active { p.accent_soft } else { p.surface }))
            .child(div().pt(px(2.0)).child(radio))
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(if allowed { p.ink } else { p.faint }))
                            .child(SharedString::from(title)),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(if allowed { p.muted } else { p.faint }))
                            .child(SharedString::from(sub.to_owned())),
                    ),
            );
        if allowed {
            row = row
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_scope_approval(sc, cx);
                }));
        }
        options_col = options_col.child(row);
    }
    let body = v_col()
        .w_full()
        .gap(px(10.0))
        .child(render_preview(appr, cx))
        .child(options_col)
        .into_any_element();
    let footer = h_row()
        .gap(px(8.0))
        .child(
            btn_secondary("chat-sc-cancel", crate::ui::tr(cx).common().cancel()).on_click(
                cx.listener(|this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                }),
            ),
        )
        .child(
            btn_primary("chat-sc-save", ch.modals_save_and_allow())
                .disabled(!offered.contains(&selected))
                .on_click(cx.listener(|this, _, _, cx| this.confirm_scope_approval(cx))),
        )
        .into_any_element();
    let title = format!("{} · {}", ch.modals_choose_approval_scope(), appr.name);
    modal_dialog(
        "chat-scope-modal",
        title,
        500.0,
        body,
        footer,
        window,
        cx,
        on_close,
    )
    .into_any_element()
}

/// Inline dock banner left behind when the user answers an approval card.
pub fn render_approval_result(notice: &LastApprovalNotice, cx: &App) -> Div {
    let ch = crate::ui::tr(cx).chat();
    let msg = match notice.decision {
        ApprovalDecision::Allow => {
            let scope = match notice.scope {
                ApprovalScope::Once => ch.trace_resolution_once(),
                ApprovalScope::Session => ch.trace_resolution_session(),
                ApprovalScope::WorkspacePattern => ch.trace_resolution_folder(),
                ApprovalScope::Tool => ch.trace_resolution_global(),
            };
            ch.approval_resolved_allow(scope)
        }
        ApprovalDecision::Deny => match notice.reason.as_deref() {
            Some(reason) if !reason.is_empty() => ch.approval_resolved_deny(reason),
            _ => ch.approval_resolved_deny_none().to_owned(),
        },
    };
    alert_banner(Tone::Neutral, msg, None, cx)
        .rounded_none()
        .border_0()
}

impl ChatView {
    /// Changes the selected radio option inside an open [`ChatModal::ScopeApproval`].
    pub fn select_scope_approval(&mut self, scope: ApprovalScope, cx: &mut Context<Self>) {
        if let Some(ChatModal::ScopeApproval { appr, selected }) = &mut self.active_modal
            && appr.offered_scopes().contains(&scope)
        {
            *selected = scope;
            cx.notify();
        }
    }

    /// Confirms the active [`ChatModal::ScopeApproval`] modal with its chosen radio scope.
    pub fn confirm_scope_approval(&mut self, cx: &mut Context<Self>) {
        let Some(ChatModal::ScopeApproval { appr, selected }) = self.active_modal.take() else {
            return;
        };
        if !appr.offered_scopes().contains(&selected) {
            cx.notify();
            return;
        }
        self.answer_approval_ext(
            appr.run_id,
            appr.call_id,
            ApprovalDecision::Allow,
            selected,
            false,
            cx,
        );
    }
}
