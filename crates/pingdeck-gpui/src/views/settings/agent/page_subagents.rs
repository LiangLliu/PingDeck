//! Settings › 智能体 › 子智能体 (`SettingsSection::Subagents`, prototype `ai/subagents`).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::model::SubagentRole;
use pingdeck_desktop::core::app::RoleWarning;
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::i18n::t;

use crate::ui::{
    Tone, UiIcon, alert_banner, badge, btn_ghost, btn_secondary, card, h_row, icon_xs, palette,
    to_hsla, v_col,
};

use super::AgentSettingsView;
use super::widgets::{model_picker_popover, num_box, render_fallback_chain, section_head, set_row};

pub(super) fn render_subagents(
    view: &AgentSettingsView,
    lang: Language,
    zh: bool,
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
        if zh {
            "恢复本区默认值"
        } else {
            "Restore defaults"
        },
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

    let sec_kinds = v_col()
        .child(section_head(
            if zh {
                "并发与内置类型"
            } else {
                "Concurrency & built-in kinds"
            },
            Some(reset_btn),
            cx,
        ))
        .child(
            div()
                .pt(px(8.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(t(lang, "set.sec.subagents.subagents.d").to_owned()),
        )
        .child(set_row(
            if zh {
                "单次运行最大子智能体数"
            } else {
                "Max sub-agents per run"
            },
            None,
            Some(SharedString::from(if zh {
                "限制主智能体一次可派生的并发子任务数量（1–8 个），所有角色共用。"
            } else {
                "Concurrent sub-agents one run may spawn (1–8), shared by all roles."
            })),
            num_box(
                &view.max_subagents_input,
                Some(if zh { "个" } else { "agents" }),
                84.0,
                cx,
            ),
            cx,
        ))
        .child(set_row(
            if zh {
                "Explore（只读探索）"
            } else {
                "Explore (read-only)"
            },
            Some(SharedString::from(if zh { "内置" } else { "Built-in" })),
            Some(SharedString::from(
                t(lang, "set.sub.kind.explore.d").to_owned(),
            )),
            badge(
                fast_label.unwrap_or_else(|| {
                    if zh {
                        "跟随主模型"
                    } else {
                        "Follows chat model"
                    }
                    .to_owned()
                }),
                Tone::Neutral,
                cx,
            ),
            cx,
        ))
        .child(set_row(
            if zh {
                "General（通用执行）"
            } else {
                "General"
            },
            Some(SharedString::from(if zh { "内置" } else { "Built-in" })),
            Some(SharedString::from(
                t(lang, "set.sub.kind.general.d").to_owned(),
            )),
            badge(
                if zh {
                    "跟随主模型或指定角色"
                } else {
                    "Chat model or role"
                },
                Tone::Neutral,
                cx,
            ),
            cx,
        ));

    // ── Section 2: Model roles (Fast, Deep, Vision) + per-role fallbacks ─────
    let suggest_btn = btn_secondary(
        "agent-roles-suggest",
        t(lang, "set.agent.roles.suggest").to_owned(),
    )
    .icon(icon_xs(UiIcon::Sparkles))
    .on_click(cx.listener(|this, _, _, cx| this.apply_role_suggestions(cx)))
    .into_any_element();

    let mut roles_col = v_col().gap(px(10.0));
    for role in SubagentRole::ALL {
        let status = view.roles.iter().find(|r| r.role == role);
        let (title_key, desc_key, fb_site) = match role {
            SubagentRole::Fast => ("set.agent.roles.fast", "set.agent.roles.fast.d", "fast"),
            SubagentRole::Deep => ("set.agent.roles.deep", "set.agent.roles.deep.d", "deep"),
            SubagentRole::Vision => (
                "set.agent.roles.vision",
                "set.agent.roles.vision.d",
                "vision",
            ),
        };
        let active_ref = status.and_then(|s| s.model.clone());
        let trigger_label = status
            .and_then(|s| s.model_label.clone())
            .unwrap_or_else(|| {
                if zh {
                    "跟随主模型（未设置）"
                } else {
                    "Follows chat model"
                }
                .to_owned()
            });

        let ent = cx.entity().clone();
        let picker = model_picker_popover(
            format!("role-pop-{}", role.as_str()),
            format!("role-btn-{}", role.as_str()),
            trigger_label,
            active_ref.clone(),
            Some(SharedString::from(if zh {
                "未设置（跟随主模型）"
            } else {
                "Not set (use chat model)"
            })),
            view.model_choices.clone(),
            lang,
            move |picked, cx| {
                let next_ref = picked.map(|c| c.id_ref());
                ent.update(cx, |this, cx| this.set_role_model(role, next_ref, cx));
            },
            cx,
        );

        let mut role_card = v_col().gap(px(8.0)).child(
            h_row()
                .justify_between()
                .gap(px(14.0))
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.0))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .child(t(lang, title_key).to_owned()),
                                )
                                .children(status.and_then(|s| {
                                    if s.model.is_none() {
                                        s.suggestion_label.as_ref().map(|lbl| {
                                            badge(
                                                if zh {
                                                    format!("建议：{lbl}")
                                                } else {
                                                    format!("Suggested: {lbl}")
                                                },
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
                                .child(t(lang, desc_key).to_owned()),
                        ),
                )
                .child(div().flex_shrink_0().child(picker)),
        );

        if let Some(st) = status {
            if let Some(unavail) = &st.unavailable {
                role_card = role_card.child(alert_banner(Tone::Danger, unavail.clone(), None, cx));
            }
            if !st.warnings.is_empty() {
                let mut warn_row = h_row().gap(px(6.0)).flex_wrap();
                for w in &st.warnings {
                    let w_key = match w {
                        RoleWarning::VisionUntested => "set.agent.roles.warn.vision_untested",
                        RoleWarning::ToolsUntested => "set.agent.roles.warn.tools_untested",
                        RoleWarning::NoPrice => "set.agent.roles.warn.no_price",
                        RoleWarning::NoTools => "set.agent.roles.warn.no_tools",
                    };
                    warn_row = warn_row.child(badge(t(lang, w_key).to_owned(), Tone::Warning, cx));
                }
                role_card = role_card.child(warn_row);
            }
        }

        let fb_chain = ag.fallbacks.for_role(Some(role));
        if active_ref.is_some() || !fb_chain.is_empty() {
            role_card = role_card.child(
                h_row()
                    .pt(px(8.0))
                    .border_t_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)))
                    .justify_between()
                    .gap(px(10.0))
                    .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                        "失败时备用"
                    } else {
                        "Fallbacks"
                    }))
                    .child(render_fallback_chain(
                        view, fb_site, fb_chain, providers, lang, zh, cx,
                    )),
            );
        }

        roles_col = roles_col.child(card(cx).p(px(14.0)).child(role_card));
    }

    let sec_roles = v_col()
        .gap(px(10.0))
        .child(section_head(
            t(lang, "set.sec.subagents.roles").to_owned(),
            Some(suggest_btn),
            cx,
        ))
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(t(lang, "set.sec.subagents.roles.d").to_owned()),
        )
        .child(roles_col);

    v_col().gap(px(22.0)).child(sec_kinds).child(sec_roles)
}
