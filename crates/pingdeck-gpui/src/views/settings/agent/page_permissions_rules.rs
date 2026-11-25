//! Remembered permission rules list and sandbox status formatting for Settings › Permissions.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::permissions::PermissionAction;
use pingdeck_desktop::core::agent::sandbox::SandboxKind;
use pingdeck_desktop::core::app::{RuleInactive, SandboxSelftest, current_time_ms};
use pingdeck_desktop::core::model::settings::Language;

use crate::ui::{StatusShape, Tone, badge, btn_danger, h_row, palette, to_hsla, v_col};

use super::AgentSettingsView;
use super::widgets::{format_ago_short, section_card_flush, section_card_padded};

pub(super) fn render_remembered_rules(
    view: &AgentSettingsView,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let now_ms = current_time_ms();
    let title = crate::ui::Tr(lang)
        .settings()
        .sec_permissions_rules()
        .to_owned();
    let desc = div()
        .px(px(4.0))
        .text_xs()
        .text_color(to_hsla(p.muted))
        .child(
            crate::ui::Tr(lang)
                .settings()
                .sec_permissions_rules_d()
                .to_owned(),
        );

    if view.permission_rules.is_empty() {
        return v_col()
            .w_full()
            .gap(px(7.0))
            .child(section_card_padded(
                title,
                None,
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(crate::ui::Tr(lang).settings().tools_rules_none().to_owned()),
                cx,
            ))
            .child(desc);
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
        let scope = r.workspace_dir.clone().unwrap_or_else(|| {
            crate::ui::Tr(lang)
                .settings()
                .tools_rules_any_folder()
                .to_owned()
        });
        let when = format_ago_short(r.created_at_ms, now_ms, lang);
        let action_tone = match r.action {
            PermissionAction::Allow => Tone::Success,
            PermissionAction::Ask => Tone::Warning,
            PermissionAction::Block => Tone::Danger,
        };
        let action_lbl = match r.action {
            PermissionAction::Allow => crate::ui::Tr(lang).settings().tools_value_allow(),
            PermissionAction::Ask => crate::ui::Tr(lang).settings().tools_value_ask(),
            PermissionAction::Block => crate::ui::Tr(lang).settings().tools_value_block(),
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
                                    crate::ui::Tr(lang)
                                        .settings()
                                        .tools_rules_no_folder()
                                        .to_owned(),
                                    Tone::Warning,
                                    cx,
                                )
                            }))
                            .children(rv.inactive.map(|why| {
                                let s = crate::ui::Tr(lang).settings();
                                let label = match why {
                                    RuleInactive::SafetyRule => s.tools_rules_inactive_safety(),
                                    RuleInactive::ChainedCommands => {
                                        s.tools_rules_inactive_chained()
                                    }
                                };
                                badge(label, Tone::Warning, cx)
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
                    crate::ui::Tr(lang)
                        .settings()
                        .tools_rules_remove()
                        .to_owned(),
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
        .w_full()
        .gap(px(7.0))
        .child(section_card_flush(title, None, list, cx))
        .child(desc)
}

pub(super) fn format_sandbox_row(
    view: &AgentSettingsView,
    lang: Language,
    p: &pingdeck_desktop::theme::ResolvedPalette,
) -> (StatusShape, String, gpui_kit::Hsla, String) {
    let Some(sb) = &view.sandbox else {
        return (
            StatusShape::Run,
            crate::ui::Tr(lang)
                .settings()
                .agent_sandbox_unknown()
                .to_owned(),
            to_hsla(p.muted),
            crate::ui::Tr(lang)
                .settings()
                .agent_sandbox_full()
                .to_owned(),
        );
    };
    let tier = match sb.tier {
        SandboxKind::Seatbelt => Some(crate::ui::Tr(lang).settings().agent_sandbox_tier_seatbelt()),
        SandboxKind::Bubblewrap => Some(
            crate::ui::Tr(lang)
                .settings()
                .agent_sandbox_tier_bubblewrap(),
        ),
        SandboxKind::None => None,
    };
    match (tier, sb.selftest) {
        (Some(name), SandboxSelftest::Passed) => (
            StatusShape::Ok,
            crate::ui::Tr(lang).settings().agent_sandbox_available(name),
            to_hsla(p.ink_2),
            crate::ui::Tr(lang)
                .settings()
                .agent_sandbox_full()
                .to_owned(),
        ),
        (Some(_), SandboxSelftest::Failed) => (
            StatusShape::Warn,
            crate::ui::Tr(lang)
                .settings()
                .agent_sandbox_unavailable()
                .to_owned(),
            to_hsla(p.warn),
            crate::ui::Tr(lang)
                .settings()
                .agent_sandbox_asks()
                .to_owned(),
        ),
        _ => (
            StatusShape::Idle,
            crate::ui::Tr(lang)
                .settings()
                .agent_sandbox_not_installed()
                .to_owned(),
            to_hsla(p.muted),
            crate::ui::Tr(lang)
                .settings()
                .agent_sandbox_install()
                .to_owned(),
        ),
    }
}
