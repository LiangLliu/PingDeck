//! Sync status card, flash banner, and Provider Pricing basis rules for "Models & Pricing".

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::current_time_ms;
use pingdeck_desktop::core::model::model_catalog::{
    CatalogOrigin, ModelCatalogFamily, SYNC_INTERVAL_DAYS,
};

use crate::ui::{
    Anchor, StatusShape, Tone, UiIcon, badge, btn_ghost, btn_secondary, card, h_row, icon_xs,
    list_row, palette, popover, section_heading, select_pill, status_indicator, to_hsla,
    toggle_switch, v_col,
};

use super::CatalogSettingsView;
use super::filters::render_provider_family_picker;
use super::format::{format_ago, format_next_due, format_sync_error, format_ymd};

pub fn render_flash_banner(
    tone: Tone,
    msg: &str,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let (bg, border, fg, shape) = match tone {
        Tone::Danger => (
            to_hsla(p.bad_soft),
            to_hsla(p.bad_line.composite_over(p.bad_soft)),
            to_hsla(p.bad),
            StatusShape::Bad,
        ),
        Tone::Warning => (
            to_hsla(p.warn_soft),
            to_hsla(p.warn_line.composite_over(p.warn_soft)),
            to_hsla(p.warn),
            StatusShape::Warn,
        ),
        Tone::Success => (
            to_hsla(p.ok_soft),
            to_hsla(p.ok_line.composite_over(p.ok_soft)),
            to_hsla(p.ok),
            StatusShape::Ok,
        ),
        _ => (
            to_hsla(p.accent_soft),
            to_hsla(p.accent_line.composite_over(p.accent_soft)),
            to_hsla(p.accent_ink),
            StatusShape::Live,
        ),
    };
    h_row()
        .justify_between()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(8.0))
                .child(status_indicator(shape, cx))
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(fg)
                        .child(SharedString::from(msg.to_owned())),
                ),
        )
        .child(
            btn_ghost("cat-flash-dismiss", "×").on_click(cx.listener(|this, _, _, cx| {
                this.flash_msg = None;
                cx.notify();
            })),
        )
}

pub fn render_sync_section(
    view: &CatalogSettingsView,
    zh: bool,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let now_ms = current_time_ms();

    let (headline, subline, interval_days, next_line, err_line) = if let Some(st) = &view.status {
        let head = match (st.origin, st.last_success_ms) {
            (CatalogOrigin::ModelsDev, Some(ts)) => {
                let ago = format_ago(ts, now_ms, zh);
                if zh {
                    format!("models.dev · 上次同步 {ago}")
                } else {
                    format!("models.dev · synced {ago}")
                }
            }
            _ => {
                let date = format_ymd(st.snapshot_at_ms);
                if zh {
                    format!("models.dev · 内置快照（{date}）")
                } else {
                    format!("models.dev · built-in snapshot ({date})")
                }
            }
        };
        let sub = if let Some(pg) = &view.page {
            if zh {
                format!(
                    "{} 个模型、{} 条目录报价，自建 {} 个（离线时继续使用本地缓存）。",
                    pg.total, st.models_dev_count, st.user_count
                )
            } else {
                format!(
                    "{} models, {} catalog offers, {} custom (local cache works offline).",
                    pg.total, st.models_dev_count, st.user_count
                )
            }
        } else if zh {
            format!(
                "{} 条目录报价，自建 {} 个（离线时继续使用本地缓存）。",
                st.models_dev_count, st.user_count
            )
        } else {
            format!(
                "{} catalog offers, {} custom (local cache works offline).",
                st.models_dev_count, st.user_count
            )
        };
        let next = st.next_due_ms.map(|ts| format_next_due(ts, now_ms, zh));
        let err = view.sync_error.clone().or_else(|| {
            st.last_error.as_deref().map(|raw| {
                format_sync_error(st.last_error_kind, st.last_error_http_status, Some(raw), zh)
            })
        });
        (head, sub, st.auto_interval_days, next, err)
    } else {
        (
            if zh {
                "正在读取模型目录…".into()
            } else {
                "Reading model catalog…".into()
            },
            String::new(),
            7,
            None,
            view.sync_error.clone(),
        )
    };

    let status_shape = if view.syncing {
        StatusShape::Run
    } else if err_line.is_some() {
        StatusShape::Warn
    } else {
        StatusShape::Ok
    };

    let interval_label = match (interval_days, zh) {
        (1, true) => "每天",
        (1, false) => "Daily",
        (30, true) => "每 30 天",
        (30, false) => "Every 30 days",
        (_, true) => "每 7 天",
        (_, false) => "Every 7 days",
    };

    let view_entity = cx.entity().clone();
    let interval_popover = popover("cat-sync-interval-pop")
        .anchor(Anchor::TopRight)
        .trigger(select_pill(
            "cat-sync-interval-btn",
            interval_label,
            false,
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut col = v_col().w(px(160.0)).gap(px(2.0));
            for days in SYNC_INTERVAL_DAYS {
                let lbl = match (days, zh) {
                    (1, true) => "每天",
                    (1, false) => "Daily",
                    (7, true) => "每 7 天",
                    (7, false) => "Every 7 days",
                    (_, true) => "每 30 天",
                    (_, false) => "Every 30 days",
                };
                let sel = days == interval_days;
                let ent = view_entity.clone();
                let pop_h = pop_cx.entity().clone();
                col = col.child(
                    list_row(
                        SharedString::from(format!("cat-int-{days}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            ent.update(cx, |this, cx| this.set_sync_interval(days, cx));
                            pop_h.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(lbl))
                    .children(sel.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            col
        });

    let mut box_el = v_col().gap(px(8.0)).child(
        h_row()
            .justify_between()
            .gap(px(14.0))
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.0))
                    .child(
                        h_row()
                            .gap(px(7.0))
                            .child(status_indicator(status_shape, cx))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(headline)),
                            ),
                    )
                    .children((!subline.is_empty()).then(|| {
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(subline))
                    }))
                    .children(next_line.map(|line| {
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.faint))
                            .child(SharedString::from(line))
                    })),
            )
            .child(
                h_row()
                    .gap(px(10.0))
                    .flex_shrink_0()
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                                "自动同步"
                            } else {
                                "Auto-sync"
                            }))
                            .child(toggle_switch("cat-auto-sync", view.auto_sync).on_change(
                                cx.listener(|this, checked: &bool, _, cx| {
                                    this.toggle_auto_sync(*checked, cx);
                                }),
                            )),
                    )
                    .children(view.auto_sync.then_some(interval_popover))
                    .child(if view.syncing {
                        btn_secondary("cat-sync-cancel", if zh { "取消" } else { "Cancel" })
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_sync(cx)))
                    } else {
                        btn_secondary("cat-sync-now", if zh { "立即同步" } else { "Sync now" })
                            .icon(icon_xs(UiIcon::Refresh))
                            .on_click(cx.listener(|this, _, _, cx| this.start_sync(cx)))
                    }),
            ),
    );

    if let Some(report) = &view.last_sync_report {
        let msg = if zh {
            format!(
                "同步完成：新增 {} · 更新 {} · 无变化 {}",
                report.added, report.updated, report.unchanged
            )
        } else {
            format!(
                "Synced: {} added · {} updated · {} unchanged",
                report.added, report.updated, report.unchanged
            )
        };
        box_el = box_el.child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(5.0))
                .rounded(px(7.0))
                .bg(to_hsla(p.ok_soft))
                .border_1()
                .border_color(to_hsla(p.ok_line.composite_over(p.ok_soft)))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .text_xs()
                        .text_color(to_hsla(p.ok))
                        .child(status_indicator(StatusShape::Ok, cx))
                        .child(SharedString::from(msg)),
                )
                .child(btn_ghost("cat-sync-rep-dismiss", "×").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.last_sync_report = None;
                        cx.notify();
                    },
                ))),
        );
    } else if let Some(err) = err_line {
        box_el = box_el.child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(7.0))
                .bg(to_hsla(p.warn_soft))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(6.0))
                        .child(status_indicator(StatusShape::Warn, cx))
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(to_hsla(p.warn))
                                .child(SharedString::from(err)),
                        ),
                )
                .child(
                    btn_ghost("cat-sync-retry", if zh { "重试" } else { "Retry" })
                        .on_click(cx.listener(|this, _, _, cx| this.start_sync(cx))),
                ),
        );
    }

    v_col()
        .gap(px(8.0))
        .child(section_heading(if zh { "同步" } else { "Sync" }, cx))
        .child(card(cx).p(px(14.0)).child(box_el))
}

pub fn render_provider_pricing_section(
    view: &CatalogSettingsView,
    zh: bool,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let families: Vec<ModelCatalogFamily> = view
        .page
        .as_ref()
        .map(|pg| pg.families.clone())
        .unwrap_or_default();

    let mut rows = v_col();
    for (idx, pv) in view.provider_pricing.iter().enumerate() {
        let current_label = pv
            .catalog_family_name
            .clone()
            .or_else(|| pv.catalog_family.clone())
            .unwrap_or_else(|| {
                if zh {
                    "跟随官方价（默认）".into()
                } else {
                    "Follow official prices".into()
                }
            });

        let coverage = if let Some(fam) = pv
            .catalog_family_name
            .as_deref()
            .or(pv.catalog_family.as_deref())
        {
            if zh {
                format!(
                    "{fam} 目录价覆盖 {} 个模型（共 {} 个），其余按官方价",
                    pv.matched, pv.models
                )
            } else {
                format!(
                    "{} of {} models priced by {fam}; rest use official prices",
                    pv.matched, pv.models
                )
            }
        } else if zh {
            format!("{} 个启用模型 · 跟随官方价", pv.models)
        } else {
            format!("{} enabled model(s) · official prices", pv.models)
        };

        let picker = render_provider_family_picker(
            idx,
            pv.provider_id.clone(),
            pv.provider_name.clone(),
            pv.catalog_family.clone(),
            pv.suggested_family.clone(),
            pv.suggested_family_name.clone(),
            current_label,
            families.clone(),
            zh,
            cx,
        );

        let mut row = h_row()
            .justify_between()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(10.0))
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(pv.provider_name.clone())),
                            )
                            .children((pv.overrides > 0).then(|| {
                                badge(
                                    if zh {
                                        format!("{} 个单独定价", pv.overrides)
                                    } else {
                                        format!("{} override(s)", pv.overrides)
                                    },
                                    Tone::Accent,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(coverage)),
                    ),
            )
            .child(div().flex_shrink_0().child(picker));

        if idx + 1 < view.provider_pricing.len() {
            row = row
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)));
        }
        rows = rows.child(row);
    }

    v_col()
        .gap(px(6.0))
        .child(section_heading(
            if zh {
                "服务商计价"
            } else {
                "Provider pricing"
            },
            cx,
        ))
        .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "让你的服务商整体按一家分发商的目录价计费。生效顺序：单个模型的服务商设置 > 服务商计价 > 模型默认价格 > 官方价 > 参考价。"
        } else {
            "Make a provider follow one distributor's catalog prices. Precedence: per-provider model > provider pricing > model default > official > reference."
        }))
        .child(card(cx).child(rows))
}
