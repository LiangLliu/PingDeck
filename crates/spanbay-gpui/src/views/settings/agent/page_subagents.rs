//! Settings › 智能体 › 子智能体 (`SettingsSection::Subagents`, prototype `ai/subagents`).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::agent::model::SubagentRole;
use spanbay_desktop::core::app::RoleWarning;
use spanbay_desktop::core::model::settings::Language;

use crate::ui::{
    Tone, UiIcon, alert_banner, badge, btn_ghost, btn_secondary, h_row, icon_xs, palette,
    settings_sub_well, to_hsla, v_col,
};

use super::AgentSettingsView;
use super::widgets::{model_picker_popover, num_box, render_fallback_chain, section_card, set_row};

pub(super) fn render_subagents(
    view: &AgentSettingsView,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ag = &view.settings.ai.agent;
    let providers = view
        .config
        .as_ref()
        .map(|c| c.providers.as_slice())
        .unwrap_or(&[]);

    // ── Section 1: Concurrency & built-in subagent kinds ─────────────────────
    let reset_btn = btn_ghost(
        "agent-sub-reset",
        crate::ui::tr(cx)
            .settings()
            .page_defaults_restore_defaults(),
    )
    .on_click(cx.listener(|this, _, window, cx| {
        this.reset_ai_keys(&["agent.max_subagents"], window, cx);
    }))
    .into_any_element();

    let fast_label = view
        .roles
        .iter()
        .find(|r| r.role == SubagentRole::Fast)
        .and_then(|r| r.model_label.clone());

    let sec_kinds = section_card(
        crate::ui::tr(cx)
            .settings()
            .page_subagents_concurrency_built_in_kinds(),
        Some(reset_btn),
        v_col()
            .w_full()
            .child(set_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_subagents_max_sub_agents_per_run(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_subagents_concurrent_sub_agents_one_run(),
                )),
                num_box(
                    &view.max_subagents_input,
                    Some(crate::ui::tr(cx).settings().agent_unit_agents()),
                    84.0,
                    cx,
                ),
                cx,
            ))
            .child(set_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_subagents_explore_read_only(),
                Some(SharedString::from(
                    crate::ui::tr(cx).settings().perm_order_builtin(),
                )),
                Some(SharedString::from(
                    crate::ui::Tr(lang)
                        .settings()
                        .sub_kind_explore_d()
                        .to_owned(),
                )),
                badge(
                    fast_label.unwrap_or_else(|| {
                        crate::ui::tr(cx)
                            .settings()
                            .page_subagents_follows_chat_model()
                            .to_owned()
                    }),
                    Tone::Neutral,
                    cx,
                ),
                cx,
            ))
            .child(set_row(
                crate::ui::tr(cx).settings().page_subagents_general(),
                Some(SharedString::from(
                    crate::ui::tr(cx).settings().perm_order_builtin(),
                )),
                Some(SharedString::from(
                    crate::ui::Tr(lang)
                        .settings()
                        .sub_kind_general_d()
                        .to_owned(),
                )),
                badge(
                    crate::ui::tr(cx)
                        .settings()
                        .page_subagents_chat_model_or_role(),
                    Tone::Neutral,
                    cx,
                ),
                cx,
            )),
        cx,
    );

    // ── Section 2: Model roles (Fast, Deep, Vision) + per-role fallbacks ─────
    let suggest_btn = btn_secondary(
        "agent-roles-suggest",
        crate::ui::Tr(lang)
            .settings()
            .agent_roles_suggest()
            .to_owned(),
    )
    .icon(icon_xs(UiIcon::Sparkles))
    .on_click(cx.listener(|this, _, _, cx| this.apply_role_suggestions(cx)))
    .into_any_element();

    let s_tr = crate::ui::Tr(lang).settings();
    let mut roles_rows = v_col().w_full();
    for role in SubagentRole::ALL {
        let status = view.roles.iter().find(|r| r.role == role);
        let (title_text, desc_text, fb_site) = match role {
            SubagentRole::Fast => (s_tr.agent_roles_fast(), s_tr.agent_roles_fast_d(), "fast"),
            SubagentRole::Deep => (s_tr.agent_roles_deep(), s_tr.agent_roles_deep_d(), "deep"),
            SubagentRole::Vision => (
                s_tr.agent_roles_vision(),
                s_tr.agent_roles_vision_d(),
                "vision",
            ),
        };
        let active_ref = status.and_then(|s| s.model.clone());
        let trigger_label = status
            .and_then(|s| s.model_label.clone())
            .unwrap_or_else(|| {
                crate::ui::tr(cx)
                    .settings()
                    .page_subagents_follows_chat_model_2()
                    .to_owned()
            });

        let ent = cx.entity().clone();
        let picker = model_picker_popover(
            format!("role-pop-{}", role.as_str()),
            format!("role-btn-{}", role.as_str()),
            trigger_label,
            active_ref.clone(),
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_subagents_not_set_use_chat_model(),
            )),
            view.model_choices.clone(),
            lang,
            move |picked, cx| {
                let next_ref = picked.map(|c| c.id_ref());
                ent.update(cx, |this, cx| this.set_role_model(role, next_ref, cx));
            },
            cx,
        );

        let mut role_row = v_col()
            .w_full()
            .py(px(14.0))
            .gap(px(10.0))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)))
            .child(
                h_row()
                    .justify_between()
                    .gap(px(14.0))
                    .child(
                        v_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.5))
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .text_size(px(13.5))
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .text_color(to_hsla(p.ink))
                                            .child(title_text),
                                    )
                                    .children(status.and_then(|s| {
                                        if s.model.is_none() {
                                            s.suggestion_label.as_ref().map(|lbl| {
                                                badge(
                                                    crate::ui::tr(cx)
                                                        .settings()
                                                        .page_subagents_suggested(lbl),
                                                    Tone::Accent,
                                                    cx,
                                                )
                                            })
                                        } else {
                                            None
                                        }
                                    })),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .child(desc_text),
                            ),
                    )
                    .child(div().flex_shrink_0().child(picker)),
            );

        if let Some(st) = status {
            if let Some(unavail) = &st.unavailable {
                role_row = role_row.child(alert_banner(Tone::Danger, unavail.clone(), None, cx));
            }
            if !st.warnings.is_empty() {
                let mut warn_row = h_row().gap(px(6.0)).flex_wrap();
                for w in &st.warnings {
                    let w_lbl = match w {
                        RoleWarning::VisionUntested => s_tr.agent_roles_warn_vision_untested(),
                        RoleWarning::ToolsUntested => s_tr.agent_roles_warn_tools_untested(),
                        RoleWarning::NoPrice => s_tr.agent_roles_warn_no_price(),
                        RoleWarning::NoTools => s_tr.agent_roles_warn_no_tools(),
                    };
                    warn_row = warn_row.child(badge(w_lbl, Tone::Warning, cx));
                }
                role_row = role_row.child(warn_row);
            }
        }

        let fb_chain = ag.fallbacks.for_role(Some(role));
        if active_ref.is_some() || !fb_chain.is_empty() {
            role_row = role_row.child(
                settings_sub_well(cx).py(px(8.0)).px(px(12.0)).child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .gap(px(10.0))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(crate::ui::tr(cx).settings().page_subagents_fallbacks()),
                        )
                        .child(render_fallback_chain(
                            view, fb_site, fb_chain, providers, lang, cx,
                        )),
                ),
            );
        }

        roles_rows = roles_rows.child(role_row);
    }

    let sec_roles = section_card(
        crate::ui::Tr(lang)
            .settings()
            .sec_subagents_roles()
            .to_owned(),
        Some(suggest_btn),
        roles_rows,
        cx,
    );

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(sec_kinds)
        .child(sec_roles)
}
