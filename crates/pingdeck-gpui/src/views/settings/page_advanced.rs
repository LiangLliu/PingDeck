//! Settings › Advanced page: JS script sandbox limits, console/test result budgets,
//! and Danger Zone Reset-All with confirmation modal + 60-second undo.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;

use crate::ui::{
    Tone, UiIcon, btn_danger, btn_ghost, btn_secondary, h_row, icon_xs, number_field, palette,
    setting_row, settings_section, to_hsla, top_accent_card, v_col,
};

use super::SettingsView;
use super::state_inputs::SettingsModal;

pub fn render_advanced(
    view: &SettingsView,
    zh: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);

    let script_sec = settings_section(
        if zh { "脚本沙箱安全限制" } else { "Script Sandbox Limits" },
        Some(
            btn_ghost("adv-reset-all-sec", if zh { "恢复默认" } else { "Reset" })
                .icon(icon_xs(UiIcon::RotateCcw))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.reset_section(CoreSection::Advanced, cx);
                }))
                .into_any_element(),
        ),
        v_col()
            .w_full()
            .child(setting_row(
                if zh { "脚本循环迭代上限" } else { "Script loop iteration limit" },
                None,
                Some(SharedString::from(if zh {
                    "防止前置/测试脚本死循环卡死执行器（1,000–100,000,000，默认 5,000,000）。"
                } else {
                    "Stops runaway loops in pre-request and test scripts (1,000–100,000,000, default 5,000,000)."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.adv_loop_limit, None, 116.0, cx))
                    .child(
                        btn_secondary("adv-save-loop", if zh { "应用" } else { "Apply" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.adv_loop_limit.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    1_000,
                                    100_000_000,
                                    CoreSection::Advanced,
                                    |n| serde_json::json!({ "script_loop_iteration_limit": n }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                if zh { "脚本最大递归深度" } else { "Script recursion limit" },
                None,
                Some(SharedString::from(if zh {
                    "单次脚本调用栈允许的最大深度（16–4,096，默认 256）。"
                } else {
                    "Maximum call-stack depth permitted in scripts (16–4,096, default 256)."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.adv_recursion_limit, None, 88.0, cx))
                    .child(
                        btn_secondary("adv-save-rec", if zh { "应用" } else { "Apply" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this
                                    .inputs
                                    .adv_recursion_limit
                                    .read(cx)
                                    .value()
                                    .to_string();
                                this.commit_u64(
                                    &v,
                                    16,
                                    4_096,
                                    CoreSection::Advanced,
                                    |n| serde_json::json!({ "script_recursion_limit": n }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                if zh { "单脚本 pm.sendRequest 上限" } else { "Max pm.sendRequest calls per script" },
                None,
                Some(SharedString::from(if zh {
                    "单个请求脚本最多发起的子 HTTP 请求次数（0–1,000，默认 50）。"
                } else {
                    "Maximum outbound pm.sendRequest calls allowed per script run (0–1,000, default 50)."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.adv_send_requests,
                        Some(if zh { "次" } else { "x" }),
                        84.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("adv-save-sendreq", if zh { "应用" } else { "Apply" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.adv_send_requests.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    0,
                                    1_000,
                                    CoreSection::Advanced,
                                    |n| serde_json::json!({ "script_max_send_requests": n }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            )),
        cx,
    );

    let console_sec = settings_section(
        if zh {
            "控制台与断言输出上限"
        } else {
            "Console & Test Output Budgets"
        },
        None,
        v_col()
            .w_full()
            .child(setting_row(
                if zh {
                    "单次运行控制台最大条数"
                } else {
                    "Max console entries per run"
                },
                None,
                Some(SharedString::from(if zh {
                    "单次请求执行保留的 console.log 条目上限（10–100,000，默认 1,000）。"
                } else {
                    "Console entries retained per request run (10–100,000, default 1,000)."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.adv_console_entries,
                        Some(if zh { "条" } else { "rows" }),
                        96.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("adv-save-cent", if zh { "应用" } else { "Apply" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.adv_console_entries.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    10,
                                    100_000,
                                    CoreSection::Advanced,
                                    |n| serde_json::json!({ "console_max_entries": n }),
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                if zh {
                    "单条控制台消息体积上限"
                } else {
                    "Max console message size"
                },
                None,
                Some(SharedString::from(if zh {
                    "单条 console 输出或脚本错误保留的最大字节数（1–4,096 KiB，默认 64 KiB）。"
                } else {
                    "Maximum bytes kept per console line or script error (1–4,096 KiB, default 64 KiB)."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.adv_console_msg_kib,
                        Some("KiB"),
                        88.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("adv-save-cbytes", if zh { "应用" } else { "Apply" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.adv_console_msg_kib.read(cx).value().to_string();
                                this.commit_kib(
                                    &v,
                                    1,
                                    4_096,
                                    CoreSection::Advanced,
                                    "console_max_message_bytes",
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                if zh {
                    "单次运行 pm.test 结果上限"
                } else {
                    "Max pm.test results per run"
                },
                None,
                Some(SharedString::from(if zh {
                    "单次请求运行记录的断言测试结果上限（10–100,000，默认 1,000）。"
                } else {
                    "Maximum pm.test assertion results kept per run (10–100,000, default 1,000)."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.adv_test_results,
                        Some(if zh { "条" } else { "rows" }),
                        96.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("adv-save-tests", if zh { "应用" } else { "Apply" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.adv_test_results.read(cx).value().to_string();
                                this.commit_u64(
                                    &v,
                                    10,
                                    100_000,
                                    CoreSection::Advanced,
                                    |n| serde_json::json!({ "max_test_results": n }),
                                    cx,
                                );
                            }),
                        ),
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
                .child(if zh { "危险操作" } else { "Danger Zone" }),
        )
        .child(
            top_accent_card(Tone::Danger, cx)
                .rounded(px(10.0))
                .child(
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
                                        .child(if zh { "重置全部设置" } else { "Reset all settings" }),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(to_hsla(p.muted))
                                        .child(if zh {
                                            "将所有设置恢复为出厂默认值（不删除工作区、历史、对话、密钥、保留策略与同步目录绑定；操作后 60 秒内支持撤销）。"
                                        } else {
                                            "Restores every setting to factory defaults (preserves workspaces, chats, secrets, retention policies, and sync folder binding; 60s undo available)."
                                        }),
                                ),
                        )
                        .child(
                            h_row()
                                .gap(px(8.0))
                                .children(view.can_undo_reset_all.then(|| {
                                    btn_secondary(
                                        "settings-undo-reset-all",
                                        if zh { "撤销上次重置" } else { "Undo last reset" },
                                    )
                                    .icon(icon_xs(UiIcon::RotateCcw))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.undo_last_reset(cx)),
                                    )
                                }))
                                .child(
                                    btn_danger(
                                        "settings-reset-all",
                                        if zh { "全部重置…" } else { "Reset All…" },
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.modal = Some(SettingsModal::ConfirmResetAll);
                                        cx.notify();
                                    })),
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
