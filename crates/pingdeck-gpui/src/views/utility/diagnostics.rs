//! Native right-hand Slide-Over Sheet for Diagnostics & Runtime Logs (`DiagnosticsSheet`).
//!
//! Replaces the legacy 6-tab UtilityPanel console with a focused 620px slide-over drawer:
//! - Clickable runtime status cards (Local Gateway, Mock Servers, Agent Runs, Flows/Runner)
//!   jumping directly to their owning module;
//! - Redacted recent warnings & errors with severity filters + one-click redacted diagnostic report;
//! - Searchable `pingdeck.log` tail with copy & reveal-in-folder actions;
//! - Dev/test-only GPU widget benchmark tab (`cfg(any(debug_assertions, feature = "test-support"))`).

use std::cell::Cell;

use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    ShellInfo, app_diagnostics_impl, app_running_work_impl, reveal_path_impl,
};
use pingdeck_desktop::core::logging::{LOG_FILE_NAME, recent_problems};
use pingdeck_desktop::services::clipboard;
use pingdeck_desktop::state::NavModule;

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::{
    InputEvent, InputState, ScrollableElement as _, Tone, UiIcon, WindowExt as _, badge, btn_ghost,
    btn_secondary, card, choice_chip, empty_inline, h_row, icon_xs, palette, search_input,
    section_heading, tab_bar_segmented, tab_item, to_hsla, tr, v_col,
};

use super::bench::{BenchTab, NativeBenchState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsoleMode {
    #[default]
    Diagnostics,
    GpuBench,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ProblemFilter {
    #[default]
    All,
    Errors,
    Warnings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticsHandoff {
    Navigate(NavModule),
}

pub struct DiagnosticsSheet {
    pub bench: NativeBenchState,
    pub console_mode: ConsoleMode,
    problem_filter: ProblemFilter,
    log_filter_input: Entity<InputState>,
    diag_log_tail: Vec<String>,
    status_note: Option<(Tone, String)>,
    pending_handoff: Cell<Option<DiagnosticsHandoff>>,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl DiagnosticsSheet {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let bench = NativeBenchState::new(window, cx);
        let ph = tr(cx).utility().con_log_filter_ph();
        let log_filter_input = cx.new(|cx| InputState::new(window, cx).placeholder(ph));
        let sub = cx.subscribe(&log_filter_input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        let mut sheet = Self {
            bench,
            console_mode: ConsoleMode::Diagnostics,
            problem_filter: ProblemFilter::All,
            log_filter_input,
            diag_log_tail: Vec::new(),
            status_note: None,
            pending_handoff: Cell::new(None),
            _subscriptions: vec![sub],
            _tasks: Vec::new(),
        };
        sheet.reload_diagnostics(cx);
        sheet
    }

    pub fn has_pending_handoff(&self) -> bool {
        let val = self.pending_handoff.take();
        let has = val.is_some();
        self.pending_handoff.set(val);
        has
    }

    pub fn take_pending_handoff(&self) -> Option<DiagnosticsHandoff> {
        self.pending_handoff.take()
    }

    pub fn reload_diagnostics(&mut self, cx: &mut Context<Self>) {
        let Some(log_path) = runtime(cx).log_file_path() else {
            self.diag_log_tail.clear();
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |_| {
                let text = std::fs::read_to_string(&log_path).unwrap_or_default();
                let lines: Vec<String> = text.lines().map(str::to_owned).collect();
                let start = lines.len().saturating_sub(160);
                lines[start..].to_vec()
            },
            |this, tail, cx| {
                this.diag_log_tail = tail;
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn copy_redacted_report(&mut self, cx: &mut Context<Self>) {
        let renderer = if cfg!(target_os = "macos") {
            "GPUI (Metal)"
        } else if cfg!(target_os = "linux") {
            "GPUI (Vulkan)"
        } else {
            "GPUI"
        };
        let shell = ShellInfo {
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            webview_version: renderer.to_owned(),
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let report = app_diagnostics_impl(state, &shell)?;
                clipboard::set_text(&report)?;
                Ok::<(), pingdeck_desktop::core::error::AppError>(())
            },
            |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.status_note = Some((
                            Tone::Success,
                            tr(cx).utility().con_copied_report().to_owned(),
                        ));
                    }
                    Err(e) => {
                        this.status_note = Some((Tone::Danger, e.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn reveal_log_folder(&mut self, cx: &mut Context<Self>) {
        let Some(path) = runtime(cx)
            .log_file_path()
            .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
        else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| reveal_path_impl(state, &path),
            |this, res, cx| {
                if let Err(err) = res {
                    this.status_note = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    fn jump_to(&mut self, target: NavModule, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_handoff
            .set(Some(DiagnosticsHandoff::Navigate(target)));
        window.close_sheet(cx);
        cx.notify();
    }

    fn render_diagnostics_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let t = tr(cx);
        let u = t.utility();
        let c = t.common();
        let rt = runtime(cx);
        let state = rt.state();
        let work = app_running_work_impl(state);
        let gw = state.llm().gateway.status(state.llm());
        let problems = recent_problems();
        let filtered_problems: Vec<&String> = problems
            .iter()
            .filter(|line| match self.problem_filter {
                ProblemFilter::All => true,
                ProblemFilter::Errors => line.contains(" ERROR "),
                ProblemFilter::Warnings => !line.contains(" ERROR "),
            })
            .collect();

        let kpi_item = |id: &'static str,
                        label: &'static str,
                        value: String,
                        target: NavModule,
                        cx: &mut Context<Self>| {
            let hover_border = to_hsla(p.accent_line.composite_over(p.surface));
            div()
                .id(id)
                .flex_1()
                .min_w(px(120.0))
                .p(px(10.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.pane))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .cursor_pointer()
                .hover(move |s| s.border_color(hover_border))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.jump_to(target, window, cx);
                }))
                .child(
                    v_col()
                        .gap(px(3.0))
                        .child(
                            h_row()
                                .justify_between()
                                .child(div().text_xs().text_color(to_hsla(p.muted)).child(label))
                                .child(
                                    div()
                                        .text_color(to_hsla(p.faint))
                                        .child(icon_xs(UiIcon::ExternalLink)),
                                ),
                        )
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .child(SharedString::from(value)),
                        ),
                )
        };

        let kpis = v_col()
            .w_full()
            .gap(px(6.0))
            .child(
                h_row()
                    .w_full()
                    .gap(px(8.0))
                    .flex_wrap()
                    .child(kpi_item(
                        "diag-kpi-gw",
                        u.con_kpi_gateway(),
                        if gw.running {
                            format!(":{}", gw.port)
                        } else {
                            c.stopped().to_owned()
                        },
                        NavModule::Gateway,
                        cx,
                    ))
                    .child(kpi_item(
                        "diag-kpi-mock",
                        u.con_kpi_mock(),
                        work.mock_servers.to_string(),
                        NavModule::Mock,
                        cx,
                    ))
                    .child(kpi_item(
                        "diag-kpi-agents",
                        u.con_kpi_agents(),
                        format!("{} (+{})", work.agent_runs, work.subagents),
                        NavModule::Chat,
                        cx,
                    ))
                    .child(kpi_item(
                        "diag-kpi-flows",
                        u.con_kpi_flows_runs(),
                        format!("{} / {}", work.flow_runs, work.collection_runs),
                        NavModule::Flows,
                        cx,
                    )),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.faint))
                    .child(u.con_kpi_hint()),
            );

        let mut prob_card = card(cx)
            .p(px(12.0))
            .gap(px(8.0))
            .child(
                h_row()
                    .w_full()
                    .justify_between()
                    .flex_wrap()
                    .gap(px(6.0))
                    .child(section_heading(
                        u.con_problems_heading(filtered_problems.len()),
                        cx,
                    ))
                    .child(
                        h_row()
                            .gap(px(5.0))
                            .child(
                                btn_ghost("util-diag-refresh", c.refresh())
                                    .icon(icon_xs(UiIcon::Refresh))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.reload_diagnostics(cx)),
                                    ),
                            )
                            .child(
                                btn_secondary("util-diag-report", u.con_copy_report())
                                    .icon(icon_xs(UiIcon::Copy))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.copy_redacted_report(cx)),
                                    ),
                            ),
                    ),
            )
            .child(
                h_row()
                    .gap(px(4.0))
                    .child(
                        choice_chip(
                            "diag-pf-all",
                            u.con_filter_all(),
                            self.problem_filter == ProblemFilter::All,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.problem_filter = ProblemFilter::All;
                            cx.notify();
                        })),
                    )
                    .child(
                        choice_chip(
                            "diag-pf-err",
                            u.con_filter_err(),
                            self.problem_filter == ProblemFilter::Errors,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.problem_filter = ProblemFilter::Errors;
                            cx.notify();
                        })),
                    )
                    .child(
                        choice_chip(
                            "diag-pf-warn",
                            u.con_filter_warn(),
                            self.problem_filter == ProblemFilter::Warnings,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.problem_filter = ProblemFilter::Warnings;
                            cx.notify();
                        })),
                    ),
            );

        if filtered_problems.is_empty() {
            prob_card = prob_card.child(empty_inline(u.con_problems_empty(), None, None, cx));
        } else {
            for line in filtered_problems.iter().rev().take(16) {
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
                                .child(SharedString::from((*line).clone())),
                        ),
                );
            }
        }

        let log_q = self.log_filter_input.read(cx).value().trim().to_lowercase();
        let filtered_logs: Vec<&String> = self
            .diag_log_tail
            .iter()
            .filter(|l| log_q.is_empty() || l.to_lowercase().contains(&log_q))
            .collect();
        let has_log_file = runtime(cx).log_file_path().is_some();

        let mut log_card = card(cx)
            .p(px(12.0))
            .gap(px(8.0))
            .child(
                h_row()
                    .w_full()
                    .justify_between()
                    .flex_wrap()
                    .gap(px(6.0))
                    .child(section_heading(u.con_log_tail_heading(LOG_FILE_NAME), cx))
                    .child(
                        h_row()
                            .gap(px(5.0))
                            .children(has_log_file.then(|| {
                                btn_ghost("util-diag-reveal", u.con_reveal_log())
                                    .icon(icon_xs(UiIcon::FolderOpen))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.reveal_log_folder(cx)),
                                    )
                            }))
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
            )
            .child(search_input(&self.log_filter_input, cx));

        if self.diag_log_tail.is_empty() {
            log_card = log_card.child(empty_inline(u.con_log_empty(), None, None, cx));
        } else if filtered_logs.is_empty() {
            log_card = log_card.child(empty_inline(u.con_log_no_match(), None, None, cx));
        } else {
            let joined: String = filtered_logs
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            log_card = log_card.child(
                div()
                    .w_full()
                    .p(px(10.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.sunken))
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(joined)),
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
}

impl Render for DiagnosticsSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let u = tr(cx).utility();
        let show_dev_bench = cfg!(any(debug_assertions, feature = "test-support"));

        let mut top = h_row().w_full().justify_between().gap(px(8.0));
        if show_dev_bench {
            top = top.child(
                tab_bar_segmented("util-con-tabs")
                    .selected_index(usize::from(self.console_mode == ConsoleMode::GpuBench))
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
                    })),
            );
        }
        if let Some((tone, msg)) = &self.status_note {
            top = top.child(badge(SharedString::from(msg.clone()), *tone, cx));
        }

        let body: AnyElement = if show_dev_bench && self.console_mode == ConsoleMode::GpuBench {
            self.bench
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
                .into_any_element()
        } else {
            self.render_diagnostics_body(cx)
        };

        let mut col = v_col().size_full().gap(px(12.0));
        if show_dev_bench || self.status_note.is_some() {
            col = col.child(top);
        }
        col.child(div().flex_1().min_h_0().w_full().child(body))
    }
}
