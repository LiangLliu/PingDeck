//! Provider Overview Keys Pool card (`render_keys_card`).

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::llm::providers::KeyStrategy;
use pingdeck_desktop::core::llm::store::{KeyRunState, Provider, ProviderKey};
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_danger, btn_ghost, btn_icon, btn_primary, btn_secondary,
    card, h_row, icon_xs, palette, section_heading, status_indicator, tab_bar_segmented, tab_item,
    text_input, to_hsla, toggle_switch, v_col,
};

use super::ProvidersView;
use super::format::{format_balance_report, format_relative_time};

pub fn render_keys_card(
    view: &ProvidersView,
    prov: &Provider,
    keys: &[ProviderKey],
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let prov_id = prov.id.clone();
    let strat = prov.key_strategy.unwrap_or(KeyStrategy::Primary);
    let strat_idx = match strat {
        KeyStrategy::Primary => 0,
        KeyStrategy::RoundRobin => 1,
        KeyStrategy::Random => 2,
    };
    let cooldown_on = prov.key_cooldown.unwrap_or(true);
    let ent = cx.entity().clone();
    let pid_strat = prov_id.clone();

    let strat_seg = tab_bar_segmented("prov-key-strat")
        .selected_index(strat_idx)
        .child(tab_item(if zh { "固定主密钥" } else { "Primary" }))
        .child(tab_item(if zh { "轮询" } else { "Round-robin" }))
        .child(tab_item(if zh { "随机" } else { "Random" }))
        .on_click(move |idx, _, cx| {
            let next = match *idx {
                1 => KeyStrategy::RoundRobin,
                2 => KeyStrategy::Random,
                _ => KeyStrategy::Primary,
            };
            let pid = pid_strat.clone();
            ent.update(cx, |this, cx| {
                this.update_provider_key_policy(pid, Some(next), None, zh, cx);
            });
        });

    let mut box_col = v_col().gap(px(10.0)).child(
        h_row()
            .justify_between()
            .gap(px(8.0))
            .flex_wrap()
            .child(
                v_col()
                    .gap(px(2.0))
                    .child(section_heading(
                        if zh {
                            format!("密钥池（{} 把密钥）", keys.len())
                        } else {
                            format!("API Keys ({})", keys.len())
                        },
                        cx,
                    ))
                    .child(div().text_size(px(11.0)).text_color(to_hsla(p.faint)).child(if zh {
                        "明文 0600 保险库仅存本机；支持多密钥轮换与 429/401 自动冷却隔离。"
                    } else {
                        "Stored locally (0600 vault); supports multi-key rotation and cooldown isolation."
                    })),
            )
            .child(
                btn_secondary(
                    "prov-toggle-add-key",
                    if view.adding_key_open {
                        if zh { "收起" } else { "Close" }
                    } else if zh {
                        "+ 添加密钥"
                    } else {
                        "+ Add key"
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.adding_key_open = !this.adding_key_open;
                    cx.notify();
                })),
            ),
    );

    if keys.len() > 1 {
        let pid_cd = prov_id.clone();
        box_col =
            box_col.child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .flex_wrap()
                    .px(px(10.0))
                    .py(px(7.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.pane))
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                                "轮换方式"
                            } else {
                                "Rotation"
                            }))
                            .child(strat_seg),
                    )
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                                "失败后冷却"
                            } else {
                                "Cooldown"
                            }))
                            .child(toggle_switch("prov-key-cd", cooldown_on).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.update_provider_key_policy(
                                        pid_cd.clone(),
                                        None,
                                        Some(!cooldown_on),
                                        zh,
                                        cx,
                                    );
                                }),
                            )),
                    ),
            );
    }

    if view.adding_key_open {
        let pid_save = prov_id.clone();
        box_col = box_col.child(
            v_col()
                .gap(px(8.0))
                .p(px(10.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.pane))
                .border_1()
                .border_color(to_hsla(p.line_strong.composite_over(p.pane)))
                .child(
                    h_row()
                        .gap(px(8.0))
                        .child(
                            div()
                                .w(px(150.0))
                                .child(text_input(&view.new_key_name_input)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(text_input(&view.new_key_secret_input)),
                        ),
                )
                .child(
                    h_row()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            h_row()
                                .gap(px(8.0))
                                .flex_1()
                                .child(
                                    div()
                                        .w(px(150.0))
                                        .child(text_input(&view.new_key_group_input)),
                                )
                                .child(
                                    div()
                                        .w(px(140.0))
                                        .child(text_input(&view.new_key_user_id_input)),
                                ),
                        )
                        .child(
                            btn_primary(
                                "prov-save-new-key",
                                if zh { "保存密钥" } else { "Save key" },
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.add_provider_key(pid_save.clone(), zh, window, cx);
                                },
                            )),
                        ),
                ),
        );
    }

    if keys.is_empty() {
        box_col = box_col.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "还没有密钥。"
        } else {
            "No API keys configured."
        }));
    } else {
        for (i, key) in keys.iter().enumerate() {
            let kid = key.id.clone();
            let pid_k = prov_id.clone();
            let runtime_state = key.runtime.map_or(
                if key.enabled {
                    KeyRunState::Ready
                } else {
                    KeyRunState::Disabled
                },
                |r| r.state,
            );
            let shape = match runtime_state {
                KeyRunState::Ready => StatusShape::Ok,
                KeyRunState::Cooling | KeyRunState::CircuitOpen => StatusShape::Warn,
                KeyRunState::Disabled => StatusShape::Idle,
            };
            let status_line = match runtime_state {
                KeyRunState::Disabled => {
                    if zh {
                        "已停用".into()
                    } else {
                        "Disabled".into()
                    }
                }
                KeyRunState::Cooling => {
                    if zh {
                        "冷却中（限流/拒接）".into()
                    } else {
                        "Cooling down".into()
                    }
                }
                KeyRunState::CircuitOpen => {
                    if zh {
                        "熔断打开".into()
                    } else {
                        "Circuit open".into()
                    }
                }
                KeyRunState::Ready => {
                    if let Some(ms) = key.last_used_ms {
                        if zh {
                            format!("可用 · {}用过", format_relative_time(ms, true))
                        } else {
                            format!("Ready · used {}", format_relative_time(ms, false))
                        }
                    } else if zh {
                        "可用 · 从未使用".into()
                    } else {
                        "Ready · never used".into()
                    }
                }
            };
            let bal_txt = key
                .balance
                .as_ref()
                .and_then(|b| b.last_good.as_ref())
                .filter(|r| r.supported)
                .map(|r| format_balance_report(r, zh));
            let revealed = view
                .revealed_provider_key
                .as_ref()
                .filter(|(id, _)| id == &key.id)
                .map(|(_, s)| s.clone());

            let kid_rev = kid.clone();
            let kid_list = kid.clone();
            let kid_cd = kid.clone();
            let kid_edit = kid.clone();
            let kid_probe = kid.clone();
            let kid_del = kid.clone();
            let pid_probe = prov_id.clone();
            let key_clone = key.clone();
            let key_for_edit = key.clone();
            let key_for_save = key.clone();
            let is_editing = view.editing_key_id.as_deref() == Some(kid.as_str());
            let is_confirm_del = view.confirm_delete_key.as_deref() == Some(kid.as_str());

            let mut row_col = v_col().gap(px(5.0)).child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .flex_wrap()
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .flex_1()
                            .min_w_0()
                            .child(status_indicator(shape, cx))
                            .child(
                                v_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.0))
                                    .child(
                                        h_row()
                                            .gap(px(6.0))
                                            .flex_wrap()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                    .text_color(to_hsla(p.ink))
                                                    .child(SharedString::from(key.name.clone())),
                                            )
                                            .child(badge(
                                                SharedString::from(if key.secret_hint.is_empty() {
                                                    if zh {
                                                        "未设置".into()
                                                    } else {
                                                        "Empty".into()
                                                    }
                                                } else {
                                                    key.secret_hint.clone()
                                                }),
                                                Tone::Neutral,
                                                cx,
                                            ))
                                            .children(
                                                key.group_name
                                                    .as_ref()
                                                    .filter(|g| !g.is_empty())
                                                    .map(|g| {
                                                        badge(
                                                            SharedString::from(g.clone()),
                                                            Tone::Accent,
                                                            cx,
                                                        )
                                                    }),
                                            )
                                            .children(bal_txt.map(|b| badge(b, Tone::Success, cx))),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(to_hsla(p.muted))
                                            .truncate()
                                            .child(SharedString::from(status_line)),
                                    ),
                            ),
                    )
                    .child(
                        h_row()
                            .gap(px(4.0))
                            .children((runtime_state == KeyRunState::Cooling).then(|| {
                                btn_ghost(
                                    SharedString::from(format!("key-cd-{i}")),
                                    if zh { "清冷却" } else { "Clear" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.clear_key_cooldown(kid_cd.clone(), zh, cx);
                                    },
                                ))
                            }))
                            .child(
                                btn_ghost(
                                    SharedString::from(format!("key-list-{i}")),
                                    if zh { "读模型" } else { "Models" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.refresh_provider_models(
                                            pid_k.clone(),
                                            Some(kid_list.clone()),
                                            zh,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .child(
                                btn_icon(
                                    ("key-reveal", i),
                                    if revealed.is_some() {
                                        UiIcon::EyeOff
                                    } else {
                                        UiIcon::Eye
                                    },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.toggle_reveal_key(kid_rev.clone(), zh, cx);
                                    },
                                )),
                            )
                            .child(
                                btn_ghost(
                                    SharedString::from(format!("key-probe-{i}")),
                                    if zh { "探测" } else { "Probe" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.open_bench(
                                            Some(pid_probe.clone()),
                                            Vec::new(),
                                            window,
                                            cx,
                                        );
                                        if let Some(bv) = &this.bench_view {
                                            let k_sel = kid_probe.clone();
                                            bv.update(cx, |b, cx| {
                                                b.selected_key_id = Some(k_sel);
                                                b.recompute_estimate(cx);
                                                cx.notify();
                                            });
                                        }
                                    },
                                )),
                            )
                            .child(
                                btn_icon(("key-edit", i), UiIcon::Pencil).on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        if this.editing_key_id.as_deref() == Some(kid_edit.as_str())
                                        {
                                            this.editing_key_id = None;
                                        } else {
                                            this.edit_key_name_input.update(cx, |s, cx| {
                                                s.set_value(key_for_edit.name.clone(), window, cx);
                                            });
                                            this.edit_key_group_input.update(cx, |s, cx| {
                                                s.set_value(
                                                    key_for_edit
                                                        .group_name
                                                        .clone()
                                                        .unwrap_or_default(),
                                                    window,
                                                    cx,
                                                );
                                            });
                                            this.edit_key_notes_input.update(cx, |s, cx| {
                                                s.set_value(
                                                    key_for_edit.notes.clone().unwrap_or_default(),
                                                    window,
                                                    cx,
                                                );
                                            });
                                            this.edit_key_secret_input
                                                .update(cx, |s, cx| s.set_value("", window, cx));
                                            this.editing_key_id = Some(kid_edit.clone());
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                toggle_switch(("key-en", i), key.enabled).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.toggle_key_enabled(key_clone.clone(), zh, cx);
                                    },
                                )),
                            )
                            .child(if is_confirm_del {
                                btn_danger(("key-del-ok", i), if zh { "确认删" } else { "Confirm" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.confirm_delete_key = None;
                                        this.delete_provider_key(kid_del.clone(), zh, cx);
                                    }))
                                    .into_any_element()
                            } else {
                                let k_arm = kid_del.clone();
                                btn_icon(("key-del", i), UiIcon::Trash)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.confirm_delete_key = Some(k_arm.clone());
                                        cx.notify();
                                    }))
                                    .into_any_element()
                            }),
                    ),
            );

            if let Some(secret) = revealed {
                let copy_s = secret.clone();
                row_col = row_col.child(
                    h_row()
                        .gap(px(6.0))
                        .px(px(8.0))
                        .py(px(5.0))
                        .rounded(px(6.0))
                        .bg(to_hsla(p.sunken))
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(secret)),
                        )
                        .child(
                            btn_ghost(("key-copy-sec", i), if zh { "复制" } else { "Copy" })
                                .icon(icon_xs(UiIcon::Copy))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let _ = clipboard::set_text(&copy_s);
                                    this.status_msg = Some((
                                        Tone::Success,
                                        if zh {
                                            "已复制密钥".into()
                                        } else {
                                            "Key copied".into()
                                        },
                                    ));
                                    cx.notify();
                                })),
                        ),
                );
            }

            if is_editing {
                row_col = row_col.child(
                    v_col()
                        .gap(px(6.0))
                        .p(px(8.0))
                        .rounded(px(6.0))
                        .bg(to_hsla(p.surface))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .w(px(140.0))
                                        .child(text_input(&view.edit_key_name_input)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .child(text_input(&view.edit_key_secret_input)),
                                ),
                        )
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .w(px(140.0))
                                        .child(text_input(&view.edit_key_group_input)),
                                )
                                .child(div().flex_1().child(text_input(&view.edit_key_notes_input)))
                                .child(
                                    btn_primary(
                                        ("key-ed-save", i),
                                        if zh { "保存" } else { "Save" },
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            this.save_edited_key(
                                                key_for_save.clone(),
                                                zh,
                                                window,
                                                cx,
                                            );
                                        },
                                    )),
                                ),
                        ),
                );
            }

            box_col = box_col.child(
                div()
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.pane))
                    .child(row_col),
            );
        }
    }

    card(cx).p(px(14.0)).child(box_col)
}
