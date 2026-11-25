//! Collection / Folder Runner tab view and re-export of Collection / Folder Settings tab.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, alert_banner, badge, btn_danger,
    btn_primary, card, checkbox, data_row, h_row, icon_xs, kv_chip, method_badge, palette,
    progress_bar, section_heading, status_indicator, text_input, to_hsla, toggle_switch, v_col,
};

use super::RequestsView;
pub use super::container_settings::render_container_settings_tab;
use super::model::{OpenTab, method_str};

pub fn render_runner_tab(
    view: &RequestsView,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::AnyElement {
    let p = palette(cx);
    let Some(OpenTab::Runner(run)) = view.active_tab() else {
        return v_col().size_full().into_any_element();
    };
    let tid = run.tab_id.clone();
    let tid_run = tid.clone();
    let tid_stop = tid.clone();
    let running = run.running;
    let items = run
        .result
        .as_ref()
        .map(|r| r.items.as_slice())
        .unwrap_or(run.live_items.as_slice());

    let scope_reqs: Vec<_> = view
        .requests
        .iter()
        .filter(|r| {
            r.collection_id.as_ref().map(|c| c.as_str()) == Some(run.collection_id.as_str())
                && match &run.folder_id {
                    Some(fid) => r.folder_id.as_ref().map(|f| f.as_str()) == Some(fid.as_str()),
                    None => true,
                }
        })
        .collect();

    let planned = (run.selected_request_ids.len().max(1) as u32 * run.iterations.max(1)) as f32;
    let pct = ((items.len() as f32) / planned * 100.0).clamp(0.0, 100.0);
    let passed = items.iter().filter(|i| i.passed).count();
    let failed = items.len().saturating_sub(passed);

    let title = match &run.folder_name {
        Some(f) => format!("{} / {f}", run.collection_name),
        None => run.collection_name.clone(),
    };

    let mut root = v_col()
        .size_full()
        .w_full()
        .p(px(16.0))
        .gap(px(12.0))
        .bg(to_hsla(p.surface))
        .overflow_y_scrollbar()
        .child(
            h_row()
                .justify_between()
                .child(
                    h_row()
                        .gap(px(8.0))
                        .child(icon_xs(UiIcon::Play))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(SharedString::from(
                                    crate::ui::tr(cx)
                                        .requests()
                                        .runner_and_settings_collection_runner(title),
                                )),
                        )
                        .children(running.then(|| status_indicator(StatusShape::Run, cx))),
                )
                .child(if running {
                    btn_danger("runner-stop", crate::ui::tr(cx).requests().run_stop())
                        .icon(icon_xs(UiIcon::Stop))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.stop_collection_run(&tid_stop, cx);
                        }))
                } else {
                    btn_primary(
                        "runner-start",
                        crate::ui::tr(cx)
                            .requests()
                            .runner_and_settings_run_collection(),
                    )
                    .icon(icon_xs(UiIcon::Play))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.start_collection_run(tid_run.clone(), None, cx);
                    }))
                }),
        );

    // Config card
    root = root.child(
        card(cx).p(px(12.0)).child(
            h_row()
                .w_full()
                .gap(px(16.0))
                .flex_wrap()
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            div().text_xs().child(
                                crate::ui::tr(cx)
                                    .requests()
                                    .runner_and_settings_iterations(),
                            ),
                        )
                        .child(div().w(px(72.0)).child(text_input(&view.runner_iter_input))),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_xs()
                                .child(crate::ui::tr(cx).requests().runner_and_settings_delay_ms()),
                        )
                        .child(
                            div()
                                .w(px(88.0))
                                .child(text_input(&view.runner_delay_input)),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            div().text_xs().child(
                                crate::ui::tr(cx)
                                    .requests()
                                    .runner_and_settings_stop_on_error(),
                            ),
                        )
                        .child(
                            toggle_switch("runner-stop-err", run.stop_on_error).on_change(
                                cx.listener(move |this, c: &bool, _, cx| {
                                    if let Some(OpenTab::Runner(r)) =
                                        this.tabs.iter_mut().find(|t| t.tab_id() == tid)
                                    {
                                        r.stop_on_error = *c;
                                    }
                                    cx.notify();
                                }),
                            ),
                        ),
                )
                .child(kv_chip(
                    crate::ui::tr(cx).requests().runner_and_settings_selected(),
                    format!("{}/{}", run.selected_request_ids.len(), scope_reqs.len()),
                    cx,
                )),
        ),
    );

    if let Some(err) = &run.error {
        root = root.child(alert_banner(Tone::Danger, err.clone(), None, cx));
    }

    // Request checklist (when idle)
    if !scope_reqs.is_empty() && items.is_empty() && !running {
        let mut checks = v_col().gap(px(4.0)).child(section_heading(
            crate::ui::tr(cx)
                .requests()
                .runner_and_settings_requests_in_execution_order(),
            cx,
        ));
        let tid_chk = run.tab_id.clone();
        for (i, req) in scope_reqs.iter().enumerate() {
            let rid = req.id.as_str().to_owned();
            let checked = run.selected_request_ids.contains(&rid);
            let tid_c = tid_chk.clone();
            let rid_c = rid.clone();
            checks = checks.child(
                data_row(cx)
                    .child(
                        checkbox(SharedString::from(format!("run-req-{i}")), checked).on_click(
                            cx.listener(move |this, c: &bool, _, cx| {
                                if let Some(OpenTab::Runner(r)) =
                                    this.tabs.iter_mut().find(|t| t.tab_id() == tid_c)
                                {
                                    if *c {
                                        r.selected_request_ids.insert(rid_c.clone());
                                    } else {
                                        r.selected_request_ids.remove(&rid_c);
                                    }
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(method_badge(method_str(req.method), cx))
                    .child(div().text_xs().child(SharedString::from(req.name.clone()))),
            );
        }
        root = root.child(card(cx).p(px(10.0)).child(checks));
    }

    // Live progress & results
    if running || !items.is_empty() {
        let mut res_box = v_col()
            .gap(px(8.0))
            .child(
                h_row().justify_between().child(
                    h_row()
                        .gap(px(6.0))
                        .child(badge(
                            crate::ui::tr(cx).requests().runner_passed(passed),
                            Tone::Success,
                            cx,
                        ))
                        .child(badge(
                            crate::ui::tr(cx).requests().runner_failed(failed),
                            if failed > 0 {
                                Tone::Danger
                            } else {
                                Tone::Neutral
                            },
                            cx,
                        ))
                        .child(kv_chip(
                            crate::ui::tr(cx).requests().runner_total(),
                            items.len().to_string(),
                            cx,
                        )),
                ),
            )
            .child(progress_bar("runner-prog", pct));

        for (idx, item) in items.iter().enumerate() {
            let status_s = item
                .status_code
                .map(|c| format!("{c} {}", item.status_text))
                .unwrap_or_else(|| item.status_text.clone());
            let req_id_open = item.request_id.clone();
            res_box = res_box.child(
                div()
                    .id(SharedString::from(format!("run-res-{idx}")))
                    .cursor_pointer()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(if item.passed { p.pane } else { p.bad_soft }))
                    .border_1()
                    .border_color(to_hsla(p.line.composite_over(p.pane)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(req) = this
                            .requests
                            .iter()
                            .find(|r| r.id.as_str() == req_id_open)
                            .cloned()
                        {
                            this.open_request_tab(&req, window, cx);
                        }
                    }))
                    .child(
                        h_row()
                            .justify_between()
                            .gap(px(8.0))
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .min_w_0()
                                    .child(badge(
                                        if item.passed { "Pass" } else { "Fail" },
                                        if item.passed {
                                            Tone::Success
                                        } else {
                                            Tone::Danger
                                        },
                                        cx,
                                    ))
                                    .child(method_badge(method_str(item.method), cx))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .truncate()
                                            .child(SharedString::from(item.request_name.clone())),
                                    ),
                            )
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .child(badge(status_s, Tone::Neutral, cx))
                                    .child(div().text_xs().text_color(to_hsla(p.muted)).child(
                                        SharedString::from(format!("{} ms", item.elapsed_ms)),
                                    )),
                            ),
                    ),
            );
        }
        root = root.child(card(cx).p(px(12.0)).child(res_box));
    }

    root.into_any_element()
}
