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
            crate::ui::Tr(view.lang)
                .environments()
                .modals_resources_domain(),
            None,
            None,
            div().w(px(280.0)).child(text_input(&view.ck_domain_input)),
            cx,
        ))
        .child(setting_row(
            crate::ui::Tr(view.lang)
                .environments()
                .modals_resources_name(),
            None,
            None,
            div().w(px(280.0)).child(text_input(&view.ck_name_input)),
            cx,
        ))
        .child(setting_row(
            crate::ui::Tr(view.lang)
                .environments()
                .modals_resources_value(),
            None,
            None,
            div().w(px(280.0)).child(text_input(&view.ck_value_input)),
            cx,
        ))
        .child(setting_row(
            crate::ui::Tr(view.lang)
                .environments()
                .modals_resources_path(),
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
            btn_secondary("ck-m-cancel", crate::ui::Tr(view.lang).common().cancel()).on_click(
                cx.listener(|this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                }),
            ),
        )
        .child(
            btn_primary(
                "ck-m-save",
                crate::ui::Tr(view.lang)
                    .environments()
                    .modals_resources_add(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.add_cookie_from_modal(secure, http_only, window, cx);
            })),
        )
        .into_any_element();
    modal_dialog(
        "env-add-cookie-modal",
        crate::ui::Tr(view.lang)
            .environments()
            .modals_resources_add_cookie(),
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
                        crate::ui::Tr(view.lang)
                            .environments()
                            .modals_resources_client_mtls(),
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
                        crate::ui::Tr(view.lang)
                            .environments()
                            .modals_resources_custom_ca(),
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
            crate::ui::Tr(view.lang)
                .environments()
                .modals_resources_host_pattern(),
            None,
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .environments()
                    .modals_resources_e_g_api_example_com(),
            )),
            div().w(px(260.0)).child(text_input(&view.cert_host_input)),
            cx,
        ))
        .child(setting_row(
            crate::ui::Tr(view.lang)
                .environments()
                .modals_resources_certificate_file(),
            None,
            None,
            h_row()
                .gap(px(6.0))
                .child(div().w(px(220.0)).child(text_input(&view.cert_path_input)))
                .child(
                    btn_secondary(
                        "cert-pick-c",
                        crate::ui::Tr(view.lang)
                            .environments()
                            .modals_resources_browse(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
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
                    })),
                ),
            cx,
        ))
        .children(is_client.then(|| {
            setting_row(
                crate::ui::Tr(view.lang)
                    .environments()
                    .modals_resources_private_key_file(),
                None,
                None,
                h_row()
                    .gap(px(6.0))
                    .child(div().w(px(220.0)).child(text_input(&view.cert_key_input)))
                    .child(
                        btn_secondary(
                            "cert-pick-k",
                            crate::ui::Tr(view.lang)
                                .environments()
                                .modals_resources_browse(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
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
                        })),
                    ),
                cx,
            )
        }))
        .children(is_client.then(|| {
            setting_row(
                crate::ui::Tr(view.lang)
                    .environments()
                    .modals_resources_key_passphrase_optional(),
                None,
                Some(SharedString::from(
                    crate::ui::Tr(view.lang)
                        .environments()
                        .modals_resources_stored_in_owner_only_0600(),
                )),
                div().w(px(260.0)).child(text_input(&view.cert_pass_input)),
                cx,
            )
        }))
        .into_any_element();
    let footer = h_row()
        .gap(px(8.0))
        .child(
            btn_secondary("cert-m-cancel", crate::ui::Tr(view.lang).common().cancel()).on_click(
                cx.listener(|this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                }),
            ),
        )
        .child(
            btn_primary(
                "cert-m-save",
                crate::ui::Tr(view.lang)
                    .environments()
                    .modals_resources_save_certificate(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.add_certificate(kind, window, cx);
            })),
        )
        .into_any_element();
    modal_dialog(
        "env-add-cert-modal",
        crate::ui::Tr(view.lang)
            .environments()
            .modals_resources_add_tls_certificate(),
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
            crate::ui::Tr(view.lang)
                .environments()
                .modals_resources_delete_tls_certificate_and_its(label),
            None,
            cx,
        ))
        .into_any_element();
    let footer = h_row()
        .gap(px(8.0))
        .child(
            btn_secondary(
                "cert-del-cancel",
                crate::ui::Tr(view.lang).common().cancel(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.active_modal = None;
                cx.notify();
            })),
        )
        .child(
            btn_danger(
                "cert-del-confirm",
                crate::ui::Tr(view.lang).common().delete(),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.delete_certificate(id.clone(), cx);
            })),
        )
        .into_any_element();
    modal_dialog(
        "env-del-cert-modal",
        crate::ui::Tr(view.lang)
            .environments()
            .modals_resources_delete_tls_certificate(),
        440.0,
        body,
        footer,
        window,
        cx,
        on_close,
    )
}
