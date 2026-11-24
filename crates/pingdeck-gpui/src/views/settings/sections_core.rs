//! Core Settings pages: General, Appearance (16 eye-care OKLCH palettes), Shortcuts,
//! Requests, Network, Mock, Data & Sync, and Advanced.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, px};
use pingdeck_desktop::core::model::settings::{
    Language, NotifyWhenAway, SettingsSection as CoreSection,
};
use pingdeck_desktop::state::SettingsSection;

use crate::ui::{
    Tone, badge, btn_danger, btn_primary, btn_secondary, card, field_row, h_row, kbd_chip,
    section_heading, toggle_switch, v_col,
};

use super::SettingsView;
use super::appearance::render_appearance;

pub fn render_core_section(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let zh = view.settings.general.language == Language::ZhCn;
    match view.active {
        SettingsSection::General => render_general(view, zh, cx),
        SettingsSection::Appearance => render_appearance(view, zh, cx),
        SettingsSection::Shortcuts => render_shortcuts(zh, cx),
        SettingsSection::Requests => render_requests(view, zh, cx),
        SettingsSection::Network => render_network(view, zh, cx),
        SettingsSection::Mock => render_mock(view, zh, cx),
        SettingsSection::Data => render_data(view, zh, cx),
        SettingsSection::Advanced => render_advanced(zh, cx),
        _ => v_col(),
    }
}

fn seg_btn(
    id: &'static str,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&mut SettingsView, &mut Context<SettingsView>) + 'static,
    cx: &mut Context<SettingsView>,
) -> impl IntoElement {
    let btn = if active {
        btn_primary(id, label)
    } else {
        btn_secondary(id, label)
    };
    btn.on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
}

fn render_general(view: &SettingsView, zh: bool, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let general = &view.settings.general;
    v_col()
        .gap(px(4.0))
        .child(section_heading(if zh { "语言" } else { "Language" }, cx))
        .child(field_row(
            if zh {
                "界面语言"
            } else {
                "Interface language"
            },
            Some(SharedString::from(if zh {
                "模型的回复语言跟随你书写的语言，不受此设置影响。"
            } else {
                "Model replies follow the language you write in."
            })),
            h_row()
                .gap(px(4.0))
                .child(seg_btn(
                    "lang-zh",
                    "简体中文",
                    general.language == Language::ZhCn,
                    |this, cx| {
                        this.patch_section(
                            CoreSection::General,
                            serde_json::json!({ "language": "zh-CN" }),
                            cx,
                        )
                    },
                    cx,
                ))
                .child(seg_btn(
                    "lang-en",
                    "English",
                    general.language == Language::En,
                    |this, cx| {
                        this.patch_section(
                            CoreSection::General,
                            serde_json::json!({ "language": "en" }),
                            cx,
                        )
                    },
                    cx,
                )),
            cx,
        ))
        .child(field_row(
            if zh {
                "恢复标签页"
            } else {
                "Restore tabs on startup"
            },
            Some(SharedString::from(if zh {
                "重新打开上次的请求标签页和工作区。"
            } else {
                "Reopen your last active workspace and request tabs."
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
        .child(field_row(
            if zh {
                "后台系统通知"
            } else {
                "System notifications when away"
            },
            Some(SharedString::from(if zh {
                "仅在窗口不在前台时发送，绝不写入命令或路径。"
            } else {
                "Sent only while PingDeck is unfocused; never includes commands or paths."
            })),
            h_row()
                .gap(px(4.0))
                .child(seg_btn(
                    "notif-off",
                    if zh { "关闭" } else { "Off" },
                    general.notify_when_away == NotifyWhenAway::Off,
                    |this, cx| {
                        this.patch_section(
                            CoreSection::General,
                            serde_json::json!({ "notify_when_away": "off" }),
                            cx,
                        )
                    },
                    cx,
                ))
                .child(seg_btn(
                    "notif-gates",
                    if zh { "需要你时" } else { "Gates" },
                    general.notify_when_away == NotifyWhenAway::Gates,
                    |this, cx| {
                        this.patch_section(
                            CoreSection::General,
                            serde_json::json!({ "notify_when_away": "gates" }),
                            cx,
                        )
                    },
                    cx,
                ))
                .child(seg_btn(
                    "notif-all",
                    if zh {
                        "需要你和运行结束"
                    } else {
                        "Gates & Finished"
                    },
                    general.notify_when_away == NotifyWhenAway::GatesAndFinished,
                    |this, cx| {
                        this.patch_section(
                            CoreSection::General,
                            serde_json::json!({ "notify_when_away": "gates_and_finished" }),
                            cx,
                        )
                    },
                    cx,
                )),
            cx,
        ))
        .child(field_row(
            if zh {
                "应用图标跳动"
            } else {
                "Dock attention bounce"
            },
            Some(SharedString::from(if zh {
                "窗口失焦时有待审批或长运行结束时跳动一次图标。"
            } else {
                "Bounce Dock icon when approval is waiting or a long run ends."
            })),
            toggle_switch("gen-dock-bounce", general.dock_bounce).on_change(cx.listener(
                |this, checked: &bool, _, cx| {
                    this.patch_section(
                        CoreSection::General,
                        serde_json::json!({ "dock_bounce": *checked }),
                        cx,
                    );
                },
            )),
            cx,
        ))
}

fn render_shortcuts(zh: bool, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let items = [
        (
            "⌘K",
            if zh {
                "搜索或跳转命令面板"
            } else {
                "Open Command Palette"
            },
        ),
        (
            "⌘B",
            if zh {
                "折叠 / 展开侧栏"
            } else {
                "Toggle Sidebar"
            },
        ),
        ("⌘,", if zh { "打开设置" } else { "Open Settings" }),
        (
            "⌘1 – ⌘9",
            if zh {
                "切换 9 大核心模块"
            } else {
                "Switch between 9 modules"
            },
        ),
        (
            "Esc",
            if zh {
                "关闭弹窗 / 命令面板"
            } else {
                "Close active overlay"
            },
        ),
        (
            "⌘Q",
            if zh {
                "优雅退出（释放端口与子进程）"
            } else {
                "Graceful quit"
            },
        ),
    ];
    let mut col = v_col().gap(px(4.0));
    for (combo, label) in items {
        col = col.child(field_row(label, None, kbd_chip(combo, cx), cx));
    }
    col
}

fn render_requests(view: &SettingsView, zh: bool, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let net = &view.settings.network;
    v_col()
        .gap(px(4.0))
        .child(field_row(
            if zh {
                "默认请求超时"
            } else {
                "Default timeout"
            },
            None,
            badge(format!("{} ms", net.timeout_ms), Tone::Neutral, cx),
            cx,
        ))
        .child(field_row(
            if zh {
                "自动跟随重定向"
            } else {
                "Follow redirects"
            },
            None,
            toggle_switch("req-follow", net.follow_redirects).on_change(cx.listener(
                |this, checked: &bool, _, cx| {
                    this.patch_section(
                        CoreSection::Network,
                        serde_json::json!({ "follow_redirects": *checked }),
                        cx,
                    );
                },
            )),
            cx,
        ))
        .child(field_row(
            if zh {
                "最大重定向次数"
            } else {
                "Max redirects"
            },
            None,
            badge(net.max_redirects.to_string(), Tone::Neutral, cx),
            cx,
        ))
        .child(field_row(
            if zh {
                "校验 TLS 证书"
            } else {
                "Verify TLS certificates"
            },
            None,
            toggle_switch("req-tls", net.verify_tls).on_change(cx.listener(
                |this, checked: &bool, _, cx| {
                    this.patch_section(
                        CoreSection::Network,
                        serde_json::json!({ "verify_tls": *checked }),
                        cx,
                    );
                },
            )),
            cx,
        ))
}

fn render_network(view: &SettingsView, zh: bool, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let net = &view.settings.network;
    v_col()
        .gap(px(4.0))
        .child(field_row(
            if zh {
                "自定义代理 URL"
            } else {
                "Custom proxy URL"
            },
            None,
            badge(
                if net.proxy_url.is_empty() {
                    "(system / none)".to_owned()
                } else {
                    net.proxy_url.clone()
                },
                Tone::Neutral,
                cx,
            ),
            cx,
        ))
        .child(field_row(
            if zh {
                "不使用代理的主机"
            } else {
                "No-proxy hosts"
            },
            None,
            badge(
                if net.no_proxy.is_empty() {
                    "(none)".to_owned()
                } else {
                    net.no_proxy.join(", ")
                },
                Tone::Neutral,
                cx,
            ),
            cx,
        ))
}

fn render_mock(view: &SettingsView, zh: bool, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let m = &view.settings.mock;
    v_col()
        .gap(px(4.0))
        .child(field_row(
            if zh {
                "Mock 内存日志上限"
            } else {
                "Mock log ring capacity"
            },
            None,
            badge(format!("{} rows", m.log_capacity), Tone::Neutral, cx),
            cx,
        ))
        .child(field_row(
            if zh {
                "默认开启 CORS"
            } else {
                "Enable CORS by default"
            },
            None,
            toggle_switch("mock-cors", m.cors_default).on_change(cx.listener(
                |this, checked: &bool, _, cx| {
                    this.patch_section(
                        CoreSection::Mock,
                        serde_json::json!({ "cors_default": *checked }),
                        cx,
                    );
                },
            )),
            cx,
        ))
}

fn render_data(view: &SettingsView, zh: bool, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let data = &view.settings.data;
    let sync = &view.settings.sync;
    v_col()
        .gap(px(4.0))
        .child(field_row(
            if zh {
                "历史记录保留策略"
            } else {
                "History retention"
            },
            None,
            badge(format!("{:?}", data.history_retention), Tone::Neutral, cx),
            cx,
        ))
        .child(field_row(
            if zh {
                "对话保留天数"
            } else {
                "Chat retention"
            },
            None,
            badge(
                format!("{} days", data.chat_retention_days),
                Tone::Neutral,
                cx,
            ),
            cx,
        ))
        .child(field_row(
            if zh {
                "响应体上限"
            } else {
                "Response body cap"
            },
            None,
            badge(
                format!("{} MiB", data.response_body_cap_bytes / (1024 * 1024)),
                Tone::Neutral,
                cx,
            ),
            cx,
        ))
        .child(field_row(
            if zh {
                "文件夹同步路径"
            } else {
                "Sync folder"
            },
            Some(SharedString::from(if zh {
                "本地优先同步：不上传云端，秘密默认不导出。"
            } else {
                "Local-first folder sync: zero cloud upload; secrets excluded by default."
            })),
            badge(
                if sync.directory.is_empty() {
                    if zh { "未配置" } else { "Not set" }.to_owned()
                } else {
                    sync.directory.clone()
                },
                Tone::Neutral,
                cx,
            ),
            cx,
        ))
}

fn render_advanced(zh: bool, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    v_col().gap(px(12.0)).child(
        card(cx).p(px(14.0)).child(field_row(
            if zh {
                "重置全部设置"
            } else {
                "Reset all settings"
            },
            Some(SharedString::from(if zh {
                "将所有设置恢复为出厂默认值（不删除工作区与数据库记录）。"
            } else {
                "Restores every settings section to factory defaults without deleting workspaces."
            })),
            btn_danger(
                "settings-reset-all",
                if zh { "全部重置" } else { "Reset All" },
            )
            .on_click(cx.listener(|this, _, _, cx| this.reset_all(cx))),
            cx,
        )),
    )
}
