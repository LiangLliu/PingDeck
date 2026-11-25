//! Resolved precedence & unresolved scan, Environment Compare diff, and Guard & Declared Hosts sub-tabs.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::app::{CompareState, HostSource};
use pingdeck_desktop::core::model::VariableResolutionSource;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_icon, btn_secondary, card,
    choice_chip, data_row, empty_inline, h_row, icon_xs, meta_pill, palette, section_header,
    setting_row, table_header, text_input, to_hsla, toggle_switch, v_col,
};

use super::state::{EnvDetailTab, EnvironmentsView};

pub fn render_resolved_tab(
    view: &EnvironmentsView,
    cx: &mut Context<EnvironmentsView>,
) -> AnyElement {
    let p = palette(cx);

    // Section 1: Resolved variable precedence
    let mut res_card = card(cx)
        .p(px(14.0))
        .gap(px(6.0))
        .child(section_header(
            view.t(
                "变量最终解析顺序（环境 > 集合 > 全局）",
                "Effective Variable Precedence",
            ),
            Some(
                btn_secondary("env-res-refresh", view.t("重新扫描", "Rescan"))
                    .icon(icon_xs(UiIcon::Refresh))
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_env_analysis(cx)))
                    .into_any_element(),
            ),
            cx,
        ))
        .child(table_header(
            &[
                (view.t("变量名", "Name"), Some(170.0)),
                (view.t("胜出层级", "Winner"), Some(110.0)),
                (view.t("最终值", "Resolved Value"), None),
                (view.t("层级覆盖", "Layers"), Some(130.0)),
            ],
            cx,
        ));

    if view.resolved_vars.is_empty() {
        res_card = res_card.child(empty_inline(
            view.t(
                "当前环境与全局均未定义变量",
                "No variables defined in environment or globals",
            ),
            None,
            None,
            cx,
        ));
    } else {
        for r in &view.resolved_vars {
            let tone = match r.source {
                VariableResolutionSource::Environment => Tone::Accent,
                VariableResolutionSource::Global => Tone::Neutral,
                VariableResolutionSource::Unresolved => Tone::Danger,
                _ => Tone::Success,
            };
            let val_s = if r.is_secret {
                "•••••••• (secret)".to_owned()
            } else {
                r.value.clone().unwrap_or_else(|| "—".to_owned())
            };
            res_card = res_card.child(
                data_row(cx)
                    .child(
                        div()
                            .w(px(170.0))
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(r.name.clone()),
                    )
                    .child(
                        div()
                            .w(px(110.0))
                            .child(badge(format!("{:?}", r.source), tone, cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(val_s),
                    )
                    .child(
                        div()
                            .w(px(130.0))
                            .child(meta_pill(format!("{} layer(s)", r.layers.len()), cx)),
                    ),
            );
        }
    }

    // Section 2: Unresolved {{var}} scan across saved workspace requests
    let mut unres_card = card(cx).p(px(14.0)).gap(px(6.0)).child(section_header(
        view.t(
            "工作区未解析变量扫描（缺失的 {{var}}）",
            "Unresolved {{var}} Scan",
        ),
        Some(
            badge(
                view.unresolved_vars.len().to_string(),
                if view.unresolved_vars.is_empty() {
                    Tone::Success
                } else {
                    Tone::Warning
                },
                cx,
            )
            .into_any_element(),
        ),
        cx,
    ));

    if view.unresolved_vars.is_empty() {
        unres_card = unres_card.child(alert_banner(
            Tone::Success,
            view.t(
                "工作区所有已保存请求的 {{变量}} 在此环境下均可正常解析。",
                "All {{variables}} across saved requests resolve cleanly in this environment.",
            ),
            None,
            cx,
        ));
    } else {
        for u in &view.unresolved_vars {
            let key_define = u.name.clone();
            let req_names: String = u
                .requests
                .iter()
                .take(3)
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            unres_card = unres_card.child(
                data_row(cx)
                    .child(badge(format!("{{{{{}}}}}", u.name), Tone::Warning, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(format!("{} req(s): {req_names}", u.request_count)),
                    )
                    .child(
                        btn_secondary(
                            SharedString::from(format!("env-def-{}", u.name)),
                            view.t("+ 定义变量", "+ Define"),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.new_var_key.update(cx, |s, cx| {
                                    s.set_value(key_define.clone(), window, cx);
                                });
                                this.detail_tab = EnvDetailTab::Variables;
                                cx.notify();
                            },
                        )),
                    ),
            );
        }
    }

    div()
        .id("env-resolved-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(
            v_col()
                .w_full()
                .p(px(16.0))
                .gap(px(14.0))
                .child(res_card)
                .child(unres_card),
        )
        .into_any_element()
}

pub fn render_compare_tab(
    view: &EnvironmentsView,
    cx: &mut Context<EnvironmentsView>,
) -> AnyElement {
    let p = palette(cx);
    let current_id = view.selected_env_id.as_deref().unwrap_or_default();
    let others: Vec<_> = view
        .environments
        .iter()
        .filter(|e| e.id != current_id)
        .collect();

    if others.is_empty() {
        return div()
            .p(px(16.0))
            .child(empty_inline(
                view.t(
                    "需要至少两个环境才能进行对比",
                    "Create a second environment to compare",
                ),
                None,
                None,
                cx,
            ))
            .into_any_element();
    }

    let mut picker = h_row().gap(px(6.0)).flex_wrap().child(
        div()
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(view.t("对比目标:", "Compare with:")),
    );
    for other in &others {
        let oid = other.id.clone();
        let active = view.compare_target_id.as_deref() == Some(other.id.as_str());
        picker = picker.child(
            choice_chip(
                SharedString::from(format!("env-cmp-{}", other.id)),
                other.name.clone(),
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.compare_target_id = Some(oid.clone());
                this.refresh_env_analysis(cx);
                cx.notify();
            })),
        );
    }

    let include_glob_btn = choice_chip(
        "env-cmp-glob",
        view.t("包含全局变量回退", "Include Globals Fallback"),
        view.compare_include_globals,
        cx,
    )
    .on_click(cx.listener(|this, _, _, cx| {
        this.compare_include_globals = !this.compare_include_globals;
        this.refresh_env_analysis(cx);
        cx.notify();
    }));

    let mut tbl = card(cx)
        .p(px(14.0))
        .gap(px(6.0))
        .child(section_header(
            view.t(
                "环境差异对比（机密在核心内恒定时间比对，不泄露明文）",
                "Environment Diff (Secrets compared constant-time in core)",
            ),
            Some(include_glob_btn.into_any_element()),
            cx,
        ))
        .child(picker)
        .child(table_header(
            &[
                (view.t("状态", "State"), Some(92.0)),
                (view.t("变量名", "Key"), Some(160.0)),
                (view.t("当前环境", "Current"), None),
                (view.t("对比目标", "Target"), None),
            ],
            cx,
        ));

    if view.compare_rows.is_empty() {
        tbl = tbl.child(empty_inline(
            view.t("无变量可比对", "No enabled variables to compare"),
            None,
            None,
            cx,
        ));
    } else {
        for row in &view.compare_rows {
            let (lbl, tone) = match row.state {
                CompareState::Same => (view.t("一致", "Same"), Tone::Success),
                CompareState::Differs => (view.t("不同", "Differs"), Tone::Warning),
                CompareState::OnlyLeft => (view.t("仅左侧", "Left Only"), Tone::Accent),
                CompareState::OnlyRight => (view.t("仅右侧", "Right Only"), Tone::Danger),
            };
            let fmt_side = |val: &Option<String>, sec: bool, glob: bool| {
                let base = if sec {
                    "••••••••".to_owned()
                } else {
                    val.clone().unwrap_or_else(|| "—".to_owned())
                };
                if glob {
                    format!("{base} (global)")
                } else {
                    base
                }
            };
            tbl = tbl.child(
                data_row(cx)
                    .child(div().w(px(92.0)).child(badge(lbl, tone, cx)))
                    .child(
                        div()
                            .w(px(160.0))
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(row.key.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(fmt_side(&row.left, row.left_secret, row.left_global)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(fmt_side(&row.right, row.right_secret, row.right_global)),
                    ),
            );
        }
    }

    div()
        .id("env-compare-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .p(px(16.0))
        .child(tbl)
        .into_any_element()
}

pub fn render_protection_tab(
    view: &EnvironmentsView,
    cx: &mut Context<EnvironmentsView>,
) -> AnyElement {
    let p = palette(cx);

    let guard_card = card(cx)
        .p(px(14.0))
        .child(section_header(
            view.t("生产环境发送保护", "Production Send Guard"),
            None,
            cx,
        ))
        .child(setting_row(
            view.t(
                "发送写请求前弹出二次确认",
                "Confirm before sending mutating requests",
            ),
            None,
            Some(SharedString::from(view.t(
                "对 POST / PUT / PATCH / DELETE 等写操作在发送或流程执行前强制要求确认。",
                "Requires confirmation before sending POST/PUT/PATCH/DELETE or running flows.",
            ))),
            toggle_switch("env-guard-send", view.env_draft_confirm).on_click(cx.listener(
                |this, _, _, cx| {
                    this.env_draft_confirm = !this.env_draft_confirm;
                    this.env_dirty = true;
                    cx.notify();
                },
            )),
            cx,
        ))
        .child(setting_row(
            view.t(
                "同时拦截只读方法（GET / HEAD / OPTIONS）",
                "Also guard safe methods (GET / HEAD / OPTIONS)",
            ),
            None,
            Some(SharedString::from(view.t(
                "仅当上方发送前确认开启时生效，保护高敏感生产查询。",
                "Applies when confirmation guard above is enabled.",
            ))),
            toggle_switch("env-guard-safe", view.env_draft_confirm_safe).on_click(cx.listener(
                |this, _, _, cx| {
                    this.env_draft_confirm_safe = !this.env_draft_confirm_safe;
                    this.env_dirty = true;
                    cx.notify();
                },
            )),
            cx,
        ));

    let mut hosts_card = card(cx)
        .p(px(14.0))
        .gap(px(8.0))
        .child(section_header(
            view.t("机密允许发送主机白名单（Declared Hosts）", "Declared Secret Hosts"),
            Some(meta_pill(format!("{} hosts", view.declared_hosts.len()), cx).into_any_element()),
            cx,
        ))
        .child(alert_banner(
            Tone::Neutral,
            view.t(
                "AI 智能体构建的请求携带机密变量时，仅允许发往此处声明的主机；用户手动发送请求不受限制。",
                "Agent-authored requests carrying secrets may only target declared hosts below; manual sends are unrestricted.",
            ),
            None,
            cx,
        ));

    for h in &view.declared_hosts {
        let is_explicit = h.source == HostSource::Explicit;
        let host_rm = h.host.clone();
        hosts_card = hosts_card.child(
            data_row(cx)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .child(h.host.clone()),
                )
                .child(badge(
                    format!("{:?}", h.source),
                    if is_explicit {
                        Tone::Accent
                    } else {
                        Tone::Neutral
                    },
                    cx,
                ))
                .children(is_explicit.then(|| {
                    btn_icon(
                        SharedString::from(format!("env-hrm-{}", h.host)),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.env_draft_hosts.retain(|x| x != &host_rm);
                        this.env_dirty = true;
                        cx.notify();
                    }))
                })),
        );
    }

    // Show newly added (unsaved) explicit hosts too
    for manual in &view.env_draft_hosts {
        if !view.declared_hosts.iter().any(|d| &d.host == manual) {
            let host_rm = manual.clone();
            hosts_card = hosts_card.child(
                data_row(cx)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .child(manual.clone()),
                    )
                    .child(badge(view.t("待保存", "Pending"), Tone::Warning, cx))
                    .child(
                        btn_icon(
                            SharedString::from(format!("env-hrm-p-{manual}")),
                            UiIcon::Trash,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.env_draft_hosts.retain(|x| x != &host_rm);
                            this.env_dirty = true;
                            cx.notify();
                        })),
                    ),
            );
        }
    }

    hosts_card = hosts_card.child(
        h_row()
            .w_full()
            .gap(px(6.0))
            .child(div().flex_1().min_w_0().child(text_input(&view.host_input)))
            .child(
                btn_secondary("env-host-add", view.t("+ 添加主机", "+ Add Host")).on_click(
                    cx.listener(|this, _, window, cx| {
                        let h = this.host_input.read(cx).value().trim().to_ascii_lowercase();
                        if h.is_empty() {
                            return;
                        }
                        if !this.env_draft_hosts.contains(&h) {
                            this.env_draft_hosts.push(h);
                            this.env_dirty = true;
                        }
                        this.host_input
                            .update(cx, |s, cx| s.set_value("", window, cx));
                        cx.notify();
                    }),
                ),
            ),
    );

    div()
        .id("env-prot-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(
            v_col()
                .w_full()
                .p(px(16.0))
                .gap(px(14.0))
                .child(guard_card)
                .child(hosts_card),
        )
        .into_any_element()
}
