//! Settings › 智能体 › 默认 (`SettingsSection::Agent`, prototype `ai/agent`).

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::model::AgentTarget;
use pingdeck_desktop::core::model::settings::{Language, PromptCacheMode, ReasoningEffortOrAuto};

use crate::ui::{
    Tone, UiIcon, badge, btn_ghost, btn_secondary, h_row, icon_xs, palette, tab_bar_segmented,
    tab_item, to_hsla, toggle_switch, v_col,
};

use super::AgentSettingsView;
use super::widgets::{
    display_model_ref, model_picker_popover, num_box, render_fallback_chain, section_card,
    section_card_flush, set_row,
};

pub(super) fn render_defaults(
    view: &AgentSettingsView,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let s = crate::ui::tr(cx).settings();
    let ag = &view.settings.ai.agent;
    let providers = view
        .config
        .as_ref()
        .map(|c| c.providers.as_slice())
        .unwrap_or(&[]);

    // ── Section 1: New chat defaults ─────────────────────────────────────────
    let (active_ref, active_label) = match &ag.default_target {
        None => (None, s.agent_target_last().to_owned()),
        Some(AgentTarget::Pinned {
            provider_id, model, ..
        }) => {
            let id_ref = format!("{provider_id}/{model}");
            let label = display_model_ref(&id_ref, providers);
            (Some(id_ref), label)
        }
        Some(AgentTarget::Named { name }) => {
            let label = display_model_ref(name, providers);
            (Some(name.clone()), label)
        }
    };

    let ent_model = cx.entity().clone();
    let default_model_picker = model_picker_popover(
        "agent-default-model-pop",
        "agent-default-model-btn",
        active_label,
        active_ref,
        Some(SharedString::from(
            s.page_defaults_last_used_follow_latest_choice(),
        )),
        view.model_choices.clone(),
        lang,
        move |picked, cx| {
            let target_json = match picked {
                None => serde_json::Value::Null,
                Some(c) => serde_json::json!({
                    "kind": "pinned",
                    "provider_id": c.provider_id,
                    "model": c.model_id,
                }),
            };
            ent_model.update(cx, |this, cx| {
                this.patch_ai(
                    serde_json::json!({ "agent": { "default_target": target_json } }),
                    None,
                    cx,
                );
            });
        },
        cx,
    );

    // Fallback chain control
    let fallback_ctl = render_fallback_chain(view, "main", &ag.fallbacks.main, providers, lang, cx);

    // Default reasoning effort segmented switch
    let effort_idx = match ag.default_effort {
        ReasoningEffortOrAuto::Auto => 0,
        ReasoningEffortOrAuto::Minimal | ReasoningEffortOrAuto::Low => 1,
        ReasoningEffortOrAuto::Medium => 2,
        ReasoningEffortOrAuto::High | ReasoningEffortOrAuto::Max => 3,
    };
    let ent_effort = cx.entity().clone();
    let effort_seg = tab_bar_segmented("agent-default-effort-seg")
        .selected_index(effort_idx)
        .child(tab_item(s.opt_network_http_version_auto()))
        .child(tab_item(s.opt_ai_agent_default_effort_low()))
        .child(tab_item(s.opt_ai_agent_default_effort_medium()))
        .child(tab_item(s.opt_ai_agent_default_effort_high()))
        .on_click(move |idx, _, cx| {
            let val = match *idx {
                1 => "low",
                2 => "medium",
                3 => "high",
                _ => "auto",
            };
            ent_effort.update(cx, |this, cx| {
                this.patch_ai(
                    serde_json::json!({ "agent": { "default_effort": val } }),
                    None,
                    cx,
                );
            });
        });

    // Default project folder
    let ws_label = ag
        .default_workspace
        .clone()
        .unwrap_or_else(|| s.agent_folder_ask().to_owned());
    let ws_ctl = h_row()
        .gap(px(6.0))
        .child(
            btn_secondary("agent-pick-default-ws", ws_label)
                .icon(icon_xs(UiIcon::FolderOpen))
                .on_click(cx.listener(|this, _, _, cx| this.pick_default_workspace(cx))),
        )
        .children(ag.default_workspace.is_some().then(|| {
            btn_ghost("agent-clear-default-ws", s.page_defaults_clear()).on_click(cx.listener(
                |this, _, _, cx| {
                    this.patch_ai(
                        serde_json::json!({ "agent": { "default_workspace": null } }),
                        None,
                        cx,
                    );
                },
            ))
        }));

    // Prompt cache mode
    let cache_idx = usize::from(ag.prompt_cache == PromptCacheMode::Off);
    let ent_cache = cx.entity().clone();
    let cache_seg = tab_bar_segmented("agent-prompt-cache-seg")
        .selected_index(cache_idx)
        .child(tab_item(s.opt_network_http_version_auto()))
        .child(tab_item(s.notify_off()))
        .on_click(move |idx, _, cx| {
            let val = if *idx == 1 { "off" } else { "auto" };
            ent_cache.update(cx, |this, cx| {
                this.patch_ai(
                    serde_json::json!({ "agent": { "prompt_cache": val } }),
                    None,
                    cx,
                );
            });
        });

    let reset_defaults_btn = btn_ghost(
        "agent-reset-defaults-sec",
        s.page_defaults_restore_defaults(),
    )
    .on_click(cx.listener(|this, _, window, cx| {
        this.reset_ai_keys(
            &[
                "agent.default_target",
                "agent.fallbacks.main",
                "agent.default_effort",
                "agent.default_workspace",
                "agent.auto_title",
                "agent.prompt_cache",
            ],
            window,
            cx,
        );
    }))
    .into_any_element();

    let sec_defaults = section_card(
        s.page_defaults_new_chat_defaults(),
        Some(reset_defaults_btn),
        v_col()
            .w_full()
            .child(set_row(
                s.f_ai_agent_default_target(),
                None,
                Some(SharedString::from(
                    s.page_defaults_model_a_new_chat_starts(),
                )),
                default_model_picker,
                cx,
            ))
            .child(set_row(
                s.page_defaults_fallback_chain(),
                None,
                Some(SharedString::from(
                    s.page_defaults_ordered_fallback_models_tried_when(),
                )),
                fallback_ctl,
                cx,
            ))
            .child(set_row(
                s.page_defaults_default_reasoning_effort(),
                None,
                Some(SharedString::from(
                    s.page_defaults_how_hard_models_reason_by(),
                )),
                effort_seg,
                cx,
            ))
            .child(set_row(
                s.page_defaults_default_project_folder(),
                Some(SharedString::from(s.page_defaults_local_only())),
                Some(SharedString::from(
                    s.page_defaults_working_folder_bound_to_new(),
                )),
                ws_ctl,
                cx,
            ))
            .child(set_row(
                s.f_ai_agent_auto_title(),
                None,
                Some(SharedString::from(s.page_defaults_gives_a_new_chat_a())),
                toggle_switch("agent-auto-title", ag.auto_title).on_change(cx.listener(
                    |this, checked: &bool, _, cx| {
                        this.patch_ai(
                            serde_json::json!({ "agent": { "auto_title": *checked } }),
                            None,
                            cx,
                        );
                    },
                )),
                cx,
            ))
            .child(set_row(
                s.f_ai_agent_prompt_cache(),
                None,
                Some(SharedString::from(
                    s.page_defaults_auto_marks_prompt_prefixes_cacheable(),
                )),
                cache_seg,
                cx,
            )),
        cx,
    );

    // ── Section 2: Context window ────────────────────────────────────────────
    let reset_ctx_btn = btn_ghost("agent-reset-ctx-sec", s.page_defaults_reset())
        .icon(icon_xs(UiIcon::RotateCcw))
        .on_click(cx.listener(|this, _, window, cx| {
            this.reset_ai_keys(
                &[
                    "agent.auto_compact",
                    "agent.compact_threshold_pct",
                    "agent.keep_recent_turns",
                ],
                window,
                cx,
            );
        }))
        .into_any_element();

    let sec_context = section_card(
        s.page_defaults_context_window(),
        Some(reset_ctx_btn),
        v_col()
            .w_full()
            .child(set_row(
                s.page_defaults_automatic_compaction(),
                None,
                Some(SharedString::from(
                    s.page_defaults_summarises_older_turns_when_the(),
                )),
                toggle_switch("agent-auto-compact", ag.auto_compact).on_change(cx.listener(
                    |this, checked: &bool, _, cx| {
                        this.patch_ai(
                            serde_json::json!({ "agent": { "auto_compact": *checked } }),
                            None,
                            cx,
                        );
                    },
                )),
                cx,
            ))
            .child(set_row(
                s.page_defaults_compact_at(),
                None,
                Some(SharedString::from(
                    s.page_defaults_share_of_context_window_50(),
                )),
                num_box(&view.compact_pct_input, Some("%"), 84.0, cx),
                cx,
            ))
            .child(set_row(
                s.f_ai_agent_keep_recent_turns(),
                None,
                Some(SharedString::from(
                    s.page_defaults_recent_turns_kept_verbatim_during(),
                )),
                num_box(&view.keep_turns_input, Some(s.agent_unit_turns()), 84.0, cx),
                cx,
            )),
        cx,
    );

    // ── Section 3: Run limits & cost caps (collapsible grouped card)
    let cost_summary = match (ag.cost_cap_usd, ag.cost_cap_cny) {
        (None, None) => s.agent_limits_no_cap().to_owned(),
        (Some(u), None) => format!("${u:.2}"),
        (None, Some(c)) => format!("¥{c:.2}"),
        (Some(u), Some(c)) => format!("${u:.2} / ¥{c:.2}"),
    };
    let summary_text = s.page_defaults_steps_concurrent(
        ag.max_tool_iterations,
        ag.max_concurrent_runs,
        cost_summary,
    );

    let reset_limits_btn = btn_ghost("agent-reset-limits-sec", s.page_defaults_reset())
        .icon(icon_xs(UiIcon::RotateCcw))
        .on_click(cx.listener(|this, _, window, cx| {
            this.reset_ai_keys(
                &[
                    "agent.max_tool_iterations",
                    "agent.max_concurrent_runs",
                    "agent.cost_cap_usd",
                    "agent.cost_cap_cny",
                    "agent.max_run_tokens",
                    "agent.wrap_up_last_step",
                    "agent.max_image_mb",
                    "agent.max_file_mb",
                    "agent.image_max_edge_px",
                ],
                window,
                cx,
            );
        }));

    let hover_bg = to_hsla(p.hover.composite_over(p.surface));
    let mut disclosure_row = h_row()
        .id("agent-limits-disclosure")
        .w_full()
        .justify_between()
        .gap(px(10.0))
        .px(px(16.0))
        .py(px(13.0))
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        .on_click(cx.listener(|this, _, _, cx| {
            this.limits_open = !this.limits_open;
            cx.notify();
        }))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(8.0))
                .flex_wrap()
                .child(
                    div()
                        .text_color(to_hsla(p.muted))
                        .child(icon_xs(if view.limits_open {
                            UiIcon::ChevronDown
                        } else {
                            UiIcon::ChevronRight
                        })),
                )
                .child(
                    div()
                        .text_size(px(13.5))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .child(s.page_defaults_advanced_run_spend_guardrails()),
                )
                .children((!view.limits_open).then(|| badge(summary_text, Tone::Neutral, cx))),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(if view.limits_open {
                    s.page_defaults_collapse()
                } else {
                    s.page_defaults_configure()
                }),
        );

    if view.limits_open {
        disclosure_row = disclosure_row
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)));
    }

    let mut limits_inner = v_col().w_full().child(disclosure_row);
    if view.limits_open {
        limits_inner = limits_inner.child(
            v_col()
                .w_full()
                .px(px(16.0))
                .mb(px(-1.0))
                .child(set_row(
                    s.page_defaults_max_tool_steps_per_run(),
                    None,
                    Some(SharedString::from(s.page_defaults_model_calls_per_run_1())),
                    num_box(&view.max_steps_input, Some(s.agent_unit_calls()), 84.0, cx),
                    cx,
                ))
                .child(set_row(
                    s.page_defaults_max_concurrent_runs(),
                    None,
                    Some(SharedString::from(s.page_defaults_runs_active_at_once_1())),
                    num_box(&view.max_runs_input, Some(s.agent_unit_runs()), 84.0, cx),
                    cx,
                ))
                .child(set_row(
                    s.page_defaults_stop_a_run_above_cost(),
                    None,
                    Some(SharedString::from(
                        s.page_defaults_leave_empty_for_off_pauses(),
                    )),
                    h_row()
                        .gap(px(10.0))
                        .child(num_box(&view.cost_usd_input, Some("USD"), 76.0, cx))
                        .child(num_box(&view.cost_cny_input, Some("CNY"), 76.0, cx)),
                    cx,
                ))
                .child(set_row(
                    s.page_defaults_token_limit_per_run(),
                    Some(SharedString::from(s.page_defaults_local_only())),
                    Some(SharedString::from(
                        s.page_defaults_pauses_at_this_many_input(),
                    )),
                    num_box(&view.max_tokens_input, Some("tok"), 108.0, cx),
                    cx,
                ))
                .child(set_row(
                    s.f_ai_agent_wrap_up_last_step(),
                    None,
                    Some(SharedString::from(
                        s.page_defaults_last_allowed_step_calls_no(),
                    )),
                    toggle_switch("agent-wrap-up", ag.wrap_up_last_step).on_change(cx.listener(
                        |this, checked: &bool, _, cx| {
                            this.patch_ai(
                                serde_json::json!({ "agent": { "wrap_up_last_step": *checked } }),
                                None,
                                cx,
                            );
                        },
                    )),
                    cx,
                ))
                .child(set_row(
                    s.page_defaults_attachment_size_image_edge(),
                    None,
                    Some(SharedString::from(s.page_defaults_limits_for_images_1_20())),
                    h_row()
                        .gap(px(8.0))
                        .child(num_box(
                            &view.max_image_input,
                            Some(s.page_defaults_mb_img()),
                            60.0,
                            cx,
                        ))
                        .child(num_box(
                            &view.max_file_input,
                            Some(s.page_defaults_mb_file()),
                            60.0,
                            cx,
                        ))
                        .child(num_box(&view.max_edge_input, Some("px"), 72.0, cx)),
                    cx,
                )),
        );
    }

    let sec_limits = section_card_flush(
        s.page_defaults_run_limits_cost_caps(),
        view.limits_open
            .then(|| reset_limits_btn.into_any_element()),
        limits_inner,
        cx,
    );

    v_col()
        .gap(px(20.0))
        .child(sec_defaults)
        .child(sec_context)
        .child(sec_limits)
}
