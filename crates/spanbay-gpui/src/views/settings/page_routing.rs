//! Settings › Routing & Failover page: shared AI retries, timeouts, circuit breaker thresholds,
//! provider probing settings, read-only failover status codes, and link to Local Gateway runtime.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::model::settings::SettingsSection as CoreSection;
use spanbay_desktop::core::model::settings::registry::RegistryConstant;
use spanbay_desktop::state::NavModule;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_ghost, btn_secondary, h_row, icon_xs, number_field,
    palette, setting_row, settings_section, status_indicator, text_input, to_hsla, v_col,
};

use super::SettingsView;

pub fn render_routing(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let p = palette(cx);
    let gw = &view.settings.ai.gateway;
    let gw_running = view.gateway_status.as_ref().is_some_and(|s| s.running);
    let gw_base = view
        .gateway_status
        .as_ref()
        .filter(|s| s.running && !s.base_url.is_empty())
        .map(|s| s.base_url.clone())
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", gw.port));

    let failover_codes = match view.registry.constants.get("failover.status_codes") {
        Some(RegistryConstant::List(codes)) => codes.join(" / "),
        _ => "401 / 402 / 403 / 429".to_owned(),
    };

    let runtime_card = div()
        .w_full()
        .p(px(16.0))
        .rounded(px(10.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line_strong.composite_over(p.pane)))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(12.0))
                .child(
                    h_row()
                        .gap(px(10.0))
                        .flex_1()
                        .min_w_0()
                        .child(status_indicator(
                            if gw_running {
                                StatusShape::Live
                            } else {
                                StatusShape::Idle
                            },
                            cx,
                        ))
                        .child(
                            v_col()
                                .min_w_0()
                                .gap(px(2.0))
                                .child(
                                    h_row()
                                        .gap(px(8.0))
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                .text_color(to_hsla(p.ink))
                                                .child(
                                                    crate::ui::tr(cx)
                                                        .settings()
                                                        .page_routing_local_ai_gateway(),
                                                ),
                                        )
                                        .child(badge(
                                            if gw_running {
                                                crate::ui::tr(cx)
                                                    .settings()
                                                    .tools_mcp_state_running()
                                            } else {
                                                crate::ui::tr(cx)
                                                    .settings()
                                                    .tools_mcp_state_stopped()
                                            },
                                            if gw_running {
                                                Tone::Success
                                            } else {
                                                Tone::Neutral
                                            },
                                            cx,
                                        )),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(to_hsla(p.muted))
                                        .truncate()
                                        .child(SharedString::from(
                                            crate::ui::tr(cx)
                                                .settings()
                                                .page_routing_endpoint_manage_start_stop_lan(
                                                    gw_base,
                                                ),
                                        )),
                                ),
                        ),
                )
                .child(
                    btn_secondary(
                        "route-jump-gateway",
                        crate::ui::tr(cx).settings().page_routing_open_gateway(),
                    )
                    .icon(icon_xs(UiIcon::ExternalLink))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.jump_to_module(NavModule::Gateway, cx);
                    })),
                ),
        );

    let retry_sec = settings_section(
        crate::ui::tr(cx)
            .settings()
            .page_routing_gateway_retries_timeouts(),
        Some(
            btn_ghost(
                "route-reset-gw",
                crate::ui::tr(cx).settings().page_defaults_reset(),
            )
            .icon(icon_xs(UiIcon::RotateCcw))
            .on_click(cx.listener(|this, _, _, cx| {
                this.reset_keys(
                    CoreSection::Ai,
                    Some(
                        [
                            "gateway.max_retries",
                            "gateway.first_byte_timeout_s",
                            "gateway.idle_timeout_s",
                        ]
                        .map(str::to_owned)
                        .to_vec(),
                    ),
                    cx,
                );
            }))
            .into_any_element(),
        ),
        v_col()
            .w_full()
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_routing_max_upstream_retries_failover_hops(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_routing_retries_and_backup_key_failovers(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.gw_max_retries,
                        Some(crate::ui::tr(cx).settings().page_advanced_x()),
                        72.0,
                        cx,
                    ))
                    .child(
                        btn_secondary(
                            "gw-save-retries",
                            crate::ui::tr(cx).common().kv_bulk_apply(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let v = this.inputs.gw_max_retries.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                0,
                                10,
                                CoreSection::Ai,
                                |n| serde_json::json!({ "gateway": { "max_retries": n } }),
                                cx,
                            );
                        })),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_routing_first_byte_timeout(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_routing_seconds_to_wait_for_the(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.gw_first_byte_s,
                        Some("s"),
                        80.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("gw-save-fb", crate::ui::tr(cx).common().kv_bulk_apply())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let v = this.inputs.gw_first_byte_s.read(cx).value().to_string();
                                this.commit_u64(
                                &v,
                                1,
                                600,
                                CoreSection::Ai,
                                |n| serde_json::json!({ "gateway": { "first_byte_timeout_s": n } }),
                                cx,
                            );
                            })),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_routing_stream_idle_timeout(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_routing_seconds_an_open_stream_may(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.gw_idle_s, Some("s"), 80.0, cx))
                    .child(
                        btn_secondary("gw-save-idle", crate::ui::tr(cx).common().kv_bulk_apply())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let v = this.inputs.gw_idle_s.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    1,
                                    3_600,
                                    CoreSection::Ai,
                                    |n| serde_json::json!({ "gateway": { "idle_timeout_s": n } }),
                                    cx,
                                );
                            })),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_routing_failover_trigger_status_codes(),
                Some(SharedString::from(
                    crate::ui::tr(cx).settings().page_routing_read_only(),
                )),
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_routing_auth_quota_rate_limit_5xx(),
                )),
                badge(format!("{failover_codes} · 5xx"), Tone::Neutral, cx),
                cx,
            )),
        cx,
    );

    let circuit_sec = settings_section(
        crate::ui::tr(cx).settings().page_routing_circuit_breaker(),
        Some(
            btn_ghost(
                "route-reset-cb",
                crate::ui::tr(cx).settings().page_defaults_reset(),
            )
            .icon(icon_xs(UiIcon::RotateCcw))
            .on_click(cx.listener(|this, _, _, cx| {
                this.reset_keys(
                    CoreSection::Ai,
                    Some(
                        [
                            "gateway.circuit_failure_threshold",
                            "gateway.circuit_window_s",
                            "gateway.circuit_open_s",
                            "gateway.circuit_open_max_s",
                        ]
                        .map(str::to_owned)
                        .to_vec(),
                    ),
                    cx,
                );
            }))
            .into_any_element(),
        ),
        v_col()
            .w_full()
            .child(setting_row(
                crate::ui::tr(cx).settings().page_routing_failure_threshold(),
                None,
                Some(SharedString::from(crate::ui::tr(cx).settings().page_routing_consecutive_failures_inside_the_window())),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.gw_cb_threshold,
                        Some(crate::ui::tr(cx).settings().page_advanced_x()),
                        72.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("gw-save-cbt", crate::ui::tr(cx).common().kv_bulk_apply()).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.gw_cb_threshold.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    1,
                                    100,
                                    CoreSection::Ai,
                                    |n| serde_json::json!({ "gateway": { "circuit_failure_threshold": n } }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx).settings().page_routing_failure_counting_window(),
                None,
                Some(SharedString::from(crate::ui::tr(cx).settings().page_routing_rolling_window_in_which_consecutive())),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.gw_cb_window_s, Some("s"), 80.0, cx))
                    .child(
                        btn_secondary("gw-save-cbw", crate::ui::tr(cx).common().kv_bulk_apply()).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.gw_cb_window_s.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    1,
                                    3_600,
                                    CoreSection::Ai,
                                    |n| serde_json::json!({ "gateway": { "circuit_window_s": n } }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx).settings().page_routing_initial_open_cooldown(),
                None,
                Some(SharedString::from(crate::ui::tr(cx).settings().page_routing_cooldown_before_a_half_open())),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.gw_cb_open_s, Some("s"), 80.0, cx))
                    .child(
                        btn_secondary("gw-save-cbo", crate::ui::tr(cx).common().kv_bulk_apply()).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.gw_cb_open_s.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    5,
                                    3_600,
                                    CoreSection::Ai,
                                    |n| serde_json::json!({ "gateway": { "circuit_open_s": n } }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx).settings().page_routing_max_backoff_cooldown(),
                None,
                Some(SharedString::from(crate::ui::tr(cx).settings().page_routing_upper_cap_for_exponential_circuit())),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.gw_cb_open_max_s, Some("s"), 88.0, cx))
                    .child(
                        btn_secondary("gw-save-cbm", crate::ui::tr(cx).common().kv_bulk_apply()).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.gw_cb_open_max_s.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    5,
                                    86_400,
                                    CoreSection::Ai,
                                    |n| serde_json::json!({ "gateway": { "circuit_open_max_s": n } }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            )),
        cx,
    );

    let probe_sec = settings_section(
        crate::ui::tr(cx).settings().page_routing_provider_connectivity_probing(),
        Some(
            btn_ghost("route-reset-probe", crate::ui::tr(cx).settings().page_defaults_reset())
                .icon(icon_xs(UiIcon::RotateCcw))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.reset_keys(
                        CoreSection::Ai,
                        Some(
                            [
                                "providers.probe_concurrency",
                                "providers.probe_timeout_s",
                                "providers.default_test_prompt",
                            ]
                            .map(str::to_owned)
                            .to_vec(),
                        ),
                        cx,
                    );
                }))
                .into_any_element(),
        ),
        v_col()
            .w_full()
            .child(setting_row(
                crate::ui::tr(cx).settings().page_routing_probe_concurrency(),
                None,
                Some(SharedString::from(crate::ui::tr(cx).settings().page_routing_models_tested_in_parallel_during())),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.prov_probe_concurrency, None, 72.0, cx))
                    .child(
                        btn_secondary("prov-save-conc", crate::ui::tr(cx).common().kv_bulk_apply()).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.prov_probe_concurrency.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    1,
                                    16,
                                    CoreSection::Ai,
                                    |n| serde_json::json!({ "providers": { "probe_concurrency": n } }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx).settings().page_routing_probe_timeout(),
                None,
                Some(SharedString::from(crate::ui::tr(cx).settings().page_routing_timeout_for_a_single_model())),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.prov_probe_timeout_s, Some("s"), 80.0, cx))
                    .child(
                        btn_secondary("prov-save-pt", crate::ui::tr(cx).common().kv_bulk_apply()).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.prov_probe_timeout_s.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    5,
                                    300,
                                    CoreSection::Ai,
                                    |n| serde_json::json!({ "providers": { "probe_timeout_s": n } }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx).settings().page_routing_default_model_test_prompt(),
                None,
                Some(SharedString::from(crate::ui::tr(cx).settings().page_routing_short_prompt_sent_when_probing())),
                h_row()
                    .gap(px(6.0))
                    .child(div().w(px(250.0)).child(text_input(&view.inputs.prov_test_prompt)))
                    .child(
                        btn_secondary("prov-save-prompt", crate::ui::tr(cx).common().save()).on_click(
                            cx.listener(|this, _, _, cx| {
                                let prompt = this.inputs.prov_test_prompt.read(cx).value().to_string();
                                this.patch_section(
                                    CoreSection::Ai,
                                    serde_json::json!({ "providers": { "default_test_prompt": prompt } }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            )),
        cx,
    );

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(runtime_card)
        .child(retry_sec)
        .child(circuit_sec)
        .child(probe_sec)
}
