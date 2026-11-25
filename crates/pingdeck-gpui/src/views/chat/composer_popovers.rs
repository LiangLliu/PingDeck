//! Native anchored popovers & context meter for the Chat composer:
//! Skills (`/`), Permission mode, Reasoning effort, searchable Model selector, and Context meter.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::model::ReasoningEffort;
use pingdeck_desktop::core::model::settings::ToolApproval;
use pingdeck_desktop::state::SettingsSection;

use crate::ui::{
    Anchor, ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, h_row, icon_xs, list_row,
    palette, pill_btn, popover, progress_bar, search_input, section_heading, to_hsla, v_col,
};

use super::ChatView;
use super::types::ChatHandoff;

pub fn render_skills_popover(
    view: &ChatView,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let skills = view.available_skills.clone();
    let selected = view.selected_skills.clone();
    let chat_ent = cx.entity().clone();
    popover("chat-skills-popover")
        .anchor(Anchor::BottomLeft)
        .trigger(pill_btn(
            "chat-skills-trigger",
            if selected.is_empty() {
                if zh { "/ 技能" } else { "/ Skills" }.to_owned()
            } else {
                format!("/ {}", selected.len())
            },
            None,
            !selected.is_empty(),
        ))
        .content(move |_, _, pop_cx| {
            let pp = palette(pop_cx);
            let mut m = v_col().w(px(260.0)).gap(px(3.0)).child(section_heading(
                if zh {
                    "可用技能 (SKILL.md)"
                } else {
                    "Skills"
                },
                pop_cx,
            ));
            if skills.is_empty() {
                m = m.child(
                    div()
                        .py(px(8.0))
                        .text_xs()
                        .text_color(to_hsla(pp.muted))
                        .child(if zh {
                            "未发现技能，可在 设置 › 技能 中配置目录"
                        } else {
                            "No skills found — configure in Settings › Skills"
                        }),
                );
            } else {
                for sk in skills.iter().filter(|s| s.enabled) {
                    let is_sel = selected.contains(&sk.name);
                    let sname = sk.name.clone();
                    let ent = chat_ent.clone();
                    let pop_h = pop_cx.entity().clone();
                    m = m.child(
                        list_row(
                            SharedString::from(format!("chat-sk-opt-{}", sk.name)),
                            is_sel,
                            pop_cx,
                            move |_, w, cx| {
                                ent.update(cx, |this, cx| {
                                    if let Some(pos) =
                                        this.selected_skills.iter().position(|x| x == &sname)
                                    {
                                        this.selected_skills.remove(pos);
                                    } else {
                                        this.selected_skills.push(sname.clone());
                                    }
                                    cx.notify();
                                });
                                pop_h.update(cx, |s, cx| s.dismiss(w, cx));
                            },
                        )
                        .justify_between()
                        .child(
                            v_col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(to_hsla(pp.ink))
                                        .child(format!("/{}", sk.name)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(to_hsla(pp.muted))
                                        .truncate()
                                        .child(sk.description.clone()),
                                ),
                        )
                        .children(is_sel.then(|| icon_xs(UiIcon::Check))),
                    );
                }
            }
            m
        })
}

pub fn render_perm_popover(
    mode: ToolApproval,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let (label, glyph) = match mode {
        ToolApproval::Ask => (if zh { "每次询问" } else { "Ask" }, UiIcon::Shield),
        ToolApproval::AutoReadOnly => (
            if zh { "只读自动" } else { "Auto reads" },
            UiIcon::ShieldCheck,
        ),
        ToolApproval::AutoAll => (
            if zh { "完全权限" } else { "Full access" },
            UiIcon::ShieldAlert,
        ),
    };
    let chat_ref = cx.entity().clone();
    let options: [(ToolApproval, &str, &str); 3] = [
        (
            ToolApproval::Ask,
            if zh { "每次询问" } else { "Ask every time" },
            if zh {
                "所有工具调用前均需你确认"
            } else {
                "Confirm before every tool call"
            },
        ),
        (
            ToolApproval::AutoReadOnly,
            if zh { "只读自动" } else { "Auto read-only" },
            if zh {
                "读取类自动放行，写入和执行仍需审批"
            } else {
                "Auto-allow reads; ask for edits and exec"
            },
        ),
        (
            ToolApproval::AutoAll,
            if zh { "完全权限" } else { "Full access" },
            if zh {
                "自动放行常规工具（硬阻止与 OS 沙箱保持开启）"
            } else {
                "Auto-run tools (hard blocks & OS sandbox stay on)"
            },
        ),
    ];

    popover("chat-perm-popover")
        .anchor(Anchor::BottomLeft)
        .trigger(pill_btn(
            "chat-perm-trigger",
            label,
            Some(glyph),
            mode == ToolApproval::AutoAll,
        ))
        .content(move |_st, _window, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut box_el =
                v_col()
                    .w(px(276.0))
                    .gap(px(4.0))
                    .child(div().px(px(6.0)).pb(px(2.0)).child(section_heading(
                        if zh {
                            "工具权限模式"
                        } else {
                            "Permission mode"
                        },
                        pop_cx,
                    )));
            for (opt, title, desc) in options {
                let selected = opt == mode;
                let entity = chat_ref.clone();
                let pop_handle = pop_cx.entity().clone();
                box_el = box_el.child(
                    list_row(
                        SharedString::from(format!("perm-opt-{opt:?}")),
                        selected,
                        pop_cx,
                        move |_, window, cx| {
                            entity.update(cx, |this, cx| this.request_permission_mode(opt, cx));
                            pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .child(
                        v_col()
                            .flex_1()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(pop_p.ink))
                                    .child(title),
                            )
                            .child(div().text_xs().text_color(to_hsla(pop_p.muted)).child(desc)),
                    )
                    .children(selected.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            let ent_link = chat_ref.clone();
            let pop_link = pop_cx.entity().clone();
            box_el.child(
                div()
                    .pt(px(4.0))
                    .border_t_1()
                    .border_color(to_hsla(pop_p.line.composite_over(pop_p.surface)))
                    .child(
                        btn_ghost(
                            "chat-perm-eval-order",
                            if zh {
                                "评估顺序与权限规则 ↗"
                            } else {
                                "Evaluation order ↗"
                            },
                        )
                        .on_click(move |_, w, cx| {
                            ent_link.update(cx, |this, cx| {
                                this.pending_handoff.set(Some(ChatHandoff::OpenSettings(
                                    SettingsSection::Permissions,
                                )));
                                cx.notify();
                            });
                            pop_link.update(cx, |st, cx| st.dismiss(w, cx));
                        }),
                    ),
            )
        })
}

pub fn render_effort_popover(
    effort: Option<ReasoningEffort>,
    missing_reasoning_quirk: bool,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let label = match effort {
        None => {
            if zh {
                "推理: 自动"
            } else {
                "Effort: Auto"
            }
        }
        Some(ReasoningEffort::Minimal) => {
            if zh {
                "推理: 极简"
            } else {
                "Effort: Minimal"
            }
        }
        Some(ReasoningEffort::Low) => {
            if zh {
                "推理: 低"
            } else {
                "Effort: Low"
            }
        }
        Some(ReasoningEffort::Medium) => {
            if zh {
                "推理: 中"
            } else {
                "Effort: Medium"
            }
        }
        Some(ReasoningEffort::High) => {
            if zh {
                "推理: 高"
            } else {
                "Effort: High"
            }
        }
        Some(ReasoningEffort::Max) => {
            if zh {
                "推理: 最高"
            } else {
                "Effort: Max"
            }
        }
    };
    let chat_ref = cx.entity().clone();
    let opts: [(Option<ReasoningEffort>, &str); 6] = [
        (
            None,
            if zh {
                "自动 (模型默认)"
            } else {
                "Auto (model default)"
            },
        ),
        (
            Some(ReasoningEffort::Minimal),
            if zh { "极简" } else { "Minimal" },
        ),
        (Some(ReasoningEffort::Low), if zh { "低" } else { "Low" }),
        (
            Some(ReasoningEffort::Medium),
            if zh { "中" } else { "Medium" },
        ),
        (Some(ReasoningEffort::High), if zh { "高" } else { "High" }),
        (Some(ReasoningEffort::Max), if zh { "最高" } else { "Max" }),
    ];

    popover("chat-effort-popover")
        .anchor(Anchor::BottomLeft)
        .trigger(pill_btn(
            "chat-effort-trigger",
            label,
            Some(UiIcon::Brain),
            effort.is_some(),
        ))
        .content(move |_st, _window, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col().w(px(230.0)).gap(px(2.0));
            for (val, text) in opts {
                let selected = val == effort;
                let entity = chat_ref.clone();
                let pop_handle = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("effort-opt-{val:?}")),
                        selected,
                        pop_cx,
                        move |_, window, cx| {
                            entity.update(cx, |this, cx| this.set_reasoning_effort(val, cx));
                            pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(text))
                    .children(selected.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            if missing_reasoning_quirk {
                list = list.child(
                    div()
                        .mt(px(4.0))
                        .pt(px(6.0))
                        .px(px(6.0))
                        .border_t_1()
                        .border_color(to_hsla(pop_p.line.composite_over(pop_p.surface)))
                        .text_size(px(11.0))
                        .text_color(to_hsla(pop_p.muted))
                        .child(if zh {
                            "自定义服务商未设置「推理参数格式」，可在「服务商 › 配置」开启。"
                        } else {
                            "Custom provider has no reasoning format; configure in Providers › Config."
                        }),
                );
            }
            list
        })
}

pub fn render_model_popover(
    view: &ChatView,
    model_label: String,
    highlight_missing: bool,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let models = view.available_models.clone();
    let current_target = view.session.as_ref().and_then(|s| s.target.clone());
    let chat_ref = cx.entity().clone();
    let search_ent = view.model_search_input.clone();

    popover("chat-model-popover")
        .anchor(Anchor::BottomRight)
        .trigger(
            pill_btn(
                "chat-model-trigger",
                SharedString::from(model_label),
                Some(if highlight_missing {
                    UiIcon::Warning
                } else {
                    UiIcon::Sparkles
                }),
                highlight_missing,
            )
            .max_w(px(280.0)),
        )
        .content(move |_st, _window, pop_cx| {
            let pop_p = palette(pop_cx);
            let q = search_ent.read(pop_cx).value().trim().to_ascii_lowercase();
            let filtered: Vec<_> = models
                .iter()
                .filter(|m| {
                    q.is_empty()
                        || m.model_id.to_ascii_lowercase().contains(&q)
                        || m.provider_name.to_ascii_lowercase().contains(&q)
                })
                .collect();

            let header = v_col()
                .gap(px(6.0))
                .px(px(6.0))
                .pb(px(6.0))
                .border_b_1()
                .border_color(to_hsla(pop_p.line.composite_over(pop_p.surface)))
                .child(
                    h_row()
                        .justify_between()
                        .child(section_heading(
                            if zh {
                                "选择对话模型"
                            } else {
                                "Select model"
                            },
                            pop_cx,
                        ))
                        .child(badge(filtered.len().to_string(), Tone::Neutral, pop_cx)),
                )
                .child(search_input(&search_ent, pop_cx));

            let body: gpui_kit::AnyElement = if filtered.is_empty() {
                div()
                    .px(px(8.0))
                    .py(px(12.0))
                    .text_xs()
                    .text_color(to_hsla(pop_p.muted))
                    .child(if zh {
                        "无匹配模型，可在「服务商」页面添加"
                    } else {
                        "No matching models — add one in Providers"
                    })
                    .into_any_element()
            } else {
                let scroll_h = ((filtered.len() as f32) * 32.0 + 8.0).clamp(64.0, 300.0);
                let mut rows = v_col().w_full().gap(px(2.0)).py(px(4.0));
                for (i, m) in filtered.iter().enumerate() {
                    let target = m.target();
                    let selected = current_target.as_ref() == Some(&target);
                    let entity = chat_ref.clone();
                    let pop_handle = pop_cx.entity().clone();
                    rows = rows.child(
                        list_row(
                            SharedString::from(format!("model-opt-{i}")),
                            selected,
                            pop_cx,
                            move |_, window, cx| {
                                let t = target.clone();
                                entity.update(cx, |this, cx| this.select_model(t, cx));
                                pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                            },
                        )
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            h_row()
                                .flex_1()
                                .min_w_0()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(to_hsla(pop_p.muted))
                                        .child(SharedString::from(format!(
                                            "{} /",
                                            m.provider_name
                                        ))),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .font_weight(if selected {
                                            gpui_kit::FontWeight::SEMIBOLD
                                        } else {
                                            gpui_kit::FontWeight::MEDIUM
                                        })
                                        .text_color(to_hsla(pop_p.ink))
                                        .truncate()
                                        .child(SharedString::from(m.model_id.clone())),
                                ),
                        )
                        .children(selected.then(|| {
                            div()
                                .flex_shrink_0()
                                .text_color(to_hsla(pop_p.accent))
                                .child(icon_xs(UiIcon::Check))
                        })),
                    );
                }
                div()
                    .w_full()
                    .h(px(scroll_h))
                    .overflow_y_scrollbar()
                    .id("chat-model-pop-scroll")
                    .child(rows)
                    .into_any_element()
            };

            v_col().w(px(340.0)).gap(px(2.0)).child(header).child(body)
        })
}

pub fn render_context_meter(view: &ChatView, cx: &gpui_kit::App) -> impl IntoElement {
    let p = palette(cx);
    let used = view.thread.context_used_tokens;
    let limit = view.thread.context_limit_tokens;
    let label = if let Some(lim) = limit.filter(|&l| l > 0) {
        let pct = ((used as f64 / lim as f64) * 100.0).min(100.0) as f32;
        return h_row()
            .gap(px(5.0))
            .child(div().w(px(44.0)).child(progress_bar("ctx-mini-bar", pct)))
            .child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(format!("{:.0}%", pct))),
            );
    } else if used >= 1000 {
        format!("{:.1}k", used as f64 / 1000.0)
    } else {
        format!("{used} tok")
    };

    h_row().child(
        div()
            .text_xs()
            .text_color(to_hsla(p.faint))
            .child(SharedString::from(label)),
    )
}
