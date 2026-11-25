//! Master sidebar pane for `HistoryView` with server-side source/status/env filters and keyset pagination.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::app::{current_time_ms, settings_get_impl};
use pingdeck_desktop::core::model::HistorySource;
use pingdeck_desktop::time::{date_bucket, format_local_time};

use crate::bridge::runtime;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_secondary, choice_chip,
    empty_inline, h_row, list_row, master_pane, meta_pill, method_badge, palette, pane_footer,
    pane_header, search_input, section_heading, to_hsla, v_col,
};

use super::state::{HistoryModal, HistoryView};

pub fn render_sidebar(view: &HistoryView, cx: &mut Context<HistoryView>) -> AnyElement {
    let p = palette(cx);
    let h = crate::ui::Tr(view.lang).history();
    let total_shown = view.total.unwrap_or(view.entries.len() as u64);

    let sources = [
        (HistorySource::Request, "req", h.src_req_short()),
        (HistorySource::Flow, "flow", h.src_flow()),
        (HistorySource::Runner, "runner", "Runner"),
        (HistorySource::Mock, "mock", "Mock"),
        (HistorySource::Agent, "agent", "Agent"),
    ];
    let mut src_row = h_row().gap(px(4.0)).flex_wrap();
    for (src, id_tag, lbl) in sources {
        let on = view.filter.sources.contains(&src);
        src_row = src_row.child(
            choice_chip(
                SharedString::from(format!("hist-src-{id_tag}")),
                lbl,
                on,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_source(src, cx))),
        );
    }

    let mut st_row = h_row().gap(px(4.0)).flex_wrap();
    for class in 2_u8..=5 {
        let on = view.filter.status_classes.contains(&class);
        st_row = st_row.child(
            choice_chip(
                SharedString::from(format!("hist-st-{class}")),
                format!("{class}xx"),
                on,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_status_class(class, cx))),
        );
    }
    st_row = st_row.child(
        choice_chip(
            "hist-st-fail",
            crate::ui::Tr(view.lang).history().sidebar_failed(),
            view.filter.include_failed,
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| this.toggle_failed(cx))),
    );
    if !view.filter.is_empty() {
        st_row = st_row.child(
            btn_ghost(
                "hist-filter-reset",
                crate::ui::Tr(view.lang).history().sidebar_reset(),
            )
            .on_click(cx.listener(|this, _, window, cx| this.reset_filters(window, cx))),
        );
    }

    let mut filter_box = v_col()
        .w_full()
        .gap(px(6.0))
        .child(search_input(&view.search_input, cx))
        .child(src_row)
        .child(st_row);

    if !view.environments.is_empty() {
        let mut env_row = h_row().gap(px(4.0)).flex_wrap();
        for env in &view.environments {
            let eid = env.id.clone();
            let on = view.filter.environment_ids.contains(&env.id);
            env_row = env_row.child(
                choice_chip(
                    SharedString::from(format!("hist-env-{}", env.id)),
                    env.name.clone(),
                    on,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_env_filter(eid.clone(), cx);
                })),
            );
        }
        filter_box = filter_box.child(env_row);
    }

    let right_actions = h_row()
        .gap(px(2.0))
        .child(
            btn_icon("hist-export-har", UiIcon::Download)
                .tooltip(
                    crate::ui::Tr(view.lang)
                        .history()
                        .sidebar_export_filtered_entries_as_har(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.export_har(None, cx))),
        )
        .child(
            btn_icon("hist-clear-filtered", UiIcon::Trash)
                .tooltip(
                    crate::ui::Tr(view.lang)
                        .history()
                        .sidebar_clear_filtered_history(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if total_shown == 0 {
                        return;
                    }
                    this.active_modal = Some(HistoryModal::ClearFiltered { count: total_shown });
                    cx.notify();
                })),
        )
        .child(
            btn_icon("hist-refresh-top", UiIcon::Refresh)
                .tooltip(crate::ui::Tr(view.lang).history().sidebar_refresh_history())
                .on_click(cx.listener(|this, _, _, cx| this.reload_page(true, cx))),
        )
        .into_any_element();

    let header = pane_header(
        crate::ui::Tr(view.lang).history().sidebar_history(),
        Some(total_shown as usize),
        Some(right_actions),
        Some(filter_box.into_any_element()),
        cx,
    );

    let mut list = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if view.entries.is_empty() {
        list = list.child(empty_inline(
            if view.workspace_total == Some(0) {
                crate::ui::Tr(view.lang)
                    .history()
                    .sidebar_no_requests_recorded_yet()
            } else {
                crate::ui::Tr(view.lang)
                    .history()
                    .sidebar_no_entries_match_this_filter()
            },
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .history()
                    .sidebar_requests_flows_runner_mock_and(),
            )),
            None,
            cx,
        ));
    } else {
        let now_ms = current_time_ms() as i64;
        let mut last_bucket: Option<u8> = None;
        for entry in &view.entries {
            let parsed_ms = entry.executed_at.trim().parse::<i64>().ok();
            let bucket = parsed_ms.map_or(2, |ms| date_bucket(ms, now_ms));
            if last_bucket != Some(bucket) {
                last_bucket = Some(bucket);
                let grp_lbl = match bucket {
                    0 => h.bucket_today(),
                    1 => h.bucket_yesterday(),
                    _ => h.bucket_earlier(),
                };
                list = list.child(
                    div()
                        .px(px(6.0))
                        .pt(px(6.0))
                        .pb(px(2.0))
                        .child(section_heading(grp_lbl, cx)),
                );
            }
            let eid = entry.id.as_str().to_owned();
            let active = view.selected_id.as_deref() == Some(entry.id.as_str());
            let method_s = format!("{:?}", entry.method).to_uppercase();
            let checks_failed = entry.check_summary.as_ref().is_some_and(|c| c.failed > 0);
            let (st_text, st_tone) = if entry.status_code == 0 || entry.error_kind.is_some() {
                ("ERR".to_owned(), Tone::Danger)
            } else if checks_failed || entry.status_code >= 500 {
                (entry.status_code.to_string(), Tone::Danger)
            } else if entry.status_code >= 400 {
                (entry.status_code.to_string(), Tone::Warning)
            } else {
                (entry.status_code.to_string(), Tone::Success)
            };
            let title = if entry.request_name.trim().is_empty() {
                entry.request_url.clone()
            } else {
                entry.request_name.clone()
            };
            let time_s = parsed_ms.map(format_local_time);

            list = list.child(
                list_row(
                    SharedString::from(format!("hist-row-{}", entry.id.as_str())),
                    active,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.select_entry(eid.clone(), cx);
                    }),
                )
                .items_start()
                .py(px(7.0))
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(3.0))
                        .child(
                            h_row()
                                .w_full()
                                .justify_between()
                                .gap(px(6.0))
                                .child(
                                    h_row()
                                        .flex_1()
                                        .min_w_0()
                                        .gap(px(5.0))
                                        .child(method_badge(&method_s, cx))
                                        .child(badge(st_text, st_tone, cx))
                                        .children(checks_failed.then(|| {
                                            badge(
                                                crate::ui::Tr(view.lang).history().sidebar_checks(),
                                                Tone::Danger,
                                                cx,
                                            )
                                        }))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                .text_color(to_hsla(p.ink))
                                                .truncate()
                                                .child(title),
                                        ),
                                )
                                .child(meta_pill(format!("{}ms", entry.elapsed_ms), cx)),
                        )
                        .child(
                            h_row()
                                .w_full()
                                .justify_between()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(11.0))
                                        .text_color(to_hsla(p.muted))
                                        .truncate()
                                        .child(entry.request_url.clone()),
                                )
                                .child(
                                    h_row()
                                        .gap(px(4.0))
                                        .flex_shrink_0()
                                        .children(time_s.map(|t| {
                                            div()
                                                .text_size(px(10.5))
                                                .text_color(to_hsla(p.faint))
                                                .child(t)
                                        }))
                                        .child(meta_pill(
                                            source_label(entry.source, view.lang),
                                            cx,
                                        )),
                                ),
                        ),
                ),
            );
        }

        if view.next_cursor.is_some() {
            list = list.child(
                div().w_full().pt(px(6.0)).flex().justify_center().child(
                    btn_secondary(
                        "hist-load-more",
                        if view.loading {
                            crate::ui::Tr(view.lang).common().loading()
                        } else {
                            crate::ui::Tr(view.lang).history().sidebar_load_more()
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.reload_page(false, cx))),
                ),
            );
        }
    }

    let cap = settings_get_impl(runtime(cx).state())
        .data
        .history_retention;
    let footer = pane_footer(
        div()
            .text_size(px(11.0))
            .text_color(to_hsla(p.muted))
            .child(
                crate::ui::tr(cx)
                    .history()
                    .sidebar_loaded_total(view.entries.len(), total_shown),
            ),
        Some(
            div()
                .text_size(px(10.5))
                .text_color(to_hsla(p.faint))
                .child(crate::ui::tr(cx).history().sidebar_cap(cap))
                .into_any_element(),
        ),
        cx,
    );

    master_pane(
        header,
        div()
            .id("hist-sidebar-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(list),
        Some(footer.into_any_element()),
        cx,
    )
    .into_any_element()
}

pub fn source_label(
    src: HistorySource,
    lang: pingdeck_desktop::core::model::settings::Language,
) -> &'static str {
    let h = crate::ui::Tr(lang).history();
    match src {
        HistorySource::Request => h.src_req(),
        HistorySource::Runner => "Runner",
        HistorySource::Flow => h.src_flow(),
        HistorySource::Mock => "Mock",
        HistorySource::Agent => "Agent",
    }
}
