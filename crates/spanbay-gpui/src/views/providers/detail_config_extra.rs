//! Provider Config Tab lower sections: Extra Headers, Compatibility (Quirks),
//! Advanced (Models URL, Relay Pricing, Proxy, Balance), Save bar & Danger Zone.

use gpui_kit::{
    Context, Div, Entity, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px,
};
use spanbay_desktop::core::llm::wire::{MaxTokensField, ReasoningStyle};

use crate::ui::{
    Anchor, Tone, UiIcon, badge, btn_danger, btn_ghost, btn_icon, btn_primary, btn_secondary, card,
    field_row, h_row, list_row, palette, popover, section_heading, select_pill, tab_bar_segmented,
    tab_item, text_input, to_hsla, toggle_switch, v_col,
};

use super::detail_config::ProviderConfigForm;
use super::format::format_reasoning_style;

pub(super) fn render_extra_cards(
    form: &ProviderConfigForm,
    cx: &mut Context<ProviderConfigForm>,
) -> Div {
    let p = palette(cx);
    let prov = &form.view.provider;
    let ent = cx.entity().clone();

    // 3. Extra Headers card.
    let mut hdr_col = v_col().gap(px(8.0)).child(section_heading(
        crate::ui::tr(cx)
            .providers()
            .prov_detail_config_extra_extra_headers(),
        cx,
    ));
    let hdr_keys: Vec<String> = prov.quirks.extra_headers.keys().cloned().collect();
    if hdr_keys.is_empty() {
        hdr_col = hdr_col.child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_no_extra_headers_configured(),
            ),
        );
    } else {
        for (i, hk) in hdr_keys.into_iter().enumerate() {
            let hint = form
                .view
                .extra_header_hints
                .get(&hk)
                .filter(|s| !s.is_empty())
                .cloned()
                .or_else(|| prov.quirks.extra_headers.get(&hk).cloned())
                .unwrap_or_else(|| "••••".into());
            let hk_del = hk.clone();
            hdr_col = hdr_col.child(
                h_row()
                    .justify_between()
                    .px(px(10.0))
                    .py(px(5.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.pane))
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child(SharedString::from(hk)),
                            )
                            .child(badge(SharedString::from(hint), Tone::Neutral, cx)),
                    )
                    .child(
                        btn_icon(("cfg-hdr-del", i), UiIcon::Trash).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.view.provider.quirks.extra_headers.remove(&hk_del);
                                cx.notify();
                            },
                        )),
                    ),
            );
        }
    }
    hdr_col = hdr_col.child(
        h_row()
            .gap(px(6.0))
            .child(div().w(px(180.0)).child(text_input(&form.new_hdr_name)))
            .child(div().flex_1().child(text_input(&form.new_hdr_val)))
            .child(
                btn_secondary(
                    "cfg-hdr-add",
                    crate::ui::tr(cx).providers().sniff_adv_header_add(),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.add_extra_header(window, cx);
                })),
            ),
    );
    let headers_card = card(cx).p(px(16.0)).child(hdr_col);

    // 4. Compatibility (Quirks) card.
    let cur_rs = prov.quirks.reasoning_request;
    let ent_rs = ent.clone();
    let rs_lbl = cur_rs.map_or_else(
        || {
            crate::ui::tr(cx)
                .providers()
                .prov_detail_config_extra_auto_preset()
        },
        |s| format_reasoning_style(s, crate::ui::active_language(cx)),
    );
    let rs_picker = popover("cfg-rs-pop")
        .anchor(Anchor::TopRight)
        .trigger(select_pill("cfg-rs-trig", rs_lbl, cur_rs.is_some(), cx))
        .content(move |_, _, cx| {
            let mut col = v_col().w(px(230.0)).gap(px(2.0)).p(px(6.0));
            let ent0 = ent_rs.clone();
            let pop0 = cx.entity().clone();
            col = col.child(
                list_row("cfg-rs-auto", cur_rs.is_none(), cx, move |_, window, cx| {
                    ent0.update(cx, |this, cx| {
                        this.view.provider.quirks.reasoning_request = None;
                        cx.notify();
                    });
                    pop0.update(cx, |s, cx| s.dismiss(window, cx));
                })
                .child(
                    div()
                        .text_xs()
                        .child(crate::ui::tr(cx).providers().sniff_adv_protocol_auto()),
                ),
            );
            for (i, s) in [
                ReasoningStyle::ReasoningEffort,
                ReasoningStyle::ReasoningObject,
                ReasoningStyle::ThinkingType,
                ReasoningStyle::EnableThinking,
                ReasoningStyle::ChatTemplateKwargs,
                ReasoningStyle::AnthropicAdaptive,
                ReasoningStyle::GeminiThinkingLevel,
                ReasoningStyle::None,
            ]
            .into_iter()
            .enumerate()
            {
                let ent = ent_rs.clone();
                let pop_h = cx.entity().clone();
                col = col.child(
                    list_row(
                        SharedString::from(format!("cfg-rs-{i}")),
                        cur_rs == Some(s),
                        cx,
                        move |_, window, cx| {
                            ent.update(cx, |this, cx| {
                                this.view.provider.quirks.reasoning_request = Some(s);
                                cx.notify();
                            });
                            pop_h.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .child(
                        div()
                            .text_xs()
                            .child(format_reasoning_style(s, crate::ui::active_language(cx))),
                    ),
                );
            }
            col
        });

    let mt_idx = match prov.quirks.max_tokens_field {
        None => 0,
        Some(MaxTokensField::MaxTokens) => 1,
        Some(MaxTokensField::MaxCompletionTokens) => 2,
    };
    let ent_mt = ent.clone();
    let mt_seg = tab_bar_segmented("cfg-mt-seg")
        .selected_index(mt_idx)
        .child(tab_item(
            crate::ui::tr(cx).providers().sniff_adv_protocol_auto(),
        ))
        .child(tab_item("max_tokens"))
        .child(tab_item("max_completion_tokens"))
        .on_click(move |idx, _, cx| {
            let next = match *idx {
                1 => Some(MaxTokensField::MaxTokens),
                2 => Some(MaxTokensField::MaxCompletionTokens),
                _ => None,
            };
            ent_mt.update(cx, |this, cx| {
                this.view.provider.quirks.max_tokens_field = next;
                cx.notify();
            });
        });

    let compat_card = card(cx).p(px(16.0)).child(
        v_col()
            .gap(px(10.0))
            .child(section_heading(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_compatibility(),
                cx,
            ))
            .child(field_row(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_reasoning_request_format(),
                None,
                rs_picker,
                cx,
            ))
            .child(field_row(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_max_tokens_field(),
                None,
                mt_seg,
                cx,
            ))
            .child(field_row(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_reasoning_response_field(),
                None,
                div()
                    .w(px(220.0))
                    .child(text_input(&form.reasoning_field_input)),
                cx,
            ))
            .child(field_row(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_anthropic_version(),
                None,
                div()
                    .w(px(180.0))
                    .child(text_input(&form.anthropic_ver_input)),
                cx,
            ))
            .child(tri_row(
                "cfg-tri-drop",
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_drop_sampling_params(),
                prov.quirks.drop_sampling_params,
                ent.clone(),
                |this, v| this.view.provider.quirks.drop_sampling_params = v,
                cx,
            ))
            .child(tri_row(
                "cfg-tri-echo",
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_echo_reasoning_in_tool_turns(),
                prov.quirks.echo_reasoning_in_tool_turns,
                ent.clone(),
                |this, v| this.view.provider.quirks.echo_reasoning_in_tool_turns = v,
                cx,
            ))
            .child(tri_row(
                "cfg-tri-dev",
                crate::ui::tr(cx)
                    .providers()
                    .cfg_opt_flag_developer_role_as_system(),
                prov.quirks.developer_role_as_system,
                ent.clone(),
                |this, v| this.view.provider.quirks.developer_role_as_system = v,
                cx,
            ))
            .child(tri_row(
                "cfg-tri-usage",
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_include_stream_usage(),
                prov.quirks.stream_usage,
                ent.clone(),
                |this, v| this.view.provider.quirks.stream_usage = v,
                cx,
            ))
            .child(tri_row(
                "cfg-tri-resp",
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_route_tools_via_responses(),
                prov.quirks.tools_via_responses,
                ent,
                |this, v| this.view.provider.quirks.tools_via_responses = v,
                cx,
            )),
    );

    // 5. Advanced card.
    let adv_card = card(cx).p(px(16.0)).child(
        v_col()
            .gap(px(10.0))
            .child(section_heading(
                crate::ui::tr(cx).providers().sniff_adv_title(),
                cx,
            ))
            .child(field_row(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_models_list_path(),
                None,
                div().w(px(280.0)).child(text_input(&form.models_url_input)),
                cx,
            ))
            .child(field_row(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_provider_proxy(),
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_detail_config_extra_empty_follows_settings_network_enter(),
                )),
                div().w(px(280.0)).child(text_input(&form.proxy_input)),
                cx,
            ))
            .child(field_row(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_config_extra_disable_balance_checks(),
                None,
                toggle_switch("cfg-bal-off", prov.balance_off).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.view.provider.balance_off = !this.view.provider.balance_off;
                        cx.notify();
                    },
                )),
                cx,
            )),
    );

    // 6. Save bar + Host-change confirmation + Delete danger card.
    let save_bar = h_row()
        .justify_between()
        .gap(px(8.0))
        .flex_wrap()
        .child(div().children(form.flash.clone().map(|(tone, msg)| badge(msg, tone, cx))))
        .child(
            h_row()
                .gap(px(6.0))
                .children(form.needs_host_confirm.then(|| {
                    btn_danger(
                        "cfg-confirm-host",
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_config_extra_confirm_host_change_disable_keys(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.save(true, cx)))
                }))
                .child(
                    btn_primary(
                        "cfg-save-btn",
                        if form.saving {
                            crate::ui::tr(cx)
                                .providers()
                                .prov_detail_config_extra_saving()
                        } else {
                            crate::ui::tr(cx)
                                .providers()
                                .prov_detail_config_extra_save_configuration()
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.saving {
                            this.save(false, cx);
                        }
                    })),
                ),
        );

    let impact_summary = form.impact.as_ref().map_or_else(
        || {
            crate::ui::tr(cx)
                .providers()
                .prov_detail_config_extra_removes_keys_models_and_probes()
                .into()
        },
        |imp| {
            crate::ui::tr(cx)
                .providers()
                .prov_detail_config_extra_deletes_key_s_model_s(
                    imp.keys,
                    imp.models,
                    imp.clients.len(),
                    imp.gateway_rules.len(),
                )
        },
    );

    let danger_card = card(cx)
        .p(px(16.0))
        .border_color(to_hsla(p.bad_line.composite_over(p.surface)))
        .child(
            v_col()
                .gap(px(8.0))
                .child(section_heading(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_detail_config_extra_danger_zone(),
                    cx,
                ))
                .child(
                    h_row()
                        .justify_between()
                        .gap(px(10.0))
                        .flex_wrap()
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(SharedString::from(impact_summary)),
                        )
                        .child(if form.confirm_delete_open {
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    btn_ghost(
                                        "cfg-del-cancel",
                                        crate::ui::tr(cx).common().cancel(),
                                    )
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.confirm_delete_open = false;
                                            cx.notify();
                                        },
                                    )),
                                )
                                .child(
                                    btn_danger(
                                        "cfg-del-confirm",
                                        crate::ui::tr(cx)
                                            .providers()
                                            .prov_detail_config_extra_confirm_delete(),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.delete_now(cx))),
                                )
                                .into_any_element()
                        } else {
                            btn_danger(
                                "cfg-del-open",
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_detail_config_extra_delete_provider(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_delete_open = true;
                                cx.notify();
                            }))
                            .into_any_element()
                        }),
                ),
        );

    v_col()
        .gap(px(14.0))
        .child(headers_card)
        .child(compat_card)
        .child(adv_card)
        .child(save_bar)
        .child(danger_card)
}

fn tri_row(
    id: &'static str,
    label: &'static str,
    val: Option<bool>,
    ent: Entity<ProviderConfigForm>,
    setter: fn(&mut ProviderConfigForm, Option<bool>),
    cx: &gpui_kit::App,
) -> Div {
    let idx = match val {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    };
    let seg = tab_bar_segmented(id)
        .selected_index(idx)
        .child(tab_item(
            crate::ui::tr(cx).providers().sniff_adv_protocol_auto(),
        ))
        .child(tab_item(crate::ui::tr(cx).providers().cfg_opt_tri_on()))
        .child(tab_item(crate::ui::tr(cx).providers().cfg_opt_tri_off()))
        .on_click(move |i, _, cx| {
            let next = match *i {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            };
            ent.update(cx, |this, cx| {
                setter(this, next);
                cx.notify();
            });
        });
    field_row(label, None, seg, cx)
}
