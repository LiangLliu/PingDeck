//! Runtime environment section for Settings › About.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::sandbox::SandboxKind;
use pingdeck_desktop::core::app::{AppInfoDto, SandboxSelftest};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::i18n::{t, t_args};
use pingdeck_desktop::theme::ResolvedPalette;

use crate::ui::{
    StatusShape, Tone, UiIcon, alert_banner, btn_ghost, btn_secondary, h_row, icon_xs, palette,
    settings_section_flush, status_indicator, to_hsla, v_col,
};

use super::{AboutSettingsView, AboutSnapshot};

pub(super) fn render_environment_section(
    view: &AboutSettingsView,
    lang: Language,
    zh: bool,
    cx: &mut Context<AboutSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let recheck = btn_ghost(
        "about-env-recheck",
        t(lang, "set.agent.sandbox.test").to_owned(),
    )
    .icon(icon_xs(UiIcon::Refresh))
    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
    .into_any_element();

    let body = if view.loading && view.snapshot.is_none() {
        h_row()
            .p(px(16.0))
            .gap(px(8.0))
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(status_indicator(StatusShape::Run, cx))
            .child(if zh {
                "正在检测运行环境…"
            } else {
                "Checking environment…"
            })
    } else if let Some(err) = &view.load_error
        && view.snapshot.is_none()
    {
        let msg = t_args(lang, "set.about.failed", &[("error", err.as_str())]);
        h_row()
            .justify_between()
            .gap(px(12.0))
            .p(px(16.0))
            .child(
                h_row()
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.0))
                    .child(status_indicator(StatusShape::Bad, cx))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(to_hsla(p.bad))
                            .child(SharedString::from(msg)),
                    ),
            )
            .child(
                btn_secondary("about-env-retry", t(lang, "set.data.retry").to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    } else if let Some(snap) = &view.snapshot {
        render_environment_rows(view, snap, lang, zh, cx)
    } else {
        v_col()
    };

    settings_section_flush(
        t(lang, "set.sec.about.build").to_owned(),
        Some(recheck),
        body,
        cx,
    )
}

fn render_environment_rows(
    view: &AboutSettingsView,
    snap: &AboutSnapshot,
    lang: Language,
    zh: bool,
    cx: &mut Context<AboutSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let info = &snap.info;
    let mut rows = v_col();

    if let Some(err) = &view.load_error {
        let msg = t_args(lang, "set.about.failed", &[("error", err.as_str())]);
        rows = rows.child(
            div().p(px(10.0)).child(alert_banner(
                Tone::Warning,
                msg,
                Some(
                    btn_ghost(
                        "about-env-stale-retry",
                        t(lang, "set.data.retry").to_owned(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
                    .into_any_element(),
                ),
                cx,
            )),
        );
    }

    // 1. Core & schema version
    let core_val = h_row()
        .gap(px(8.0))
        .flex_wrap()
        .justify_end()
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink))
                .child(SharedString::from(format!("v{}", info.core_version))),
        )
        .child(
            div()
                .px(px(6.0))
                .py(px(1.0))
                .rounded(px(4.0))
                .bg(to_hsla(p.sunken))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(format!(
                    "Schema #{}",
                    info.schema_version
                ))),
        );
    rows = rows.child(info_row(
        t(lang, "set.about.core"),
        None,
        core_val,
        true,
        cx,
    ));

    // 2. OS, architecture & renderer
    let sys_val = h_row()
        .gap(px(8.0))
        .flex_wrap()
        .justify_end()
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink))
                .child(SharedString::from(format_system_label(
                    &info.os, &info.arch,
                ))),
        )
        .child(div().text_xs().text_color(to_hsla(p.faint)).child("·"))
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(info.webview_version.clone())),
        );
    rows = rows.child(info_row(
        t(lang, "set.about.system"),
        None,
        sys_val,
        true,
        cx,
    ));

    // 3. Secret storage health (plaintext 0600 SQLite file — never claim encryption)
    let secret_sub = if info.secret_store.ok {
        Some(if cfg!(windows) {
            t(lang, "set.data.secrets.d.windows")
        } else {
            t(lang, "set.data.secrets.d")
        })
    } else {
        info.secret_store.error.as_deref()
    };
    let secret_val = if info.secret_store.ok {
        h_row()
            .gap(px(6.0))
            .child(status_indicator(StatusShape::Ok, cx))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(format!(
                        "{} · {}",
                        t(lang, "set.data.secrets.ok"),
                        info.secret_store.backend
                    ))),
            )
    } else {
        h_row()
            .gap(px(6.0))
            .child(status_indicator(StatusShape::Bad, cx))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(to_hsla(p.bad))
                    .child(t(lang, "set.data.secrets.down").to_owned()),
            )
    };
    rows = rows.child(info_row(
        t(lang, "set.data.secrets"),
        secret_sub,
        secret_val,
        true,
        cx,
    ));

    // 4. OS command sandbox status
    let (sb_shape, sb_text, sb_color) = format_sandbox(info, lang, &p);
    let sandbox_val = h_row()
        .gap(px(6.0))
        .child(status_indicator(sb_shape, cx))
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(sb_color)
                .child(SharedString::from(sb_text)),
        );
    rows = rows.child(info_row(
        t(lang, "set.agent.sandbox"),
        None,
        sandbox_val,
        true,
        cx,
    ));

    // 5. Runtime status summary (gateway, providers, recent warnings)
    let runtime_val = render_runtime_summary(snap, zh, cx);
    rows.child(info_row(
        if zh { "当前状态" } else { "Runtime status" },
        None,
        runtime_val,
        false,
        cx,
    ))
}

fn render_runtime_summary(snap: &AboutSnapshot, zh: bool, cx: &gpui_kit::App) -> gpui_kit::Div {
    let p = palette(cx);
    let gw_shape = if snap.gateway_running {
        StatusShape::Live
    } else {
        StatusShape::Idle
    };
    let gw_text = if snap.gateway_running {
        if snap.gateway_lan {
            if zh {
                format!("网关运行中 (:{}, 局域网)", snap.gateway_port)
            } else {
                format!("Gateway live (:{}, LAN)", snap.gateway_port)
            }
        } else if zh {
            format!("网关运行中 (:{})", snap.gateway_port)
        } else {
            format!("Gateway live (:{})", snap.gateway_port)
        }
    } else if zh {
        "网关已停止".to_owned()
    } else {
        "Gateway stopped".to_owned()
    };
    let prov_text = if snap.total_providers == snap.enabled_providers {
        if zh {
            format!("{} 个服务商已启用", snap.enabled_providers)
        } else {
            format!("{} provider(s) enabled", snap.enabled_providers)
        }
    } else if zh {
        format!(
            "{}/{} 个服务商已启用",
            snap.enabled_providers, snap.total_providers
        )
    } else {
        format!(
            "{}/{} providers enabled",
            snap.enabled_providers, snap.total_providers
        )
    };

    let mut row = h_row()
        .gap(px(8.0))
        .flex_wrap()
        .justify_end()
        .child(
            h_row()
                .gap(px(5.0))
                .child(status_indicator(gw_shape, cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(gw_text)),
                ),
        )
        .child(div().text_xs().text_color(to_hsla(p.faint)).child("·"))
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(prov_text)),
        );

    if snap.recent_problem_count > 0 {
        let warn_txt = if zh {
            format!("{} 条近期告警", snap.recent_problem_count)
        } else {
            format!("{} recent warning(s)", snap.recent_problem_count)
        };
        row = row
            .child(div().text_xs().text_color(to_hsla(p.faint)).child("·"))
            .child(
                h_row()
                    .gap(px(4.0))
                    .child(status_indicator(StatusShape::Warn, cx))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.warn))
                            .child(SharedString::from(warn_txt)),
                    ),
            );
    }
    row
}

fn info_row(
    label: &str,
    subtitle: Option<&str>,
    value: impl IntoElement,
    bottom_border: bool,
    cx: &gpui_kit::App,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut left = v_col().flex_1().min_w_0().gap(px(2.5)).child(
        div()
            .text_size(px(13.0))
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .text_color(to_hsla(p.ink))
            .child(SharedString::from(label.to_owned())),
    );
    if let Some(sub) = subtitle
        && !sub.is_empty()
    {
        left = left.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(sub.to_owned())),
        );
    }
    let mut row = h_row()
        .justify_between()
        .items_center()
        .gap(px(16.0))
        .px(px(16.0))
        .py(px(11.5))
        .child(left)
        .child(div().flex_shrink_0().child(value));
    if bottom_border {
        row = row
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)));
    }
    row
}

fn format_system_label(os: &str, arch: &str) -> String {
    let os_name = match os {
        "macos" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    };
    let arch_name = match (os, arch) {
        ("macos", "aarch64") => "Apple Silicon (aarch64)".to_owned(),
        ("macos", "x86_64") => "Intel (x86_64)".to_owned(),
        _ => arch.to_owned(),
    };
    format!("{os_name} · {arch_name}")
}

fn format_sandbox(
    info: &AppInfoDto,
    lang: Language,
    p: &ResolvedPalette,
) -> (StatusShape, String, gpui_kit::Hsla) {
    let tier_label = match info.sandbox.tier {
        SandboxKind::Seatbelt => Some(t(lang, "set.agent.sandbox.tier.seatbelt")),
        SandboxKind::Bubblewrap => Some(t(lang, "set.agent.sandbox.tier.bubblewrap")),
        SandboxKind::None => None,
    };
    match (tier_label, info.sandbox.selftest) {
        (Some(tier), SandboxSelftest::Passed) => (
            StatusShape::Ok,
            t_args(lang, "set.agent.sandbox.available", &[("tier", tier)]),
            to_hsla(p.ink_2),
        ),
        (Some(_), SandboxSelftest::Failed) => (
            StatusShape::Warn,
            t(lang, "set.agent.sandbox.unavailable").to_owned(),
            to_hsla(p.warn),
        ),
        _ => (
            StatusShape::Idle,
            t(lang, "set.agent.sandbox.notInstalled").to_owned(),
            to_hsla(p.muted),
        ),
    }
}
