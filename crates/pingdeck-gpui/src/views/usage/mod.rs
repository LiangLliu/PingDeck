//! Usage Analytics & Gateway Request Logs module (`NavModule::Usage`).
//!
//! Directly wired to `llm_usage_summary_impl`, `llm_log_list_impl`, `llm_log_detail_impl`,
//! and live `Sequenced<LlmEvent>` updates.

use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    LogDetailDto, current_time_ms, llm_log_get_impl, llm_log_list_impl, llm_usage_summary_impl,
    settings_get_impl,
};
use pingdeck_desktop::core::llm::store::RequestLogEntry;
use pingdeck_desktop::core::llm::usage::{UsageQuery, UsageSummary};
use pingdeck_desktop::core::model::settings::Language;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into};
use crate::ui::{
    BarChart, Column, ScrollableElement as _, TableDelegate, TableState, Tone, UiIcon, badge,
    btn_primary, btn_secondary, card, empty_state, h_row, palette, section_heading, table_column,
    to_hsla, v_col, virtual_table,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangePreset {
    Day1,
    Day7,
    Day30,
}

impl RangePreset {
    fn span_ms(self) -> i64 {
        match self {
            Self::Day1 => 24 * 60 * 60 * 1000,
            Self::Day7 => 7 * 24 * 60 * 60 * 1000,
            Self::Day30 => 30 * 24 * 60 * 60 * 1000,
        }
    }
}

struct UsageLogDelegate {
    columns: Vec<Column>,
    rows: Vec<RequestLogEntry>,
}

impl UsageLogDelegate {
    fn new(rows: Vec<RequestLogEntry>) -> Self {
        Self {
            columns: vec![
                table_column("id", "ID").width(px(64.0)),
                table_column("model", "Model").width(px(200.0)),
                table_column("provider", "Provider").width(px(140.0)),
                table_column("status", "Status").width(px(76.0)),
                table_column("latency", "Latency").width(px(96.0)),
                table_column("tokens", "Tokens").width(px(110.0)),
            ],
            rows,
        }
    }
}

impl TableDelegate for UsageLogDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        self.columns
            .get(col_ix)
            .cloned()
            .unwrap_or_else(|| table_column("?", "?"))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let p = palette(cx);
        let text = self
            .rows
            .get(row_ix)
            .map(|row| match col_ix {
                0 => row.id.to_string(),
                1 => row.requested_model.clone(),
                2 => row.provider_name.clone().unwrap_or_else(|| "-".into()),
                3 => row.status.to_string(),
                4 => format!("{} ms", row.latency_ms),
                5 => format!(
                    "{}/{}",
                    row.input_tokens.unwrap_or(0),
                    row.output_tokens.unwrap_or(0)
                ),
                _ => String::new(),
            })
            .unwrap_or_default();
        div()
            .px(px(8.0))
            .text_xs()
            .text_color(to_hsla(p.ink))
            .truncate()
            .child(SharedString::from(text))
    }
}

#[derive(Clone)]
struct DayBarPoint {
    day: SharedString,
    requests: f64,
}

fn kpi_card(label: &'static str, value: String, cx: &App) -> gpui_kit::Div {
    let p = palette(cx);
    card(cx).flex_1().p(px(14.0)).child(
        v_col()
            .gap(px(4.0))
            .child(div().text_xs().text_color(to_hsla(p.muted)).child(label))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink))
                    .child(SharedString::from(value)),
            ),
    )
}

pub struct UsageView {
    range: RangePreset,
    summary: Option<UsageSummary>,
    logs: Vec<RequestLogEntry>,
    table_state: Entity<TableState<UsageLogDelegate>>,
    selected_detail: Option<LogDetailDto>,
    _tasks: Vec<Task<()>>,
}

impl UsageView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table_state =
            cx.new(|cx| TableState::new(UsageLogDelegate::new(Vec::new()), window, cx));
        let mut view = Self {
            range: RangePreset::Day7,
            summary: None,
            logs: Vec::new(),
            table_state,
            selected_detail: None,
            _tasks: Vec::new(),
        };
        view.reload(cx);
        view
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let now = current_time_ms();
        let query = UsageQuery {
            provider_id: None,
            key_id: None,
            client_id: None,
            from_ms: now - self.range.span_ms(),
            to_ms: now,
            group_by: Vec::new(),
            sources: Vec::new(),
            tz_offset_minutes: 0,
            bucket_minutes: None,
        };
        let t1 = spawn_async_into(
            cx,
            move |state| async move { llm_usage_summary_impl(&state, query).await },
            |this, res, cx| {
                if let Ok(summary) = res {
                    this.summary = Some(summary);
                }
                cx.notify();
            },
        );
        let t2 = spawn_blocking_into(
            cx,
            |state| llm_log_list_impl(state, None, Some(200), None),
            |this, res, cx| {
                if let Ok(rows) = res {
                    this.logs = rows.clone();
                    this.table_state.update(cx, |state, cx| {
                        state.delegate_mut().rows = rows;
                        cx.notify();
                    });
                }
                cx.notify();
            },
        );
        self._tasks.push(t1);
        self._tasks.push(t2);
    }

    pub fn inspect_log(&mut self, row_id: i64, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_log_get_impl(state, row_id),
            |this, res, cx| {
                if let Ok(detail) = res {
                    this.selected_detail = Some(detail);
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn range_btn(
        &self,
        id: &'static str,
        label: &'static str,
        preset: RangePreset,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.range == preset {
            btn_primary(id, label)
        } else {
            btn_secondary(id, label)
        }
        .on_click(cx.listener(move |this, _, _, cx| {
            this.range = preset;
            this.reload(cx);
        }))
    }
}

impl Render for UsageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let zh = settings_get_impl(runtime(cx).state()).general.language == Language::ZhCn;

        let (reqs, ok_reqs, failed, in_tok, out_tok) = self
            .summary
            .as_ref()
            .map(|s| {
                (
                    s.totals.requests,
                    s.totals.ok_requests,
                    s.totals.failed,
                    s.totals.input_tokens,
                    s.totals.output_tokens,
                )
            })
            .unwrap_or((0, 0, 0, 0, 0));

        let kpi_reqs = kpi_card(
            if zh { "总请求" } else { "Total Requests" },
            reqs.to_string(),
            cx,
        );
        let kpi_ok = kpi_card(
            if zh { "成功 / 失败" } else { "OK / Failed" },
            format!("{ok_reqs} / {failed}"),
            cx,
        );
        let kpi_in = kpi_card(
            if zh { "输入 Tokens" } else { "Input Tokens" },
            in_tok.to_string(),
            cx,
        );
        let kpi_out = kpi_card(
            if zh { "输出 Tokens" } else { "Output Tokens" },
            out_tok.to_string(),
            cx,
        );

        let chart_points: Vec<DayBarPoint> = self
            .summary
            .as_ref()
            .map(|s| {
                s.series
                    .iter()
                    .map(|d| DayBarPoint {
                        day: SharedString::from(d.day.clone()),
                        requests: d.usage.requests as f64,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let chart_widget = if chart_points.is_empty() {
            empty_state(
                UiIcon::Usage,
                if zh {
                    "暂无用量图表数据"
                } else {
                    "No usage data in range"
                },
                if zh {
                    "通过本地网关或对话发起 AI 请求后，这里将实时绘制用量趋势。"
                } else {
                    "Send AI calls through Chat or the local Gateway to populate charts."
                },
                cx,
            )
            .into_any_element()
        } else {
            BarChart::new(chart_points)
                .band(|d: &DayBarPoint| d.day.clone())
                .value(|d: &DayBarPoint| d.requests)
                .into_any_element()
        };

        v_col()
            .size_full()
            .bg(to_hsla(p.surface))
            .p(px(20.0))
            .gap(px(14.0))
            .overflow_y_scrollbar()
            .child(
                h_row()
                    .justify_between()
                    .child(
                        div()
                            .text_base()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(if zh {
                                "用量与网关日志"
                            } else {
                                "Usage & Gateway Logs"
                            }),
                    )
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(self.range_btn("usage-1d", "24h", RangePreset::Day1, cx))
                            .child(self.range_btn("usage-7d", "7d", RangePreset::Day7, cx))
                            .child(self.range_btn("usage-30d", "30d", RangePreset::Day30, cx))
                            .child(
                                btn_secondary("usage-refresh", if zh { "刷新" } else { "Refresh" })
                                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                            ),
                    ),
            )
            .child(
                h_row()
                    .gap(px(10.0))
                    .child(kpi_reqs)
                    .child(kpi_ok)
                    .child(kpi_in)
                    .child(kpi_out),
            )
            .child(
                card(cx).h(px(210.0)).p(px(12.0)).child(
                    v_col()
                        .size_full()
                        .gap(px(6.0))
                        .child(section_heading(
                            if zh {
                                "每日请求趋势"
                            } else {
                                "Daily Request Trend"
                            },
                            cx,
                        ))
                        .child(div().flex_1().min_h_0().child(chart_widget)),
                ),
            )
            .child(
                card(cx).flex_1().min_h(px(280.0)).p(px(12.0)).child(
                    v_col()
                        .size_full()
                        .gap(px(8.0))
                        .child(
                            h_row()
                                .justify_between()
                                .child(section_heading(
                                    format!(
                                        "{} ({})",
                                        if zh { "调用日志" } else { "Request Logs" },
                                        self.logs.len()
                                    ),
                                    cx,
                                ))
                                .child(badge("Virtualized DataTable", Tone::Accent, cx)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .child(virtual_table(&self.table_state)),
                        ),
                ),
            )
    }
}
