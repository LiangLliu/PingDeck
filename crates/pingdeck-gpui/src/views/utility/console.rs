//! 2-tab Diagnostic Console inside `UtilityPanel`:
//! 1. Diagnostics & Activity (live RunningWork summary, recent redacted problems, log file tail)
//! 2. Native GPU Perf Bench (`NativeBenchState`).

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::app::app_running_work_impl;
use pingdeck_desktop::core::logging::{LOG_FILE_NAME, recent_problems};
use pingdeck_desktop::services::clipboard;

use crate::bridge::runtime;
use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_secondary, card, empty_inline, h_row, icon_xs,
    kpi_card, palette, section_heading, tab_bar_segmented, tab_item, to_hsla, tr, v_col,
};

use super::UtilityPanel;
use super::bench::BenchTab;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsoleMode {
    #[default]
    Diagnostics,
    GpuBench,
}

pub fn render_console(panel: &UtilityPanel, cx: &mut Context<UtilityPanel>) -> AnyElement {
    let u = tr(cx).utility();
    let tabs = tab_bar_segmented("util-con-tabs")
        .selected_index(usize::from(panel.console_mode == ConsoleMode::GpuBench))
        .child(tab_item(u.con_tab_diagnostics()))
        .child(tab_item(u.con_tab_gpu_bench()))
        .on_click(cx.listener(|this, idx: &usize, _, cx| {
            this.console_mode = if *idx == 1 {
                ConsoleMode::GpuBench
            } else {
                ConsoleMode::Diagnostics
            };
            if this.console_mode == ConsoleMode::Diagnostics {
                this.reload_diagnostics(cx);
            }
            cx.notify();
        }));

    let body: AnyElement = match panel.console_mode {
        ConsoleMode::Diagnostics => render_diagnostics_body(panel, cx),
        ConsoleMode::GpuBench => panel
            .bench
            .render(
                cx,
                |this, tab: BenchTab, cx| {
                    this.bench.tab = tab;
                    cx.notify();
                },
                |this, cx| {
                    this.bench.append_stream_burst();
                    cx.notify();
                },
                |this, window, cx| {
                    this.bench.load_10mib_json(window, cx);
                    cx.notify();
                },
            )
            .into_any_element(),
    };

    v_col()
        .size_full()
        .gap(px(12.0))
        .child(h_row().w_full().justify_between().child(tabs))
        .child(div().flex_1().min_h_0().w_full().child(body))
        .into_any_element()
}

fn render_diagnostics_body(panel: &UtilityPanel, cx: &mut Context<UtilityPanel>) -> AnyElement {
    let p = palette(cx);
    let t = tr(cx);
    let u = t.utility();
    let c = t.common();
    let rt = runtime(cx);
    let state = rt.state();
    let work = app_running_work_impl(state);
    let gw = state.llm().gateway.status(state.llm());
    let problems = recent_problems();

    let kpis = h_row()
        .w_full()
        .gap(px(8.0))
        .child(kpi_card(
            u.con_kpi_gateway(),
            if gw.running {
                format!(":{}", gw.port)
            } else {
                c.stopped().to_owned()
            },
            None,
            cx,
        ))
        .child(kpi_card(
            u.con_kpi_mock(),
            work.mock_servers.to_string(),
            None,
            cx,
        ))
        .child(kpi_card(
            u.con_kpi_agents(),
            format!("{} (+{} sub)", work.agent_runs, work.subagents),
            None,
            cx,
        ))
        .child(kpi_card(
            u.con_kpi_flows_runs(),
            format!("{} / {}", work.flow_runs, work.collection_runs),
            None,
            cx,
        ));

    let mut prob_card = card(cx).p(px(12.0)).gap(px(6.0)).child(
        h_row()
            .w_full()
            .justify_between()
            .child(section_heading(u.con_problems_heading(problems.len()), cx))
            .child(
                h_row()
                    .gap(px(6.0))
                    .child(
                        btn_secondary("util-diag-refresh", c.refresh())
                            .icon(icon_xs(UiIcon::Refresh))
                            .on_click(cx.listener(|this, _, _, cx| this.reload_diagnostics(cx))),
                    )
                    .child(
                        btn_secondary("util-diag-copy", u.con_copy_logs())
                            .icon(icon_xs(UiIcon::Copy))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let text = this.diag_log_tail.join("\n");
                                let _ = clipboard::set_text(&text);
                                this.status_note = Some((
                                    Tone::Success,
                                    tr(cx).utility().con_copied_logs().to_owned(),
                                ));
                                cx.notify();
                            })),
                    ),
            ),
    );
    if problems.is_empty() {
        prob_card = prob_card.child(empty_inline(u.con_problems_empty(), None, None, cx));
    } else {
        for line in problems.iter().rev().take(12) {
            let tone = if line.contains(" ERROR ") {
                Tone::Danger
            } else {
                Tone::Warning
            };
            prob_card = prob_card.child(
                h_row()
                    .gap(px(6.0))
                    .child(badge(
                        if tone == Tone::Danger { "ERR" } else { "WARN" },
                        tone,
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(SharedString::from(line.clone())),
                    ),
            );
        }
    }

    let mut log_card = card(cx)
        .p(px(12.0))
        .gap(px(6.0))
        .child(section_heading(u.con_log_tail_heading(LOG_FILE_NAME), cx));
    if panel.diag_log_tail.is_empty() {
        log_card = log_card.child(empty_inline(u.con_log_empty(), None, None, cx));
    } else {
        log_card = log_card.child(
            div()
                .w_full()
                .p(px(10.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .text_size(px(11.0))
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(panel.diag_log_tail.join("\n"))),
        );
    }

    div()
        .id("util-diag-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(
            v_col()
                .w_full()
                .gap(px(12.0))
                .child(kpis)
                .child(prob_card)
                .child(log_card),
        )
        .into_any_element()
}
