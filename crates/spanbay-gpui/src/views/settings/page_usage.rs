//! Settings › Usage & Logging page: usage rollup retention, balance auto-refresh interval,
//! read-only gateway body capture summary (configured in Local Gateway › Access),
//! and read-only retention constants from `settings_registry_get`.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, px};
use spanbay_desktop::core::model::settings::SettingsSection as CoreSection;
use spanbay_desktop::core::model::settings::limits::CaptureMode;
use spanbay_desktop::core::model::settings::registry::RegistryConstant;
use spanbay_desktop::state::{NavModule, SettingsSection};

use crate::ui::{
    Tone, UiIcon, alert_banner, badge, btn_ghost, btn_secondary, choice_chip, h_row, icon_xs,
    kv_line, number_field, setting_row, settings_section, settings_section_padded, v_col,
};

use super::SettingsView;
use super::state_inputs::format_bytes;

pub fn render_usage(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let prov = &view.settings.ai.providers;
    let gw = &view.settings.ai.gateway;

    let (capture_label, capture_tone) = match gw.capture_mode {
        CaptureMode::Off => (crate::ui::tr(cx).settings().notify_off(), Tone::Neutral),
        CaptureMode::ErrorsOnly => (
            crate::ui::tr(cx).settings().page_usage_errors_only(),
            Tone::Accent,
        ),
        CaptureMode::All => (
            crate::ui::tr(cx).settings().page_usage_all_requests(),
            Tone::Warning,
        ),
    };

    let reg_num = |key: &str, fallback: i64| -> i64 {
        match view.registry.constants.get(key) {
            Some(RegistryConstant::Number(n)) => *n,
            _ => fallback,
        }
    };
    let log_max_rows = reg_num("log.max_rows", 100_000);
    let log_max_days = reg_num("log.max_days", 90);
    let cap_max_days = reg_num("capture.max_days", 7);
    let cap_max_mb = reg_num("capture.max_mb", 500);

    let retention_sec = settings_section(
        crate::ui::tr(cx).settings().page_usage_usage_retention_balance_refresh(),
        Some(
            h_row()
                .gap(px(6.0))
                .child(
                    btn_ghost("usage-reset-sec", crate::ui::tr(cx).settings().page_defaults_reset())
                        .icon(icon_xs(UiIcon::RotateCcw))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reset_keys(
                                CoreSection::Ai,
                                Some(
                                    [
                                        "providers.usage_retention_days",
                                        "providers.balance_refresh_min",
                                    ]
                                    .into_iter()
                                    .map(str::to_owned)
                                    .collect(),
                                ),
                                cx,
                            );
                        })),
                )
                .child(
                    btn_secondary("usage-jump-mod", crate::ui::tr(cx).settings().page_usage_open_usage())
                        .icon(icon_xs(UiIcon::ExternalLink))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.jump_to_module(NavModule::Usage, cx);
                        })),
                )
                .into_any_element(),
        ),
        v_col()
            .w_full()
            .child(setting_row(
                crate::ui::tr(cx).settings().page_usage_usage_record_retention(),
                None,
                Some(SharedString::from(match view.usage_retention_preview {
                    Some((rows, bytes)) => crate::ui::tr(cx)
                        .settings()
                        .usage_retention_desc_preview(format_bytes(bytes), rows),
                    None => crate::ui::tr(cx).settings().usage_retention_desc().to_owned(),
                })),
                h_row()
                    .gap(px(6.0))
                    .children([30u16, 90, 180, 365].into_iter().map(|days| {
                        choice_chip(
                            SharedString::from(format!("usage-days-{days}")),
                            crate::ui::tr(cx).settings().page_data_d(days),
                            prov.usage_retention_days == days,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.patch_section(
                                CoreSection::Ai,
                                serde_json::json!({ "providers": { "usage_retention_days": days } }),
                                cx,
                            );
                        }))
                    }))
                    .child(number_field(
                        &view.inputs.prov_usage_days,
                        Some(crate::ui::tr(cx).settings().page_usage_d()),
                        72.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("usage-save-days", crate::ui::tr(cx).common().kv_bulk_apply()).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.prov_usage_days.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    30,
                                    3_650,
                                    CoreSection::Ai,
                                    |n| serde_json::json!({ "providers": { "usage_retention_days": n } }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx).settings().page_usage_balance_auto_refresh_interval(),
                None,
                Some(SharedString::from(crate::ui::tr(cx).settings().page_usage_0_refreshes_only_on_demand())),
                h_row()
                    .gap(px(6.0))
                    .children([0u16, 15, 60, 360].into_iter().map(|mins| {
                        let label = if mins == 0 {
                            crate::ui::tr(cx).settings().page_usage_manual().to_owned()
                        } else { crate::ui::tr(cx).settings().page_usage_m(mins) };
                        choice_chip(
                            SharedString::from(format!("bal-min-{mins}")),
                            label,
                            prov.balance_refresh_min == mins,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.patch_section(
                                CoreSection::Ai,
                                serde_json::json!({ "providers": { "balance_refresh_min": mins } }),
                                cx,
                            );
                        }))
                    }))
                    .child(number_field(
                        &view.inputs.prov_balance_min,
                        Some(crate::ui::tr(cx).settings().page_usage_m_2()),
                        68.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("usage-save-bal", crate::ui::tr(cx).common().kv_bulk_apply()).on_click(
                            cx.listener(|this, _, _, cx| {
                                let raw = this.inputs.prov_balance_min.read(cx).value().trim().to_owned();
                                let Ok(n) = raw.parse::<u16>() else {
                                    this.set_flash(
                                        Tone::Danger,
                                        crate::ui::tr(cx).settings().page_usage_enter_0_manual_or_an(),
                                        cx,
                                    );
                                    return;
                                };
                                if n != 0 && !(5..=1440).contains(&n) {
                                    this.set_flash(
                                        Tone::Danger,
                                        crate::ui::tr(cx).settings().page_usage_refresh_interval_must_be_0(),
                                        cx,
                                    );
                                    return;
                                }
                                this.patch_section(
                                    CoreSection::Ai,
                                    serde_json::json!({ "providers": { "balance_refresh_min": n } }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            )),
        cx,
    );

    let mut capture_sec = v_col().w_full().gap(px(10.0)).child(settings_section(
        crate::ui::tr(cx)
            .settings()
            .page_usage_gateway_payload_capture(),
        None,
        setting_row(
            crate::ui::tr(cx).settings().page_usage_capture_mode(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_usage_store_request_response_payloads_locally(),
            )),
            h_row()
                .gap(px(8.0))
                .child(badge(capture_label, capture_tone, cx))
                .child(
                    btn_secondary(
                        "usage-jump-gateway",
                        crate::ui::tr(cx)
                            .settings()
                            .page_usage_configure_in_gateway(),
                    )
                    .icon(icon_xs(UiIcon::ExternalLink))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.jump_to_module(NavModule::Gateway, cx);
                    })),
                ),
            cx,
        ),
        cx,
    ));

    if gw.capture_mode == CaptureMode::All {
        capture_sec = capture_sec.child(alert_banner(
            Tone::Warning,
            crate::ui::tr(cx)
                .settings()
                .page_usage_all_requests_stores_full_prompts(cap_max_days, cap_max_mb),
            Some(
                btn_secondary(
                    "usage-go-data",
                    crate::ui::tr(cx).settings().page_usage_open_data_sync(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.set_section(SettingsSection::Data, cx);
                }))
                .into_any_element(),
            ),
            cx,
        ));
    }

    let constants_sec = settings_section_padded(
        crate::ui::tr(cx)
            .settings()
            .page_usage_fixed_log_capture_bounds_read(),
        None,
        v_col()
            .w_full()
            .gap(px(8.0))
            .child(kv_line(
                crate::ui::tr(cx)
                    .settings()
                    .page_usage_gateway_log_max_rows(),
                format!("{log_max_rows}"),
                220.0,
                cx,
            ))
            .child(kv_line(
                crate::ui::tr(cx)
                    .settings()
                    .page_usage_gateway_log_max_age(),
                crate::ui::tr(cx).settings().page_usage_days(log_max_days),
                220.0,
                cx,
            ))
            .child(kv_line(
                crate::ui::tr(cx)
                    .settings()
                    .page_usage_captured_body_max_age(),
                crate::ui::tr(cx).settings().page_usage_days_2(cap_max_days),
                220.0,
                cx,
            ))
            .child(kv_line(
                crate::ui::tr(cx)
                    .settings()
                    .page_usage_captured_body_storage_cap(),
                format!("{cap_max_mb} MiB"),
                220.0,
                cx,
            )),
        cx,
    );

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(retention_sec)
        .child(capture_sec)
        .child(constants_sec)
}
