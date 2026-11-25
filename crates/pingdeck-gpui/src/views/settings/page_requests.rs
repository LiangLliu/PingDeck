//! Settings › Request Defaults page: HTTP transport defaults, editor/viewer preferences,
//! and response size / preview thresholds — all live-validated and resettable per section.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, px};
use pingdeck_desktop::core::model::models::HttpVersionPreference;
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;

use crate::ui::{
    Disableable as _, Tone, UiIcon, alert_banner, btn_ghost, btn_secondary, choice_chip, h_row,
    icon_xs, number_field, setting_row, settings_section, tab_bar_segmented, tab_item,
    toggle_switch, v_col,
};

use super::SettingsView;

pub fn render_requests(
    view: &SettingsView,
    zh: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let net = &view.settings.network;
    let ed = &view.settings.editor;

    let http_idx = match net.http_version {
        HttpVersionPreference::Auto => 0,
        HttpVersionPreference::Http10 => 1,
        HttpVersionPreference::Http11 => 2,
        HttpVersionPreference::Http2 => 3,
    };
    let ent_http = cx.entity().clone();

    let transport_rows = v_col()
        .w_full()
        .child(setting_row(
            if zh { "默认请求超时" } else { "Default timeout" },
            None,
            Some(SharedString::from(if zh {
                "新建请求继承此超时（1–3,600,000 ms，默认 30,000 ms）；也用于 OAuth 与 pm.sendRequest。"
            } else {
                "Inherited by new requests (1–3,600,000 ms, default 30,000 ms); also applies to OAuth & pm.sendRequest."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.req_timeout_ms, Some("ms"), 100.0, cx))
                .child(
                    btn_secondary("req-save-timeout", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.req_timeout_ms.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                1,
                                3_600_000,
                                CoreSection::Network,
                                |n| serde_json::json!({ "timeout_ms": n }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "传输失败重试次数" } else { "Transport retries" },
            None,
            Some(SharedString::from(if zh {
                "发生网络层失败时的自动重试次数（0–10，默认 0）。"
            } else {
                "Automatic retries after transport-level failures (0–10, default 0)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.req_retry_count,
                    Some(if zh { "次" } else { "x" }),
                    72.0,
                    cx,
                ))
                .child(
                    btn_secondary("req-save-retries", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.req_retry_count.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                0,
                                10,
                                CoreSection::Network,
                                |n| serde_json::json!({ "retry_count": n }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "首选 HTTP 版本" } else { "Preferred HTTP version" },
            None,
            Some(SharedString::from(if zh {
                "默认由 ALPN 自动协商，也可强制指定协议版本。"
            } else {
                "Auto negotiates via ALPN by default, or lock a specific HTTP version."
            })),
            tab_bar_segmented("req-http-ver-seg")
                .selected_index(http_idx)
                .child(tab_item(if zh { "自动" } else { "Auto" }))
                .child(tab_item("HTTP/1.0"))
                .child(tab_item("HTTP/1.1"))
                .child(tab_item("HTTP/2"))
                .on_click(move |idx, _, cx| {
                    let wire = match *idx {
                        1 => "Http10",
                        2 => "Http11",
                        3 => "Http2",
                        _ => "Auto",
                    };
                    ent_http.update(cx, |this, cx| {
                        this.patch_section(
                            CoreSection::Network,
                            serde_json::json!({ "http_version": wire }),
                            cx,
                        );
                    });
                }),
            cx,
        ))
        .child(setting_row(
            if zh { "自动跟随重定向" } else { "Follow redirects" },
            None,
            Some(SharedString::from(if zh {
                "遇到 3xx 响应时自动跟踪 Location 目标地址。"
            } else {
                "Automatically follow HTTP 3xx Location headers."
            })),
            toggle_switch("req-follow-redir", net.follow_redirects).on_change(cx.listener(
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
        .child(setting_row(
            if zh { "最大重定向跳数" } else { "Max redirect hops" },
            None,
            Some(SharedString::from(if zh {
                "单次请求允许的最大重定向次数（0–50，默认 10）。"
            } else {
                "Maximum redirect hops allowed per request (0–50, default 10)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.req_max_redirects, None, 72.0, cx))
                .child(
                    btn_secondary("req-save-redir", if zh { "应用" } else { "Apply" })
                        .disabled(!net.follow_redirects)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let v = this.inputs.req_max_redirects.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                0,
                                50,
                                CoreSection::Network,
                                |n| serde_json::json!({ "max_redirects": n }),
                                cx,
                            );
                        })),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "校验 TLS 证书" } else { "Verify TLS certificates" },
            None,
            Some(SharedString::from(if zh {
                "建议始终保持开启；如需信任内网自签名证书，请在「网络」页配置自定义 CA 证书包。"
            } else {
                "Keep enabled whenever possible; for internal self-signed roots, add a custom CA bundle in Network."
            })),
            toggle_switch("req-verify-tls", net.verify_tls).on_change(cx.listener(
                |this, checked: &bool, _, cx| {
                    this.patch_section(
                        CoreSection::Network,
                        serde_json::json!({ "verify_tls": *checked }),
                        cx,
                    );
                },
            )),
            cx,
        ));

    let mut transport = v_col().w_full().gap(px(10.0)).child(settings_section(
        if zh {
            "HTTP 请求默认值"
        } else {
            "HTTP Request Defaults"
        },
        Some(
            btn_ghost("req-reset-net", if zh { "恢复默认" } else { "Reset" })
                .icon(icon_xs(UiIcon::RotateCcw))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.reset_keys(
                        CoreSection::Network,
                        Some(
                            [
                                "timeout_ms",
                                "retry_count",
                                "http_version",
                                "follow_redirects",
                                "max_redirects",
                                "verify_tls",
                            ]
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                        ),
                        cx,
                    );
                }))
                .into_any_element(),
        ),
        transport_rows,
        cx,
    ));

    if !net.verify_tls {
        transport = transport.child(alert_banner(
            Tone::Warning,
            if zh {
                "全局 TLS 证书校验当前已关闭，新请求将接受任意自签名或过期证书。"
            } else {
                "Global TLS verification is disabled; new requests will accept self-signed or expired certificates."
            },
            None,
            cx,
        ));
    }

    let editor_sec = settings_section(
        if zh { "编辑器与高亮" } else { "Editor & Syntax Highlighting" },
        Some(
            btn_ghost("req-reset-editor", if zh { "恢复默认" } else { "Reset" })
                .icon(icon_xs(UiIcon::RotateCcw))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.reset_section(CoreSection::Editor, cx);
                }))
                .into_any_element(),
        ),
        v_col()
            .w_full()
            .child(setting_row(
                if zh { "Tab 缩进宽度" } else { "Tab size" },
                None,
                Some(SharedString::from(if zh {
                    "请求正文、JSON 与脚本编辑器按下 Tab 插入的空格数。"
                } else {
                    "Number of spaces inserted by Tab in body and script editors."
                })),
                h_row()
                    .gap(px(6.0))
                    .children([2u8, 4, 8].into_iter().map(|spaces| {
                        choice_chip(
                            SharedString::from(format!("ed-tab-{spaces}")),
                            if zh {
                                format!("{spaces} 空格")
                            } else {
                                format!("{spaces} spaces")
                            },
                            ed.tab_size == spaces,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.patch_section(
                                CoreSection::Editor,
                                serde_json::json!({ "tab_size": spaces }),
                                cx,
                            );
                        }))
                    })),
                cx,
            ))
            .child(setting_row(
                if zh { "自动换行" } else { "Word wrap" },
                None,
                Some(SharedString::from(if zh {
                    "在编辑器和响应查看器中自动折行显示超长文本。"
                } else {
                    "Wrap long lines automatically inside request and response editors."
                })),
                toggle_switch("ed-word-wrap", ed.word_wrap).on_change(cx.listener(
                    |this, checked: &bool, _, cx| {
                        this.patch_section(
                            CoreSection::Editor,
                            serde_json::json!({ "word_wrap": *checked }),
                            cx,
                        );
                    },
                )),
                cx,
            ))
            .child(setting_row(
                if zh { "请求编辑器语法高亮上限" } else { "Editor highlight limit" },
                None,
                Some(SharedString::from(if zh {
                    "超过此大小的请求正文将回退为纯文本以保证流畅编辑（1–65,536 KiB，默认 1,024 KiB）。"
                } else {
                    "Request bodies above this size fall back to plain text for smooth typing (1–65,536 KiB, default 1,024 KiB)."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.editor_highlight_kib, Some("KiB"), 96.0, cx))
                    .child(
                        btn_secondary("ed-save-hl", if zh { "应用" } else { "Apply" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.editor_highlight_kib.read(cx).value().to_string();
                                this.commit_kib(
                                    &v,
                                    1,
                                    65_536,
                                    CoreSection::Editor,
                                    "editor_highlight_limit_bytes",
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                if zh { "响应查看器语法高亮上限" } else { "Viewer highlight limit" },
                None,
                Some(SharedString::from(if zh {
                    "超过此大小的响应正文不执行语法着色（1–65,536 KiB，默认 512 KiB）。"
                } else {
                    "Response bodies larger than this skip syntax coloring (1–65,536 KiB, default 512 KiB)."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.viewer_highlight_kib, Some("KiB"), 96.0, cx))
                    .child(
                        btn_secondary("vw-save-hl", if zh { "应用" } else { "Apply" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.viewer_highlight_kib.read(cx).value().to_string();
                                this.commit_kib(
                                    &v,
                                    1,
                                    65_536,
                                    CoreSection::Editor,
                                    "viewer_highlight_limit_bytes",
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            )),
        cx,
    );

    let response_sec = settings_section(
        if zh { "响应体与大报文阈值" } else { "Response & Large Body Thresholds" },
        Some(
            btn_ghost("req-reset-resp", if zh { "恢复默认" } else { "Reset" })
                .icon(icon_xs(UiIcon::RotateCcw))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.reset_keys(
                        CoreSection::Data,
                        Some(
                            [
                                "response_body_cap_bytes",
                                "inline_threshold_bytes",
                                "preview_bytes",
                            ]
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                        ),
                        cx,
                    );
                }))
                .into_any_element(),
        ),
        v_col()
            .w_full()
            .child(setting_row(
                if zh { "单次响应读取上限" } else { "Response body cap" },
                None,
                Some(SharedString::from(if zh {
                    "单次响应最多读取入内存的字节数，超出部分截断（1–512 MiB，默认 10 MiB）。"
                } else {
                    "Maximum bytes buffered for a single response; excess is truncated (1–512 MiB, default 10 MiB)."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.resp_body_cap_mib, Some("MiB"), 84.0, cx))
                    .child(
                        btn_secondary("resp-save-cap", if zh { "应用" } else { "Apply" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.resp_body_cap_mib.read(cx).value().to_string();
                                this.commit_mib(
                                    &v,
                                    1,
                                    512,
                                    CoreSection::Data,
                                    "response_body_cap_bytes",
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                if zh { "内联传输阈值" } else { "Inline body threshold" },
                None,
                Some(SharedString::from(if zh {
                    "小于此大小的响应直接内联展示，更大响应按句柄分块加载（1–65,536 KiB，默认 1,024 KiB）。"
                } else {
                    "Responses up to this size render inline; larger ones load on demand via handle (1–65,536 KiB, default 1,024 KiB)."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.resp_inline_kib, Some("KiB"), 96.0, cx))
                    .child(
                        btn_secondary("resp-save-inline", if zh { "应用" } else { "Apply" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.resp_inline_kib.read(cx).value().to_string();
                                this.commit_kib(
                                    &v,
                                    1,
                                    65_536,
                                    CoreSection::Data,
                                    "inline_threshold_bytes",
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(setting_row(
                if zh { "大响应初始预览大小" } else { "Large-body preview size" },
                None,
                Some(SharedString::from(if zh {
                    "超过内联阈值时默认首屏加载的预览字节数，不能大于上方内联阈值（默认 64 KiB）。"
                } else {
                    "Initial preview chunk shown for oversized bodies; must not exceed inline threshold (default 64 KiB)."
                })),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(&view.inputs.resp_preview_kib, Some("KiB"), 96.0, cx))
                    .child(
                        btn_secondary("resp-save-preview", if zh { "应用" } else { "Apply" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                let v = this.inputs.resp_preview_kib.read(cx).value().to_string();
                                this.commit_kib(
                                    &v,
                                    1,
                                    65_536,
                                    CoreSection::Data,
                                    "preview_bytes",
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
        .gap(px(22.0))
        .child(transport)
        .child(editor_sec)
        .child(response_sec)
}
