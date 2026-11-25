//! Confirmation banners and zero-layout-shift bottom status/cell-detail docks for `BenchView`.

use gpui_kit::{
    Context, Div, Entity, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::llm::store::ProviderKey;

use crate::ui::{
    Anchor, Tone, UiIcon, alert_banner, badge, btn_ghost, btn_icon, btn_primary, h_row, list_row,
    palette, popover, progress_bar, select_pill, to_hsla, v_col,
};

use super::bench::BenchView;
use super::format::{format_probe_status, format_probe_test};

pub(super) fn render_key_picker(
    view: &BenchView,
    enabled_keys: &[ProviderKey],
    ent: Entity<BenchView>,
    cx: &mut Context<BenchView>,
) -> impl gpui_kit::IntoElement + use<> {
    let cur_key_name = view
        .selected_key_id
        .as_ref()
        .and_then(|kid| enabled_keys.iter().find(|k| &k.id == kid))
        .map(|k| k.name.clone())
        .unwrap_or_else(|| {
            crate::ui::tr(cx)
                .providers()
                .prov_bench_panels_auto_key()
                .into()
        });
    let keys_for_pop = enabled_keys.to_vec();
    popover("bench-key-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            "bench-key-trig",
            cur_key_name,
            view.selected_key_id.is_some(),
            cx,
        ))
        .content(move |_, _, cx| {
            let mut col = v_col().w(px(210.0)).gap(px(2.0)).p(px(6.0));
            let ent0 = ent.clone();
            let pop0 = cx.entity().clone();
            col = col.child(
                list_row("bk-auto", false, cx, move |_, window, cx| {
                    ent0.update(cx, |this, cx| {
                        this.selected_key_id = None;
                        this.recompute_estimate(cx);
                        cx.notify();
                    });
                    pop0.update(cx, |s, cx| s.dismiss(window, cx));
                })
                .child(
                    div()
                        .text_xs()
                        .child(crate::ui::tr(cx).providers().prov_bench_panels_auto_key_2()),
                ),
            );
            for (i, k) in keys_for_pop.iter().enumerate() {
                let kid = k.id.clone();
                let ent_i = ent.clone();
                let pop_h = cx.entity().clone();
                col = col.child(
                    list_row(
                        SharedString::from(format!("bk-{i}")),
                        false,
                        cx,
                        move |_, window, cx| {
                            ent_i.update(cx, |this, cx| {
                                this.selected_key_id = Some(kid.clone());
                                this.recompute_estimate(cx);
                                cx.notify();
                            });
                            pop_h.update(cx, |s, cx| s.dismiss(window, cx));
                        },
                    )
                    .child(div().text_xs().child(SharedString::from(k.name.clone()))),
                );
            }
            col
        })
}

pub(super) fn render_conc_picker(
    view: &BenchView,
    ent: Entity<BenchView>,
    cx: &mut Context<BenchView>,
) -> impl gpui_kit::IntoElement + use<> {
    let cur_conc = view.concurrency;
    popover("bench-conc-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            "bench-conc-trig",
            crate::ui::tr(cx)
                .providers()
                .prov_bench_panels_label(cur_conc),
            false,
            cx,
        ))
        .content(move |_, _, cx| {
            let mut col = v_col().w(px(120.0)).gap(px(2.0)).p(px(6.0));
            for c in [1u8, 2, 4, 8] {
                let ent_i = ent.clone();
                let pop_h = cx.entity().clone();
                col = col.child(
                    list_row(
                        SharedString::from(format!("bc-n-{c}")),
                        cur_conc == c,
                        cx,
                        move |_, window, cx| {
                            ent_i.update(cx, |this, cx| {
                                this.concurrency = c;
                                cx.notify();
                            });
                            pop_h.update(cx, |s, cx| s.dismiss(window, cx));
                        },
                    )
                    .child(
                        div().text_xs().child(SharedString::from(
                            crate::ui::tr(cx)
                                .providers()
                                .prov_bench_panels_concurrent(c),
                        )),
                    ),
                );
            }
            col
        })
}

pub(super) fn render_confirm_banners(view: &BenchView, cx: &mut Context<BenchView>) -> Div {
    let mut confirm_banners = v_col().px(px(20.0)).gap(px(6.0));
    if let Some(retry_opt) = view.pending_cost_confirm.clone() {
        let est_summary = view
            .estimate
            .as_ref()
            .map(|e| e.summary.clone())
            .unwrap_or_default();
        confirm_banners = confirm_banners.child(alert_banner(
            Tone::Warning,
            crate::ui::tr(cx)
                .providers()
                .prov_bench_panels_this_benchmark_run_incurs_estimated(est_summary),
            Some(
                h_row()
                    .gap(px(6.0))
                    .child(
                        btn_ghost("bench-cost-cancel", crate::ui::tr(cx).common().cancel())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pending_cost_confirm = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        btn_primary(
                            "bench-cost-go",
                            crate::ui::tr(cx)
                                .providers()
                                .prov_bench_panels_confirm_run(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.start_run(retry_opt.clone(), true, cx);
                        })),
                    )
                    .into_any_element(),
            ),
            cx,
        ));
    }
    if let Some(prev) = &view.pending_apply_preview {
        let en = prev.plan.enable.len();
        let dis = prev.plan.disable.len();
        let cr = prev.plan.create.len();
        confirm_banners = confirm_banners.child(alert_banner(
            Tone::Accent,
            crate::ui::tr(cx)
                .providers()
                .prov_bench_panels_apply_probe_results_to_enable(&prev.provider_name, en, dis, cr),
            Some(
                h_row()
                    .gap(px(6.0))
                    .child(
                        btn_ghost("bench-app-cancel", crate::ui::tr(cx).common().cancel())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pending_apply_preview = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        btn_primary(
                            "bench-app-go",
                            crate::ui::tr(cx).providers().prov_bench_panels_apply(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_apply_results(cx);
                        })),
                    )
                    .into_any_element(),
            ),
            cx,
        ));
    }
    confirm_banners
}

pub(super) fn render_status_bar(view: &BenchView, cx: &mut Context<BenchView>) -> Option<Div> {
    if !(view.running || view.progress_total > 0 || view.flash.is_some()) {
        return None;
    }
    let p = palette(cx);
    let pct = if view.progress_total > 0 {
        (view.progress_done as f32 / view.progress_total as f32) * 100.0
    } else {
        0.0
    };
    let mut status_bar = v_col()
        .flex_shrink_0()
        .gap(px(6.0))
        .px(px(20.0))
        .py(px(9.0))
        .bg(to_hsla(p.pane))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)));
    status_bar = status_bar.child(
        h_row()
            .justify_between()
            .gap(px(8.0))
            .child(
                h_row()
                    .gap(px(8.0))
                    .children((view.running || view.progress_total > 0).then(|| {
                        div().text_xs().child(SharedString::from(format!(
                            "{} {}/{}",
                            if view.running {
                                crate::ui::tr(cx).providers().prov_bench_panels_progress()
                            } else {
                                crate::ui::tr(cx).providers().prov_bench_panels_last_run()
                            },
                            view.progress_done,
                            view.progress_total
                        )))
                    }))
                    .children(
                        view.flash
                            .as_ref()
                            .map(|(tone, msg)| badge(msg.clone(), *tone, cx)),
                    ),
            )
            .children(view.last_summary.as_ref().map(|s| {
                badge(
                    format!("{} ok · {} err", s.passed, s.failed),
                    if s.failed == 0 {
                        Tone::Success
                    } else {
                        Tone::Warning
                    },
                    cx,
                )
            })),
    );
    if view.running || view.progress_total > 0 {
        status_bar = status_bar.child(progress_bar("bench-prog", pct));
    }
    Some(status_bar)
}

pub(super) fn render_cell_detail_bar(view: &BenchView, cx: &mut Context<BenchView>) -> Option<Div> {
    let (mid, ck) = view.selected_cell.as_ref()?;
    let cell = view.cells.get(&(mid.clone(), ck.clone()))?;
    let p = palette(cx);
    let (lbl, tone) = format_probe_status(cell.status, crate::ui::active_language(cx));
    let detail_line = cell
        .result
        .as_ref()
        .map(|r| {
            format!(
                "HTTP {:?} · {}ms · TTFT {:?}ms · {}",
                r.status,
                r.latency_ms.unwrap_or(0),
                r.ttft_ms,
                if r.message.is_empty() {
                    &cell.reason
                } else {
                    &r.message
                }
            )
        })
        .unwrap_or_else(|| cell.reason.clone());
    Some(
        h_row()
            .flex_shrink_0()
            .justify_between()
            .gap(px(8.0))
            .px(px(20.0))
            .py(px(9.0))
            .bg(to_hsla(p.pane))
            .border_t_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(
                h_row()
                    .gap(px(8.0))
                    .flex_1()
                    .min_w_0()
                    .child(badge(SharedString::from(mid.clone()), Tone::Accent, cx))
                    .child(badge(
                        format_probe_test(cell.test, crate::ui::active_language(cx)),
                        Tone::Neutral,
                        cx,
                    ))
                    .child(badge(lbl, tone, cx))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(detail_line)),
                    ),
            )
            .child(
                btn_icon("bc-detail-close", UiIcon::Close).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.selected_cell = None;
                        cx.notify();
                    },
                )),
            ),
    )
}
