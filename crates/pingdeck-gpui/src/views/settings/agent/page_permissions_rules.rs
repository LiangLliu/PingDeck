//! Remembered permission rules list and sandbox status formatting for Settings › Permissions.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::permissions::PermissionAction;
use pingdeck_desktop::core::agent::sandbox::SandboxKind;
use pingdeck_desktop::core::app::{RuleInactive, SandboxSelftest, current_time_ms};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::i18n::{t, t_args};

use crate::ui::{StatusShape, Tone, badge, btn_danger, card, h_row, palette, to_hsla, v_col};

use super::AgentSettingsView;
use super::widgets::{format_ago_short, section_head};

pub(super) fn render_remembered_rules(
    view: &AgentSettingsView,
    lang: Language,
    zh: bool,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let now_ms = current_time_ms();
    let header = section_head(t(lang, "set.sec.permissions.rules").to_owned(), None, cx);
    let desc = div()
        .text_xs()
        .text_color(to_hsla(p.muted))
        .child(t(lang, "set.sec.permissions.rules.d").to_owned());

    if view.permission_rules.is_empty() {
        return v_col().gap(px(8.0)).child(header).child(desc).child(
            card(cx).p(px(14.0)).child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(t(lang, "set.tools.rules.none").to_owned()),
            ),
        );
    }

    let mut list = v_col();
    let total = view.permission_rules.len();
    for (idx, rv) in view.permission_rules.iter().enumerate() {
        let r = &rv.rule;
        let rid = r.id.clone();
        let summary = match &r.arg_pattern {
            Some(arg) => format!("{}  {}", r.tool_pattern, arg),
            None => r.tool_pattern.clone(),
        };
        let scope = r
            .workspace_dir
            .clone()
            .unwrap_or_else(|| t(lang, "set.tools.rules.anyFolder").to_owned());
        let when = format_ago_short(r.created_at_ms, now_ms, zh);
        let action_tone = match r.action {
            PermissionAction::Allow => Tone::Success,
            PermissionAction::Ask => Tone::Warning,
            PermissionAction::Block => Tone::Danger,
        };
        let action_lbl = match r.action {
            PermissionAction::Allow => t(lang, "set.tools.value.allow"),
            PermissionAction::Ask => t(lang, "set.tools.value.ask"),
            PermissionAction::Block => t(lang, "set.tools.value.block"),
        };

        let mut row = h_row()
            .justify_between()
            .gap(px(12.0))
            .px(px(12.0))
            .py(px(10.0))
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.0))
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .flex_wrap()
                            .child(badge(action_lbl.to_owned(), action_tone, cx))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(summary)),
                            )
                            .children((!rv.folder_exists).then(|| {
                                badge(
                                    t(lang, "set.tools.rules.noFolder").to_owned(),
                                    Tone::Warning,
                                    cx,
                                )
                            }))
                            .children(rv.inactive.map(|why| {
                                let k = match why {
                                    RuleInactive::SafetyRule => "set.tools.rules.inactive.safety",
                                    RuleInactive::ChainedCommands => {
                                        "set.tools.rules.inactive.chained"
                                    }
                                };
                                badge(t(lang, k).to_owned(), Tone::Warning, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.faint))
                            .child(SharedString::from(format!("{scope} · {when}"))),
                    ),
            )
            .child(
                btn_danger(
                    SharedString::from(format!("perm-rule-del-{idx}")),
                    t(lang, "set.tools.rules.remove").to_owned(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.delete_permission_rule(rid.clone(), cx);
                })),
            );
        if idx + 1 < total {
            row = row
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)));
        }
        list = list.child(row);
    }

    v_col()
        .gap(px(8.0))
        .child(header)
        .child(desc)
        .child(card(cx).overflow_hidden().child(list))
}

pub(super) fn format_sandbox_row(
    view: &AgentSettingsView,
    lang: Language,
    p: &pingdeck_desktop::theme::ResolvedPalette,
) -> (StatusShape, String, gpui_kit::Hsla, String) {
    let Some(sb) = &view.sandbox else {
        return (
            StatusShape::Run,
            t(lang, "set.agent.sandbox.unknown").to_owned(),
            to_hsla(p.muted),
            t(lang, "set.agent.sandbox.full").to_owned(),
        );
    };
    let tier = match sb.tier {
        SandboxKind::Seatbelt => Some(t(lang, "set.agent.sandbox.tier.seatbelt")),
        SandboxKind::Bubblewrap => Some(t(lang, "set.agent.sandbox.tier.bubblewrap")),
        SandboxKind::None => None,
    };
    match (tier, sb.selftest) {
        (Some(name), SandboxSelftest::Passed) => (
            StatusShape::Ok,
            t_args(lang, "set.agent.sandbox.available", &[("tier", name)]),
            to_hsla(p.ink_2),
            t(lang, "set.agent.sandbox.full").to_owned(),
        ),
        (Some(_), SandboxSelftest::Failed) => (
            StatusShape::Warn,
            t(lang, "set.agent.sandbox.unavailable").to_owned(),
            to_hsla(p.warn),
            t(lang, "set.agent.sandbox.asks").to_owned(),
        ),
        _ => (
            StatusShape::Idle,
            t(lang, "set.agent.sandbox.notInstalled").to_owned(),
            to_hsla(p.muted),
            t(lang, "set.agent.sandbox.install").to_owned(),
        ),
    }
}
