//! Settings › 智能体 › 权限 (`SettingsSection::Permissions`, prototype `ai/permissions`).

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::permissions::PermissionAction;
use pingdeck_desktop::core::model::settings::{Language, ToolApproval};

use crate::ui::{
    Tone, UiIcon, badge, btn_ghost, btn_secondary, h_row, icon_xs, palette, status_indicator,
    tab_bar_segmented, tab_item, text_input, to_hsla, toggle_switch, v_col,
};

use super::AgentSettingsView;
use super::page_permissions_rules::{format_sandbox_row, render_remembered_rules};
use super::widgets::{num_box, section_card, section_card_flush, set_row};

pub(super) fn render_permissions(
    view: &AgentSettingsView,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ag = &view.settings.ai.agent;
    let policies = view
        .config
        .as_ref()
        .map(|c| c.tool_policies.as_slice())
        .unwrap_or(&[]);

    let pinned_cnt = view.blocked_rules.iter().filter(|r| r.pinned).count();
    let user_block_cnt = ag.blocked_commands.len();
    let tool_block_cnt = policies
        .iter()
        .filter(|tp| tp.policy == PermissionAction::Block)
        .count()
        + view
            .permission_rules
            .iter()
            .filter(|rv| rv.rule.action == PermissionAction::Block)
            .count();
    let stored_allow_cnt = view
        .permission_rules
        .iter()
        .filter(|rv| rv.rule.action == PermissionAction::Allow)
        .count();
    let tool_allow_cnt = policies
        .iter()
        .filter(|tp| tp.policy == PermissionAction::Allow)
        .count();

    let mode_label = match ag.default_permission_mode {
        ToolApproval::Ask => crate::ui::Tr(lang).settings().perm_mode_ask(),
        ToolApproval::AutoReadOnly | ToolApproval::AutoAll => {
            crate::ui::Tr(lang).settings().perm_mode_auto_read_only()
        }
    };

    // ── Section 1: Fixed 6-step evaluation order (`.perm-order`) ─────────────
    let steps: [(usize, &str, String, String, Tone); 6] = [
        (
            1,
            crate::ui::Tr(lang).settings().perm_order_hard_block(),
            crate::ui::Tr(lang)
                .settings()
                .perm_order_hard_block_d()
                .to_owned(),
            crate::ui::tr(cx)
                .settings()
                .page_permissions_built_in(pinned_cnt + user_block_cnt),
            Tone::Neutral,
        ),
        (
            2,
            crate::ui::Tr(lang).settings().perm_order_tool_block(),
            crate::ui::Tr(lang)
                .settings()
                .perm_order_tool_block_d()
                .to_owned(),
            crate::ui::tr(cx)
                .settings()
                .page_permissions_label(tool_block_cnt),
            if tool_block_cnt > 0 {
                Tone::Warning
            } else {
                Tone::Neutral
            },
        ),
        (
            3,
            crate::ui::Tr(lang).settings().perm_order_forced_ask(),
            crate::ui::Tr(lang)
                .settings()
                .perm_order_forced_ask_d()
                .to_owned(),
            crate::ui::Tr(lang)
                .settings()
                .perm_order_builtin()
                .to_owned(),
            Tone::Neutral,
        ),
        (
            4,
            crate::ui::Tr(lang).settings().perm_order_stored_rule(),
            crate::ui::Tr(lang)
                .settings()
                .perm_order_stored_rule_d()
                .to_owned(),
            crate::ui::tr(cx)
                .settings()
                .page_permissions_label_2(stored_allow_cnt),
            if stored_allow_cnt > 0 {
                Tone::Accent
            } else {
                Tone::Neutral
            },
        ),
        (
            5,
            crate::ui::Tr(lang).settings().perm_order_tool_allow(),
            crate::ui::Tr(lang)
                .settings()
                .perm_order_tool_allow_d()
                .to_owned(),
            crate::ui::tr(cx)
                .settings()
                .page_permissions_label_3(tool_allow_cnt),
            Tone::Neutral,
        ),
        (
            6,
            crate::ui::Tr(lang).settings().perm_order_mode_default(),
            crate::ui::Tr(lang)
                .settings()
                .perm_order_mode_default_d(mode_label),
            mode_label.to_owned(),
            Tone::Neutral,
        ),
    ];

    let mut perm_box = v_col();
    for (idx, (num, title, desc, tag, tone)) in steps.into_iter().enumerate() {
        let mut row = h_row()
            .px(px(12.0))
            .py(px(9.0))
            .gap(px(10.0))
            .child(
                div()
                    .size(px(20.0))
                    .rounded(px(999.0))
                    .bg(to_hsla(p.sunken))
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_shrink_0()
                    .text_size(px(11.0))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(num.to_string())),
            )
            .child(
                div()
                    .w(px(124.0))
                    .flex_shrink_0()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink))
                    .child(SharedString::from(title.to_owned())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(desc)),
            )
            .child(div().flex_shrink_0().child(badge(tag, tone, cx)));
        if idx < 5 {
            row = row
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)));
        }
        perm_box = perm_box.child(row);
    }

    let sec_order = v_col()
        .w_full()
        .gap(px(8.0))
        .child(section_card_flush(
            crate::ui::tr(cx)
                .settings()
                .page_permissions_fixed_permission_evaluation_order_top(),
            None,
            perm_box,
            cx,
        ))
        .child(
            div()
                .px(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(crate::ui::Tr(lang).settings().perm_order_full().to_owned()),
        );

    // ── Section 2: Defaults, sandbox, timeouts & safety rules ────────────────
    let mode_idx = usize::from(ag.default_permission_mode != ToolApproval::Ask);
    let ent_mode = cx.entity().clone();
    let mode_seg = tab_bar_segmented("agent-perm-mode-seg")
        .selected_index(mode_idx)
        .child(tab_item(
            crate::ui::Tr(lang).settings().perm_mode_ask().to_owned(),
        ))
        .child(tab_item(
            crate::ui::Tr(lang)
                .settings()
                .perm_mode_auto_read_only()
                .to_owned(),
        ))
        .on_click(move |idx, _, cx| {
            let val = if *idx == 0 { "ask" } else { "auto_read_only" };
            ent_mode.update(cx, |this, cx| {
                this.patch_ai(
                    serde_json::json!({ "agent": { "default_permission_mode": val } }),
                    None,
                    cx,
                );
            });
        });

    let (sb_shape, sb_text, sb_fg, sb_detail) = format_sandbox_row(view, lang, &p);
    let sandbox_ctl = h_row()
        .gap(px(8.0))
        .child(
            h_row()
                .gap(px(5.0))
                .child(status_indicator(sb_shape, cx))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(sb_fg)
                        .child(SharedString::from(sb_text)),
                ),
        )
        .child(
            btn_ghost(
                "agent-perm-sandbox-recheck",
                crate::ui::Tr(lang)
                    .settings()
                    .agent_sandbox_test()
                    .to_owned(),
            )
            .icon(icon_xs(UiIcon::Refresh))
            .on_click(cx.listener(|this, _, _, cx| this.reload_all(cx))),
        );

    let reset_perm_btn = btn_ghost(
        "agent-perm-reset",
        crate::ui::tr(cx).settings().page_defaults_reset(),
    )
    .icon(icon_xs(UiIcon::RotateCcw))
    .on_click(cx.listener(|this, _, window, cx| {
        this.reset_ai_keys(
            &[
                "agent.default_permission_mode",
                "agent.sandbox_writes_to_workspace",
                "agent.sandbox_auto_run",
                "agent.approval_timeout_s",
                "agent.shell_timeout_s",
                "agent.blocked_commands",
                "agent.shell_env_passthrough",
            ],
            window,
            cx,
        );
    }))
    .into_any_element();

    let sec_defaults = section_card(
        crate::ui::tr(cx)
            .settings()
            .page_permissions_default_permissions_sandbox(),
        Some(reset_perm_btn),
        v_col()
            .w_full()
            .child(set_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_permissions_default_permission_mode(),
                None,
                Some(SharedString::from(
                    crate::ui::Tr(lang)
                        .settings()
                        .f_ai_agent_default_permission_mode_d()
                        .to_owned(),
                )),
                mode_seg,
                cx,
            ))
            .child(set_row(
                crate::ui::Tr(lang).settings().agent_sandbox().to_owned(),
                None,
                Some(SharedString::from(sb_detail)),
                sandbox_ctl,
                cx,
            ))
            .child(set_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_permissions_confine_shell_writes_to_workspace(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_permissions_uses_the_os_sandbox_to(),
                )),
                toggle_switch("agent-sandbox-writes", ag.sandbox_writes_to_workspace).on_change(
                    cx.listener(|this, checked: &bool, _, cx| {
                        this.patch_ai(
                        serde_json::json!({ "agent": { "sandbox_writes_to_workspace": *checked } }),
                        None,
                        cx,
                    );
                    }),
                ),
                cx,
            ))
            .child(set_row(
                crate::ui::Tr(lang)
                    .settings()
                    .f_ai_agent_sandbox_auto_run()
                    .to_owned(),
                Some(SharedString::from(
                    crate::ui::tr(cx).settings().page_defaults_local_only(),
                )),
                Some(SharedString::from(
                    crate::ui::Tr(lang)
                        .settings()
                        .f_ai_agent_sandbox_auto_run_d()
                        .to_owned(),
                )),
                toggle_switch("agent-sandbox-auto", ag.sandbox_auto_run).on_change(cx.listener(
                    |this, checked: &bool, _, cx| {
                        this.patch_ai(
                            serde_json::json!({ "agent": { "sandbox_auto_run": *checked } }),
                            None,
                            cx,
                        );
                    },
                )),
                cx,
            ))
            .child(set_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_permissions_unattended_approval_timeout(),
                None,
                Some(SharedString::from(
                    crate::ui::Tr(lang)
                        .settings()
                        .f_ai_agent_approval_timeout_s_d()
                        .to_owned(),
                )),
                num_box(
                    &view.approval_timeout_input,
                    Some(crate::ui::tr(cx).settings().unit_s()),
                    84.0,
                    cx,
                ),
                cx,
            ))
            .child(set_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_permissions_shell_command_timeout(),
                None,
                Some(SharedString::from(
                    crate::ui::Tr(lang)
                        .settings()
                        .f_ai_agent_shell_timeout_s_d()
                        .to_owned(),
                )),
                num_box(
                    &view.shell_timeout_input,
                    Some(crate::ui::tr(cx).settings().unit_s()),
                    84.0,
                    cx,
                ),
                cx,
            ))
            .child(render_chip_list_row(
                crate::ui::Tr(lang).settings().f_ai_agent_blocked_commands(),
                crate::ui::Tr(lang)
                    .settings()
                    .f_ai_agent_blocked_commands_d(),
                &ag.blocked_commands,
                &view.blocked_cmd_input,
                "blocked-cmd",
                crate::ui::Tr(lang).settings().agent_rules_add(),
                |this, window, cx| this.add_blocked_command(window, cx),
                |this, item, cx| this.remove_blocked_command(&item, cx),
                cx,
            ))
            .child(render_chip_list_row(
                crate::ui::Tr(lang)
                    .settings()
                    .f_ai_agent_shell_env_passthrough(),
                crate::ui::Tr(lang)
                    .settings()
                    .f_ai_agent_shell_env_passthrough_d(),
                &ag.shell_env_passthrough,
                &view.env_pass_input,
                "env-pass",
                crate::ui::Tr(lang).settings().agent_env_add(),
                |this, window, cx| this.add_shell_env(window, cx),
                |this, item, cx| this.remove_shell_env(&item, cx),
                cx,
            )),
        cx,
    );

    // ── Section 3: Remembered rules ──────────────────────────────────────────
    let sec_rules = render_remembered_rules(view, lang, cx);

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(sec_order)
        .child(sec_defaults)
        .child(sec_rules)
}

#[allow(clippy::too_many_arguments)]
fn render_chip_list_row(
    label: &str,
    desc: &str,
    items: &[String],
    input: &gpui_kit::Entity<crate::ui::InputState>,
    id_prefix: &'static str,
    add_label: &str,
    on_add: impl Fn(&mut AgentSettingsView, &mut gpui_kit::Window, &mut Context<AgentSettingsView>)
    + 'static,
    on_remove: impl Fn(&mut AgentSettingsView, String, &mut Context<AgentSettingsView>)
    + Clone
    + 'static,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut chips = h_row().gap(px(6.0)).flex_wrap().pt(px(6.0));
    for (idx, item) in items.iter().enumerate() {
        let item_c = item.clone();
        let rm = on_remove.clone();
        chips = chips.child(
            h_row()
                .gap(px(5.0))
                .px(px(7.0))
                .py(px(2.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .border_1()
                .border_color(to_hsla(p.line_strong.composite_over(p.sunken)))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(item.clone()))
                .child(
                    div()
                        .id(SharedString::from(format!("{id_prefix}-rm-{idx}")))
                        .cursor_pointer()
                        .text_color(to_hsla(p.muted))
                        .hover(|s| s.text_color(to_hsla(p.bad)))
                        .on_click(cx.listener(move |this, _, _, cx| rm(this, item_c.clone(), cx)))
                        .child("×"),
                ),
        );
    }

    let add_bar = h_row()
        .gap(px(6.0))
        .child(div().w(px(190.0)).child(text_input(input)))
        .child(
            btn_secondary(
                SharedString::from(format!("{id_prefix}-add")),
                add_label.to_owned(),
            )
            .on_click(cx.listener(move |this, _, window, cx| on_add(this, window, cx))),
        );

    v_col()
        .py(px(12.5))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .justify_between()
                .gap(px(16.0))
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.5))
                        .child(
                            div()
                                .text_size(px(13.5))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .child(SharedString::from(label.to_owned())),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(SharedString::from(desc.to_owned())),
                        ),
                )
                .child(div().flex_shrink_0().child(add_bar)),
        )
        .children((!items.is_empty()).then_some(chips))
}
