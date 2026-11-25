//! Utility tabs backed by core services: Sync, Cookies, TLS Certificates, and Collection Runner.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_danger, btn_primary, btn_secondary, card,
    empty_state, field_row, h_row, section_heading, tr, v_col,
};

use super::UtilityPanel;

pub fn render_sync(panel: &UtilityPanel, cx: &mut Context<UtilityPanel>) -> gpui_kit::AnyElement {
    let t = tr(cx);
    let u = t.utility();
    let c = t.common();
    let mut col = v_col().size_full().gap(px(12.0)).overflow_y_scrollbar();
    if let Some(st) = &panel.sync_status {
        let dir = if st.directory.is_empty() {
            u.sync_not_configured().to_owned()
        } else {
            st.directory.clone()
        };
        col = col.child(
            card(cx).p(px(14.0)).child(
                v_col()
                    .child(field_row(
                        u.sync_field_dir(),
                        None,
                        badge(SharedString::from(dir), Tone::Neutral, cx),
                        cx,
                    ))
                    .child(field_row(
                        u.sync_field_auto(),
                        None,
                        badge(
                            if st.auto_export { c.on() } else { c.off() },
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
                        u.sync_field_secrets(),
                        None,
                        badge(
                            if st.include_secrets { c.yes() } else { c.no() },
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
                btn_primary("util-sync-exp", u.sync_export_now())
                    .on_click(cx.listener(|this, _, _, cx| this.run_sync_export(cx))),
            )
            .child(
                btn_secondary("util-sync-imp", u.sync_import_folder())
                    .on_click(cx.listener(|this, _, _, cx| this.run_sync_import(cx))),
            ),
    );
    col.into_any_element()
}

pub fn render_cookies(
    panel: &UtilityPanel,
    cx: &mut Context<UtilityPanel>,
) -> gpui_kit::AnyElement {
    let u = tr(cx).utility();
    let mut col = v_col()
        .size_full()
        .gap(px(10.0))
        .overflow_y_scrollbar()
        .child(
            h_row()
                .justify_between()
                .child(section_heading(u.cookies_heading(panel.cookies.len()), cx))
                .child(
                    btn_secondary("util-ck-exp", u.cookies_clear_expired())
                        .on_click(cx.listener(|this, _, _, cx| this.clear_expired_cookies(cx))),
                ),
        );
    if panel.cookies.is_empty() {
        col = col.child(empty_state(
            UiIcon::Cookie,
            u.cookies_empty_title(),
            u.cookies_empty_desc(),
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

pub fn render_certs(panel: &UtilityPanel, cx: &mut Context<UtilityPanel>) -> gpui_kit::AnyElement {
    let t = tr(cx);
    let u = t.utility();
    let c = t.common();
    let mut col = v_col()
        .size_full()
        .gap(px(10.0))
        .overflow_y_scrollbar()
        .child(section_heading(
            u.certs_heading(panel.certificates.len()),
            cx,
        ));
    if panel.certificates.is_empty() {
        col = col.child(empty_state(
            UiIcon::Shield,
            u.certs_empty_title(),
            u.certs_empty_desc(),
            cx,
        ));
    } else {
        for cert in &panel.certificates {
            let cid = cert.id.as_str().to_owned();
            col = col.child(
                card(cx).p(px(10.0)).child(
                    h_row()
                        .justify_between()
                        .child(div().text_xs().child(SharedString::from(format!(
                            "{} — {}",
                            cert.host_pattern, cert.cert_path
                        ))))
                        .child(
                            btn_danger(SharedString::from(format!("cert-del-{cid}")), c.delete())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_cert(cid.clone(), cx);
                                })),
                        ),
                ),
            );
        }
    }
    col.into_any_element()
}

pub fn render_runner(panel: &UtilityPanel, cx: &mut Context<UtilityPanel>) -> gpui_kit::AnyElement {
    let t = tr(cx);
    let u = t.utility();
    let c = t.common();
    let mut col = v_col()
        .size_full()
        .gap(px(10.0))
        .overflow_y_scrollbar()
        .child(section_heading(
            u.runner_heading(panel.collections.len()),
            cx,
        ));
    if panel.collections.is_empty() {
        col = col.child(empty_state(
            UiIcon::Play,
            u.runner_empty_title(),
            u.runner_empty_desc(),
            cx,
        ));
    } else {
        for col_item in &panel.collections {
            let cid = col_item.id.as_str().to_owned();
            col = col.child(
                card(cx).p(px(10.0)).child(
                    h_row()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .child(SharedString::from(col_item.name.clone())),
                        )
                        .child(
                            btn_primary(
                                SharedString::from(format!("run-col-{cid}")),
                                if panel.running_collection {
                                    c.running()
                                } else {
                                    c.run()
                                },
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.run_collection(cid.clone(), cx);
                                },
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
