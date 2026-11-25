//! Usage overview trend chart card backed by `BarChart` with explicit plot height,
//! palette-matched accent bars, Y-axis formatting, X-axis density throttling, and rich tooltips.

use gpui_kit::{
    AnyElement, Context, Corners, IntoElement as _, ParentElement as _, SharedString, Styled as _,
    div, px,
};

use crate::ui::{
    BarChart, UiIcon, card, choice_chip, empty_state, h_row, palette, section_header, to_hsla,
};

use super::state::{ChartMetric, UsageView, fmt_compact, fmt_latency};

#[derive(Clone)]
struct ChartPoint {
    label: SharedString,
    full_day: SharedString,
    value: f64,
}

fn currency_symbol(code: &str) -> &str {
    match code {
        "USD" => "$",
        "CNY" | "JPY" => "¥",
        "EUR" => "€",
        "GBP" => "£",
        _ => "",
    }
}

fn format_tick_value(
    metric: ChartMetric,
    value: f64,
    currency: &str,
    max_val: f64,
) -> SharedString {
    match metric {
        ChartMetric::Calls | ChartMetric::Tokens => {
            if value <= 0.0 {
                SharedString::from("0")
            } else if max_val < 5.0 && (value - value.round()).abs() > 0.05 {
                SharedString::from(format!("{value:.1}"))
            } else {
                SharedString::from(fmt_compact(value.round() as u64))
            }
        }
        ChartMetric::Cost => {
            let sym = currency_symbol(currency);
            let suffix = if sym.is_empty() {
                format!(" {currency}")
            } else {
                String::new()
            };
            if value <= 0.0 {
                SharedString::from(format!("{sym}0{suffix}"))
            } else if max_val < 0.1 {
                SharedString::from(format!("{sym}{value:.4}{suffix}"))
            } else {
                SharedString::from(format!("{sym}{value:.2}{suffix}"))
            }
        }
        ChartMetric::FailRate => {
            if max_val < 5.0 && value > 0.0 {
                SharedString::from(format!("{value:.1}%"))
            } else {
                SharedString::from(format!("{value:.0}%"))
            }
        }
        ChartMetric::Latency => SharedString::from(fmt_latency(value.round() as u64)),
    }
}

fn format_tooltip_value(metric: ChartMetric, value: f64, currency: &str) -> SharedString {
    match metric {
        ChartMetric::Calls | ChartMetric::Tokens => {
            SharedString::from(fmt_compact(value.round() as u64))
        }
        ChartMetric::Cost => {
            let sym = currency_symbol(currency);
            let suffix = if sym.is_empty() {
                format!(" {currency}")
            } else {
                String::new()
            };
            if value > 0.0 && value < 0.01 {
                SharedString::from(format!("{sym}{value:.4}{suffix}"))
            } else {
                SharedString::from(format!("{sym}{value:.2}{suffix}"))
            }
        }
        ChartMetric::FailRate => SharedString::from(format!("{value:.1}%")),
        ChartMetric::Latency => SharedString::from(fmt_latency(value.round() as u64)),
    }
}

pub fn render_trend_chart(view: &UsageView, cx: &mut Context<UsageView>) -> AnyElement {
    let p = palette(cx);
    let zh = view.is_zh();
    let cur = view.active_currency();
    let mut metric_bar = h_row().gap(px(4.0)).flex_wrap();
    for m in ChartMetric::ALL {
        metric_bar = metric_bar.child(
            choice_chip(
                SharedString::from(format!("u-cm-{m:?}")),
                m.label(zh),
                view.chart_metric == m,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.chart_metric = m;
                cx.notify();
            })),
        );
    }

    if view.chart_metric == ChartMetric::Cost
        && let Some(sum) = &view.summary
        && sum.currencies.len() > 1
    {
        for c in &sum.currencies {
            let cc = c.clone();
            metric_bar = metric_bar.child(
                choice_chip(
                    SharedString::from(format!("u-cur-{c}")),
                    c.clone(),
                    cur == *c,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_currency = Some(cc.clone());
                    cx.notify();
                })),
            );
        }
    }

    let series = view
        .summary
        .as_ref()
        .map(|s| s.series.as_slice())
        .unwrap_or(&[]);
    let multi_year = series
        .first()
        .and_then(|a| a.day.get(..4))
        .zip(series.last().and_then(|b| b.day.get(..4)))
        .is_some_and(|(first_yr, last_yr)| first_yr != last_yr);

    let same_date = series
        .first()
        .and_then(|a| a.day.get(..10))
        .zip(series.last().and_then(|b| b.day.get(..10)))
        .is_some_and(|(a, b)| a == b);

    let points: Vec<ChartPoint> = series
        .iter()
        .map(|slot| {
            let (short_lbl, full_lbl) =
                if let Some((date_part, time_part)) = slot.day.split_once('T') {
                    let short = if same_date {
                        time_part.to_owned()
                    } else {
                        let md = date_part
                            .strip_prefix("20")
                            .and_then(|r| r.get(3..))
                            .unwrap_or(date_part);
                        format!("{md} {time_part}")
                    };
                    (short, format!("{date_part} {time_part}"))
                } else {
                    let short = if multi_year {
                        slot.day.strip_prefix("20").unwrap_or(&slot.day).to_owned()
                    } else {
                        slot.day
                            .strip_prefix("20")
                            .and_then(|rest| rest.get(3..))
                            .unwrap_or(&slot.day)
                            .to_owned()
                    };
                    (short, slot.day.clone())
                };
            let val = match view.chart_metric {
                ChartMetric::Calls => slot.usage.requests as f64,
                ChartMetric::Tokens => slot.usage.total_tokens() as f64,
                ChartMetric::Cost => {
                    slot.usage.cost_micro.get(&cur).copied().unwrap_or(0) as f64 / 1_000_000.0
                }
                ChartMetric::FailRate => {
                    let d = slot.usage.requests.saturating_sub(slot.usage.canceled);
                    if d > 0 {
                        slot.usage.failed as f64 / d as f64 * 100.0
                    } else {
                        0.0
                    }
                }
                ChartMetric::Latency => view
                    .latency_by_day
                    .get(&slot.day)
                    .filter(|(_, c)| *c > 0)
                    .map_or(0.0, |(sum, c)| *sum as f64 / *c as f64),
            };
            ChartPoint {
                label: SharedString::from(short_lbl),
                full_day: SharedString::from(full_lbl),
                value: val,
            }
        })
        .collect();

    let max_val = points.iter().map(|pt| pt.value).fold(0.0_f64, f64::max);
    let total_reqs = view.summary.as_ref().map_or(0, |s| s.totals.requests);

    let chart_el = if points.is_empty() || max_val <= 0.0 {
        let (glyph, title, sub) = if total_reqs > 0 && view.chart_metric == ChartMetric::FailRate {
            (
                UiIcon::CircleCheck,
                view.t(
                    "该时间范围内 0 次失败（0.0%）",
                    "0 failures (0.0%) in range",
                ),
                view.t(
                    "所有已路由至服务商的调用均未发生失败。",
                    "All routed provider calls completed without failures.",
                ),
            )
        } else if total_reqs > 0 && view.chart_metric == ChartMetric::Cost {
            (
                UiIcon::Usage,
                view.t("该时间范围内估算费用为 $0", "$0 estimated cost in range"),
                view.t(
                    "命中免费模型，或本地模型目录尚未设置对应单价。",
                    "Calls used free models or models without configured local pricing.",
                ),
            )
        } else if total_reqs > 0 && view.chart_metric == ChartMetric::Latency {
            (
                UiIcon::Usage,
                view.t("该时间范围内暂无耗时样本", "No latency samples in range"),
                view.t(
                    "仅含有效耗时记录的调用会计入延迟趋势。",
                    "Only calls with recorded latency measurements appear here.",
                ),
            )
        } else {
            (
                UiIcon::Usage,
                view.t("该时间范围内暂无趋势数据", "No trend data in range"),
                view.t(
                    "通过本地网关、对话或 AI 请求发送调用后，这里将按本地时区实时聚合。",
                    "Send AI calls via Gateway, Chat, or Requests to populate this chart.",
                ),
            )
        };
        empty_state(glyph, title, sub, cx).into_any_element()
    } else {
        let metric = view.chart_metric;
        let bar_fill = if metric == ChartMetric::FailRate {
            to_hsla(p.warn)
        } else {
            to_hsla(p.accent)
        };
        let cur_tick = cur.clone();
        let cur_tip = cur;
        let point_count = points.len();
        let mut chart = BarChart::new(points)
            .id("usage-trend-barchart")
            .appear_key((metric as u8, view.range as u8, point_count))
            .band(|d: &ChartPoint| d.label.clone())
            .value(|d: &ChartPoint| d.value)
            .value_axis(true)
            .value_tick_count(4)
            .value_tick_format(move |v| format_tick_value(metric, v, &cur_tick, max_val))
            .max_band_width(px(32.0))
            .corner_radii(Corners {
                top_left: px(5.0),
                top_right: px(5.0),
                bottom_left: px(2.0),
                bottom_right: px(2.0),
            })
            .fill(move |_, _, _, _| bar_fill)
            .name(metric.label(zh))
            .tooltip_title(|d: &ChartPoint| d.full_day.clone())
            .tooltip_value(move |_, v| format_tooltip_value(metric, v, &cur_tip))
            .interactive(true);
        if point_count > 10 {
            chart = chart.band_tick_count(8);
        }
        chart.into_any_element()
    };

    card(cx)
        .w_full()
        .p(px(14.0))
        .gap(px(10.0))
        .child(section_header(
            view.t(
                "趋势图表（仅计已路由至服务商的调用）",
                "Usage Trend (routed calls)",
            ),
            Some(metric_bar.into_any_element()),
            cx,
        ))
        .child(div().w_full().h(px(184.0)).child(chart_el))
        .into_any_element()
}
