//! Bottom execution results & run history panel for `FlowsView`.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, badge, data_row, empty_inline, h_row, kv_line,
    list_row, meta_pill, palette, status_indicator, subtab_strip, tab_bar_underline, tab_item,
    to_hsla, v_col,
};

use super::state::{FlowRunTab, FlowsView};

pub fn render_run_panel(view: &FlowsView, cx: &mut Context<FlowsView>) -> AnyElement {
    let p = palette(cx);
    let tab_idx = match view.run_tab {
        FlowRunTab::LiveRun => 0,
        FlowRunTab::Variables => 1,
        FlowRunTab::History => 2,
    };
    let tabs = tab_bar_underline("flow-run-tabs")
        .selected_index(tab_idx)
        .on_click(cx.listener(|this, idx: &usize, _, cx| {
            this.run_tab = match *idx {
                1 => FlowRunTab::Variables,
                2 => FlowRunTab::History,
                _ => FlowRunTab::LiveRun,
            };
            cx.notify();
        }))
        .child(tab_item(
            crate::ui::Tr(view.lang).flows().run_panel_live_run(),
        ))
        .child(tab_item(crate::ui::tr(cx).flows().run_panel_variables(
            view.last_result.as_ref().map_or(0, |r| r.variables.len()),
        )))
        .child(tab_item(
            crate::ui::tr(cx)
                .flows()
                .run_panel_past_runs(view.past_runs.len()),
        ));

    let body: AnyElement = match view.run_tab {
        FlowRunTab::LiveRun => render_live_run(view, cx),
        FlowRunTab::Variables => render_variables(view, cx),
        FlowRunTab::History => render_history(view, cx),
    };

    v_col()
        .size_full()
        .min_h_0()
        .bg(to_hsla(p.surface))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(subtab_strip(tabs, None, cx))
        .child(
            div()
                .id("flow-run-panel-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .p(px(12.0))
                .child(body),
        )
        .into_any_element()
}

fn render_live_run(view: &FlowsView, cx: &mut Context<FlowsView>) -> AnyElement {
    let p = palette(cx);
    if view.running {
        let mut col = v_col().w_full().gap(px(6.0)).child(
            h_row()
                .gap(px(8.0))
                .child(status_indicator(StatusShape::Run, cx))
                .child(
                    div().text_xs().text_color(to_hsla(p.ink)).child(
                        crate::ui::Tr(view.lang)
                            .flows()
                            .run_panel_running_flow_steps_finished(
                                view.live_steps.len(),
                                view.live_total,
                            ),
                    ),
                ),
        );
        let mut ordered: Vec<_> = view.live_steps.values().collect();
        ordered.sort_by_key(|s| s.index);
        for s in ordered {
            col = col.child(render_step_summary_row(
                s.index,
                &s.step_name,
                s.status_code,
                s.elapsed_ms,
                s.passed,
                s.skipped,
                s.error.as_deref(),
                cx,
            ));
        }
        return col.into_any_element();
    }

    let Some(res) = &view.last_result else {
        return empty_inline(
            crate::ui::Tr(view.lang).flows().run_panel_no_flow_run_yet(),
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .flows()
                    .run_panel_click_run_flow_above_to(),
            )),
            None,
            cx,
        )
        .into_any_element();
    };

    let mut col = v_col().w_full().gap(px(6.0)).child(
        h_row()
            .gap(px(8.0))
            .child(badge(
                if res.cancelled {
                    crate::ui::Tr(view.lang).flows().row_cancelled()
                } else if res.passed {
                    crate::ui::Tr(view.lang).flows().run_panel_passed()
                } else {
                    crate::ui::Tr(view.lang).flows().run_panel_failed()
                },
                if res.cancelled {
                    Tone::Warning
                } else if res.passed {
                    Tone::Success
                } else {
                    Tone::Danger
                },
                cx,
            ))
            .child(meta_pill(format!("{} ms", res.elapsed_ms), cx))
            .child(meta_pill(
                format!("{}/{}", res.steps.len(), res.steps_total),
                cx,
            )),
    );
    for (idx, st) in res.steps.iter().enumerate() {
        let sid = st.step_id.clone();
        col = col.child(
            div()
                .id(SharedString::from(format!("flow-lres-{idx}")))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_step_id = Some(sid.clone());
                    this.step_tab = super::state::FlowStepTab::Output;
                    this.pending_sync = true;
                    cx.notify();
                }))
                .child(render_step_summary_row(
                    idx,
                    &st.step_name,
                    st.status_code,
                    st.elapsed_ms,
                    st.passed,
                    st.skipped,
                    st.error.as_deref(),
                    cx,
                )),
        );
    }
    col.into_any_element()
}

fn render_variables(view: &FlowsView, cx: &mut Context<FlowsView>) -> AnyElement {
    let Some(res) = &view.last_result else {
        return empty_inline(
            crate::ui::Tr(view.lang)
                .flows()
                .run_panel_run_the_flow_to_inspect(),
            None,
            None,
            cx,
        )
        .into_any_element();
    };
    if res.variables.is_empty() {
        return empty_inline(
            crate::ui::Tr(view.lang)
                .flows()
                .run_panel_no_variables_captured_in_this(),
            None,
            None,
            cx,
        )
        .into_any_element();
    }
    let mut col = v_col().w_full().gap(px(4.0));
    for kv in &res.variables {
        let is_secret = res.secret_variables.iter().any(|s| s == &kv.key);
        col = col.child(kv_line(
            kv.key.clone(),
            if is_secret {
                "•••••••• (secret)".to_owned()
            } else {
                kv.value.clone()
            },
            180.0,
            cx,
        ));
    }
    col.into_any_element()
}

fn render_history(view: &FlowsView, cx: &mut Context<FlowsView>) -> AnyElement {
    let p = palette(cx);
    if view.past_runs.is_empty() {
        return empty_inline(
            crate::ui::Tr(view.lang)
                .flows()
                .run_panel_no_past_runs_recorded(),
            None,
            None,
            cx,
        )
        .into_any_element();
    }
    let mut col = v_col().w_full().gap(px(6.0));
    for run in &view.past_runs {
        let rid = run.run_id.clone();
        let expanded = view.selected_past_run_id.as_deref() == Some(run.run_id.as_str());
        let label = format!(
            "{}/{} · {} ms",
            run.steps_passed, run.steps_total, run.elapsed_ms
        );
        col = col.child(
            list_row(
                SharedString::from(format!("flow-prun-{}", run.run_id)),
                expanded,
                cx,
                cx.listener(move |this, _, _, cx| {
                    this.selected_past_run_id =
                        if this.selected_past_run_id.as_deref() == Some(&rid) {
                            None
                        } else {
                            Some(rid.clone())
                        };
                    cx.notify();
                }),
            )
            .child(badge(
                if run.cancelled {
                    "CANCEL"
                } else if run.passed {
                    "PASS"
                } else {
                    "FAIL"
                },
                if run.cancelled {
                    Tone::Warning
                } else if run.passed {
                    Tone::Success
                } else {
                    Tone::Danger
                },
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(to_hsla(p.ink))
                    .truncate()
                    .child(
                        run.failed_step_name
                            .as_deref()
                            .map(|n| format!("Failed at {n}"))
                            .unwrap_or_else(|| label.clone()),
                    ),
            )
            .child(meta_pill(label, cx)),
        );
        if expanded {
            for s in &run.steps {
                col = col.child(div().pl(px(16.0)).child(render_step_summary_row(
                    s.index,
                    &s.step_name,
                    s.status_code,
                    s.elapsed_ms,
                    s.passed,
                    s.skipped,
                    s.error.as_deref(),
                    cx,
                )));
            }
        }
    }
    col.into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_step_summary_row(
    idx: usize,
    name: &str,
    status_code: Option<u16>,
    elapsed_ms: u64,
    passed: bool,
    skipped: bool,
    error: Option<&str>,
    cx: &gpui_kit::App,
) -> AnyElement {
    let p = palette(cx);
    data_row(cx)
        .child(meta_pill(format!("#{}", idx + 1), cx))
        .child(badge(
            if skipped {
                "SKIP".to_owned()
            } else if let Some(code) = status_code {
                code.to_string()
            } else if passed {
                "OK".to_owned()
            } else {
                "ERR".to_owned()
            },
            if skipped {
                Tone::Warning
            } else if passed {
                Tone::Success
            } else {
                Tone::Danger
            },
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(to_hsla(p.ink))
                .truncate()
                .child(name.to_owned()),
        )
        .children(error.map(|e| {
            div()
                .max_w(px(260.0))
                .text_xs()
                .text_color(to_hsla(p.bad))
                .truncate()
                .child(e.to_owned())
        }))
        .child(meta_pill(format!("{elapsed_ms} ms"), cx))
        .into_any_element()
}
