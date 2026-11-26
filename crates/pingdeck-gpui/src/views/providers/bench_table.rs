//! Rendering & probe-test mapping helpers for `BenchView`:
//! pinned header & filter toolbar, sticky-header scrollable 7-column probe matrix,
//! and zero-layout-shift bottom-docked progress/status/cell-detail bar.

use gpui_kit::{
    Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::app::ProbeExportFormat;
use pingdeck_desktop::core::llm::probe::ProbeStatus;
use pingdeck_desktop::core::llm::probe::plan::CheckSet;

use crate::ui::{
    Anchor, DEFAULT_PAGE_SIZES, PaginationBar, ScrollableElement as _, Tone, UiIcon, badge,
    btn_ghost, btn_primary, btn_secondary, card, checkbox, filter_chip, h_row, icon_xs, list_row,
    page_slice, pagination_bar, palette, popover, search_input, select_pill, tab_bar_segmented,
    tab_item, to_hsla, v_col,
};

use super::bench::{BenchFilter, BenchView};
use super::bench_panels::{
    render_cell_detail_bar, render_conc_picker, render_confirm_banners, render_key_picker,
    render_status_bar,
};
use super::format::format_probe_status;

pub(super) fn render_bench(view: &BenchView, cx: &mut Context<BenchView>) -> Div {
    let p = palette(cx);
    let ent = cx.entity().clone();

    let cur_prov_name = view
        .selected_provider_id
        .as_ref()
        .and_then(|id| view.providers.iter().find(|pr| &pr.id == id))
        .map(|pr| pr.name.clone())
        .unwrap_or_else(|| {
            crate::ui::tr(cx)
                .providers()
                .prov_bench_table_pick_provider()
                .into()
        });

    let provs_clone = view.providers.clone();
    let ent_p = ent.clone();
    let prov_picker = popover("bench-prov-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            "bench-prov-trig",
            cur_prov_name,
            view.selected_provider_id.is_some(),
            cx,
        ))
        .content(move |_, _, cx| {
            let mut col = v_col().w(px(230.0)).gap(px(2.0)).p(px(6.0));
            for (i, pr) in provs_clone.iter().enumerate() {
                let pid = pr.id.clone();
                let ent = ent_p.clone();
                let pop_h = cx.entity().clone();
                col = col.child(
                    list_row(
                        SharedString::from(format!("bp-{i}")),
                        false,
                        cx,
                        move |_, window, cx| {
                            ent.update(cx, |this, cx| {
                                this.selected_provider_id = Some(pid.clone());
                                this.selected_key_id = None;
                                this.selected_models.clear();
                                this.bench_page = 0;
                                this.reload(cx);
                            });
                            pop_h.update(cx, |s, cx| s.dismiss(window, cx));
                        },
                    )
                    .child(div().text_xs().child(SharedString::from(pr.name.clone()))),
                );
            }
            col
        });

    let set_idx = match view.check_set {
        CheckSet::Quick => 0,
        CheckSet::Standard => 1,
        CheckSet::Full => 2,
    };
    let ent_seg = ent.clone();
    let set_seg = tab_bar_segmented("bench-check-set")
        .selected_index(set_idx)
        .child(tab_item(
            crate::ui::tr(cx).providers().prov_bench_table_quick(),
        ))
        .child(tab_item(crate::ui::tr(cx).providers().bench_set_standard()))
        .child(tab_item(
            crate::ui::tr(cx).providers().prov_bench_table_full(),
        ))
        .on_click(move |idx, _, cx| {
            let next = match *idx {
                1 => CheckSet::Standard,
                2 => CheckSet::Full,
                _ => CheckSet::Quick,
            };
            ent_seg.update(cx, |this, cx| {
                this.check_set = next;
                this.recompute_estimate(cx);
                cx.notify();
            });
        });

    let run_label = if view.selected_models.is_empty() {
        crate::ui::tr(cx)
            .providers()
            .prov_bench_table_run_all()
            .to_owned()
    } else {
        crate::ui::tr(cx)
            .providers()
            .prov_bench_table_run_selected(view.selected_models.len())
    };

    let pid_hdr = view.selected_provider_id.as_deref().unwrap_or("");
    let enabled_keys: Vec<_> = view
        .keys
        .iter()
        .filter(|k| k.provider_id == pid_hdr && k.enabled)
        .cloned()
        .collect();
    let key_picker = render_key_picker(view, &enabled_keys, ent.clone(), cx);
    let conc_picker = render_conc_picker(view, ent.clone(), cx);

    let header = h_row()
        .flex_shrink_0()
        .justify_between()
        .gap(px(10.0))
        .px(px(20.0))
        .py(px(12.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(8.0))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(
                            crate::ui::tr(cx)
                                .providers()
                                .prov_bench_table_model_test_bench(),
                        ),
                )
                .child(prov_picker)
                .children((enabled_keys.len() > 1).then_some(key_picker))
                .child(set_seg)
                .child(conc_picker),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(6.0))
                .child(if view.running {
                    btn_secondary(
                        "bench-stop",
                        crate::ui::tr(cx).providers().prov_bench_table_stop(),
                    )
                    .icon(icon_xs(UiIcon::Stop))
                    .on_click(cx.listener(|this, _, _, cx| this.stop_run(cx)))
                } else {
                    btn_primary("bench-run", run_label)
                        .icon(icon_xs(UiIcon::Play))
                        .on_click(cx.listener(|this, _, _, cx| this.request_start_run(None, cx)))
                })
                .children(view.run_id.clone().filter(|_| !view.running).map(|rid| {
                    btn_secondary(
                        "bench-retry",
                        crate::ui::tr(cx)
                            .providers()
                            .prov_bench_table_retry_failed(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.request_start_run(Some(rid.clone()), cx);
                    }))
                }))
                .children((view.run_id.is_some() && !view.running).then(|| {
                    btn_secondary(
                        "bench-apply",
                        crate::ui::tr(cx)
                            .providers()
                            .prov_bench_table_apply_results(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.open_apply_preview(cx)))
                }))
                .children((view.run_id.is_some() && !view.running).then(|| {
                    btn_ghost(
                        "bench-exp-csv",
                        crate::ui::tr(cx).providers().prov_bench_table_export_csv(),
                    )
                    .icon(icon_xs(UiIcon::Download))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.export_probe_results(ProbeExportFormat::Csv, cx);
                    }))
                }))
                .children(view.last_apply_id.is_some().then(|| {
                    btn_ghost(
                        "bench-undo",
                        crate::ui::tr(cx).providers().prov_bench_table_undo_apply(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.undo_apply(cx)))
                })),
        );

    let q = view.search_input.read(cx).value().trim().to_lowercase();
    let pid = view.selected_provider_id.as_deref().unwrap_or("");
    let prov_models: Vec<_> = view
        .models
        .iter()
        .filter(|m| {
            m.provider_id == pid
                && (view.include_disabled || m.enabled)
                && (q.is_empty() || m.model_id.to_lowercase().contains(&q))
        })
        .filter(|m| match view.filter {
            BenchFilter::All => true,
            BenchFilter::Failed => view.cells.iter().any(|((mid, _), c)| {
                mid == &m.model_id
                    && matches!(
                        c.status,
                        ProbeStatus::Rejected
                            | ProbeStatus::Unreachable
                            | ProbeStatus::InvalidResponse
                            | ProbeStatus::TimedOut
                            | ProbeStatus::RateLimited
                    )
            }),
            BenchFilter::Slow => view
                .cells
                .get(&(m.model_id.clone(), "chat".into()))
                .is_some_and(|c| {
                    c.result
                        .as_ref()
                        .and_then(|r| r.ttft_ms)
                        .is_some_and(|ms| ms >= 2000)
                }),
        })
        .collect();

    let toolbar = h_row()
        .flex_shrink_0()
        .justify_between()
        .gap(px(8.0))
        .px(px(20.0))
        .pt(px(14.0))
        .pb(px(10.0))
        .child(
            h_row()
                .gap(px(8.0))
                .child(
                    div()
                        .w(px(240.0))
                        .child(search_input(&view.search_input, cx)),
                )
                .child(
                    filter_chip(
                        "bf-all",
                        crate::ui::tr(cx).providers().models_filter_all(),
                        None,
                        view.filter == BenchFilter::All,
                        cx,
                    )
                    .h(px(26.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.filter = BenchFilter::All;
                        this.bench_page = 0;
                        cx.notify();
                    })),
                )
                .child(
                    filter_chip(
                        "bf-fail",
                        crate::ui::tr(cx).providers().models_filter_failed(),
                        None,
                        view.filter == BenchFilter::Failed,
                        cx,
                    )
                    .h(px(26.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.filter = BenchFilter::Failed;
                        this.bench_page = 0;
                        cx.notify();
                    })),
                )
                .child(
                    filter_chip(
                        "bf-slow",
                        crate::ui::tr(cx).providers().prov_bench_table_slow(),
                        None,
                        view.filter == BenchFilter::Slow,
                        cx,
                    )
                    .h(px(26.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.filter = BenchFilter::Slow;
                        this.bench_page = 0;
                        cx.notify();
                    })),
                )
                .child(
                    filter_chip(
                        "bf-inc-dis",
                        crate::ui::tr(cx)
                            .providers()
                            .prov_bench_table_include_disabled(),
                        None,
                        view.include_disabled,
                        cx,
                    )
                    .h(px(26.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.include_disabled = !this.include_disabled;
                        this.bench_page = 0;
                        this.recompute_estimate(cx);
                        cx.notify();
                    })),
                ),
        )
        .children(
            view.estimate
                .as_ref()
                .map(|e| badge(SharedString::from(e.summary.clone()), Tone::Neutral, cx)),
        );

    let confirm_banners = render_confirm_banners(view, cx);

    let cols: [(&str, &str); 7] = [
        ("chat", crate::ui::tr(cx).providers().prov_keytest_chat()),
        (
            "stream",
            crate::ui::tr(cx).providers().prov_bench_table_ttft(),
        ),
        ("tools", crate::ui::tr(cx).common().cap_tools()),
        ("json_schema", "JSON"),
        ("vision", crate::ui::tr(cx).common().cap_vision()),
        (
            "reasoning",
            crate::ui::tr(cx).providers().prov_bench_table_reason(),
        ),
        ("cache", crate::ui::tr(cx).common().cap_cache()),
    ];

    let mut thead = h_row()
        .flex_shrink_0()
        .px(px(12.0))
        .py(px(8.0))
        .gap(px(8.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(to_hsla(p.muted))
        .child(div().w(px(22.0)))
        .child(
            div()
                .flex_1()
                .min_w(px(180.0))
                .child(crate::ui::tr(cx).providers().bench_col_model()),
        );
    for (_, title) in cols {
        thead = thead.child(div().w(px(82.0)).flex().justify_center().child(title));
    }

    let (page_rows, clamped_page, _) =
        page_slice(&prov_models, view.bench_page, view.bench_page_size);
    let mut tbody = v_col();
    if prov_models.is_empty() {
        tbody = tbody.child(
            div()
                .p(px(24.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_bench_table_no_models_to_test_for(),
                ),
        );
    } else {
        let shown = page_rows.len();
        for (ri, m) in page_rows.iter().enumerate() {
            let mid = m.model_id.clone();
            let mid_chk = mid.clone();
            let checked = view.selected_models.contains(&mid);
            let row_bg = to_hsla(if checked { p.accent_soft } else { p.surface });
            let hover_bg = to_hsla(if checked { p.accent_soft } else { p.pane });
            let mut row = h_row()
                .px(px(12.0))
                .py(px(7.0))
                .gap(px(8.0))
                .bg(row_bg)
                .hover(move |s| s.bg(hover_bg))
                .child(
                    div()
                        .w(px(22.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            checkbox(SharedString::from(format!("bm-chk-{mid}")), checked)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.selected_models.contains(&mid_chk) {
                                        this.selected_models.remove(&mid_chk);
                                    } else {
                                        this.selected_models.insert(mid_chk.clone());
                                    }
                                    this.recompute_estimate(cx);
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(180.0))
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(SharedString::from(mid.clone())),
                );
            for (ci, (col_key, _)) in cols.iter().enumerate() {
                let cell_opt = view.cells.get(&(mid.clone(), (*col_key).to_owned()));
                let cell_mid = mid.clone();
                let cell_ck = (*col_key).to_owned();
                let el: gpui_kit::AnyElement = match cell_opt {
                    Some(cell) => {
                        let (lbl, tone) =
                            format_probe_status(cell.status, crate::ui::active_language(cx));
                        let txt = if cell.status == ProbeStatus::Ready {
                            if *col_key == "stream" {
                                cell.result
                                    .as_ref()
                                    .and_then(|r| r.ttft_ms.or(r.latency_ms))
                                    .map_or_else(|| lbl.to_owned(), |ms| format!("{ms}ms"))
                            } else if *col_key == "chat" {
                                cell.result
                                    .as_ref()
                                    .and_then(|r| r.latency_ms)
                                    .map_or_else(|| lbl.to_owned(), |ms| format!("{ms}ms"))
                            } else {
                                lbl.to_owned()
                            }
                        } else {
                            lbl.to_owned()
                        };
                        btn_ghost(SharedString::from(format!("bc-{ri}-{ci}")), "")
                            .px(px(2.0))
                            .child(badge(txt, tone, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected_cell = Some((cell_mid.clone(), cell_ck.clone()));
                                cx.notify();
                            }))
                            .into_any_element()
                    }
                    None => div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.faint))
                        .child("—")
                        .into_any_element(),
                };
                row = row.child(div().w(px(82.0)).flex().justify_center().child(el));
            }
            if ri + 1 < shown {
                row = row
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)));
            }
            tbody = tbody.child(row);
        }
    }

    let mut inner_card = card(cx)
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(thead)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .id(SharedString::from(format!("bench-scroll-{pid}")))
                .child(tbody),
        );
    if !prov_models.is_empty() {
        inner_card = inner_card.child(
            div()
                .flex_shrink_0()
                .px(px(12.0))
                .py(px(8.0))
                .bg(to_hsla(p.pane))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(pagination_bar(
                    PaginationBar {
                        id_prefix: "bench-pg",
                        page: clamped_page,
                        page_size: view.bench_page_size,
                        total: prov_models.len(),
                        page_sizes: DEFAULT_PAGE_SIZES,
                        loading: false,
                        compact: false,
                    },
                    |this: &mut BenchView, next_page, cx| {
                        this.bench_page = next_page;
                        cx.notify();
                    },
                    Some(|this: &mut BenchView, next_size, cx| {
                        this.bench_page_size = next_size;
                        this.bench_page = 0;
                        cx.notify();
                    }),
                    cx,
                )),
        );
    }
    let table_card = div()
        .flex_1()
        .min_h_0()
        .px(px(20.0))
        .pb(px(14.0))
        .child(inner_card);

    let mut root = v_col()
        .size_full()
        .child(header)
        .child(toolbar)
        .child(confirm_banners)
        .child(table_card);

    if let Some(status_bar) = render_status_bar(view, cx) {
        root = root.child(status_bar);
    }
    if let Some(detail_bar) = render_cell_detail_bar(view, cx) {
        root = root.child(detail_bar);
    }

    root
}
