//! Settings › Usage & Logging page: usage rollup retention, balance auto-refresh interval,
//! gateway body capture mode, and read-only retention constants from `settings_registry_get`.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;
use pingdeck_desktop::core::model::settings::limits::CaptureMode;
use pingdeck_desktop::core::model::settings::registry::RegistryConstant;
use pingdeck_desktop::state::{NavModule, SettingsSection};

use crate::ui::{
    Tone, UiIcon, alert_banner, btn_ghost, btn_secondary, choice_chip, h_row, icon_xs, kv_line,
    number_field, section_header, setting_row, tab_bar_segmented, tab_item, v_col,
};

use super::SettingsView;
use super::state_inputs::format_bytes;

pub fn render_usage(
    view: &SettingsView,
    zh: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let prov = &view.settings.ai.providers;
    let gw = &view.settings.ai.gateway;

    let capture_idx = match gw.capture_mode {
        CaptureMode::Off => 0,
        CaptureMode::ErrorsOnly => 1,
        CaptureMode::All => 2,
    };
    let ent_cap = cx.entity().clone();

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

    let retention_sec = v_col()
        .w_full()
        .child(section_header(
            if zh { "用量汇总与余额刷新" } else { "Usage Retention & Balance Refresh" },
            Some(
                h_row()
                    .gap(px(6.0))
                    .child(
                        btn_ghost("usage-reset-sec", if zh { "恢复默认" } else { "Reset" })
                            .icon(icon_xs(UiIcon::RotateCcw))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reset_keys(
                                    CoreSection::Ai,
                                    Some(
                                        [
                                            "providers.usage_retention_days",
                                            "providers.balance_refresh_min",
                                            "gateway.capture_mode",
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
                        btn_secondary("usage-jump-mod", if zh { "查看用量与日志" } else { "Open Usage" })
                            .icon(icon_xs(UiIcon::ExternalLink))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.jump_to_module(NavModule::Usage, cx);
                            })),
                    )
                    .into_any_element(),
            ),
            cx,
        ))
        .child(setting_row(
            if zh { "单次调用用量保留天数" } else { "Usage record retention" },
            None,
            Some(SharedString::from(match view.usage_retention_preview {
                Some((rows, bytes)) if zh => format!(
                    "本地保存每次 AI 调用 Token 与费用的天数（30–3,650 天，默认 365 天）。当前用量表占 {}，按此天数待清理 {} 条。",
                    format_bytes(bytes),
                    rows
                ),
                Some((rows, bytes)) => format!(
                    "Days of per-call token/cost rows stored locally (30–3,650d). Table uses {}; {rows} row(s) older than this window.",
                    format_bytes(bytes)
                ),
                None if zh => "本地保存每次 AI 调用 Token 与费用的天数（30–3,650 天，默认 365 天）。".into(),
                None => "Days of per-call token and cost rows stored locally (30–3,650 days, default 365).".into(),
            })),
            h_row()
                .gap(px(6.0))
                .children([30u16, 90, 180, 365].into_iter().map(|days| {
                    choice_chip(
                        SharedString::from(format!("usage-days-{days}")),
                        if zh {
                            format!("{days} 天")
                        } else {
                            format!("{days}d")
                        },
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
                    Some(if zh { "天" } else { "d" }),
                    72.0,
                    cx,
                ))
                .child(
                    btn_secondary("usage-save-days", if zh { "应用" } else { "Apply" }).on_click(
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
            if zh { "服务商余额自动刷新间隔" } else { "Balance auto-refresh interval" },
            None,
            Some(SharedString::from(if zh {
                "0 表示仅手动点击刷新；开启自动刷新可设 5–1,440 分钟。"
            } else {
                "0 refreshes only on demand; set 5–1,440 minutes for automatic background balance polls."
            })),
            h_row()
                .gap(px(6.0))
                .children([0u16, 15, 60, 360].into_iter().map(|mins| {
                    let label = if mins == 0 {
                        if zh { "手动".to_owned() } else { "Manual".to_owned() }
                    } else if zh {
                        format!("{mins} 分")
                    } else {
                        format!("{mins}m")
                    };
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
                    Some(if zh { "分" } else { "m" }),
                    68.0,
                    cx,
                ))
                .child(
                    btn_secondary("usage-save-bal", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let raw = this.inputs.prov_balance_min.read(cx).value().trim().to_owned();
                            let zh = this.is_zh();
                            let Ok(n) = raw.parse::<u16>() else {
                                this.set_flash(
                                    Tone::Danger,
                                    if zh {
                                        "请输入 0（手动）或 5–1440 之间的整数"
                                    } else {
                                        "Enter 0 (manual) or an integer between 5 and 1440"
                                    },
                                    cx,
                                );
                                return;
                            };
                            if n != 0 && !(5..=1440).contains(&n) {
                                this.set_flash(
                                    Tone::Danger,
                                    if zh {
                                        "刷新间隔必须是 0（关闭）或 5–1440 分钟"
                                    } else {
                                        "Refresh interval must be 0 (manual) or 5–1440 minutes"
                                    },
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
        ));

    let mut capture_sec = v_col()
        .w_full()
        .child(section_header(
            if zh { "网关请求正文捕获" } else { "Gateway Payload Capture" },
            None,
            cx,
        ))
        .child(setting_row(
            if zh { "捕获模式" } else { "Capture mode" },
            None,
            Some(SharedString::from(if zh {
                "用于排查外部客户端调用网关时的提示词与原始错误报文；所有捕获数据仅存本地且自动滚动清理。"
            } else {
                "Store request/response payloads locally for debugging gateway traffic; automatically bounded and pruned."
            })),
            tab_bar_segmented("usage-capture-seg")
                .selected_index(capture_idx)
                .child(tab_item(if zh { "关闭" } else { "Off" }))
                .child(tab_item(if zh { "仅错误" } else { "Errors only" }))
                .child(tab_item(if zh { "全部捕获" } else { "All requests" }))
                .on_click(move |idx, _, cx| {
                    let mode = match *idx {
                        1 => "errors_only",
                        2 => "all",
                        _ => "off",
                    };
                    ent_cap.update(cx, |this, cx| {
                        this.patch_section(
                            CoreSection::Ai,
                            serde_json::json!({ "gateway": { "capture_mode": mode } }),
                            cx,
                        );
                    });
                }),
            cx,
        ));

    if gw.capture_mode == CaptureMode::All {
        capture_sec = capture_sec.child(
            div().pt(px(8.0)).child(alert_banner(
                Tone::Warning,
                if zh {
                    format!("「全部捕获」会把完整对话提示词与响应存入本地数据库（受 {cap_max_days} 天 / {cap_max_mb} MiB 上限约束），可随时在「数据与同步」页一键清空。")
                } else {
                    format!("\"All requests\" stores full prompts and responses locally (capped at {cap_max_days} days / {cap_max_mb} MiB); clear anytime in Data & Sync.")
                },
                Some(
                    btn_secondary("usage-go-data", if zh { "前往数据与同步 →" } else { "Open Data & Sync →" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_section(SettingsSection::Data, cx);
                        }))
                        .into_any_element(),
                ),
                cx,
            )),
        );
    }

    let constants_sec = v_col()
        .w_full()
        .gap(px(6.0))
        .child(section_header(
            if zh {
                "固定存储与日志上限（只读）"
            } else {
                "Fixed Log & Capture Bounds (Read-Only)"
            },
            None,
            cx,
        ))
        .child(kv_line(
            if zh {
                "网关日志最大行数"
            } else {
                "Gateway log max rows"
            },
            format!("{log_max_rows}"),
            220.0,
            cx,
        ))
        .child(kv_line(
            if zh {
                "网关日志保留天数"
            } else {
                "Gateway log max age"
            },
            if zh {
                format!("{log_max_days} 天")
            } else {
                format!("{log_max_days} days")
            },
            220.0,
            cx,
        ))
        .child(kv_line(
            if zh {
                "捕获正文保留天数"
            } else {
                "Captured body max age"
            },
            if zh {
                format!("{cap_max_days} 天")
            } else {
                format!("{cap_max_days} days")
            },
            220.0,
            cx,
        ))
        .child(kv_line(
            if zh {
                "捕获正文总容量上限"
            } else {
                "Captured body storage cap"
            },
            format!("{cap_max_mb} MiB"),
            220.0,
            cx,
        ));

    v_col()
        .w_full()
        .gap(px(22.0))
        .child(retention_sec)
        .child(capture_sec)
        .child(constants_sec)
}
