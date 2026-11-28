//! Tool approval card docked above the composer (`.overhaul/chat-redesign-spec.md` §7.1):
//! - Full preview (command with bold operators + cwd; full scrollable unified diff with Review Panel link),
//! - Compact single-line sandbox & permission-layer metadata,
//! - Allow Once (`↵ Enter`), Allow Scope (`允许范围 ▾`), and Deny with Feedback (`Tab` or click
//!   `拒绝` -> type guidance -> `↵ Enter` denies and continues the turn without aborting).

use std::ops::Range;

use gpui_kit::{
    AnyElement, App, Context, Div, Focusable as _, HighlightStyle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, SharedString, Styled as _, StyledText, Window,
    div, px,
};
use spanbay_desktop::core::agent::approvals::ApprovalAnswer;
use spanbay_desktop::core::agent::model::{
    ApprovalDecision, ApprovalScope, RiskLevel, RiskTag, SandboxKind, ToolApprovalPreview,
    ToolClass,
};
use spanbay_desktop::core::app::current_time_ms;
use spanbay_desktop::state::{GraphNodeStatus, LiveApproval};

use crate::ui::{
    PayloadBlockOptions, ScrollableElement as _, Tone, WaitKind, badge, btn_danger, btn_ghost,
    btn_primary, btn_secondary, diff_lines_preview, h_row, mono_font_family, palette,
    payload_block, status_badge, text_input, to_hsla, v_col,
};
use crate::views::runs::status::format_relative_age;

use super::ChatView;
pub use super::gates_extra::render_approval_result;
use super::types::ChatModal;

impl ChatView {
    /// Denies one call; `reason` goes back to the model with the tool result so the agent adjusts
    /// its approach and continues the current turn.
    pub fn deny_approval(
        &mut self,
        run_id: String,
        call_id: String,
        reason: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let answer = ApprovalAnswer {
            decision: ApprovalDecision::Deny,
            scope: ApprovalScope::Once,
            message: reason,
            edited_arguments: None,
            allow_network_once: false,
        };
        self.send_approval_answer(run_id, call_id, answer, cx);
    }

    /// Opens the deny-reason feedback field on the card of `call_id` (`Tab` or `拒绝` button).
    pub fn start_deny(&mut self, call_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.deny_reason_call = Some(call_id);
        self.deny_reason_input.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    #[must_use]
    pub fn active_deny_reason_call(&self) -> Option<&str> {
        self.deny_reason_call.as_deref()
    }

    pub fn set_deny_reason_text(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = text.into();
        self.deny_reason_input
            .update(cx, |inp, cx| inp.set_value(text, window, cx));
    }

    /// Denies the call whose deny-reason field is open; an empty reason sends none.
    pub fn submit_deny(&mut self, cx: &mut Context<Self>) {
        let Some(call_id) = self.deny_reason_call.take() else {
            return;
        };
        let Some(run_id) = self
            .thread
            .pending_approvals
            .iter()
            .find(|a| a.call_id == call_id)
            .map(|a| a.run_id.clone())
        else {
            return;
        };
        let reason = self.deny_reason_input.read(cx).value().trim().to_owned();
        self.deny_approval(run_id, call_id, (!reason.is_empty()).then_some(reason), cx);
        cx.notify();
    }

    /// Esc on the focused card: closes an open reason field first (innermost layer), otherwise
    /// denies without a reason; focus returns to the composer.
    fn escape_card(
        &mut self,
        run_id: String,
        call_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.deny_reason_call.as_deref() == Some(call_id.as_str()) {
            self.deny_reason_call = None;
            window.focus(&self.approval_focus, cx);
        } else {
            self.deny_approval(run_id, call_id, None, cx);
            self.composer.update(cx, |c, cx| c.focus(window, cx));
        }
        cx.notify();
    }
}

/// Tray tone of an approval row: danger for high-risk or destructive requests, else warning.
pub(super) fn approval_tone(appr: &LiveApproval) -> Tone {
    let req = &appr.request;
    let risk = req.card.as_deref().map_or(req.risk, |c| c.risk);
    if risk == RiskLevel::High || req.destructive {
        Tone::Danger
    } else {
        Tone::Warning
    }
}

/// Renders one inline approval card in the gate slot above the composer.
pub fn render_approval_gate(
    view: &ChatView,
    appr: &LiveApproval,
    first: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let req = &appr.request;
    let card_info = req.card.as_deref();
    let risk = card_info.map_or(req.risk, |c| c.risk);
    let denying = view.deny_reason_call.as_deref() == Some(appr.call_id.as_str());
    let run_once = appr.run_id.clone();
    let call_once = appr.call_id.clone();
    let run_net = appr.run_id.clone();
    let call_net = appr.call_id.clone();
    let run_sess = appr.run_id.clone();
    let call_sess = appr.call_id.clone();
    let call_deny = appr.call_id.clone();
    let appr_modal = appr.clone();

    let scopes = appr.offered_scopes();
    let can_session = scopes.contains(&ApprovalScope::Session);
    let has_more_scopes = scopes.len() > 1
        || scopes.contains(&ApprovalScope::WorkspacePattern)
        || scopes.contains(&ApprovalScope::Tool);

    let deny_row = denying.then(|| {
        h_row()
            .id(SharedString::from(format!(
                "appr-deny-row-{}",
                appr.call_id
            )))
            .gap(px(6.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(text_input(&view.deny_reason_input)),
            )
            .child(
                btn_ghost(
                    SharedString::from(format!("appr-deny-cancel-{}", appr.call_id)),
                    crate::ui::tr(cx).common().cancel(),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.deny_reason_call = None;
                    window.focus(&this.approval_focus, cx);
                    cx.notify();
                })),
            )
            .child(
                btn_danger(
                    SharedString::from(format!("appr-deny-confirm-{}", appr.call_id)),
                    crate::ui::tr(cx).chat().approval_confirm_deny(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.submit_deny(cx))),
            )
    });
    let ch = crate::ui::tr(cx).chat();
    let actions = h_row()
        .gap(px(6.0))
        .flex_wrap()
        .children((!denying).then(|| {
            btn_secondary(
                SharedString::from(format!("appr-deny-{call_deny}")),
                ch.approval_deny(),
            )
            .on_click(cx.listener(move |this, _, w, cx| this.start_deny(call_deny.clone(), w, cx)))
        }))
        .children(can_session.then(|| {
            btn_secondary(
                SharedString::from(format!("appr-sess-{call_sess}")),
                ch.gates_allow_in_chat(),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.answer_approval_ext(
                    run_sess.clone(),
                    call_sess.clone(),
                    ApprovalDecision::Allow,
                    ApprovalScope::Session,
                    false,
                    cx,
                );
            }))
        }))
        .children(has_more_scopes.then(|| {
            let cid_m = appr_modal.call_id.clone();
            btn_ghost(
                SharedString::from(format!("appr-scope-{cid_m}")),
                ch.gates_scope(),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                let initial = appr_modal
                    .offered_scopes()
                    .first()
                    .copied()
                    .unwrap_or(ApprovalScope::Once);
                this.active_modal = Some(ChatModal::ScopeApproval {
                    appr: Box::new(appr_modal.clone()),
                    selected: initial,
                });
                cx.notify();
            }))
        }))
        .children(appr.request.network.then(|| {
            btn_secondary(
                SharedString::from(format!("appr-net-{call_net}")),
                ch.gates_allow_net(),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.answer_approval_ext(
                    run_net.clone(),
                    call_net.clone(),
                    ApprovalDecision::Allow,
                    ApprovalScope::Once,
                    true,
                    cx,
                );
            }))
        }))
        .child(
            btn_primary(
                SharedString::from(format!("appr-once-{call_once}")),
                ch.approval_allow_once(),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.answer_approval_ext(
                    run_once.clone(),
                    call_once.clone(),
                    ApprovalDecision::Allow,
                    ApprovalScope::Once,
                    false,
                    cx,
                );
            })),
        );
    let mut card = v_col().w_full();
    if first {
        let (run_key, call_key) = (appr.run_id.clone(), appr.call_id.clone());
        card = card
            .track_focus(&view.approval_focus)
            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, window, cx| {
                if this.active_modal.is_some() {
                    if ev.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.dismiss_modal_if_open(cx);
                    }
                    return;
                }
                let in_deny_input = this
                    .deny_reason_input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window);
                if ev.keystroke.key == "tab" && !in_deny_input {
                    cx.stop_propagation();
                    this.start_deny(call_key.clone(), window, cx);
                } else if ev.keystroke.key == "enter" && !in_deny_input {
                    cx.stop_propagation();
                    this.answer_approval_ext(
                        run_key.clone(),
                        call_key.clone(),
                        ApprovalDecision::Allow,
                        ApprovalScope::Once,
                        false,
                        cx,
                    );
                } else if ev.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.escape_card(run_key.clone(), call_key.clone(), window, cx);
                }
            }));
    }
    let risk_tone = match risk {
        RiskLevel::High => Tone::Danger,
        RiskLevel::Medium => Tone::Warning,
        RiskLevel::Low => Tone::Neutral,
    };
    let ch = crate::ui::tr(cx).chat();
    let risk_badge_text = match (
        risk == RiskLevel::High,
        card_info.and_then(|c| c.risk_reason.as_deref()),
    ) {
        (true, Some(why)) if !why.is_empty() => format!("{} · {why}", risk_label(risk, cx)),
        _ => risk_label(risk, cx).to_owned(),
    };
    let waited = (appr.requested_at_ms() > 0).then(|| {
        let now_ms = current_time_ms();
        let diff_s = now_ms.saturating_sub(appr.requested_at_ms()).max(0) / 1_000;
        if diff_s < 60 {
            ch.approval_waited_just_now().to_owned()
        } else {
            ch.approval_waited(format_relative_age(appr.requested_at_ms(), now_ms, cx))
        }
    });
    let mut head_badges = h_row()
        .gap(px(4.0))
        .flex_wrap()
        .child(badge(appr.name.clone(), Tone::Neutral, cx))
        .child(badge(risk_badge_text, risk_tone, cx));
    for tag in card_info.map_or(&[][..], |c| c.tags.as_slice()) {
        let label = match tag {
            RiskTag::ChainedCommand => ch.approval_tag_chained_command(),
            RiskTag::NoSandbox => ch.approval_tag_no_sandbox(),
            RiskTag::SandboxOff => ch.approval_tag_sandbox_off(),
        };
        head_badges = head_badges.child(badge(label, Tone::Warning, cx));
    }
    if let Some(waited) = waited {
        head_badges = head_badges.child(
            div()
                .pl(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(waited)),
        );
    }
    let esc_hint = first.then(|| {
        div()
            .text_size(px(11.0))
            .text_color(to_hsla(p.faint))
            .child(if denying {
                ch.approval_esc_hint_open()
            } else {
                ch.approval_esc_hint()
            })
    });

    card.child(
        v_col()
            .p(px(12.0))
            .gap(px(8.0))
            .child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .gap(px(7.0))
                            .child(status_badge(
                                GraphNodeStatus::Waiting,
                                Some(WaitKind::Approval),
                                cx,
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(req.summary.clone())),
                            ),
                    )
                    .child(head_badges),
            )
            .child(render_preview(appr, cx))
            .children(reason_lines(appr, cx))
            .children(deny_row)
            .child(
                h_row()
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .child(div().flex_1().min_w_0().children(esc_hint))
                    .child(actions),
            ),
    )
}

fn risk_label(risk: RiskLevel, cx: &App) -> &'static str {
    let ch = crate::ui::tr(cx).chat();
    match risk {
        RiskLevel::Low => ch.approval_risk_low(),
        RiskLevel::Medium => ch.approval_risk_medium(),
        RiskLevel::High => ch.approval_risk_high(),
    }
}

/// Compact single-line sandbox & permission-layer metadata, plus a warning line if a stored rule could not be used.
fn reason_lines(appr: &LiveApproval, cx: &App) -> Vec<Div> {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let req = &appr.request;
    let card = req.card.as_deref();
    let why = card
        .and_then(|c| c.why_code.as_deref())
        .and_then(|code| match code {
            "mcp_untrusted" => Some(ch.approval_why_mcp_untrusted()),
            "protected_path" => Some(ch.approval_why_protected_path()),
            "stdin_targets" => Some(ch.approval_why_stdin_targets()),
            "no_sandbox" => Some(ch.approval_why_no_sandbox()),
            "tool_setting" => Some(ch.approval_why_tool_setting()),
            "ask_rule" => Some(ch.approval_why_ask_rule()),
            "folder_busy" => Some(ch.approval_why_folder_busy()),
            "url_provenance" => Some(ch.approval_why_url_provenance()),
            "shell_remote" => Some(ch.approval_why_shell_remote()),
            _ => None,
        })
        .map(str::to_owned)
        .or_else(|| card.and_then(|c| c.why.clone()))
        .unwrap_or_else(|| req.reason.clone());
    let layer = card.and_then(|c| c.layer.as_deref()).and_then(|l| match l {
        "forced_ask" => Some(ch.approval_layer_forced_ask()),
        "stored_rule" => Some(ch.approval_layer_stored_rule()),
        "mode_default" => Some(ch.approval_layer_mode_default()),
        _ => None,
    });
    let sandbox = (req.class == ToolClass::Exec).then(|| {
        match card.map_or(req.sandbox, |c| c.sandbox.usable()) {
            SandboxKind::Seatbelt => ch.approval_sandbox_seatbelt(),
            SandboxKind::Bubblewrap => ch.approval_sandbox_bubblewrap(),
            SandboxKind::None => ch.approval_sandbox_none(),
        }
    });

    let mut parts: Vec<String> = Vec::with_capacity(3);
    if let Some(sb) = sandbox {
        parts.push(sb.to_owned());
    }
    if let Some(ly) = layer {
        parts.push(ly.to_owned());
    }
    if !why.is_empty() {
        parts.push(why);
    }

    let mut out = Vec::with_capacity(2);
    if !parts.is_empty() {
        out.push(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(parts.join(" · "))),
        );
    }
    if let Some(rule) = card.and_then(|c| c.rule_not_used.clone()) {
        out.push(
            div()
                .text_xs()
                .text_color(to_hsla(p.warn))
                .child(SharedString::from(ch.approval_rule_not_used(rule))),
        );
    }
    out
}

/// What the call will do, by preview kind; the full arguments when there is no preview.
pub(super) fn render_preview(appr: &LiveApproval, cx: &App) -> AnyElement {
    let p = palette(cx);
    let req = &appr.request;
    let mono = mono_font_family(cx);
    let boxed = || {
        v_col()
            .gap(px(4.0))
            .px(px(9.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .bg(to_hsla(p.sunken))
            .text_xs()
            .text_color(to_hsla(p.ink_2))
    };
    match &req.preview {
        Some(ToolApprovalPreview::Command { cmd, cwd }) => {
            let bold = HighlightStyle {
                font_weight: Some(gpui_kit::FontWeight::BOLD),
                ..HighlightStyle::default()
            };
            let ranges: Vec<(Range<usize>, HighlightStyle)> = operator_highlight_ranges(
                cmd,
                req.card
                    .as_deref()
                    .map_or(&[][..], |c| c.operators.as_slice()),
            )
            .into_iter()
            .map(|range| (range, bold))
            .collect();
            boxed()
                .child(
                    div()
                        .font_family(mono)
                        .child(StyledText::new(cmd.clone()).with_highlights(ranges)),
                )
                .child(
                    div()
                        .text_color(to_hsla(p.muted))
                        .child(crate::ui::tr(cx).chat().approval_cwd(cwd)),
                )
                .into_any_element()
        }
        Some(ToolApprovalPreview::Diff { path, unified_diff }) => v_col()
            .gap(px(4.0))
            .child(
                div()
                    .font_family(mono)
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(path.clone())),
            )
            .child(
                div()
                    .id(SharedString::from(format!("appr-diff-{}", appr.call_id)))
                    .max_h(px(240.0))
                    .overflow_y_scrollbar()
                    .child(diff_lines_preview(unified_diff, usize::MAX, cx)),
            )
            .into_any_element(),
        Some(ToolApprovalPreview::Url { url }) => boxed()
            .font_family(mono)
            .child(SharedString::from(url.clone()))
            .into_any_element(),
        Some(ToolApprovalPreview::Summary { text }) => boxed()
            .child(SharedString::from(text.clone()))
            .into_any_element(),
        None => payload_block(
            SharedString::from(format!("appr-args-{}", appr.call_id)),
            req.full_text.as_deref().unwrap_or(&req.arguments),
            PayloadBlockOptions {
                max_height_px: Some(180.0),
                show_header: false,
                ..PayloadBlockOptions::default()
            },
            cx,
        ),
    }
}

/// Converts character-indexed [`OperatorHit`] entries into sorted, non-overlapping UTF-8 byte
/// ranges within `cmd`, clamping and merging any overlapping spans.
pub(super) fn operator_highlight_ranges(
    cmd: &str,
    operators: &[spanbay_desktop::core::agent::model::permissions::OperatorHit],
) -> Vec<Range<usize>> {
    let mut raw: Vec<Range<usize>> = operators
        .iter()
        .filter_map(|op| {
            let start = cmd.char_indices().nth(op.start)?.0;
            let end = start.checked_add(op.source_len())?.min(cmd.len());
            (start < end && cmd.is_char_boundary(start) && cmd.is_char_boundary(end))
                .then_some(start..end)
        })
        .collect();
    raw.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(raw.len());
    for range in raw {
        if let Some(last) = merged.last_mut()
            && range.start < last.end
        {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    debug_assert!(
        merged.iter().all(|r| r.start < r.end
            && cmd.is_char_boundary(r.start)
            && cmd.is_char_boundary(r.end))
    );
    merged
}

#[cfg(test)]
mod tests {
    use super::operator_highlight_ranges;
    use spanbay_desktop::core::agent::model::permissions::OperatorHit;

    #[test]
    fn operator_highlight_ranges_handles_cjk_consecutive_newlines_and_overlaps() {
        let cmd = "echo 你好\n\ncargo test && echo 再见\n";
        let ops = vec![
            OperatorHit {
                op: "\\n".to_owned(),
                start: 7,
            },
            OperatorHit {
                op: "\\n".to_owned(),
                start: 8,
            },
            OperatorHit {
                op: "&&".to_owned(),
                start: 20,
            },
            OperatorHit {
                op: "\\n".to_owned(),
                start: 30,
            },
        ];
        let ranges = operator_highlight_ranges(cmd, &ops);
        assert_eq!(ranges.len(), 4);
        for (idx, r) in ranges.iter().enumerate() {
            assert!(r.start < r.end);
            assert!(cmd.is_char_boundary(r.start));
            assert!(cmd.is_char_boundary(r.end));
            if idx > 0 {
                assert!(ranges[idx - 1].end <= r.start);
            }
        }
        assert_eq!(&cmd[ranges[0].clone()], "\n");
        assert_eq!(&cmd[ranges[1].clone()], "\n");
        assert_eq!(&cmd[ranges[2].clone()], "&&");
        assert_eq!(&cmd[ranges[3].clone()], "\n");
    }
}
