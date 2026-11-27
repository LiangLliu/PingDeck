//! Workspace TLS Client (mTLS) and Custom CA Certificates tab for `EnvironmentsView`.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use spanbay_desktop::core::model::models::TlsCertificateKind;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_icon, btn_primary, card,
    data_row, empty_state_with_actions, h_row, icon_xs, meta_pill, palette, section_header,
    to_hsla, toggle_switch, toolbar_strip, v_col,
};

use super::state::{EnvModal, EnvironmentsView};

pub fn render_certs_tab(view: &EnvironmentsView, cx: &mut Context<EnvironmentsView>) -> AnyElement {
    let p = palette(cx);

    let bar = toolbar_strip(cx)
        .justify_between()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ink))
                .child(
                    crate::ui::Tr(view.lang)
                        .environments()
                        .tab_certs_workspace_tls_certificates_mtls_custom(),
                ),
        )
        .child(
            btn_primary(
                "cert-add-btn",
                crate::ui::Tr(view.lang)
                    .environments()
                    .tab_certs_add_certificate(),
            )
            .icon(icon_xs(UiIcon::Plus))
            .on_click(cx.listener(|this, _, window, cx| {
                this.cert_host_input
                    .update(cx, |s, cx| s.set_value("", window, cx));
                this.cert_path_input
                    .update(cx, |s, cx| s.set_value("", window, cx));
                this.cert_key_input
                    .update(cx, |s, cx| s.set_value("", window, cx));
                this.cert_pass_input
                    .update(cx, |s, cx| s.set_value("", window, cx));
                this.active_modal = Some(EnvModal::AddCert {
                    kind: TlsCertificateKind::Client,
                });
                cx.notify();
            })),
        );

    let body = if view.certificates.is_empty() {
        empty_state_with_actions(
            UiIcon::Certificate,
            crate::ui::Tr(view.lang)
                .environments()
                .tab_certs_no_tls_certificates_configured(),
            crate::ui::Tr(view.lang)
                .environments()
                .tab_certs_add_mtls_client_certificates_or(),
            Some(
                btn_primary(
                    "cert-empty-add",
                    crate::ui::Tr(view.lang)
                        .environments()
                        .tab_certs_add_certificate(),
                )
                .icon(icon_xs(UiIcon::Plus))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.active_modal = Some(EnvModal::AddCert {
                        kind: TlsCertificateKind::Client,
                    });
                    cx.notify();
                }))
                .into_any_element(),
            ),
            cx,
        )
        .into_any_element()
    } else {
        let mut list = card(cx).p(px(14.0)).gap(px(8.0)).child(section_header(
            crate::ui::Tr(view.lang)
                .environments()
                .tab_certs_configured_certificates(),
            None,
            cx,
        ));
        for cert in &view.certificates {
            let cert_toggle = cert.clone();
            let cid_del = cert.id.as_str().to_owned();
            let host_label = if cert.host_pattern.is_empty() {
                "*".to_owned()
            } else {
                cert.host_pattern.clone()
            };
            let is_client = cert.kind == TlsCertificateKind::Client;
            let del_lbl = format!("{host_label} ({})", cert.cert_path);

            list = list.child(
                data_row(cx)
                    .py(px(8.0))
                    .child(badge(
                        if is_client {
                            "mTLS Client"
                        } else {
                            "Custom CA"
                        },
                        if is_client {
                            Tone::Accent
                        } else {
                            Tone::Success
                        },
                        cx,
                    ))
                    .child(
                        v_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .text_color(to_hsla(p.ink))
                                            .child(host_label),
                                    )
                                    .children(cert.has_passphrase.then(|| {
                                        meta_pill(
                                            crate::ui::Tr(view.lang)
                                                .environments()
                                                .tab_certs_passphrase_set(),
                                            cx,
                                        )
                                    })),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(if let Some(key) = &cert.key_path {
                                        format!("cert: {} · key: {key}", cert.cert_path)
                                    } else {
                                        format!("cert: {}", cert.cert_path)
                                    }),
                            ),
                    )
                    .child(
                        toggle_switch(
                            SharedString::from(format!("cert-en-{}", cert.id.as_str())),
                            cert.enabled,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_certificate(cert_toggle.clone(), cx);
                        })),
                    )
                    .child(
                        btn_icon(
                            SharedString::from(format!("cert-del-{}", cert.id.as_str())),
                            UiIcon::Trash,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.active_modal = Some(EnvModal::DeleteCert {
                                id: cid_del.clone(),
                                label: del_lbl.clone(),
                            });
                            cx.notify();
                        })),
                    ),
            );
        }

        div()
            .id("cert-list-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(
                v_col()
                    .w_full()
                    .p(px(16.0))
                    .gap(px(12.0))
                    .child(alert_banner(
                        Tone::Neutral,
                        crate::ui::tr(cx)
                            .environments()
                            .tab_certs_private_key_passphrases_are_stored(),
                        None,
                        cx,
                    ))
                    .child(list),
            )
            .into_any_element()
    };

    v_col()
        .size_full()
        .min_h_0()
        .child(bar)
        .child(div().flex_1().min_h_0().w_full().child(body))
        .into_any_element()
}
