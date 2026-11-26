//! Gateway Fallback Rules tab matching `docs/design/prototype/js/data-subtabs.js`:
//! single unified table card (`Primary Model | Ordered Fallback Chain | Status | Today | Actions`),
//! inline rule editor, and read-only route preview panel.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::llm::store::ModelRef;
use pingdeck_desktop::state::SettingsSection;

use crate::ui::{
    Anchor, ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_primary,
    btn_secondary, card, h_row, icon_xs, list_row, palette, popover, section_heading, select_pill,
    table_header, to_hsla, v_col,
};
use crate::views::providers::format::format_rule_status;

use super::{GatewayHandoff, GatewayView};

pub fn render_rules_tab(view: &GatewayView, cx: &mut Context<GatewayView>) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let cm = crate::ui::tr(cx).common();

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
            pr.prov_gateway_rules_add_rule(),
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
                        .child(
                            crate::ui::tr(cx)
                                .providers()
                                .prov_gateway_rules_no_models_available_without_a(),
                        ),
                );
            } else {
                for (idx, (pid, mid, display)) in add_candidates.iter().take(60).enumerate() {
                    let pid = pid.clone();
                    let mid = mid.clone();
                    let ent = ent.clone();
                    let pop_h = cx.entity().clone();
                    list = list.child(
                        list_row(
                            SharedString::from(format!("rule-pick-{idx}")),
                            false,
                            cx,
                            move |_, window, cx| {
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
                                pop_h.update(cx, |s, cx| s.dismiss(window, cx));
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

    let mut table_card = v_col().gap(px(6.0)).child(
        h_row()
            .justify_between()
            .gap(px(10.0))
            .pb(px(4.0))
            .flex_wrap()
            .child(section_heading(
                pr.prov_gateway_rules_gateway_fallback_rules_locked_after(),
                cx,
            ))
            .child(
                h_row()
                    .gap(px(6.0))
                    .child(
                        btn_ghost(
                            "gw-open-routing-settings",
                            pr.prov_gateway_routing_settings_link(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pending_handoff
                                .set(Some(GatewayHandoff::OpenSettings(SettingsSection::Routing)));
                            cx.notify();
                        })),
                    )
                    .child(add_btn),
            ),
    );

    if view.gateway_rules.is_empty() {
        table_card = table_card.child(
            div()
                .py(px(16.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(pr.prov_gateway_rules_no_fallback_rules_configured_click()),
        );
    } else {
        table_card = table_card.child(table_header(
            &[
                (pr.prov_gateway_rules_col_primary(), Some(195.0)),
                (pr.prov_gateway_rules_col_chain(), None),
                (pr.prov_gateway_rules_col_status(), Some(96.0)),
                (pr.prov_gateway_rules_col_hits(), Some(64.0)),
                (pr.prov_gateway_rules_col_actions(), Some(168.0)),
            ],
            cx,
        ));
        for (idx, rule) in view.gateway_rules.iter().enumerate() {
            let (st_lbl, st_tone) = format_rule_status(rule.status, crate::ui::active_language(cx));
            let primary_disp = format!(
                "{}/{}",
                view.provider_name(&rule.primary.provider_id),
                rule.primary.model
            );
            let mut chain_row = h_row().flex_wrap().gap(px(4.0));
            for (fi, fb) in rule.fallbacks.iter().enumerate() {
                if fi > 0 {
                    chain_row =
                        chain_row.child(div().text_xs().text_color(to_hsla(p.faint)).child("→"));
                }
                let fb_tone = rule.fallback_status.get(fi).map_or(Tone::Neutral, |s| {
                    format_rule_status(*s, crate::ui::active_language(cx)).1
                });
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
            let hits = view.today_hits_for_rule(rule);

            table_card = table_card.child(
                h_row()
                    .w_full()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)))
                    .child(
                        h_row()
                            .w(px(195.0))
                            .flex_shrink_0()
                            .gap(px(5.0))
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(primary_disp)),
                            )
                            .children(
                                rule.orphan.then(|| {
                                    badge(pr.prov_gateway_rules_orphan(), Tone::Danger, cx)
                                }),
                            ),
                    )
                    .child(div().flex_1().min_w_0().child(chain_row))
                    .child(
                        div()
                            .w(px(96.0))
                            .flex_shrink_0()
                            .child(badge(st_lbl, st_tone, cx)),
                    )
                    .child(
                        div()
                            .w(px(64.0))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .child(hits.to_string()),
                    )
                    .child(
                        h_row()
                            .w(px(168.0))
                            .flex_shrink_0()
                            .justify_end()
                            .gap(px(4.0))
                            .child(
                                btn_ghost(
                                    SharedString::from(format!("rule-prev-{idx}")),
                                    pr.prov_gateway_rules_preview(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.load_route_preview(primary_for_prev.clone(), None, cx);
                                    },
                                )),
                            )
                            .children((!rule.orphan).then(|| {
                                btn_secondary(
                                    SharedString::from(format!("rule-edit-{idx}")),
                                    pr.gwa_routing_rules_edit(),
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
                                btn_icon(
                                    SharedString::from(format!("rule-del-{idx}")),
                                    UiIcon::Trash,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.delete_gateway_rule(primary_for_del.clone(), cx);
                                    },
                                )),
                            ),
                    ),
            );
        }
    }

    let mut root = v_col()
        .gap(px(14.0))
        .child(card(cx).p(px(14.0)).child(table_card));

    if let Some(editor) = &view.rule_editor {
        root = root.child(render_rule_editor_card(view, editor, cx));
    }
    if let Some(preview) = &view.route_preview {
        root = root.child(render_route_preview_card(preview, cx));
    }

    let _ = cm;
    root
}

fn render_rule_editor_card(
    view: &GatewayView,
    editor: &super::RuleEditorState,
    cx: &mut Context<GatewayView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let cm = crate::ui::tr(cx).common();
    let primary_disp = format!(
        "{}/{}",
        view.provider_name(&editor.primary.provider_id),
        editor.primary.model
    );
    let mut chain_rows = v_col().gap(px(5.0));
    if editor.chain.is_empty() {
        chain_rows = chain_rows.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(pr.prov_gateway_rules_no_fallbacks_yet_pick_up()),
        );
    } else {
        for (i, item) in editor.chain.iter().enumerate() {
            let disp = format!("{}/{}", view.provider_name(&item.provider_id), item.model);
            chain_rows =
                chain_rows.child(
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
            pr.prov_gateway_rules_add_fallback(),
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
                let pop_h = cx.entity().clone();
                let pname = ent.read(cx).provider_name(&pid);
                list = list.child(
                    list_row(
                        SharedString::from(format!("fb-cand-{i}")),
                        false,
                        cx,
                        move |_, window, cx| {
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
                            pop_h.update(cx, |s, cx| s.dismiss(window, cx));
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
                            .child(SharedString::from(
                                pr.prov_gateway_rules_edit_fallbacks_when_fails(primary_disp),
                            )),
                    )
                    .child(
                        btn_icon("rule-ed-close", UiIcon::Close).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.rule_editor = None;
                                cx.notify();
                            },
                        )),
                    ),
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
                                btn_ghost("rule-ed-cancel", cm.cancel()).on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.rule_editor = None;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                btn_primary("rule-ed-save", pr.prov_gateway_rules_save_rule())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_rule_editor(cx);
                                    })),
                            ),
                    ),
            ),
    )
}

fn render_route_preview_card(
    preview: &pingdeck_desktop::core::app::RoutePreview,
    cx: &mut Context<GatewayView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let mut items_col = v_col().gap(px(5.0));
    if let Some(err) = &preview.error {
        items_col = items_col.child(badge(err.clone(), Tone::Danger, cx));
    }
    for item in &preview.items {
        let (v_lbl, v_tone) = match item.verdict.as_str() {
            "first" => (pr.prov_gateway_rules_first(), Tone::Success),
            "fallback" => (pr.gwa_preview_fallback(), Tone::Accent),
            _ => (pr.prov_gateway_rules_skip(), Tone::Neutral),
        };
        let key_info = if !item.keys_order.is_empty() {
            pr.prov_gateway_rules_keys(item.keys_order.join(" → "))
        } else if let Some(skip) = &item.skip_reason {
            pr.prov_gateway_rules_skipped(skip)
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

    card(cx).p(px(14.0)).child(
        v_col()
            .gap(px(8.0))
            .child(
                h_row()
                    .justify_between()
                    .child(h_row().gap(px(6.0)).child(icon_xs(UiIcon::Branch)).child(
                        section_heading(
                            pr.prov_gateway_rules_route_preview_max_attempts_read(
                                &preview.requested,
                                preview.max_attempts,
                            ),
                            cx,
                        ),
                    ))
                    .child(
                        btn_icon("rule-prev-close", UiIcon::Close).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.route_preview = None;
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(items_col),
    )
}
