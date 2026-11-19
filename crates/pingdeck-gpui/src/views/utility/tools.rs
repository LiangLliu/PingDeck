//! Utility tabs backed by core services: Sync, Cookies, TLS Certificates, and Collection Runner.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_danger, btn_primary, btn_secondary, card,
    empty_state, field_row, h_row, section_heading, v_col,
};

use super::UtilityPanel;

pub fn render_sync(
    panel: &UtilityPanel,
    zh: bool,
    cx: &mut Context<UtilityPanel>,
) -> gpui_kit::AnyElement {
    let mut col = v_col().size_full().gap(px(12.0)).overflow_y_scrollbar();
    if let Some(st) = &panel.sync_status {
        let dir = if st.directory.is_empty() {
            if zh {
                "未设置（在 设置 -> 文件夹同步 配置）"
            } else {
                "Not configured (Settings -> Folder Sync)"
            }
            .to_owned()
        } else {
            st.directory.clone()
        };
        col = col.child(
            card(cx).p(px(14.0)).child(
                v_col()
                    .child(field_row(
                        "Directory",
                        None,
                        badge(SharedString::from(dir), Tone::Neutral, cx),
                        cx,
                    ))
                    .child(field_row(
                        "Auto-Export",
                        None,
                        badge(
                            if st.auto_export { "ON" } else { "OFF" },
                            if st.auto_export {
                                Tone::Success
                            } else {
                                Tone::Neutral
                            },
                            cx,
                        ),
                        cx,
                    ))
                    .child(field_row(
                        "Include Secrets",
                        None,
                        badge(
                            if st.include_secrets { "YES" } else { "NO" },
                            if st.include_secrets {
                                Tone::Warning
                            } else {
                                Tone::Neutral
                            },
                            cx,
                        ),
                        cx,
                    )),
            ),
        );
    }
    col = col.child(
        h_row()
            .gap(px(8.0))
            .child(
                btn_primary("util-sync-exp", if zh { "立即导出" } else { "Export Now" })
                    .on_click(cx.listener(|this, _, _, cx| this.run_sync_export(cx))),
            )
            .child(
                btn_secondary(
                    "util-sync-imp",
                    if zh {
                        "从目录导入"
                    } else {
                        "Import from Folder"
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| this.run_sync_import(cx))),
            ),
    );
    col.into_any_element()
}

pub fn render_cookies(
    panel: &UtilityPanel,
    zh: bool,
    cx: &mut Context<UtilityPanel>,
) -> gpui_kit::AnyElement {
    let mut col = v_col()
        .size_full()
        .gap(px(10.0))
        .overflow_y_scrollbar()
        .child(
            h_row()
                .justify_between()
                .child(section_heading(
                    format!(
                        "{} ({})",
                        if zh { "Cookie 罐" } else { "Cookies" },
                        panel.cookies.len()
                    ),
                    cx,
                ))
                .child(
                    btn_secondary("util-ck-exp", if zh { "清理过期" } else { "Clear Expired" })
                        .on_click(cx.listener(|this, _, _, cx| this.clear_expired_cookies(cx))),
                ),
        );
    if panel.cookies.is_empty() {
        col = col.child(empty_state(
            UiIcon::Cookie,
            if zh {
                "当前工作区无 Cookie"
            } else {
                "No cookies stored"
            },
            if zh {
                "发送 HTTP 请求收到的 Set-Cookie 会自动保存于此。"
            } else {
                "Set-Cookie headers from HTTP responses appear here."
            },
            cx,
        ));
    } else {
        for ck in &panel.cookies {
            col = col.child(
                card(cx).p(px(10.0)).child(
                    h_row()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .child(SharedString::from(format!("{} ({})", ck.name, ck.domain))),
                        )
                        .child(badge(
                            SharedString::from(ck.path.clone()),
                            Tone::Neutral,
                            cx,
                        )),
                ),
            );
        }
    }
    col.into_any_element()
}

pub fn render_certs(
    panel: &UtilityPanel,
    zh: bool,
    cx: &mut Context<UtilityPanel>,
) -> gpui_kit::AnyElement {
    let mut col = v_col()
        .size_full()
        .gap(px(10.0))
        .overflow_y_scrollbar()
        .child(section_heading(
            format!(
                "{} ({})",
                if zh { "TLS 证书" } else { "TLS Certificates" },
                panel.certificates.len()
            ),
            cx,
        ));
    if panel.certificates.is_empty() {
        col = col.child(empty_state(
            UiIcon::Shield,
            if zh {
                "无自定义 TLS 证书"
            } else {
                "No custom TLS certificates"
            },
            if zh {
                "支持按域名配置 mTLS 客户端证书与企业私有 CA。"
            } else {
                "Configure per-host mTLS client certificates and custom CA bundles."
            },
            cx,
        ));
    } else {
        for cert in &panel.certificates {
            let cid = cert.id.as_str().to_owned();
            col =
                col.child(
                    card(cx).p(px(10.0)).child(
                        h_row()
                            .justify_between()
                            .child(div().text_xs().child(SharedString::from(format!(
                                "{} — {}",
                                cert.host_pattern, cert.cert_path
                            ))))
                            .child(
                                btn_danger(
                                    SharedString::from(format!("cert-del-{cid}")),
                                    if zh { "删除" } else { "Delete" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.delete_cert(cid.clone(), cx),
                                )),
                            ),
                    ),
                );
        }
    }
    col.into_any_element()
}

pub fn render_runner(
    panel: &UtilityPanel,
    zh: bool,
    cx: &mut Context<UtilityPanel>,
) -> gpui_kit::AnyElement {
    let mut col = v_col()
        .size_full()
        .gap(px(10.0))
        .overflow_y_scrollbar()
        .child(section_heading(
            format!(
                "{} ({})",
                if zh {
                    "集合批量运行器"
                } else {
                    "Collection Runner"
                },
                panel.collections.len()
            ),
            cx,
        ));
    if panel.collections.is_empty() {
        col = col.child(empty_state(
            UiIcon::Play,
            if zh {
                "当前工作区无集合"
            } else {
                "No collections in workspace"
            },
            if zh {
                "导入 Postman 集合或创建集合后即可在此顺序批量执行。"
            } else {
                "Import a Postman collection or create one to run requests in batch."
            },
            cx,
        ));
    } else {
        for c in &panel.collections {
            let cid = c.id.as_str().to_owned();
            col = col.child(
                card(cx).p(px(10.0)).child(
                    h_row()
                        .justify_between()
                        .child(div().text_xs().child(SharedString::from(c.name.clone())))
                        .child(
                            btn_primary(
                                SharedString::from(format!("run-col-{cid}")),
                                if panel.running_collection {
                                    "Running…"
                                } else if zh {
                                    "运行"
                                } else {
                                    "Run"
                                },
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.run_collection(cid.clone(), cx),
                            )),
                        ),
                ),
            );
        }
    }
    if !panel.runner_events.is_empty() {
        col = col.child(
            card(cx).p(px(10.0)).child(
                v_col().gap(px(4.0)).children(
                    panel
                        .runner_events
                        .iter()
                        .rev()
                        .take(20)
                        .map(|ev| div().text_xs().child(SharedString::from(ev.clone()))),
                ),
            ),
        );
    }
    col.into_any_element()
}
