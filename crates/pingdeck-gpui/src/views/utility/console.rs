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
    kpi_card, palette, section_heading, tab_bar_segmented, tab_item, to_hsla, v_col,
};

use super::UtilityPanel;
use super::bench::BenchTab;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsoleMode {
    #[default]
    Diagnostics,
    GpuBench,
}

pub fn render_console(
    panel: &UtilityPanel,
    zh: bool,
    cx: &mut Context<UtilityPanel>,
) -> AnyElement {
    let tabs = tab_bar_segmented("util-con-tabs")
        .selected_index(usize::from(panel.console_mode == ConsoleMode::GpuBench))
        .child(tab_item(if zh {
            "诊断日志与运行态"
        } else {
            "Diagnostics & Activity"
        }))
        .child(tab_item(if zh {
            "GPU 性能基准"
        } else {
            "Native GPU Bench"
        }))
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
        ConsoleMode::Diagnostics => render_diagnostics_body(panel, zh, cx),
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

fn render_diagnostics_body(
    panel: &UtilityPanel,
    zh: bool,
    cx: &mut Context<UtilityPanel>,
) -> AnyElement {
    let p = palette(cx);
    let rt = runtime(cx);
    let state = rt.state();
    let work = app_running_work_impl(state);
    let gw = state.llm().gateway.status(state.llm());
    let problems = recent_problems();

    let kpis = h_row()
        .w_full()
        .gap(px(8.0))
        .child(kpi_card(
            if zh { "本地 AI 网关" } else { "AI Gateway" },
            if gw.running {
                format!(":{}", gw.port)
            } else if zh {
                "已停止".into()
            } else {
                "Off".into()
            },
            None,
            cx,
        ))
        .child(kpi_card(
            if zh { "Mock 服务" } else { "Mock Servers" },
            work.mock_servers.to_string(),
            None,
            cx,
        ))
        .child(kpi_card(
            if zh { "智能体运行" } else { "Agent Runs" },
            format!("{} (+{} sub)", work.agent_runs, work.subagents),
            None,
            cx,
        ))
        .child(kpi_card(
            if zh {
                "流程 / Runner"
            } else {
                "Flows / Runs"
            },
            format!("{} / {}", work.flow_runs, work.collection_runs),
            None,
            cx,
        ));

    let mut prob_card = card(cx).p(px(12.0)).gap(px(6.0)).child(
        h_row()
            .w_full()
            .justify_between()
            .child(section_heading(
                if zh {
                    format!("近期告警与错误（已自动脱敏 · {} 条）", problems.len())
                } else {
                    format!("Recent Warnings & Errors (Redacted · {})", problems.len())
                },
                cx,
            ))
            .child(
                h_row()
                    .gap(px(6.0))
                    .child(
                        btn_secondary("util-diag-refresh", if zh { "刷新" } else { "Refresh" })
                            .icon(icon_xs(UiIcon::Refresh))
                            .on_click(cx.listener(|this, _, _, cx| this.reload_diagnostics(cx))),
                    )
                    .child(
                        btn_secondary("util-diag-copy", if zh { "复制日志" } else { "Copy Logs" })
                            .icon(icon_xs(UiIcon::Copy))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let text = this.diag_log_tail.join("\n");
                                let _ = clipboard::set_text(&text);
                                this.status_note = Some((
                                    Tone::Success,
                                    "Copied redacted diagnostic log tail".into(),
                                ));
                                cx.notify();
                            })),
                    ),
            ),
    );
    if problems.is_empty() {
        prob_card = prob_card.child(empty_inline(
            if zh {
                "本进程运行至今未记录任何警告或错误"
            } else {
                "No warnings or errors recorded in this session"
            },
            None,
            None,
            cx,
        ));
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

    let mut log_card = card(cx).p(px(12.0)).gap(px(6.0)).child(section_heading(
        if zh {
            format!("诊断日志尾部 ({LOG_FILE_NAME})")
        } else {
            format!("Diagnostic Log Tail ({LOG_FILE_NAME})")
        },
        cx,
    ));
    if panel.diag_log_tail.is_empty() {
        log_card = log_card.child(empty_inline(
            if zh {
                "日志文件为空或尚未创建"
            } else {
                "Log file is empty or not yet created"
            },
            None,
            None,
            cx,
        ));
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
