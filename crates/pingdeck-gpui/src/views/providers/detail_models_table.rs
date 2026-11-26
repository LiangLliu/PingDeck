//! Provider Models Tab table rendering: sticky header + independently scrollable body
//! with stable model-keyed IDs and zero-shift selection feedback.

use std::collections::HashMap;

use gpui_kit::{
    Context, Div, InteractiveElement as _, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::app::{InputModality, ProviderModelView};
use pingdeck_desktop::core::llm::probe::ProbeStatus;
use pingdeck_desktop::core::llm::probe::listing::Presence;
use pingdeck_desktop::core::llm::store::ProbeResult;

use crate::ui::{
    DEFAULT_PAGE_SIZES, PaginationBar, ScrollableElement as _, Tone, UiIcon, badge, btn_icon, card,
    checkbox, h_row, page_slice, pagination_bar, palette, to_hsla, toggle_switch, v_col,
};

use super::ProvidersView;
use super::format::{format_price_state, format_probe_status, format_tokens};

pub(super) fn render_models_table(
    view: &ProvidersView,
    prov_id: &str,
    filtered: &[&ProviderModelView],
    latest_probe: &HashMap<&str, &ProbeResult>,
    total_cnt: usize,
    cx: &mut Context<ProvidersView>,
) -> Div {
    let p = palette(cx);
    let (page_rows, clamped_page, _) =
        page_slice(filtered, view.models_page, view.models_page_size);
    let visible_cnt = page_rows.len();
    let all_visible_selected = visible_cnt > 0
        && page_rows
            .iter()
            .all(|m| view.selected_models.contains(&m.model.model_id));
    let visible_ids: Vec<String> = page_rows.iter().map(|m| m.model.model_id.clone()).collect();

    let thead = h_row()
        .flex_shrink_0()
        .px(px(12.0))
        .py(px(8.0))
        .gap(px(10.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(to_hsla(p.muted))
        .child(
            div()
                .w(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    checkbox("pm-sel-all", all_visible_selected).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if all_visible_selected {
                                for id in &visible_ids {
                                    this.selected_models.remove(id);
                                }
                            } else {
                                for id in &visible_ids {
                                    this.selected_models.insert(id.clone());
                                }
                            }
                            cx.notify();
                        },
                    )),
                ),
        )
        .child(
            div().flex_1().min_w(px(200.0)).child(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_models_table_model_id(),
            ),
        )
        .child(
            div().w(px(105.0)).child(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_models_table_context_out(),
            ),
        )
        .child(
            div().w(px(110.0)).flex().justify_end().child(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_models_table_price_1m(),
            ),
        )
        .child(
            div()
                .w(px(110.0))
                .flex()
                .justify_end()
                .child(crate::ui::tr(cx).providers().models_col_last()),
        )
        .child(
            div().w(px(110.0)).flex().justify_end().child(
                crate::ui::tr(cx)
                    .providers()
                    .prov_detail_models_table_enabled(),
            ),
        );

    let mut tbody = v_col();
    if filtered.is_empty() {
        tbody = tbody.child(
            v_col().items_center().gap(px(6.0)).py(px(28.0)).child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(if total_cnt == 0 {
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_models_table_no_models_yet_click_refresh()
                    } else {
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_models_table_no_models_match_the_current()
                    }),
            ),
        );
    } else {
        for (idx, mv) in page_rows.iter().enumerate() {
            let m = &mv.model;
            let mid = m.model_id.clone();
            let mid_chk = mid.clone();
            let mid_tog = mid.clone();
            let mid_copy = mid.clone();
            let mid_del = mid.clone();
            let pid_tog = prov_id.to_owned();
            let pid_del = prov_id.to_owned();
            let checked = view.selected_models.contains(&m.model_id);
            let is_new = m.first_seen_ms.is_some() && m.new_cleared_at_ms.is_none();
            let is_removed = m.presence == Presence::NotInProvider;

            let ctx_val = m.context.or(mv.catalog_context);
            let out_val = m.max_output.or(mv.catalog_output);
            let ctx_str = match (ctx_val, out_val) {
                (Some(c), Some(o)) => format!("{} / {}", format_tokens(c), format_tokens(o)),
                (Some(c), None) => format_tokens(c),
                _ => "—".into(),
            };
            let price_str =
                format_price_state(mv.effective_price.as_ref(), crate::ui::active_language(cx));

            let probe_badge = latest_probe.get(m.model_id.as_str()).map(|pr| {
                let st = if pr.ok {
                    ProbeStatus::Ready
                } else {
                    match pr.status_word.as_str() {
                        "rejected" => ProbeStatus::Rejected,
                        "rate_limited" => ProbeStatus::RateLimited,
                        "timed_out" => ProbeStatus::TimedOut,
                        "unreachable" => ProbeStatus::Unreachable,
                        _ => ProbeStatus::InvalidResponse,
                    }
                };
                let (lbl, tone) = format_probe_status(st, crate::ui::active_language(cx));
                let text = if pr.ok {
                    pr.ttft_ms
                        .or(pr.latency_ms)
                        .map_or_else(|| lbl.to_owned(), |ms| format!("{ms} ms"))
                } else {
                    lbl.to_owned()
                };
                badge(text, tone, cx)
            });

            let mut tags = h_row().gap(px(4.0)).flex_wrap();
            if is_new {
                tags = tags.child(badge(
                    crate::ui::tr(cx).providers().models_filter_new(),
                    Tone::Accent,
                    cx,
                ));
            }
            if is_removed {
                tags = tags.child(badge(
                    crate::ui::tr(cx).providers().prov_detail_models_removed(),
                    Tone::Warning,
                    cx,
                ));
            }
            if m.tools {
                tags = tags.child(cap_tag(crate::ui::tr(cx).common().cap_tools(), cx));
            }
            if m.capabilities.reasoning.supported {
                tags = tags.child(cap_tag(
                    crate::ui::tr(cx).providers().prov_bench_table_reason(),
                    cx,
                ));
            }
            if m.vision || mv.inputs.iter().any(|i| i.modality == InputModality::Image) {
                tags = tags.child(cap_tag(crate::ui::tr(cx).common().cap_vision(), cx));
            }
            if mv.inputs.iter().any(|i| i.modality == InputModality::Pdf) {
                tags = tags.child(cap_tag("PDF", cx));
            }

            let row_bg = to_hsla(if checked { p.accent_soft } else { p.surface });
            let hover_bg = to_hsla(if checked { p.accent_soft } else { p.pane });
            let mut row_el = h_row()
                .px(px(12.0))
                .py(px(8.0))
                .gap(px(10.0))
                .bg(row_bg)
                .hover(move |s| s.bg(hover_bg))
                .child(
                    div()
                        .w(px(22.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            checkbox(SharedString::from(format!("pm-chk-{mid}")), checked)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.selected_models.contains(&mid_chk) {
                                        this.selected_models.remove(&mid_chk);
                                    } else {
                                        this.selected_models.insert(mid_chk.clone());
                                    }
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    v_col()
                        .flex_1()
                        .min_w(px(200.0))
                        .gap(px(3.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(if m.enabled { p.ink } else { p.muted }))
                                .truncate()
                                .child(SharedString::from(mid.clone())),
                        )
                        .child(tags),
                )
                .child(
                    div()
                        .w(px(105.0))
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(ctx_str)),
                )
                .child(
                    div()
                        .w(px(110.0))
                        .flex()
                        .justify_end()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(price_str)),
                )
                .child(
                    div()
                        .w(px(110.0))
                        .flex()
                        .justify_end()
                        .child(match probe_badge {
                            Some(b) => b,
                            None => div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.faint))
                                .child(
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_detail_models_table_untested(),
                                ),
                        }),
                )
                .child(
                    h_row()
                        .w(px(110.0))
                        .justify_end()
                        .gap(px(4.0))
                        .child(
                            toggle_switch(SharedString::from(format!("pm-en-{mid}")), m.enabled)
                                .on_click(cx.listener({
                                    let next_en = !m.enabled;
                                    move |this, _, _, cx| {
                                        this.toggle_single_model(
                                            pid_tog.clone(),
                                            mid_tog.clone(),
                                            next_en,
                                            cx,
                                        );
                                    }
                                })),
                        )
                        .child(
                            btn_icon(SharedString::from(format!("pm-copy-{mid}")), UiIcon::Copy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let _ =
                                        pingdeck_desktop::services::clipboard::set_text(&mid_copy);
                                    this.status_msg = Some((
                                        Tone::Success,
                                        crate::ui::tr(cx)
                                            .providers()
                                            .prov_detail_models_table_model_id_copied()
                                            .into(),
                                    ));
                                    cx.notify();
                                })),
                        )
                        .child(
                            btn_icon(SharedString::from(format!("pm-del-{mid}")), UiIcon::Trash)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_single_model(pid_del.clone(), mid_del.clone(), cx);
                                })),
                        ),
                );

            if idx + 1 < visible_cnt {
                row_el = row_el
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)));
            }
            tbody = tbody.child(row_el);
        }
    }

    let mut table_card = card(cx)
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
                .id(SharedString::from(format!("pm-scroll-{prov_id}")))
                .child(tbody),
        );

    if !filtered.is_empty() {
        table_card = table_card.child(
            div()
                .flex_shrink_0()
                .px(px(12.0))
                .py(px(8.0))
                .bg(to_hsla(p.pane))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(pagination_bar(
                    PaginationBar {
                        id_prefix: "pm-pg",
                        page: clamped_page,
                        page_size: view.models_page_size,
                        total: filtered.len(),
                        page_sizes: DEFAULT_PAGE_SIZES,
                        loading: false,
                        compact: false,
                    },
                    |this: &mut ProvidersView, next_page, cx| {
                        this.models_page = next_page;
                        cx.notify();
                    },
                    Some(|this: &mut ProvidersView, next_size, cx| {
                        this.models_page_size = next_size;
                        this.models_page = 0;
                        cx.notify();
                    }),
                    cx,
                )),
        );
    }

    table_card
}

fn cap_tag(text: &'static str, cx: &gpui_kit::App) -> Div {
    let p = palette(cx);
    div()
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .bg(to_hsla(p.sunken))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .text_size(px(10.5))
        .text_color(to_hsla(p.muted))
        .child(text)
}
