//! Gateway Fallback Rules tab: ordered failover chains per primary model,
//! live route-order preview (`llm_route_preview_impl`), and inline rule editor.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::llm::store::ModelRef;

use crate::ui::{
    Anchor, ScrollableElement as _, Tone, UiIcon, badge, btn_danger, btn_ghost, btn_icon,
    btn_primary, btn_secondary, card, h_row, icon_xs, list_row, palette, popover, section_heading,
    select_pill, to_hsla, v_col,
};

use super::ProvidersView;
use super::format::format_rule_status;

pub fn render_rules_tab(
    view: &ProvidersView,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut root = v_col().gap(px(14.0));

    // Top bar with explanation & "+ Add rule" primary picker popover.
    let mut add_candidates: Vec<(String, String, String)> = Vec::new();
    for m in &view.models {
        let already = view.gateway_rules.iter().any(|r| {
            r.primary.provider_id == m.model.provider_id && r.primary.model == m.model.model_id
        });
        if !already {
            let pname = view.provider_name(&m.model.provider_id);
            add_candidates.push((
                m.model.provider_id.clone(),
                m.model.model_id.clone(),
                format!("{pname}/{}", m.model.model_id),
            ));
        }
    }
    let ent = cx.entity().clone();
    let add_btn = popover("gw-rule-add-pop")
        .anchor(Anchor::TopRight)
        .trigger(btn_primary(
            "gw-rule-add-trig",
            if zh { "+ 添加规则" } else { "+ Add rule" },
        ))
        .content(move |_, _, cx| {
            let p = palette(cx);
            let list_h = ((add_candidates.len().min(60) as f32) * 30.0 + 12.0).clamp(56.0, 340.0);
            let mut list = v_col().w(px(300.0)).gap(px(2.0)).p(px(6.0));
            if add_candidates.is_empty() {
                list = list.child(
                    div()
                        .p(px(8.0))
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(if zh {
                            "没有可配置规则的模型（请先添加服务商并同步模型）"
                        } else {
                            "No models available without a rule yet"
                        }),
                );
            } else {
                for (idx, (pid, mid, display)) in add_candidates.iter().take(60).enumerate() {
                    let pid = pid.clone();
                    let mid = mid.clone();
                    let ent = ent.clone();
                    list = list.child(
                        list_row(
                            SharedString::from(format!("rule-pick-{idx}")),
                            false,
                            cx,
                            move |_, _, cx| {
                                ent.update(cx, |this, cx| {
                                    this.start_edit_rule(
                                        ModelRef {
                                            provider_id: pid.clone(),
                                            model: mid.clone(),
                                        },
                                        Vec::new(),
                                        cx,
                                    );
                                });
                            },
                        )
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .child(SharedString::from(display.clone())),
                        ),
                    );
                }
            }
            div()
                .w(px(300.0))
                .h(px(list_h))
                .overflow_y_scrollbar()
                .id("gw-rule-add-scroll")
                .child(list)
        });

    root = root.child(
        h_row()
            .justify_between()
            .gap(px(10.0))
            .flex_wrap()
            .child(
                v_col()
                    .gap(px(2.0))
                    .child(section_heading(
                        if zh {
                            "网关备用规则（首个内容事件后绝不切换）"
                        } else {
                            "Gateway Fallback Rules (locked after first content)"
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(if zh {
                                "外部客户端请求左侧主模型失败时，网关按顺序改用右侧备用模型（最多 5 个）。"
                            } else {
                                "When a primary model fails before streaming starts, the gateway tries up to 5 ordered fallbacks."
                            }),
                    ),
            )
            .child(add_btn),
    );

    // Inline rule editor when active.
    if let Some(editor) = &view.rule_editor {
        let primary_disp = format!(
            "{}/{}",
            view.provider_name(&editor.primary.provider_id),
            editor.primary.model
        );
        let mut chain_rows = v_col().gap(px(5.0));
        if editor.chain.is_empty() {
            chain_rows =
                chain_rows.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                    "还没有备用模型，从下方候选列表中点击添加（最多 5 个）。"
                } else {
                    "No fallbacks yet. Pick up to 5 compatible models below."
                }));
        } else {
            for (i, item) in editor.chain.iter().enumerate() {
                let disp = format!("{}/{}", view.provider_name(&item.provider_id), item.model);
                chain_rows = chain_rows.child(
                    h_row()
                        .justify_between()
                        .px(px(9.0))
                        .py(px(5.0))
                        .rounded(px(6.0))
                        .bg(to_hsla(p.surface))
                        .border_1()
                        .border_color(to_hsla(p.line.composite_over(p.surface)))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(badge(format!("#{}", i + 1), Tone::Accent, cx))
                                .child(div().text_xs().text_color(to_hsla(p.ink)).child(disp)),
                        )
                        .child(
                            h_row()
                                .gap(px(3.0))
                                .children((i > 0).then(|| {
                                    btn_icon(("rule-up", i), UiIcon::ChevronUp).on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            if let Some(ed) = &mut this.rule_editor
                                                && i > 0
                                                && i < ed.chain.len()
                                            {
                                                ed.chain.swap(i - 1, i);
                                                cx.notify();
                                            }
                                        }),
                                    )
                                }))
                                .children((i + 1 < editor.chain.len()).then(|| {
                                    btn_icon(("rule-dn", i), UiIcon::ChevronDown).on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            if let Some(ed) = &mut this.rule_editor
                                                && i + 1 < ed.chain.len()
                                            {
                                                ed.chain.swap(i, i + 1);
                                                cx.notify();
                                            }
                                        }),
                                    )
                                }))
                                .child(btn_icon(("rule-rm", i), UiIcon::Close).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        if let Some(ed) = &mut this.rule_editor
                                            && i < ed.chain.len()
                                        {
                                            ed.chain.remove(i);
                                            cx.notify();
                                        }
                                    }),
                                )),
                        ),
                );
            }
        }

        let compatible: Vec<_> = editor
            .candidates
            .iter()
            .filter(|c| {
                c.compatible
                    && !editor
                        .chain
                        .iter()
                        .any(|x| x.provider_id == c.provider_id && x.model == c.model)
            })
            .cloned()
            .collect();
        let ent_pick = cx.entity().clone();
        let can_add = editor.chain.len() < 5 && !compatible.is_empty();
        let add_fb_pop = popover("gw-fb-cand-pop")
            .anchor(Anchor::TopLeft)
            .trigger(select_pill(
                "gw-fb-cand-trig",
                if zh {
                    "+ 添加备用模型"
                } else {
                    "+ Add fallback"
                },
                false,
                cx,
            ))
            .content(move |_, _, cx| {
                let p = palette(cx);
                let list_h = ((compatible.len().min(60) as f32) * 30.0 + 12.0).clamp(56.0, 320.0);
                let mut list = v_col().w(px(300.0)).gap(px(2.0)).p(px(6.0));
                for (i, c) in compatible.iter().take(60).enumerate() {
                    let pid = c.provider_id.clone();
                    let mid = c.model.clone();
                    let ent = ent_pick.clone();
                    let pname = ent.read(cx).provider_name(&pid);
                    list = list.child(
                        list_row(
                            SharedString::from(format!("fb-cand-{i}")),
                            false,
                            cx,
                            move |_, _, cx| {
                                ent.update(cx, |this, cx| {
                                    if let Some(ed) = &mut this.rule_editor
                                        && ed.chain.len() < 5
                                    {
                                        ed.chain.push(ModelRef {
                                            provider_id: pid.clone(),
                                            model: mid.clone(),
                                        });
                                        cx.notify();
                                    }
                                });
                            },
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(format!("{pname} / {}", c.model))),
                        ),
                    );
                }
                div()
                    .w(px(300.0))
                    .h(px(list_h))
                    .overflow_y_scrollbar()
                    .id("gw-fb-cand-scroll")
                    .child(list)
            });

        root =
            root.child(
                card(cx).p(px(14.0)).bg(to_hsla(p.pane)).child(
                    v_col()
                        .gap(px(10.0))
                        .child(
                            h_row()
                                .justify_between()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .child(SharedString::from(if zh {
                                            format!("编辑备用链：{primary_disp} 失败时")
                                        } else {
                                            format!("Edit fallbacks when {primary_disp} fails")
                                        })),
                                )
                                .child(btn_icon("rule-ed-close", UiIcon::Close).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.rule_editor = None;
                                        cx.notify();
                                    }),
                                )),
                        )
                        .child(chain_rows)
                        .child(
                            h_row()
                                .justify_between()
                                .child(div().children(can_add.then_some(add_fb_pop)))
                                .child(
                                    h_row()
                                        .gap(px(6.0))
                                        .child(
                                            btn_ghost(
                                                "rule-ed-cancel",
                                                if zh { "取消" } else { "Cancel" },
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.rule_editor = None;
                                                    cx.notify();
                                                }),
                                            ),
                                        )
                                        .child(
                                            btn_primary(
                                                "rule-ed-save",
                                                if zh { "保存规则" } else { "Save rule" },
                                            )
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.save_rule_editor(zh, cx);
                                                }),
                                            ),
                                        ),
                                ),
                        ),
                ),
            );
    }

    // Existing rules list.
    if view.gateway_rules.is_empty() {
        root = root.child(card(cx).p(px(20.0)).child(
            div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                "没有备用规则：请求失败时直接把错误返回给客户端。点击「+ 添加规则」开始配置。"
            } else {
                "No fallback rules configured. Click + Add rule to create an ordered failover chain."
            }),
        ));
    } else {
        for (idx, rule) in view.gateway_rules.iter().enumerate() {
            let (st_lbl, st_tone) = format_rule_status(rule.status, zh);
            let primary_disp = format!(
                "{}/{}",
                view.provider_name(&rule.primary.provider_id),
                rule.primary.model
            );
            let mut chain_row = h_row().flex_wrap().gap(px(5.0));
            for (fi, fb) in rule.fallbacks.iter().enumerate() {
                if fi > 0 {
                    chain_row =
                        chain_row.child(div().text_xs().text_color(to_hsla(p.faint)).child("→"));
                }
                let fb_tone = rule
                    .fallback_status
                    .get(fi)
                    .map_or(Tone::Neutral, |s| format_rule_status(*s, zh).1);
                chain_row = chain_row.child(badge(
                    format!("{}/{}", view.provider_name(&fb.provider_id), fb.model),
                    fb_tone,
                    cx,
                ));
            }
            let primary_for_prev = rule.primary.id_form();
            let primary_for_edit = rule.primary.clone();
            let fallbacks_for_edit = rule.fallbacks.clone();
            let primary_for_del = rule.primary.clone();

            root = root.child(
                card(cx).p(px(12.0)).child(
                    h_row()
                        .justify_between()
                        .gap(px(10.0))
                        .flex_wrap()
                        .child(
                            v_col()
                                .flex_1()
                                .min_w_0()
                                .gap(px(5.0))
                                .child(
                                    h_row()
                                        .gap(px(6.0))
                                        .flex_wrap()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                .text_color(to_hsla(p.ink))
                                                .child(SharedString::from(primary_disp)),
                                        )
                                        .child(badge(st_lbl, st_tone, cx))
                                        .children(rule.orphan.then(|| {
                                            badge(
                                                if zh { "主模型已删除" } else { "Orphan" },
                                                Tone::Danger,
                                                cx,
                                            )
                                        })),
                                )
                                .child(chain_row),
                        )
                        .child(
                            h_row()
                                .gap(px(4.0))
                                .child(
                                    btn_ghost(
                                        SharedString::from(format!("rule-prev-{idx}")),
                                        if zh { "预览顺序" } else { "Preview" },
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.load_route_preview(
                                                primary_for_prev.clone(),
                                                None,
                                                cx,
                                            );
                                        },
                                    )),
                                )
                                .children((!rule.orphan).then(|| {
                                    btn_secondary(
                                        SharedString::from(format!("rule-edit-{idx}")),
                                        if zh { "编辑" } else { "Edit" },
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.start_edit_rule(
                                                primary_for_edit.clone(),
                                                fallbacks_for_edit.clone(),
                                                cx,
                                            );
                                        },
                                    ))
                                }))
                                .child(
                                    btn_danger(
                                        SharedString::from(format!("rule-del-{idx}")),
                                        if zh { "删除" } else { "Delete" },
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.delete_gateway_rule(
                                                primary_for_del.clone(),
                                                zh,
                                                cx,
                                            );
                                        },
                                    )),
                                ),
                        ),
                ),
            );
        }
    }

    // Route preview card when loaded.
    if let Some(preview) = &view.route_preview {
        let mut items_col = v_col().gap(px(5.0));
        if let Some(err) = &preview.error {
            items_col = items_col.child(badge(err.clone(), Tone::Danger, cx));
        }
        for item in &preview.items {
            let (v_lbl, v_tone) = match item.verdict.as_str() {
                "first" => (if zh { "会先试" } else { "First" }, Tone::Success),
                "fallback" => (if zh { "备用" } else { "Fallback" }, Tone::Accent),
                _ => (if zh { "跳过" } else { "Skip" }, Tone::Neutral),
            };
            let key_info = if !item.keys_order.is_empty() {
                if zh {
                    format!("密钥：{}", item.keys_order.join(" → "))
                } else {
                    format!("Keys: {}", item.keys_order.join(" → "))
                }
            } else if let Some(skip) = &item.skip_reason {
                if zh {
                    format!("跳过原因：{skip}")
                } else {
                    format!("Skipped: {skip}")
                }
            } else {
                String::new()
            };
            items_col = items_col.child(
                h_row()
                    .justify_between()
                    .px(px(9.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.pane))
                    .child(
                        h_row().gap(px(8.0)).child(badge(v_lbl, v_tone, cx)).child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .child(SharedString::from(format!(
                                    "{}/{}",
                                    item.provider_name, item.model
                                ))),
                        ),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(key_info)),
                    ),
            );
        }

        root = root.child(
            card(cx).p(px(14.0)).child(
                v_col()
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .justify_between()
                            .child(h_row().gap(px(6.0)).child(icon_xs(UiIcon::Branch)).child(
                                section_heading(
                                    if zh {
                                        format!(
                                            "路由预览：{}（最多 {} 次尝试，只读）",
                                            preview.requested, preview.max_attempts
                                        )
                                    } else {
                                        format!(
                                            "Route Preview: {} (max {} attempts, read-only)",
                                            preview.requested, preview.max_attempts
                                        )
                                    },
                                    cx,
                                ),
                            ))
                            .child(btn_icon("rule-prev-close", UiIcon::Close).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.route_preview = None;
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(items_col),
            ),
        );
    }

    root
}
