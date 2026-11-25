//! Step 2 (Live Sniff Progress & Protocol Evidence) and Step 3 (Model Review & Save)
//! for `SniffProviderSheet`.

use gpui_kit::{
    Context, Div, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::llm::sniff::{KeyCheckStatus, RouteVerdict};

use crate::ui::{
    StatusShape, Tone, badge, btn_ghost, btn_primary, btn_secondary, card, checkbox, field_row,
    h_row, multiline_input, palette, progress_bar, search_input, section_heading, status_indicator,
    text_input, to_hsla, v_col,
};

use super::format::{format_balance_report, format_family, format_protocol};
use super::sniff_sheet::SniffProviderSheet;
use super::sniff_sheet_actions::format_sniff_warning;

pub(super) fn render_step_two(
    sheet: &SniffProviderSheet,
    cx: &mut Context<SniffProviderSheet>,
) -> Div {
    let p = palette(cx);
    let mut out = v_col().gap(px(12.0));

    if sheet.scanning {
        let done_n = sheet.live_rows.iter().filter(|r| r.done).count();
        let tot = sheet.live_rows.len().max(1);
        let pct = ((done_n as f32 / tot as f32) * 100.0).clamp(5.0, 100.0);
        let mut rows_col = v_col().gap(px(4.0));
        for row in &sheet.live_rows {
            rows_col = rows_col.child(
                h_row()
                    .justify_between()
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(5.0))
                    .bg(to_hsla(p.pane))
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .min_w_0()
                            .child(status_indicator(
                                if row.done {
                                    StatusShape::Ok
                                } else {
                                    StatusShape::Run
                                },
                                cx,
                            ))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .truncate()
                                    .child(SharedString::from(row.label.clone())),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(row.status.map_or_else(
                                || "…".into(),
                                |s| format!("{s} ({}ms)", row.latency_ms.unwrap_or(0)),
                            ))),
                    ),
            );
        }
        return out.child(
            card(cx).p(px(12.0)).child(
                v_col()
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_sniff_sheet_steps_scanning_protocols_models_balance(),
                                ),
                            )
                            .child(
                                btn_secondary(
                                    "sn-cancel",
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_sniff_sheet_steps_cancel(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_sniff(cx))),
                            ),
                    )
                    .child(progress_bar("sn-prog", pct))
                    .child(rows_col),
            ),
        );
    }

    let Some(rep) = &sheet.report else {
        return out.child(
            btn_secondary(
                "sn-retry-1",
                crate::ui::tr(cx).providers().prov_sniff_sheet_steps_back(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.step = 1;
                cx.notify();
            })),
        );
    };

    let key_badge = match rep.key_status {
        KeyCheckStatus::Accepted => badge(
            crate::ui::tr(cx)
                .providers()
                .prov_sniff_sheet_steps_key_valid(),
            Tone::Success,
            cx,
        ),
        KeyCheckStatus::Rejected => badge(
            crate::ui::tr(cx).providers().prov_bal_err_auth(),
            Tone::Danger,
            cx,
        ),
        KeyCheckStatus::QuotaExhausted => badge(
            crate::ui::tr(cx)
                .providers()
                .prov_sniff_sheet_steps_quota_exhausted(),
            Tone::Warning,
            cx,
        ),
        KeyCheckStatus::ModelsBlockedButChatWorks => badge(
            crate::ui::tr(cx)
                .providers()
                .prov_sniff_sheet_steps_chat_works(),
            Tone::Warning,
            cx,
        ),
        KeyCheckStatus::Untested => badge(
            crate::ui::tr(cx)
                .providers()
                .prov_sniff_sheet_steps_untested(),
            Tone::Neutral,
            cx,
        ),
    };

    if let Some(dup) = &sheet.duplicate
        && let Some(existing) = &dup.provider
    {
        let eid = existing.id.clone();
        out = out.child(
            card(cx)
                .p(px(10.0))
                .bg(to_hsla(p.warn_soft))
                .border_color(to_hsla(p.warn_line.composite_over(p.warn_soft)))
                .child(
                    h_row()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            div().text_xs().child(SharedString::from(
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_sniff_sheet_steps_host_already_exists_as(&existing.name),
                            )),
                        )
                        .child(
                            btn_secondary(
                                "sn-add-key-dup",
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_sniff_sheet_steps_add_key_to_it(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.add_key_to_existing(eid.clone(), cx);
                                },
                            )),
                        ),
                ),
        );
    }

    if !rep.warnings.is_empty() {
        let msgs: Vec<String> = rep
            .warnings
            .iter()
            .map(|w| format_sniff_warning(w, crate::ui::active_language(cx)))
            .collect();
        out = out.child(crate::ui::alert_banner(
            Tone::Warning,
            msgs.join(" · "),
            None,
            cx,
        ));
    }

    let bal_summary = rep
        .balance
        .as_ref()
        .filter(|b| b.supported)
        .map(|b| format_balance_report(b, crate::ui::active_language(cx)))
        .unwrap_or_else(|| {
            crate::ui::tr(cx)
                .providers()
                .prov_format_no_balance_api()
                .into()
        });

    let mut ep_table = v_col().gap(px(5.0));
    for (i, ep) in rep.endpoints.iter().enumerate() {
        let supported = ep.route.is_supported();
        let checked = sheet.checked_protocols.contains(&ep.protocol);
        let is_prim = sheet.primary_protocol == Some(ep.protocol);
        let proto = ep.protocol;
        let verdict_badge = match ep.route {
            RouteVerdict::Works => badge(
                crate::ui::tr(cx).providers().prov_ep_works(),
                Tone::Success,
                cx,
            ),
            RouteVerdict::Exists => badge(
                crate::ui::tr(cx)
                    .providers()
                    .prov_sniff_sheet_steps_exists(),
                Tone::Accent,
                cx,
            ),
            RouteVerdict::Absent => badge(
                crate::ui::tr(cx)
                    .providers()
                    .prov_sniff_sheet_steps_absent(),
                Tone::Neutral,
                cx,
            ),
            _ => badge(format!("{:?}", ep.route), Tone::Warning, cx),
        };
        ep_table = ep_table.child(
            h_row()
                .justify_between()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.pane))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(checkbox(("sn-ep-chk", i), checked).on_click(cx.listener(
                            move |this, _, _, cx| {
                                if !supported {
                                    return;
                                }
                                if this.checked_protocols.contains(&proto) {
                                    this.checked_protocols.remove(&proto);
                                } else {
                                    this.checked_protocols.insert(proto);
                                }
                                cx.notify();
                            },
                        )))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(format_protocol(proto, crate::ui::active_language(cx))),
                        )
                        .child(verdict_badge)
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.faint))
                                .truncate()
                                .child(SharedString::from(ep.sdk_base_url.clone())),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(5.0))
                        .children((checked && proto.is_chat()).then(|| {
                            if is_prim {
                                badge(
                                    crate::ui::tr(cx).providers().sniff_proto_primary(),
                                    Tone::Success,
                                    cx,
                                )
                                .into_any_element()
                            } else {
                                btn_ghost(
                                    SharedString::from(format!("sn-prim-{i}")),
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_sniff_sheet_steps_primary(),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.primary_protocol = Some(proto);
                                    cx.notify();
                                }))
                                .into_any_element()
                            }
                        })),
                ),
        );
    }

    out.child(
        card(cx).p(px(12.0)).child(
            v_col()
                .gap(px(8.0))
                .child(
                    h_row()
                        .justify_between()
                        .flex_wrap()
                        .gap(px(6.0))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(badge(
                                    format_family(rep.family, crate::ui::active_language(cx)),
                                    Tone::Accent,
                                    cx,
                                ))
                                .children(
                                    rep.version
                                        .as_ref()
                                        .map(|v| badge(v.clone(), Tone::Neutral, cx)),
                                )
                                .child(key_badge)
                                .child(badge(bal_summary, Tone::Neutral, cx)),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.faint))
                                .child(SharedString::from(format!(
                                    "{} reqs · {}ms · {} models",
                                    rep.requests_used,
                                    rep.elapsed_ms,
                                    rep.models.len()
                                ))),
                        ),
                )
                .child(field_row(
                    crate::ui::tr(cx).providers().sniff_id_name(),
                    None,
                    div().w(px(220.0)).child(text_input(&sheet.name_input)),
                    cx,
                ))
                .child(section_heading(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_sniff_sheet_steps_detected_protocols(),
                    cx,
                ))
                .child(ep_table),
        ),
    )
    .child(
        h_row()
            .justify_between()
            .child(
                btn_secondary(
                    "sn-back-1",
                    crate::ui::tr(cx)
                        .providers()
                        .prov_sniff_sheet_steps_edit_address(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.step = 1;
                    cx.notify();
                })),
            )
            .child(
                btn_primary(
                    "sn-next-3",
                    crate::ui::tr(cx)
                        .providers()
                        .prov_sniff_sheet_steps_next_review_models(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.step = 3;
                    cx.notify();
                })),
            ),
    )
}

pub(super) fn render_step_three(
    sheet: &SniffProviderSheet,
    cx: &mut Context<SniffProviderSheet>,
) -> Div {
    let p = palette(cx);
    let mut out = v_col().gap(px(12.0));

    let Some(rep) = &sheet.report else {
        return out;
    };

    let q = sheet
        .models_filter_input
        .read(cx)
        .value()
        .trim()
        .to_lowercase();
    let all_models = rep.models.clone();
    let filtered: Vec<_> = all_models
        .iter()
        .filter(|m| q.is_empty() || m.id.to_lowercase().contains(&q))
        .collect();
    let sel_cnt = sheet.checked_models.len();

    if all_models.is_empty() {
        out = out.child(
            card(cx).p(px(12.0)).child(
                v_col()
                    .gap(px(8.0))
                    .child(
                        div().text_xs().text_color(to_hsla(p.muted)).child(
                            crate::ui::tr(cx)
                                .providers()
                                .prov_sniff_sheet_steps_no_model_list_returned_enter(),
                        ),
                    )
                    .child(
                        div()
                            .h(px(140.0))
                            .child(multiline_input(&sheet.manual_models_input)),
                    ),
            ),
        );
    } else {
        let mut m_list = v_col().gap(px(3.0));
        for (i, m) in filtered.iter().take(120).enumerate() {
            let id = m.id.clone();
            let id_chk = id.clone();
            let chk = sheet.checked_models.contains(&id);
            m_list = m_list.child(
                h_row()
                    .justify_between()
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(5.0))
                    .bg(to_hsla(p.pane))
                    .child(
                        h_row()
                            .gap(px(7.0))
                            .min_w_0()
                            .child(checkbox(("sn-m-chk", i), chk).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if this.checked_models.contains(&id_chk) {
                                        this.checked_models.remove(&id_chk);
                                    } else {
                                        this.checked_models.insert(id_chk.clone());
                                    }
                                    cx.notify();
                                },
                            )))
                            .child(div().text_xs().truncate().child(SharedString::from(id))),
                    )
                    .child(badge(format!("{:?}", m.kind), Tone::Neutral, cx)),
            );
        }
        let all_ids: Vec<String> = all_models.iter().map(|m| m.id.clone()).collect();
        let def_ids: Vec<String> = all_models
            .iter()
            .filter(|m| m.default_enabled)
            .map(|m| m.id.clone())
            .collect();

        out = out.child(
            card(cx).p(px(12.0)).child(
                v_col()
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .justify_between()
                            .gap(px(8.0))
                            .flex_wrap()
                            .child(
                                div()
                                    .w(px(220.0))
                                    .child(search_input(&sheet.models_filter_input, cx)),
                            )
                            .child(
                                h_row()
                                    .gap(px(4.0))
                                    .child(badge(
                                        crate::ui::tr(cx)
                                            .providers()
                                            .prov_sniff_sheet_steps_selected(
                                                sel_cnt,
                                                all_models.len(),
                                            ),
                                        Tone::Accent,
                                        cx,
                                    ))
                                    .child(
                                        btn_ghost(
                                            "sn-m-all",
                                            crate::ui::tr(cx)
                                                .providers()
                                                .prov_sniff_sheet_steps_all(),
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.checked_models =
                                                    all_ids.iter().cloned().collect();
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .child(
                                        btn_ghost(
                                            "sn-m-def",
                                            crate::ui::tr(cx)
                                                .providers()
                                                .prov_sniff_sheet_steps_chat(),
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.checked_models =
                                                    def_ids.iter().cloned().collect();
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .child(
                                        btn_ghost(
                                            "sn-m-none",
                                            crate::ui::tr(cx).providers().bench_apply_none(),
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.checked_models.clear();
                                                cx.notify();
                                            }),
                                        ),
                                    ),
                            ),
                    )
                    .child(m_list),
            ),
        );
    }

    out.child(
        h_row()
            .justify_between()
            .child(
                btn_secondary(
                    "sn-back-2",
                    crate::ui::tr(cx)
                        .providers()
                        .prov_sniff_sheet_steps_back_2(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.step = 2;
                    cx.notify();
                })),
            )
            .child(
                btn_primary(
                    "sn-save-final",
                    if sheet.saving {
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_config_extra_saving()
                    } else if sheet.resniff_provider_id.is_some() {
                        crate::ui::tr(cx)
                            .providers()
                            .prov_sniff_sheet_steps_apply_changes()
                    } else {
                        crate::ui::tr(cx)
                            .providers()
                            .prov_sniff_sheet_steps_save_provider()
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if !this.saving {
                        this.save_from_sniff(true, cx);
                    }
                })),
            ),
    )
}
