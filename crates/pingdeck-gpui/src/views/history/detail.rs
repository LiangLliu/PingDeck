//! Detail pane header, undo banner, and sub-tab host for `HistoryView`.

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::app::current_time_ms;
use pingdeck_desktop::time::format_smart_timestamp;

use crate::ui::{
    Tone, UiIcon, alert_banner, badge, btn_ghost, btn_icon, btn_primary, btn_secondary,
    dismissible_banner, empty_state, h_row, icon_xs, meta_pill, method_badge, palette,
    subtab_strip, tab_bar_underline, tab_item, to_hsla, toolbar_strip, v_col,
};

use super::detail_tabs::render_detail_tab;
use super::sidebar::source_label;
use super::state::{HistoryDetailTab, HistoryView};

pub fn render_detail(view: &HistoryView, cx: &mut Context<HistoryView>) -> AnyElement {
    let p = palette(cx);
    let zh = view.is_zh();

    let mut top_banners = v_col().w_full().gap(px(6.0));
    if let Some((_, count)) = &view.last_undo {
        top_banners = top_banners.child(
            div().px(px(12.0)).pt(px(8.0)).child(alert_banner(
                Tone::Warning,
                if zh {
                    format!("已删除 {count} 条历史记录（60 秒内可撤销）")
                } else {
                    format!("Deleted {count} history entry (undo available for 60s)")
                },
                Some(
                    btn_secondary("hist-undo-btn", view.t("撤销删除", "Undo"))
                        .icon(icon_xs(UiIcon::RotateCcw))
                        .on_click(cx.listener(|this, _, _, cx| this.undo_delete(cx)))
                        .into_any_element(),
                ),
                cx,
            )),
        );
    }
    if let Some((tone, msg)) = &view.status_msg {
        top_banners = top_banners.child(div().px(px(12.0)).pt(px(8.0)).child(dismissible_banner(
            "hist-banner-close",
            *tone,
            msg.clone(),
            cx.listener(|this, _, _, cx| {
                this.status_msg = None;
                cx.notify();
            }),
            cx,
        )));
    }
    if let Some(dropped) = view.mock_dropped
        && dropped > 0
    {
        top_banners = top_banners.child(div().px(px(12.0)).pt(px(6.0)).child(alert_banner(
            Tone::Warning,
            if zh {
                format!("Mock 历史写入队列满时丢弃了 {dropped} 条高频命中记录。")
            } else {
                format!("{dropped} mock hit(s) were dropped while the history queue was full.")
            },
            None,
            cx,
        )));
    }

    let Some(entry) = view.active_entry().cloned() else {
        return v_col()
            .size_full()
            .child(top_banners)
            .child(
                div().flex_1().min_h_0().child(empty_state(
                    UiIcon::History,
                    view.t("选择一条历史记录", "Select a History Entry"),
                    view.t(
                        "查看脱敏后的响应正文、响应头、实发请求、请求快照、阶段耗时与校验结果，或一键回到请求模块重放。",
                        "Inspect response body, headers, wire request, snapshot, timing waterfall, and checks, or replay in Requests.",
                    ),
                    cx,
                )),
            )
            .into_any_element();
    };

    let method_s = format!("{:?}", entry.method).to_uppercase();
    let checks_failed = entry.check_summary.as_ref().is_some_and(|c| c.failed > 0);
    let (st_text, st_tone) = if entry.status_code == 0 || entry.error_kind.is_some() {
        (
            entry
                .error_kind
                .clone()
                .unwrap_or_else(|| "ERROR".to_owned()),
            Tone::Danger,
        )
    } else if checks_failed {
        (
            if zh {
                format!("{} · 断言失败", entry.status_code)
            } else {
                format!("{} · Checks Failed", entry.status_code)
            },
            Tone::Danger,
        )
    } else {
        let tone = if entry.status_code >= 500 {
            Tone::Danger
        } else if entry.status_code >= 400 {
            Tone::Warning
        } else {
            Tone::Success
        };
        (format!("{} {}", entry.status_code, entry.status_text), tone)
    };

    let entry_open = entry.clone();
    let entry_agent = entry.clone();
    let eid_har = entry.id.as_str().to_owned();
    let eid_del = entry.id.as_str().to_owned();

    let header = toolbar_strip(cx)
        .justify_between()
        .child(
            v_col()
                .flex_1()
                .min_w_0()
                .gap(px(4.0))
                .child(
                    h_row()
                        .w_full()
                        .gap(px(6.0))
                        .child(method_badge(&method_s, cx))
                        .child(badge(st_text, st_tone, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(if entry.request_name.trim().is_empty() {
                                    entry.request_url.clone()
                                } else {
                                    entry.request_name.clone()
                                }),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .flex_wrap()
                        .child(
                            div()
                                .max_w(px(380.0))
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(entry.request_url.clone()),
                        )
                        .child(meta_pill(format!("{} ms", entry.elapsed_ms), cx))
                        .child(meta_pill(format_bytes(entry.body_size_bytes), cx))
                        .children(
                            (!entry.protocol.is_empty())
                                .then(|| meta_pill(entry.protocol.clone(), cx)),
                        )
                        .child(meta_pill(source_label(entry.source, zh), cx))
                        .children(
                            (!entry.source_name.is_empty())
                                .then(|| meta_pill(entry.source_name.clone(), cx)),
                        )
                        .children(
                            (!entry.collection_path.is_empty())
                                .then(|| meta_pill(entry.collection_path.join(" / "), cx)),
                        )
                        .children(
                            entry
                                .origin_label
                                .as_ref()
                                .filter(|s| !s.is_empty())
                                .map(|s| meta_pill(s.clone(), cx)),
                        )
                        .children(
                            (!entry.environment_name.is_empty())
                                .then(|| meta_pill(entry.environment_name.clone(), cx)),
                        ),
                ),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .flex_shrink_0()
                .child(
                    btn_primary("hist-open-req", view.t("在请求中打开", "Open in Requests"))
                        .icon(icon_xs(UiIcon::Send))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_in_requests(&entry_open, cx);
                        })),
                )
                .child(
                    btn_secondary("hist-ask-agent", view.t("问 Agent", "Ask Agent"))
                        .icon(icon_xs(UiIcon::Sparkles))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ask_agent(&entry_agent, cx);
                        })),
                )
                .child(
                    btn_ghost("hist-single-har", "HAR")
                        .icon(icon_xs(UiIcon::Download))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.export_har(Some(eid_har.clone()), cx);
                        })),
                )
                .child(
                    btn_icon("hist-del-one", UiIcon::Trash)
                        .tooltip(view.t("删除此记录", "Delete entry"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.delete_entry(eid_del.clone(), cx);
                        })),
                ),
        );

    let mut tabs = tab_bar_underline("hist-detail-tabs")
        .selected_index(view.detail_tab.index())
        .on_click(cx.listener(|this, idx: &usize, _, cx| {
            this.detail_tab = HistoryDetailTab::from_index(*idx);
            cx.notify();
        }));
    for tab in HistoryDetailTab::ALL {
        let lbl = match tab {
            HistoryDetailTab::Headers if !entry.response_headers.is_empty() => SharedString::from(
                format!("{} ({})", tab.label(zh), entry.response_headers.len()),
            ),
            HistoryDetailTab::Checks => {
                if let Some(cs) = &entry.check_summary {
                    SharedString::from(format!(
                        "{} ({}/{})",
                        tab.label(zh),
                        cs.passed,
                        cs.passed + cs.failed
                    ))
                } else {
                    SharedString::from(tab.label(zh))
                }
            }
            _ => SharedString::from(tab.label(zh)),
        };
        tabs = tabs.child(tab_item(lbl));
    }

    v_col()
        .size_full()
        .min_h_0()
        .child(header)
        .child(top_banners)
        .child(subtab_strip(
            tabs,
            Some(meta_pill(format_timestamp(&entry.executed_at, zh), cx).into_any_element()),
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(render_detail_tab(view, &entry, cx)),
        )
        .into_any_element()
}

pub fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn format_timestamp(raw: &str, zh: bool) -> String {
    if let Ok(ms) = raw.trim().parse::<i64>() {
        return format_smart_timestamp(ms, current_time_ms() as i64, zh);
    }
    raw.to_owned()
}
