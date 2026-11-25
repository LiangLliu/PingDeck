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

    // ── Section 1: New chat defaults ─────────────────────────────────────────
    let (active_ref, active_label) = match &ag.default_target {
        None => (None, if zh { "上次使用" } else { "Last used" }.to_owned()),
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
        Some(SharedString::from(if zh {
            "上次使用（跟随最近选择）"
        } else {
            "Last used (follow latest choice)"
        })),
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
    let fallback_ctl =
        render_fallback_chain(view, "main", &ag.fallbacks.main, providers, lang, zh, cx);

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
        .child(tab_item(if zh { "自动" } else { "Auto" }))
        .child(tab_item(if zh { "低" } else { "Low" }))
        .child(tab_item(if zh { "中" } else { "Medium" }))
        .child(tab_item(if zh { "高" } else { "High" }))
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
        .unwrap_or_else(|| if zh { "每次询问" } else { "Ask each time" }.to_owned());
    let ws_ctl = h_row()
        .gap(px(6.0))
        .child(
            btn_secondary("agent-pick-default-ws", ws_label)
                .icon(icon_xs(UiIcon::FolderOpen))
                .on_click(cx.listener(|this, _, _, cx| this.pick_default_workspace(cx))),
        )
        .children(ag.default_workspace.is_some().then(|| {
            btn_ghost("agent-clear-default-ws", if zh { "清除" } else { "Clear" }).on_click(
                cx.listener(|this, _, _, cx| {
                    this.patch_ai(
                        serde_json::json!({ "agent": { "default_workspace": null } }),
                        None,
                        cx,
                    );
                }),
            )
        }));

    // Prompt cache mode
    let cache_idx = usize::from(ag.prompt_cache == PromptCacheMode::Off);
    let ent_cache = cx.entity().clone();
    let cache_seg = tab_bar_segmented("agent-prompt-cache-seg")
        .selected_index(cache_idx)
        .child(tab_item(if zh { "自动" } else { "Auto" }))
        .child(tab_item(if zh { "关闭" } else { "Off" }))
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
        if zh {
            "恢复本区默认值"
        } else {
            "Restore defaults"
        },
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
        if zh { "新对话默认配置" } else { "New chat defaults" },
        Some(reset_defaults_btn),
        v_col()
            .w_full()
            .child(set_row(
                if zh { "默认模型" } else { "Default model" },
                None,
                Some(SharedString::from(if zh {
                    "新建对话时自动选中的主模型；“上次使用”跟随最近选择。"
                } else {
                    "Model a new chat starts with. Last used follows your latest choice."
                })),
                default_model_picker,
                cx,
            ))
            .child(set_row(
                if zh { "主备用链" } else { "Fallback chain" },
                None,
                Some(SharedString::from(if zh {
                    "主模型失败（限流、服务端错误、超时、模型不存在）时的自动回退顺序，最多 5 个。"
                } else {
                    "Ordered fallback models tried when the chat model fails (up to 5)."
                })),
                fallback_ctl,
                cx,
            ))
            .child(set_row(
                if zh { "默认思考力度" } else { "Default reasoning effort" },
                None,
                Some(SharedString::from(if zh {
                    "支持推理强度的模型按此默认档位运行，自动按模型支持范围裁剪。"
                } else {
                    "How hard models reason by default, clamped to what each model supports."
                })),
                effort_seg,
                cx,
            ))
            .child(set_row(
                if zh { "默认项目目录" } else { "Default project folder" },
                Some(SharedString::from(if zh { "仅这台电脑" } else { "Local only" })),
                Some(SharedString::from(if zh {
                    "新对话自动绑定的工作目录；未设置时仅在对话中按需绑定。"
                } else {
                    "Working folder bound to new chats. File and shell tools stay inside it."
                })),
                ws_ctl,
                cx,
            ))
            .child(set_row(
                if zh { "自动命名对话" } else { "Name chats automatically" },
                None,
                Some(SharedString::from(if zh {
                    "首轮对话结束后自动生成简短标题。"
                } else {
                    "Gives a new chat a short title after the first answer."
                })),
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
                if zh { "提示缓存" } else { "Prompt cache" },
                None,
                Some(SharedString::from(if zh {
                    "“自动”在支持缓存的目标上把提示前缀标为可缓存；“关闭”不发送缓存标记。"
                } else {
                    "Auto marks prompt prefixes cacheable on supported targets; Off sends no cache hints."
                })),
                cache_seg,
                cx,
            )),
        cx,
    );

    // ── Section 2: Context window ────────────────────────────────────────────
    let reset_ctx_btn = btn_ghost("agent-reset-ctx-sec", if zh { "恢复默认" } else { "Reset" })
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
        if zh {
            "上下文窗口"
        } else {
            "Context window"
        },
        Some(reset_ctx_btn),
        v_col()
            .w_full()
            .child(set_row(
                if zh {
                    "自动压缩上下文"
                } else {
                    "Automatic compaction"
                },
                None,
                Some(SharedString::from(if zh {
                    "接近模型上下文上限时自动摘要早期对话，保持长对话可继续运行。"
                } else {
                    "Summarises older turns when the context window fills up."
                })),
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
                if zh {
                    "触发压缩阈值"
                } else {
                    "Compact at"
                },
                None,
                Some(SharedString::from(if zh {
                    "上下文占用超过该比例（50–95%）时启动压缩。"
                } else {
                    "Share of context window (50–95%) that triggers compaction."
                })),
                num_box(&view.compact_pct_input, Some("%"), 84.0, cx),
                cx,
            ))
            .child(set_row(
                if zh {
                    "保留最近轮数"
                } else {
                    "Keep last turns"
                },
                None,
                Some(SharedString::from(if zh {
                    "压缩时原文保留的最近对话轮数（1–20 轮）。"
                } else {
                    "Recent turns kept verbatim during compaction (1–20)."
                })),
                num_box(
                    &view.keep_turns_input,
                    Some(if zh { "轮" } else { "turns" }),
                    84.0,
                    cx,
                ),
                cx,
            )),
        cx,
    );

    // ── Section 3: Run limits & cost caps (collapsible grouped card)
    let cost_summary = match (ag.cost_cap_usd, ag.cost_cap_cny) {
        (None, None) => if zh { "费用不限" } else { "No cost limit" }.to_owned(),
        (Some(u), None) => format!("${u:.2}"),
        (None, Some(c)) => format!("¥{c:.2}"),
        (Some(u), Some(c)) => format!("${u:.2} / ¥{c:.2}"),
    };
    let summary_text = if zh {
        format!(
            "单轮 {} 步 · {} 并发 · {}",
            ag.max_tool_iterations, ag.max_concurrent_runs, cost_summary
        )
    } else {
        format!(
            "{} steps · {} concurrent · {}",
            ag.max_tool_iterations, ag.max_concurrent_runs, cost_summary
        )
    };

    let reset_limits_btn = btn_ghost(
        "agent-reset-limits-sec",
        if zh { "恢复默认" } else { "Reset" },
    )
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
                        .child(if zh {
                            "高级运行与费用护栏"
                        } else {
                            "Advanced run & spend guardrails"
                        }),
                )
                .children((!view.limits_open).then(|| badge(summary_text, Tone::Neutral, cx))),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(if view.limits_open {
                    if zh { "收起" } else { "Collapse" }
                } else if zh {
                    "展开配置"
                } else {
                    "Configure"
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
                    if zh { "单轮最大工具调用步数" } else { "Max tool steps per run" },
                    None,
                    Some(SharedString::from(if zh {
                        "防止无限工具循环的安全上限（1–100 次），触发后暂停并询问是否继续。"
                    } else {
                        "Model calls per run (1–100) before pausing to ask."
                    })),
                    num_box(
                        &view.max_steps_input,
                        Some(if zh { "次" } else { "calls" }),
                        84.0,
                        cx,
                    ),
                    cx,
                ))
                .child(set_row(
                    if zh { "最大并发顶层运行数" } else { "Max concurrent runs" },
                    None,
                    Some(SharedString::from(if zh {
                        "同时在后台运行的对话上限（1–8 个），超出的运行自动排队。"
                    } else {
                        "Runs active at once (1–8); extra runs queue."
                    })),
                    num_box(
                        &view.max_runs_input,
                        Some(if zh { "个" } else { "runs" }),
                        84.0,
                        cx,
                    ),
                    cx,
                ))
                .child(set_row(
                    if zh { "单次运行费用暂停上限" } else { "Stop a run above (cost cap)" },
                    None,
                    Some(SharedString::from(if zh {
                        "留空表示关闭；触发后弹出费用门禁由你决定是否继续。"
                    } else {
                        "Leave empty for Off. Pauses and asks when estimated spend passes the limit."
                    })),
                    h_row()
                        .gap(px(10.0))
                        .child(num_box(&view.cost_usd_input, Some("USD"), 76.0, cx))
                        .child(num_box(&view.cost_cny_input, Some("CNY"), 76.0, cx)),
                    cx,
                ))
                .child(set_row(
                    if zh { "每次运行的 Token 上限" } else { "Token limit per run" },
                    Some(SharedString::from(if zh { "仅这台电脑" } else { "Local only" })),
                    Some(SharedString::from(if zh {
                        "输入加输出 token 达到该值时暂停并询问；留空表示关闭。"
                    } else {
                        "Pauses at this many input + output tokens; empty is Off."
                    })),
                    num_box(&view.max_tokens_input, Some("tok"), 108.0, cx),
                    cx,
                ))
                .child(set_row(
                    if zh { "最后一步写总结" } else { "Summarise on the last step" },
                    None,
                    Some(SharedString::from(if zh {
                        "允许的最后一步不调用工具，让模型总结当前进展与停在哪里。"
                    } else {
                        "Last allowed step calls no tools and asks the model to sum up."
                    })),
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
                    if zh { "附件体积与图片长边上限" } else { "Attachment size & image edge" },
                    None,
                    Some(SharedString::from(if zh {
                        "超限图片/文件将被拒绝，大图按长边（512–2048 px）等比缩放。"
                    } else {
                        "Limits for images (1–20 MB), files (1–100 MB), and max image edge (512–2048 px)."
                    })),
                    h_row()
                        .gap(px(8.0))
                        .child(num_box(
                            &view.max_image_input,
                            Some(if zh { "MB 图" } else { "MB img" }),
                            60.0,
                            cx,
                        ))
                        .child(num_box(
                            &view.max_file_input,
                            Some(if zh { "MB 文件" } else { "MB file" }),
                            60.0,
                            cx,
                        ))
                        .child(num_box(&view.max_edge_input, Some("px"), 72.0, cx)),
                    cx,
                )),
        );
    }

    let sec_limits = section_card_flush(
        if zh {
            "运行限制与费用上限"
        } else {
            "Run limits & cost caps"
        },
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
