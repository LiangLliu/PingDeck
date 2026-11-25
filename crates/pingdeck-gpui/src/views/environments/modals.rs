//! Modal dialogs for `EnvironmentsView`: Delete Env, Import/Export Env, Edit Variable, Add Cookie, Add/Delete Cert.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};
use pingdeck_desktop::core::model::models::TlsCertificateKind;
use pingdeck_desktop::services::{FileFilter, FilePickerSpec, HostDialog as _, RfdDialog};

use crate::ui::{
    Tone, UiIcon, alert_banner, btn_danger, btn_primary, btn_secondary, choice_chip, code_editor,
    h_row, icon_xs, modal_dialog, palette, setting_row, text_input, to_hsla, toggle_switch, v_col,
};

use super::state::{EnvModal, EnvironmentsView};

pub fn render_modal(
    view: &EnvironmentsView,
    window: &Window,
    cx: &mut Context<EnvironmentsView>,
) -> Option<AnyElement> {
    let modal = view.active_modal.clone()?;
    let p = palette(cx);
    let entity = cx.entity().clone();
    let on_close = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        entity.update(cx, |this, cx| {
            this.active_modal = None;
            cx.notify();
        });
    };

    let el = match modal {
        EnvModal::DeleteEnv { id, name } => {
            let id_go = id.clone();
            let name_go = name.clone();
            let body = div()
                .text_sm()
                .text_color(to_hsla(p.ink))
                .child(if view.is_zh() {
                    format!("确定要删除环境「{name}」及其所有变量与机密吗？删除后 60 秒内支持撤销恢复。")
                } else {
                    format!("Delete environment \"{name}\" and its secrets? Undo remains available for 60 seconds.")
                })
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-del-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_danger("env-del-confirm", view.t("删除环境", "Delete")).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.delete_env(id_go.clone(), name_go.clone(), cx);
                        }),
                    ),
                )
                .into_any_element();
            modal_dialog(
                "env-delete-modal",
                view.t("删除环境", "Delete Environment"),
                430.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        EnvModal::ImportEnv => {
            let body = v_col()
                .w_full()
                .gap(px(10.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(view.t(
                            "粘贴 Postman 环境 JSON，或从磁盘载入文件。",
                            "Paste Postman environment JSON or load a file.",
                        )))
                        .child(
                            btn_secondary("env-imp-file", view.t("选择文件…", "Load File…"))
                                .icon(icon_xs(UiIcon::Upload))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.load_import_file(window, cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(220.0))
                        .child(code_editor(&view.import_editor)),
                )
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-imp-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary("env-imp-go", view.t("导入环境", "Import"))
                        .on_click(cx.listener(|this, _, _, cx| this.import_env_from_editor(cx))),
                )
                .into_any_element();
            modal_dialog(
                "env-import-modal",
                view.t("导入环境", "Import Environment"),
                580.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        EnvModal::ExportEnv {
            id,
            name,
            include_secrets,
        } => {
            let id_t = id.clone();
            let name_t = name.clone();
            let body = v_col()
                .w_full()
                .gap(px(10.0))
                .child(setting_row(
                    view.t(
                        "在导出文件中包含机密明文",
                        "Include secret plaintext values",
                    ),
                    None,
                    Some(SharedString::from(view.t(
                        "默认关闭以防止凭据外泄，机密键名会以空值保留。",
                        "Off by default to prevent secret leaks; secret keys are exported empty.",
                    ))),
                    toggle_switch("env-exp-sec", include_secrets).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.active_modal = Some(EnvModal::ExportEnv {
                                id: id_t.clone(),
                                name: name_t.clone(),
                                include_secrets: !include_secrets,
                            });
                            cx.notify();
                        },
                    )),
                    cx,
                ))
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-exp-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary("env-exp-save", view.t("导出 JSON…", "Export JSON…")).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.export_env(id.clone(), name.clone(), include_secrets, cx);
                        }),
                    ),
                )
                .into_any_element();
            modal_dialog(
                "env-export-modal",
                view.t("导出环境", "Export Environment"),
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        EnvModal::EditVar {
            is_global,
            index,
            is_secret,
        } => {
            let body = v_col()
                .w_full()
                .gap(px(10.0))
                .child(setting_row(
                    view.t("变量名 Key", "Key"),
                    None,
                    None,
                    div().w(px(280.0)).child(text_input(&view.edit_var_key)),
                    cx,
                ))
                .child(setting_row(
                    if is_secret {
                        view.t(
                            "新机密值（留空保留原值）",
                            "New Secret (blank keeps stored)",
                        )
                    } else {
                        view.t("变量值 Value", "Value")
                    },
                    None,
                    None,
                    div().w(px(280.0)).child(text_input(&view.edit_var_val)),
                    cx,
                ))
                .child(setting_row(
                    view.t("备注 Note", "Note"),
                    None,
                    None,
                    div().w(px(280.0)).child(text_input(&view.edit_var_note)),
                    cx,
                ))
                .child(setting_row(
                    view.t("设为机密变量", "Secret Variable"),
                    None,
                    None,
                    toggle_switch("env-ed-sec", is_secret).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.active_modal = Some(EnvModal::EditVar {
                                is_global,
                                index,
                                is_secret: !is_secret,
                            });
                            cx.notify();
                        },
                    )),
                    cx,
                ))
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-ed-cancel", view.t("取消", "Cancel")).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        },
                    )),
                )
                .child(
                    btn_primary("env-ed-apply", view.t("应用更改", "Apply")).on_click(cx.listener(
                        move |this, _, _, cx| {
                            let key = this.edit_var_key.read(cx).value().trim().to_owned();
                            let val = this.edit_var_val.read(cx).value().to_string();
                            let note = this.edit_var_note.read(cx).value().trim().to_owned();
                            if key.is_empty() {
                                return;
                            }
                            let list = if is_global {
                                &mut this.globals_draft
                            } else {
                                &mut this.env_draft_vars
                            };
                            if let Some(row) = list.get_mut(index) {
                                row.key = key;
                                row.note = note;
                                row.is_secret = is_secret;
                                if is_secret {
                                    if !val.is_empty() {
                                        row.value = Some(val);
                                        row.has_secret = true;
                                    }
                                } else {
                                    row.value = Some(val);
                                    row.has_secret = false;
                                    row.secret_hint = None;
                                }
                                if is_global {
                                    this.globals_dirty = true;
                                } else {
                                    this.env_dirty = true;
                                }
                            }
                            this.active_modal = None;
                            cx.notify();
                        },
                    )),
                )
                .into_any_element();
            modal_dialog(
                "env-edit-var-modal",
                view.t("编辑变量", "Edit Variable"),
                500.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        EnvModal::AddCookie { secure, http_only } => {
            let body =
                v_col()
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
                            .child(choice_chip("ck-m-sec", "Secure", secure, cx).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.active_modal = Some(EnvModal::AddCookie {
                                        secure: !secure,
                                        http_only,
                                    });
                                    cx.notify();
                                }),
                            ))
                            .child(
                                choice_chip("ck-m-http", "HttpOnly", http_only, cx).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.active_modal = Some(EnvModal::AddCookie {
                                            secure,
                                            http_only: !http_only,
                                        });
                                        cx.notify();
                                    }),
                                ),
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
        EnvModal::AddCert { kind } => {
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
                            btn_secondary("cert-pick-c", view.t("浏览", "Browse")).on_click(
                                cx.listener(|this, _, window, cx| {
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
                                }),
                            ),
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
        EnvModal::DeleteCert { id, label } => {
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
                    btn_secondary("cert-del-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
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
    };
    Some(el)
}
