//! Resolved precedence & unresolved scan, Environment Compare diff, and Guard & Declared Hosts sub-tabs.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use spanbay_desktop::core::app::{CompareState, HostSource};
use spanbay_desktop::core::model::VariableResolutionSource;

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
    let ev = crate::ui::Tr(view.lang).environments();
    let p = palette(cx);

    // Section 1: Resolved variable precedence
    let mut res_card = card(cx)
        .p(px(14.0))
        .gap(px(6.0))
        .child(section_header(
            ev.tab_analysis_effective_variable_precedence(),
            Some(
                btn_secondary("env-res-refresh", ev.tab_analysis_rescan())
                    .icon(icon_xs(UiIcon::Refresh))
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_env_analysis(cx)))
                    .into_any_element(),
            ),
            cx,
        ))
        .child(table_header(
            &[
                (ev.tab_analysis_name(), Some(170.0)),
                (ev.tab_analysis_winner(), Some(110.0)),
                (ev.tab_analysis_resolved_value(), None),
                (ev.tab_analysis_overridden_layers(), Some(180.0)),
            ],
            cx,
        ));

    if view.resolved_vars.is_empty() {
        res_card = res_card.child(empty_inline(
            ev.tab_analysis_no_variables_defined_in_environment(),
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
            let layers_summary = if r.layers.len() <= 1 {
                format!("{} layer", r.layers.len())
            } else {
                let parts: Vec<String> = r
                    .layers
                    .iter()
                    .skip(1)
                    .map(|l| {
                        let v = if !l.enabled {
                            "off".to_owned()
                        } else if l.is_secret {
                            "•••".to_owned()
                        } else {
                            l.value.clone().unwrap_or_else(|| "∅".to_owned())
                        };
                        format!("{:?}={v}", l.source)
                    })
                    .collect();
                format!("{} · {}", r.layers.len(), parts.join(", "))
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
                            .w(px(180.0))
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(layers_summary),
                    ),
            );
        }
    }

    // Section 2: Unresolved {{var}} scan across saved workspace requests
    let mut unres_card = card(cx).p(px(14.0)).gap(px(6.0)).child(section_header(
        ev.tab_analysis_unresolved_scan(),
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
            ev.tab_analysis_all_across_saved_requests_resolve(),
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
                            ev.tab_analysis_define(),
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
    let ev = crate::ui::Tr(view.lang).environments();
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
                ev.tab_analysis_create_a_second_environment_to(),
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
            .child(ev.tab_analysis_compare_with()),
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
        ev.tab_analysis_include_globals_fallback(),
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
            ev.tab_analysis_environment_diff_secrets_compared_constant(),
            Some(include_glob_btn.into_any_element()),
            cx,
        ))
        .child(picker)
        .child(table_header(
            &[
                (ev.cmp_state(), Some(92.0)),
                (ev.tab_analysis_key(), Some(160.0)),
                (ev.tab_analysis_current(), None),
                (ev.tab_analysis_target(), None),
            ],
            cx,
        ));

    if view.compare_rows.is_empty() {
        tbl = tbl.child(empty_inline(
            ev.tab_analysis_no_enabled_variables_to_compare(),
            None,
            None,
            cx,
        ));
    } else {
        for row in &view.compare_rows {
            let (lbl, tone) = match row.state {
                CompareState::Same => (ev.tab_analysis_same(), Tone::Success),
                CompareState::Differs => (ev.tab_analysis_differs(), Tone::Warning),
                CompareState::OnlyLeft => (ev.tab_analysis_left_only(), Tone::Accent),
                CompareState::OnlyRight => (ev.tab_analysis_right_only(), Tone::Danger),
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
            let mut cmp_row = data_row(cx)
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
                );
            if row.state == CompareState::Differs {
                cmp_row = cmp_row.rounded(px(5.0)).bg(to_hsla(p.warn_soft));
            } else if matches!(row.state, CompareState::OnlyLeft | CompareState::OnlyRight) {
                cmp_row = cmp_row.rounded(px(5.0)).bg(to_hsla(p.accent_soft));
            }
            tbl = tbl.child(cmp_row);
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
    let ev = crate::ui::Tr(view.lang).environments();
    let p = palette(cx);

    let guard_card = card(cx)
        .p(px(14.0))
        .child(section_header(
            ev.tab_analysis_production_send_guard(),
            None,
            cx,
        ))
        .child(setting_row(
            ev.tab_analysis_confirm_before_sending_mutating_requests(),
            None,
            Some(SharedString::from(
                ev.tab_analysis_requires_confirmation_before_sending_post(),
            )),
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
            ev.tab_analysis_also_guard_safe_methods_get(),
            None,
            Some(SharedString::from(
                ev.tab_analysis_applies_when_confirmation_guard_above(),
            )),
            toggle_switch("env-guard-safe", view.env_draft_confirm_safe).on_click(cx.listener(
                |this, _, _, cx| {
                    this.env_draft_confirm_safe = !this.env_draft_confirm_safe;
                    this.env_dirty = true;
                    cx.notify();
                },
            )),
            cx,
        ))
        .children(view.env_draft_confirm.then(|| {
            let mut pills = h_row().gap(px(5.0)).flex_wrap().pt(px(6.0)).child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(ev.tab_analysis_guarded_methods()),
            );
            for m in ["POST", "PUT", "PATCH", "DELETE"] {
                pills = pills.child(badge(m, Tone::Warning, cx));
            }
            if view.env_draft_confirm_safe {
                for m in ["GET", "HEAD", "OPTIONS"] {
                    pills = pills.child(badge(m, Tone::Accent, cx));
                }
            }
            pills
        }));

    let mut hosts_card = card(cx)
        .p(px(14.0))
        .gap(px(8.0))
        .child(section_header(
            ev.tab_analysis_declared_secret_hosts(),
            Some(meta_pill(format!("{} hosts", view.declared_hosts.len()), cx).into_any_element()),
            cx,
        ))
        .child(alert_banner(
            Tone::Neutral,
            ev.tab_analysis_agent_authored_requests_carrying_secrets(),
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
                    .child(badge(ev.tab_analysis_pending(), Tone::Warning, cx))
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
                btn_secondary("env-host-add", ev.tab_analysis_add_host()).on_click(cx.listener(
                    |this, _, window, cx| {
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
                    },
                )),
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
