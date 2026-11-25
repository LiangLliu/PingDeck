//! Sync status card, flash banner, and Provider Pricing basis rules for "Models & Pricing".

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::current_time_ms;
use pingdeck_desktop::core::model::model_catalog::{
    CatalogOrigin, ModelCatalogFamily, SYNC_INTERVAL_DAYS,
};

use crate::ui::{
    Anchor, StatusShape, Tone, UiIcon, alert_banner, badge, btn_ghost, btn_secondary,
    dismissible_banner, h_row, icon_xs, list_row, palette, popover, select_pill,
    settings_section_flush, settings_section_padded, status_indicator, to_hsla, toggle_switch,
    v_col,
};

use super::CatalogSettingsView;
use super::filters::render_provider_family_picker;
use super::format::{format_ago, format_next_due, format_sync_error, format_ymd};

pub fn render_flash_banner(
    tone: Tone,
    msg: &str,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    dismissible_banner(
        "cat-flash-dismiss",
        tone,
        msg.to_owned(),
        cx.listener(|this, _, _, cx| {
            this.flash_msg = None;
            cx.notify();
        }),
        cx,
    )
}

pub fn render_sync_section(
    view: &CatalogSettingsView,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let lang = crate::ui::active_language(cx);
    let s_tr = crate::ui::tr(cx).settings();
    let now_ms = current_time_ms();

    let (headline, subline, interval_days, next_line, err_line) = if let Some(st) = &view.status {
        let head = match (st.origin, st.last_success_ms) {
            (CatalogOrigin::ModelsDev, Some(ts)) => {
                let ago = format_ago(ts, now_ms, lang);
                s_tr.sync_section_models_dev_synced(ago)
            }
            _ => {
                let date = format_ymd(st.snapshot_at_ms);
                s_tr.sync_section_models_dev_built_in_snapshot(date)
            }
        };
        let sub = if let Some(pg) = &view.page {
            s_tr.sync_section_models_catalog_offers_custom_local(
                pg.total,
                st.models_dev_count,
                st.user_count,
            )
        } else {
            s_tr.sync_section_catalog_offers_custom_local_cache(st.models_dev_count, st.user_count)
        };
        let next = st.next_due_ms.map(|ts| format_next_due(ts, now_ms, lang));
        let err = view.sync_error.clone().or_else(|| {
            st.last_error.as_deref().map(|raw| {
                format_sync_error(
                    st.last_error_kind,
                    st.last_error_http_status,
                    Some(raw),
                    lang,
                )
            })
        });
        (head, sub, st.auto_interval_days, next, err)
    } else {
        (
            s_tr.sync_section_reading_model_catalog().into(),
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

    let interval_label = match interval_days {
        1 => s_tr.catalog_interval_daily(),
        30 => s_tr.catalog_interval_30d(),
        _ => s_tr.catalog_interval_7d(),
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
            let s_pop = crate::ui::tr(pop_cx).settings();
            let mut col = v_col().w(px(160.0)).gap(px(2.0));
            for days in SYNC_INTERVAL_DAYS {
                let lbl = match days {
                    1 => s_pop.catalog_interval_daily(),
                    7 => s_pop.catalog_interval_7d(),
                    _ => s_pop.catalog_interval_30d(),
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
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .child(crate::ui::tr(cx).settings().catalog_auto()),
                            )
                            .child(toggle_switch("cat-auto-sync", view.auto_sync).on_change(
                                cx.listener(|this, checked: &bool, _, cx| {
                                    this.toggle_auto_sync(*checked, cx);
                                }),
                            )),
                    )
                    .children(view.auto_sync.then_some(interval_popover))
                    .child(if view.syncing {
                        btn_secondary("cat-sync-cancel", crate::ui::tr(cx).common().cancel())
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_sync(cx)))
                    } else {
                        btn_secondary(
                            "cat-sync-now",
                            crate::ui::tr(cx).settings().catalog_sync_now(),
                        )
                        .icon(icon_xs(UiIcon::Refresh))
                        .on_click(cx.listener(|this, _, _, cx| this.start_sync(cx)))
                    }),
            ),
    );

    if let Some(report) = &view.last_sync_report {
        let msg = crate::ui::tr(cx)
            .settings()
            .sync_section_synced_added_updated_unchanged(
                report.added,
                report.updated,
                report.unchanged,
            );
        box_el = box_el.child(dismissible_banner(
            "cat-sync-rep-dismiss",
            Tone::Success,
            msg,
            cx.listener(|this, _, _, cx| {
                this.last_sync_report = None;
                cx.notify();
            }),
            cx,
        ));
    } else if let Some(err) = err_line {
        box_el = box_el.child(alert_banner(
            Tone::Warning,
            err,
            Some(
                btn_ghost("cat-sync-retry", crate::ui::tr(cx).common().retry())
                    .on_click(cx.listener(|this, _, _, cx| this.start_sync(cx)))
                    .into_any_element(),
            ),
            cx,
        ));
    }

    settings_section_padded(
        crate::ui::tr(cx).settings().sec_catalog_sync(),
        None,
        box_el,
        cx,
    )
}

pub fn render_provider_pricing_section(
    view: &CatalogSettingsView,
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
                crate::ui::tr(cx)
                    .settings()
                    .filters_follow_official_prices()
                    .into()
            });

        let coverage = if let Some(fam) = pv
            .catalog_family_name
            .as_deref()
            .or(pv.catalog_family.as_deref())
        {
            crate::ui::tr(cx)
                .settings()
                .sync_section_of_models_priced_by_rest(pv.matched, pv.models, fam)
        } else {
            crate::ui::tr(cx)
                .settings()
                .sync_section_enabled_model_s_official_prices(pv.models)
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
                                    crate::ui::tr(cx)
                                        .settings()
                                        .sync_section_override_s(pv.overrides),
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
        .w_full()
        .gap(px(8.0))
        .child(settings_section_flush(
            crate::ui::tr(cx).settings().sec_catalog_providers(),
            None,
            rows,
            cx,
        ))
        .child(
            div()
                .px(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(
                    crate::ui::tr(cx)
                        .settings()
                        .sync_section_make_a_provider_follow_one(),
                ),
        )
}
