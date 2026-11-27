//! Step 2 (Live Sniff Progress & Protocol Evidence) and Step 3 (Model Review & Save)
//! body sections for `SniffProviderSheet` (footer actions live in the pinned modal footer).

use gpui_kit::{
    Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use spanbay_desktop::core::llm::sniff::{KeyCheckStatus, RouteVerdict};

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, alert_banner, badge, btn_ghost, btn_secondary, card,
    checkbox, field_row, h_row, multiline_input, palette, progress_bar, search_input,
    section_heading, status_indicator, text_input, to_hsla, tr, v_col,
};

use super::format::{format_balance_report, format_family, format_protocol};
use super::sniff_sheet::SniffProviderSheet;
use super::sniff_sheet_actions::format_sniff_warning;

pub(super) fn render_step_two(
    sheet: &SniffProviderSheet,
    cx: &mut Context<SniffProviderSheet>,
) -> Div {
    let p = palette(cx);
    let pr = tr(cx).providers();
    let mut out = v_col().w_full().gap(px(12.0));

    if sheet.scanning {
        let done_n = sheet.live_rows.iter().filter(|r| r.done).count();
        let tot = sheet.live_rows.len().max(1);
        let pct = ((done_n as f32 / tot as f32) * 100.0).clamp(5.0, 100.0);
        let mut rows_col = v_col().w_full().gap(px(4.0));
        for row in &sheet.live_rows {
            rows_col = rows_col.child(
                h_row()
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.pane))
                    .child(
                        h_row()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.0))
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
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(11.0))
                                    .truncate()
                                    .child(SharedString::from(row.label.clone())),
                            ),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
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
                    .w_full()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(pr.prov_sniff_sheet_steps_scanning_protocols_models_balance()),
                    )
                    .child(progress_bar("sn-prog", pct))
                    .child(
                        div()
                            .id("sn-live-rows-scroll")
                            .w_full()
                            .max_h(px(260.0))
                            .overflow_y_scrollbar()
                            .child(rows_col),
                    ),
            ),
        );
    }

    let Some(rep) = &sheet.report else {
        return out;
    };

    let key_badge = match rep.key_status {
        KeyCheckStatus::Accepted => badge(pr.prov_sniff_sheet_steps_key_valid(), Tone::Success, cx),
        KeyCheckStatus::Rejected => badge(pr.prov_bal_err_auth(), Tone::Danger, cx),
        KeyCheckStatus::QuotaExhausted => badge(
            pr.prov_sniff_sheet_steps_quota_exhausted(),
            Tone::Warning,
            cx,
        ),
        KeyCheckStatus::ModelsBlockedButChatWorks => {
            badge(pr.prov_sniff_sheet_steps_chat_works(), Tone::Warning, cx)
        }
        KeyCheckStatus::Untested => badge(pr.prov_sniff_sheet_steps_untested(), Tone::Neutral, cx),
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
                        .w_full()
                        .justify_between()
                        .gap(px(8.0))
                        .child(div().flex_1().min_w_0().text_xs().child(SharedString::from(
                            pr.prov_sniff_sheet_steps_host_already_exists_as(&existing.name),
                        )))
                        .child(
                            btn_secondary(
                                "sn-add-key-dup",
                                pr.prov_sniff_sheet_steps_add_key_to_it(),
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
        out = out.child(alert_banner(Tone::Warning, msgs.join(" · "), None, cx));
    }

    let bal_summary = rep
        .balance
        .as_ref()
        .filter(|b| b.supported)
        .map(|b| format_balance_report(b, crate::ui::active_language(cx)))
        .unwrap_or_else(|| pr.prov_format_no_balance_api().into());

    let mut ep_table = v_col().w_full().gap(px(5.0));
    for (i, ep) in rep.endpoints.iter().enumerate() {
        let supported = ep.route.is_supported();
        let checked = sheet.checked_protocols.contains(&ep.protocol);
        let is_prim = sheet.primary_protocol == Some(ep.protocol);
        let proto = ep.protocol;
        let verdict_badge = match ep.route {
            RouteVerdict::Works => badge(pr.prov_ep_works(), Tone::Success, cx),
            RouteVerdict::Exists => badge(pr.prov_sniff_sheet_steps_exists(), Tone::Accent, cx),
            RouteVerdict::Absent => badge(pr.prov_sniff_sheet_steps_absent(), Tone::Neutral, cx),
            _ => badge(format!("{:?}", ep.route), Tone::Warning, cx),
        };
        ep_table = ep_table.child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.pane))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
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
                                .flex_shrink_0()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(format_protocol(proto, crate::ui::active_language(cx))),
                        )
                        .child(div().flex_shrink_0().child(verdict_badge))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.faint))
                                .truncate()
                                .child(SharedString::from(ep.sdk_base_url.clone())),
                        ),
                )
                .child(h_row().flex_shrink_0().gap(px(5.0)).children(
                    (checked && proto.is_chat()).then(|| {
                        if is_prim {
                            badge(pr.sniff_proto_primary(), Tone::Success, cx).into_any_element()
                        } else {
                            btn_ghost(
                                SharedString::from(format!("sn-prim-{i}")),
                                pr.prov_sniff_sheet_steps_primary(),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.primary_protocol = Some(proto);
                                cx.notify();
                            }))
                            .into_any_element()
                        }
                    }),
                )),
        );
    }

    out.child(
        card(cx).p(px(12.0)).child(
            v_col()
                .w_full()
                .gap(px(8.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .flex_wrap()
                        .gap(px(6.0))
                        .child(
                            h_row()
                                .flex_wrap()
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
                    pr.sniff_id_name(),
                    None,
                    div().w(px(240.0)).child(text_input(&sheet.name_input)),
                    cx,
                ))
                .child(section_heading(
                    pr.prov_sniff_sheet_steps_detected_protocols(),
                    cx,
                ))
                .child(ep_table),
        ),
    )
}

pub(super) fn render_step_three(
    sheet: &SniffProviderSheet,
    cx: &mut Context<SniffProviderSheet>,
) -> Div {
    let p = palette(cx);
    let pr = tr(cx).providers();
    let out = v_col().w_full().gap(px(12.0));

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
        return out.child(
            card(cx).p(px(12.0)).child(
                v_col()
                    .w_full()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(pr.prov_sniff_sheet_steps_no_model_list_returned_enter()),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(160.0))
                            .child(multiline_input(&sheet.manual_models_input)),
                    ),
            ),
        );
    }

    let mut m_list = v_col().w_full().gap(px(3.0));
    for (i, m) in filtered.iter().take(120).enumerate() {
        let id = m.id.clone();
        let id_chk = id.clone();
        let chk = sheet.checked_models.contains(&id);
        m_list = m_list.child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.pane))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(7.0))
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
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .truncate()
                                .child(SharedString::from(id)),
                        ),
                )
                .child(div().flex_shrink_0().child(badge(
                    format!("{:?}", m.kind),
                    Tone::Neutral,
                    cx,
                ))),
        );
    }

    let all_ids: Vec<String> = all_models.iter().map(|m| m.id.clone()).collect();
    let def_ids: Vec<String> = all_models
        .iter()
        .filter(|m| m.default_enabled)
        .map(|m| m.id.clone())
        .collect();

    out.child(
        card(cx).p(px(12.0)).child(
            v_col()
                .w_full()
                .gap(px(8.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .gap(px(8.0))
                        .flex_wrap()
                        .child(
                            div()
                                .w(px(230.0))
                                .child(search_input(&sheet.models_filter_input, cx)),
                        )
                        .child(
                            h_row()
                                .gap(px(4.0))
                                .child(badge(
                                    pr.prov_sniff_sheet_steps_selected(sel_cnt, all_models.len()),
                                    Tone::Accent,
                                    cx,
                                ))
                                .child(
                                    btn_ghost("sn-m-all", pr.prov_sniff_sheet_steps_all())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.checked_models = all_ids.iter().cloned().collect();
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    btn_ghost("sn-m-def", pr.prov_sniff_sheet_steps_chat())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.checked_models = def_ids.iter().cloned().collect();
                                            cx.notify();
                                        })),
                                )
                                .child(btn_ghost("sn-m-none", pr.bench_apply_none()).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.checked_models.clear();
                                        cx.notify();
                                    }),
                                )),
                        ),
                )
                .child(
                    div()
                        .id("sn-models-list-scroll")
                        .w_full()
                        .max_h(px(280.0))
                        .overflow_y_scrollbar()
                        .child(m_list),
                ),
        ),
    )
}
