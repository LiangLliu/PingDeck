//! Resource modal dialogs for `EnvironmentsView`: Add Cookie, Add TLS Certificate, Delete TLS Certificate.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};
use pingdeck_desktop::core::model::models::TlsCertificateKind;
use pingdeck_desktop::services::{FileFilter, FilePickerSpec, HostDialog as _, RfdDialog};

use crate::ui::{
    Tone, alert_banner, btn_danger, btn_primary, btn_secondary, choice_chip, h_row, modal_dialog,
    setting_row, text_input, v_col,
};

use super::state::{EnvModal, EnvironmentsView};

pub fn render_add_cookie_modal(
    view: &EnvironmentsView,
    secure: bool,
    http_only: bool,
    window: &Window,
    cx: &mut Context<EnvironmentsView>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let body = v_col()
        .w_full()
        .gap(px(8.0))
        .child(setting_row(
            view.t("域名 Domain", "Domain"),
            None,
            None,
            div().w(px(280.0)).child(text_input(&view.ck_domain_input)),
            cx,
        ))
        .child(setting_row(
            view.t("名称 Name", "Name"),
            None,
            None,
            div().w(px(280.0)).child(text_input(&view.ck_name_input)),
            cx,
        ))
        .child(setting_row(
            view.t("值 Value", "Value"),
            None,
            None,
            div().w(px(280.0)).child(text_input(&view.ck_value_input)),
            cx,
        ))
        .child(setting_row(
            view.t("路径 Path", "Path"),
            None,
            None,
            div().w(px(280.0)).child(text_input(&view.ck_path_input)),
            cx,
        ))
        .child(
            h_row()
                .gap(px(8.0))
                .child(
                    choice_chip("ck-m-sec", "Secure", secure, cx).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.active_modal = Some(EnvModal::AddCookie {
                                secure: !secure,
                                http_only,
                            });
                            cx.notify();
                        },
                    )),
                )
                .child(
                    choice_chip("ck-m-http", "HttpOnly", http_only, cx).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.active_modal = Some(EnvModal::AddCookie {
                                secure,
                                http_only: !http_only,
                            });
                            cx.notify();
                        },
                    )),
                ),
        )
        .into_any_element();
    let footer = h_row()
        .gap(px(8.0))
        .child(
            btn_secondary("ck-m-cancel", view.t("取消", "Cancel")).on_click(cx.listener(
                |this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                },
            )),
        )
        .child(
            btn_primary("ck-m-save", view.t("添加", "Add")).on_click(cx.listener(
                move |this, _, window, cx| {
                    this.add_cookie_from_modal(secure, http_only, window, cx);
                },
            )),
        )
        .into_any_element();
    modal_dialog(
        "env-add-cookie-modal",
        view.t("添加 Cookie", "Add Cookie"),
        480.0,
        body,
        footer,
        window,
        cx,
        on_close,
    )
}

pub fn render_add_cert_modal(
    view: &EnvironmentsView,
    kind: TlsCertificateKind,
    window: &Window,
    cx: &mut Context<EnvironmentsView>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let is_client = kind == TlsCertificateKind::Client;
    let body = v_col()
        .w_full()
        .gap(px(8.0))
        .child(
            h_row()
                .gap(px(6.0))
                .child(
                    choice_chip(
                        "cert-k-client",
                        view.t("mTLS 客户端证书", "Client mTLS"),
                        is_client,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = Some(EnvModal::AddCert {
                            kind: TlsCertificateKind::Client,
                        });
                        cx.notify();
                    })),
                )
                .child(
                    choice_chip(
                        "cert-k-ca",
                        view.t("自定义 CA 根证书", "Custom CA"),
                        !is_client,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = Some(EnvModal::AddCert {
                            kind: TlsCertificateKind::Ca,
                        });
                        cx.notify();
                    })),
                ),
        )
        .child(setting_row(
            view.t("匹配主机模式", "Host Pattern"),
            None,
            Some(SharedString::from(view.t(
                "如 api.example.com 或 *.internal:8443，CA 可留空或填 *",
                "e.g. api.example.com or * for CA",
            ))),
            div().w(px(260.0)).child(text_input(&view.cert_host_input)),
            cx,
        ))
        .child(setting_row(
            view.t("证书文件 (.pem/.crt/.p12)", "Certificate File"),
            None,
            None,
            h_row()
                .gap(px(6.0))
                .child(div().w(px(220.0)).child(text_input(&view.cert_path_input)))
                .child(
                    btn_secondary("cert-pick-c", view.t("浏览", "Browse")).on_click(cx.listener(
                        |this, _, window, cx| {
                            if let Some(path) = RfdDialog.pick_file(&FilePickerSpec {
                                title: None,
                                filters: vec![FileFilter {
                                    name: "Certificates".to_owned(),
                                    extensions: vec![
                                        "pem".into(),
                                        "crt".into(),
                                        "cer".into(),
                                        "p12".into(),
                                        "pfx".into(),
                                    ],
                                }],
                                starting_dir: None,
                            }) {
                                this.cert_path_input.update(cx, |s, cx| {
                                    s.set_value(path.display().to_string(), window, cx);
                                });
                                cx.notify();
                            }
                        },
                    )),
                ),
            cx,
        ))
        .children(is_client.then(|| {
            setting_row(
                view.t("私钥文件 (.key/.pem，PKCS#12 可留空)", "Private Key File"),
                None,
                None,
                h_row()
                    .gap(px(6.0))
                    .child(div().w(px(220.0)).child(text_input(&view.cert_key_input)))
                    .child(
                        btn_secondary("cert-pick-k", view.t("浏览", "Browse")).on_click(
                            cx.listener(|this, _, window, cx| {
                                if let Some(path) = RfdDialog.pick_file(&FilePickerSpec {
                                    title: None,
                                    filters: vec![FileFilter {
                                        name: "Private Keys".to_owned(),
                                        extensions: vec!["key".into(), "pem".into()],
                                    }],
                                    starting_dir: None,
                                }) {
                                    this.cert_key_input.update(cx, |s, cx| {
                                        s.set_value(path.display().to_string(), window, cx);
                                    });
                                    cx.notify();
                                }
                            }),
                        ),
                    ),
                cx,
            )
        }))
        .children(is_client.then(|| {
            setting_row(
                view.t("私钥口令（可选）", "Key Passphrase (optional)"),
                None,
                Some(SharedString::from(view.t(
                    "仅写入本地 0600 明文 .secrets 保险库。",
                    "Stored in owner-only 0600 plaintext .secrets vault.",
                ))),
                div().w(px(260.0)).child(text_input(&view.cert_pass_input)),
                cx,
            )
        }))
        .into_any_element();
    let footer = h_row()
        .gap(px(8.0))
        .child(
            btn_secondary("cert-m-cancel", view.t("取消", "Cancel")).on_click(cx.listener(
                |this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                },
            )),
        )
        .child(
            btn_primary("cert-m-save", view.t("保存证书", "Save Certificate")).on_click(
                cx.listener(move |this, _, window, cx| {
                    this.add_certificate(kind, window, cx);
                }),
            ),
        )
        .into_any_element();
    modal_dialog(
        "env-add-cert-modal",
        view.t("添加 TLS 证书", "Add TLS Certificate"),
        560.0,
        body,
        footer,
        window,
        cx,
        on_close,
    )
}

pub fn render_delete_cert_modal(
    view: &EnvironmentsView,
    id: String,
    label: String,
    window: &Window,
    cx: &mut Context<EnvironmentsView>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let body = v_col()
        .gap(px(8.0))
        .child(alert_banner(
            Tone::Warning,
            if view.is_zh() {
                format!("确定删除 TLS 证书「{label}」及其关联私钥口令？")
            } else {
                format!("Delete TLS certificate \"{label}\" and its stored passphrase?")
            },
            None,
            cx,
        ))
        .into_any_element();
    let footer = h_row()
        .gap(px(8.0))
        .child(
            btn_secondary("cert-del-cancel", view.t("取消", "Cancel")).on_click(cx.listener(
                |this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                },
            )),
        )
        .child(
            btn_danger("cert-del-confirm", view.t("删除", "Delete")).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.delete_certificate(id.clone(), cx);
                },
            )),
        )
        .into_any_element();
    modal_dialog(
        "env-del-cert-modal",
        view.t("删除 TLS 证书", "Delete TLS Certificate"),
        440.0,
        body,
        footer,
        window,
        cx,
        on_close,
    )
}
