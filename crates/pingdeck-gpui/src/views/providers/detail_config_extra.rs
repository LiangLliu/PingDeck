//! Provider Config Tab lower sections: Extra Headers, Compatibility (Quirks),
//! Advanced (Models URL, Relay Pricing, Proxy, Balance), Save bar & Danger Zone.

use gpui_kit::{
    Context, Div, Entity, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::llm::wire::{MaxTokensField, ReasoningStyle};

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
    let zh = form.zh;
    let prov = &form.view.provider;
    let ent = cx.entity().clone();

    // 3. Extra Headers card.
    let mut hdr_col = v_col().gap(px(8.0)).child(section_heading(
        if zh {
            "附加请求头（写只读，凭据类脱敏）"
        } else {
            "Extra Headers"
        },
        cx,
    ));
    let hdr_keys: Vec<String> = prov.quirks.extra_headers.keys().cloned().collect();
    if hdr_keys.is_empty() {
        hdr_col = hdr_col.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "没有额外的请求头。"
        } else {
            "No extra headers configured."
        }));
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
                btn_secondary("cfg-hdr-add", if zh { "添加请求头" } else { "Add header" })
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
            if zh {
                "自动（跟随预设）"
            } else {
                "Auto (preset)"
            }
        },
        |s| format_reasoning_style(s, zh),
    );
    let rs_picker = popover("cfg-rs-pop")
        .anchor(Anchor::TopRight)
        .trigger(select_pill("cfg-rs-trig", rs_lbl, cur_rs.is_some(), cx))
        .content(move |_, _, cx| {
            let mut col = v_col().w(px(230.0)).gap(px(2.0)).p(px(6.0));
            let ent0 = ent_rs.clone();
            col = col.child(
                list_row("cfg-rs-auto", cur_rs.is_none(), cx, move |_, _, cx| {
                    ent0.update(cx, |this, cx| {
                        this.view.provider.quirks.reasoning_request = None;
                        cx.notify();
                    });
                })
                .child(div().text_xs().child(if zh { "自动" } else { "Auto" })),
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
                col = col.child(
                    list_row(
                        SharedString::from(format!("cfg-rs-{i}")),
                        cur_rs == Some(s),
                        cx,
                        move |_, _, cx| {
                            ent.update(cx, |this, cx| {
                                this.view.provider.quirks.reasoning_request = Some(s);
                                cx.notify();
                            });
                        },
                    )
                    .child(div().text_xs().child(format_reasoning_style(s, zh))),
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
        .child(tab_item(if zh { "自动" } else { "Auto" }))
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
                if zh {
                    "兼容性（Quirks）"
                } else {
                    "Compatibility"
                },
                cx,
            ))
            .child(field_row(
                if zh {
                    "思考 / 推理参数格式"
                } else {
                    "Reasoning request format"
                },
                None,
                rs_picker,
                cx,
            ))
            .child(field_row(
                if zh {
                    "最大输出 token 字段"
                } else {
                    "Max tokens field"
                },
                None,
                mt_seg,
                cx,
            ))
            .child(field_row(
                if zh {
                    "推理响应字段名"
                } else {
                    "Reasoning response field"
                },
                None,
                div()
                    .w(px(220.0))
                    .child(text_input(&form.reasoning_field_input)),
                cx,
            ))
            .child(field_row(
                if zh {
                    "Anthropic-Version 头"
                } else {
                    "Anthropic version"
                },
                None,
                div()
                    .w(px(180.0))
                    .child(text_input(&form.anthropic_ver_input)),
                cx,
            ))
            .child(tri_row(
                "cfg-tri-drop",
                if zh {
                    "丢弃采样参数 (temperature/top_p)"
                } else {
                    "Drop sampling params"
                },
                prov.quirks.drop_sampling_params,
                zh,
                ent.clone(),
                |this, v| this.view.provider.quirks.drop_sampling_params = v,
                cx,
            ))
            .child(tri_row(
                "cfg-tri-echo",
                if zh {
                    "随工具结果回传推理内容"
                } else {
                    "Echo reasoning in tool turns"
                },
                prov.quirks.echo_reasoning_in_tool_turns,
                zh,
                ent.clone(),
                |this, v| this.view.provider.quirks.echo_reasoning_in_tool_turns = v,
                cx,
            ))
            .child(tri_row(
                "cfg-tri-dev",
                if zh {
                    "将 developer 角色作为 system 发送"
                } else {
                    "Send developer role as system"
                },
                prov.quirks.developer_role_as_system,
                zh,
                ent.clone(),
                |this, v| this.view.provider.quirks.developer_role_as_system = v,
                cx,
            ))
            .child(tri_row(
                "cfg-tri-usage",
                if zh {
                    "流式请求附带 token 用量"
                } else {
                    "Include stream usage"
                },
                prov.quirks.stream_usage,
                zh,
                ent.clone(),
                |this, v| this.view.provider.quirks.stream_usage = v,
                cx,
            ))
            .child(tri_row(
                "cfg-tri-resp",
                if zh {
                    "带工具请求走 Responses 协议"
                } else {
                    "Route tools via Responses"
                },
                prov.quirks.tools_via_responses,
                zh,
                ent,
                |this, v| this.view.provider.quirks.tools_via_responses = v,
                cx,
            )),
    );

    // 5. Advanced card.
    let adv_card = card(cx).p(px(16.0)).child(
        v_col()
            .gap(px(10.0))
            .child(section_heading(if zh { "高级" } else { "Advanced" }, cx))
            .child(field_row(
                if zh {
                    "模型列表地址"
                } else {
                    "Models list path"
                },
                None,
                div().w(px(280.0)).child(text_input(&form.models_url_input)),
                cx,
            ))
            .child(field_row(
                if zh {
                    "中转价格倍率与币种"
                } else {
                    "Relay pricing multiplier"
                },
                Some(SharedString::from(if zh {
                    "按官方价倍率计费的中转站（例如 1 USD = 1 CNY 填倍率 1、币种 CNY）。"
                } else {
                    "Multiply official prices by a factor and optional billing currency."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(div().w(px(110.0)).child(text_input(&form.relay_mult_input)))
                    .child(div().w(px(90.0)).child(text_input(&form.relay_curr_input))),
                cx,
            ))
            .child(field_row(
                if zh {
                    "独立代理地址"
                } else {
                    "Provider proxy"
                },
                Some(SharedString::from(if zh {
                    "留空跟随「设置 › 网络」；填 direct 表示直连不走代理。"
                } else {
                    "Empty follows Settings › Network; enter direct to bypass proxy."
                })),
                div().w(px(280.0)).child(text_input(&form.proxy_input)),
                cx,
            ))
            .child(field_row(
                if zh {
                    "关闭余额自动查询"
                } else {
                    "Disable balance checks"
                },
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
                        if zh {
                            "确认改到新主机（并停用旧密钥）"
                        } else {
                            "Confirm host change (disable keys)"
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.save(true, cx)))
                }))
                .child(
                    btn_primary(
                        "cfg-save-btn",
                        if form.saving {
                            if zh { "保存中…" } else { "Saving…" }
                        } else if zh {
                            "保存配置"
                        } else {
                            "Save configuration"
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
            if zh {
                "会一并删除该服务商下的密钥、模型和探测记录（保留历史调用日志与用量）。".into()
            } else {
                "Removes keys, models, and probes for this provider (historical logs are kept)."
                    .into()
            }
        },
        |imp| {
            if zh {
                format!(
                    "将删除 {} 把密钥、{} 个模型；受影响：{} 个客户端、{} 条网关备用规则。",
                    imp.keys,
                    imp.models,
                    imp.clients.len(),
                    imp.gateway_rules.len()
                )
            } else {
                format!(
                    "Deletes {} key(s), {} model(s); affects {} client(s), {} gateway rule(s).",
                    imp.keys,
                    imp.models,
                    imp.clients.len(),
                    imp.gateway_rules.len()
                )
            }
        },
    );

    let danger_card = card(cx)
        .p(px(16.0))
        .border_color(to_hsla(p.bad_line.composite_over(p.surface)))
        .child(
            v_col()
                .gap(px(8.0))
                .child(section_heading(
                    if zh { "删除服务商" } else { "Danger Zone" },
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
                                    btn_ghost("cfg-del-cancel", if zh { "取消" } else { "Cancel" })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.confirm_delete_open = false;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    btn_danger(
                                        "cfg-del-confirm",
                                        if zh {
                                            "确认永久删除"
                                        } else {
                                            "Confirm delete"
                                        },
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.delete_now(cx))),
                                )
                                .into_any_element()
                        } else {
                            btn_danger(
                                "cfg-del-open",
                                if zh {
                                    "删除服务商…"
                                } else {
                                    "Delete provider…"
                                },
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
    zh: bool,
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
        .child(tab_item(if zh { "自动" } else { "Auto" }))
        .child(tab_item(if zh { "开" } else { "On" }))
        .child(tab_item(if zh { "关" } else { "Off" }))
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
