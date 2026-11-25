//! Settings › General page: interface language, startup behavior, and background notifications.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::settings::{
    Language, NotifyWhenAway, SettingsSection as CoreSection,
};

use crate::ui::{
    Disableable as _, UiIcon, btn_ghost, icon_xs, section_header, setting_row, tab_bar_segmented,
    tab_item, toggle_switch, v_col,
};

use super::SettingsView;

pub fn render_general(
    view: &SettingsView,
    zh: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let general = &view.settings.general;
    let lang_idx = usize::from(general.language == Language::En);
    let notif_idx = match general.notify_when_away {
        NotifyWhenAway::Off => 0,
        NotifyWhenAway::Gates => 1,
        NotifyWhenAway::GatesAndFinished => 2,
    };

    let ent_lang = cx.entity().clone();
    let ent_notif = cx.entity().clone();

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(
            v_col()
                .w_full()
                .child(section_header(
                    if zh { "语言与启动" } else { "Language & Startup" },
                    Some(
                        btn_ghost(
                            "gen-reset-sec",
                            if zh { "恢复默认" } else { "Reset" },
                        )
                        .icon(icon_xs(UiIcon::RotateCcw))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reset_section(CoreSection::General, cx);
                        }))
                        .into_any_element(),
                    ),
                    cx,
                ))
                .child(setting_row(
                    if zh { "界面语言" } else { "Interface language" },
                    None,
                    Some(SharedString::from(if zh {
                        "立即切换界面文案语言；AI 模型的回复语言始终跟随你的输入语言。"
                    } else {
                        "Switch UI copy immediately; AI replies always follow the language you write in."
                    })),
                    tab_bar_segmented("gen-lang-seg")
                        .selected_index(lang_idx)
                        .child(tab_item("简体中文"))
                        .child(tab_item("English"))
                        .on_click(move |idx, _, cx| {
                            let val = if *idx == 1 { "en" } else { "zh-CN" };
                            ent_lang.update(cx, |this, cx| {
                                this.patch_section(
                                    CoreSection::General,
                                    serde_json::json!({ "language": val }),
                                    cx,
                                );
                            });
                        }),
                    cx,
                ))
                .child(setting_row(
                    if zh { "启动时恢复上次会话" } else { "Restore session on launch" },
                    None,
                    Some(SharedString::from(if zh {
                        "重新打开上次退出前的工作区与请求标签页。"
                    } else {
                        "Reopen your last active workspace and request tabs when PingDeck starts."
                    })),
                    toggle_switch("gen-restore", general.restore_session).on_change(cx.listener(
                        |this, checked: &bool, _, cx| {
                            this.patch_section(
                                CoreSection::General,
                                serde_json::json!({ "restore_session": *checked }),
                                cx,
                            );
                        },
                    )),
                    cx,
                ))
                .child(setting_row(
                    if zh { "启动时检查更新" } else { "Check for updates on launch" },
                    Some(SharedString::from(if zh { "默认关闭" } else { "Off by default" })),
                    Some(SharedString::from(if zh {
                        "开启后将在启动时向官方发布接口查询新版本（绝不发送遥测或本地数据）。"
                    } else {
                        "Query the release feed on startup for newer versions (zero telemetry or local data)."
                    })),
                    toggle_switch("gen-updates", general.check_updates_on_launch).on_change(
                        cx.listener(|this, checked: &bool, _, cx| {
                            this.patch_section(
                                CoreSection::General,
                                serde_json::json!({ "check_updates_on_launch": *checked }),
                                cx,
                            );
                        }),
                    ),
                    cx,
                )),
        )
        .child(
            v_col()
                .w_full()
                .child(section_header(
                    if zh { "后台提醒" } else { "Background Notifications" },
                    None,
                    cx,
                ))
                .child(setting_row(
                    if zh { "失焦时发送系统通知" } else { "Notify when window is unfocused" },
                    None,
                    Some(SharedString::from(if zh {
                        "仅在窗口不在前台时推送，通知正文绝不包含命令、路径或秘密内容。"
                    } else {
                        "Sent only while PingDeck is in the background; never includes commands, paths, or secrets."
                    })),
                    tab_bar_segmented("gen-notify-seg")
                        .selected_index(notif_idx)
                        .child(tab_item(if zh { "关闭" } else { "Off" }))
                        .child(tab_item(if zh { "仅需要你时" } else { "Gates only" }))
                        .child(tab_item(if zh { "需要你与结束" } else { "Gates & finished" }))
                        .on_click(move |idx, _, cx| {
                            let val = match *idx {
                                0 => "off",
                                2 => "gates_and_finished",
                                _ => "gates",
                            };
                            ent_notif.update(cx, |this, cx| {
                                this.patch_section(
                                    CoreSection::General,
                                    serde_json::json!({ "notify_when_away": val }),
                                    cx,
                                );
                            });
                        }),
                    cx,
                ))
                .child(setting_row(
                    if zh { "程序坞图标跳动提醒" } else { "Dock attention bounce" },
                    (general.notify_when_away == NotifyWhenAway::Off).then(|| {
                        SharedString::from(if zh {
                            "已被上方关闭"
                        } else {
                            "Muted by setting above"
                        })
                    }),
                    Some(SharedString::from(if zh {
                        "窗口不在前台且有待审批门控或长运行（≥ 30 秒）结束时跳动一次图标；角标数字始终显示待审批数。"
                    } else {
                        "Bounce the Dock icon once for pending approvals or finished runs (≥ 30s) while unfocused; badge count always tracks approvals."
                    })),
                    div().child(
                        toggle_switch("gen-dock-bounce", general.dock_bounce)
                            .disabled(general.notify_when_away == NotifyWhenAway::Off)
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.patch_section(
                                    CoreSection::General,
                                    serde_json::json!({ "dock_bounce": *checked }),
                                    cx,
                                );
                            })),
                    ),
                    cx,
                )),
        )
}
