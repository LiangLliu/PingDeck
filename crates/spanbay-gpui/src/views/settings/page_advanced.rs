//! Settings › Advanced page: JS script sandbox limits, console/test result budgets,
//! and Danger Zone Reset-All with confirmation modal + 60-second undo.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::model::settings::SettingsSection as CoreSection;

use crate::ui::{
    Tone, UiIcon, btn_danger, btn_ghost, btn_secondary, h_row, icon_xs, number_field, palette,
    setting_row, settings_section, to_hsla, top_accent_card, v_col,
};

use super::SettingsView;
use super::state_inputs::SettingsModal;

pub fn render_advanced(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let p = palette(cx);

    let script_sec = settings_section(
        crate::ui::tr(cx)
            .settings()
            .page_advanced_script_sandbox_limits(),
        Some(
            btn_ghost(
                "adv-reset-all-sec",
                crate::ui::tr(cx).settings().page_defaults_reset(),
            )
            .icon(icon_xs(UiIcon::RotateCcw))
            .on_click(cx.listener(|this, _, _, cx| {
                this.reset_section(CoreSection::Advanced, cx);
            }))
            .into_any_element(),
        ),
        v_col()
            .w_full()
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_advanced_script_loop_iteration_limit(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_advanced_stops_runaway_loops_in_pre(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.adv_loop_limit, None, 116.0, cx))
                    .child(
                        btn_secondary("adv-save-loop", crate::ui::tr(cx).common().kv_bulk_apply())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let v = this.inputs.adv_loop_limit.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    1_000,
                                    100_000_000,
                                    CoreSection::Advanced,
                                    |n| serde_json::json!({ "script_loop_iteration_limit": n }),
                                    cx,
                                );
                            })),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_advanced_script_recursion_limit(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_advanced_maximum_call_stack_depth_permitted(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.adv_recursion_limit,
                        None,
                        88.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("adv-save-rec", crate::ui::tr(cx).common().kv_bulk_apply())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let v =
                                    this.inputs.adv_recursion_limit.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    16,
                                    4_096,
                                    CoreSection::Advanced,
                                    |n| serde_json::json!({ "script_recursion_limit": n }),
                                    cx,
                                );
                            })),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_advanced_max_pm_sendrequest_calls_per(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_advanced_maximum_outbound_pm_sendrequest_calls(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.adv_send_requests,
                        Some(crate::ui::tr(cx).settings().page_advanced_x()),
                        84.0,
                        cx,
                    ))
                    .child(
                        btn_secondary(
                            "adv-save-sendreq",
                            crate::ui::tr(cx).common().kv_bulk_apply(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let v = this.inputs.adv_send_requests.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                0,
                                1_000,
                                CoreSection::Advanced,
                                |n| serde_json::json!({ "script_max_send_requests": n }),
                                cx,
                            );
                        })),
                    ),
                cx,
            )),
        cx,
    );

    let console_sec = settings_section(
        crate::ui::tr(cx)
            .settings()
            .page_advanced_console_test_output_budgets(),
        None,
        v_col()
            .w_full()
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_advanced_max_console_entries_per_run(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_advanced_console_entries_retained_per_request(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.adv_console_entries,
                        Some(crate::ui::tr(cx).settings().page_advanced_rows()),
                        96.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("adv-save-cent", crate::ui::tr(cx).common().kv_bulk_apply())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let v =
                                    this.inputs.adv_console_entries.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    10,
                                    100_000,
                                    CoreSection::Advanced,
                                    |n| serde_json::json!({ "console_max_entries": n }),
                                    cx,
                                );
                            })),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_advanced_max_console_message_size(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_advanced_maximum_bytes_kept_per_console(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.adv_console_msg_kib,
                        Some("KiB"),
                        88.0,
                        cx,
                    ))
                    .child(
                        btn_secondary(
                            "adv-save-cbytes",
                            crate::ui::tr(cx).common().kv_bulk_apply(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let v = this.inputs.adv_console_msg_kib.read(cx).value().to_string();
                            this.commit_kib(
                                &v,
                                1,
                                4_096,
                                CoreSection::Advanced,
                                "console_max_message_bytes",
                                cx,
                            );
                        })),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_advanced_max_pm_test_results_per(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_advanced_maximum_pm_test_assertion_results(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.adv_test_results,
                        Some(crate::ui::tr(cx).settings().page_advanced_rows()),
                        96.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("adv-save-tests", crate::ui::tr(cx).common().kv_bulk_apply())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let v = this.inputs.adv_test_results.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    10,
                                    100_000,
                                    CoreSection::Advanced,
                                    |n| serde_json::json!({ "max_test_results": n }),
                                    cx,
                                );
                            })),
                    ),
                cx,
            )),
        cx,
    );

    let danger_zone = v_col()
        .w_full()
        .gap(px(7.0))
        .child(
            div()
                .px(px(4.0))
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.bad))
                .child(crate::ui::tr(cx).settings().page_advanced_danger_zone()),
        )
        .child(
            top_accent_card(Tone::Danger, cx).rounded(px(10.0)).child(
                h_row()
                    .w_full()
                    .p(px(16.0))
                    .justify_between()
                    .gap(px(16.0))
                    .child(
                        v_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .child(
                                        crate::ui::tr(cx)
                                            .settings()
                                            .page_advanced_reset_all_settings(),
                                    ),
                            )
                            .child(
                                div().text_xs().text_color(to_hsla(p.muted)).child(
                                    crate::ui::tr(cx)
                                        .settings()
                                        .page_advanced_restores_every_setting_to_factory(),
                                ),
                            ),
                    )
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .children(view.can_undo_reset_all.then(|| {
                                btn_secondary(
                                    "settings-undo-reset-all",
                                    crate::ui::tr(cx).settings().page_advanced_undo_last_reset(),
                                )
                                .icon(icon_xs(UiIcon::RotateCcw))
                                .on_click(cx.listener(|this, _, _, cx| this.undo_last_reset(cx)))
                            }))
                            .child(
                                btn_danger(
                                    "settings-reset-all",
                                    crate::ui::tr(cx).settings().page_advanced_reset_all(),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.modal = Some(SettingsModal::ConfirmResetAll);
                                        cx.notify();
                                    },
                                )),
                            ),
                    ),
            ),
        );

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(script_sec)
        .child(console_sec)
        .child(danger_zone)
}
