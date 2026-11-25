//! Per-request Network options: all 7 follow-settings/override rows and transport toggles,
//! styled consistently with PingDeck Settings via `ui::{section_header, setting_row, choice_chip, number_field}`.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::NetworkField;
use pingdeck_desktop::core::model::models::HttpVersionPreference;
use pingdeck_desktop::state::SettingsSection;

use crate::ui::{
    ScrollableElement as _, btn_ghost, choice_chip, h_row, number_field, palette, section_header,
    setting_row, tab_bar_segmented, tab_item, text_input, to_hsla, toggle_switch, v_col,
};

use super::RequestsView;
use super::model::RequestsHandoff;

pub fn render_network_tab(
    view: &RequestsView,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::AnyElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().into_any_element();
    };
    let net = &tab.draft.network_options;
    let global = &view.settings.network;

    let inherit_pill = |field: NetworkField, id: &'static str, cx: &mut Context<RequestsView>| {
        let inherited = net.inherit.contains(&field);
        choice_chip(
            id,
            if inherited {
                crate::ui::tr(cx).requests().editor_network_follow_global()
            } else {
                crate::ui::tr(cx).requests().editor_network_override()
            },
            !inherited,
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(t) = this.active_req_mut() {
                if let Some(pos) = t
                    .draft
                    .network_options
                    .inherit
                    .iter()
                    .position(|f| *f == field)
                {
                    t.draft.network_options.inherit.remove(pos);
                } else {
                    t.draft.network_options.inherit.push(field);
                }
                t.recompute_dirty();
            }
            cx.notify();
        }))
    };

    let hv_idx = match net.http_version {
        HttpVersionPreference::Auto => 0,
        HttpVersionPreference::Http10 => 1,
        HttpVersionPreference::Http11 => 2,
        HttpVersionPreference::Http2 => 3,
    };
    let ent_hv = cx.entity().clone();

    v_col()
        .size_full()
        .w_full()
        .p(px(14.0))
        .gap(px(4.0))
        .overflow_y_scrollbar()
        .child(section_header(
            crate::ui::tr(cx)
                .requests()
                .editor_network_per_request_network_options(),
            Some(
                btn_ghost(
                    "net-open-global",
                    crate::ui::tr(cx)
                        .requests()
                        .editor_network_network_settings(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.emit_handoff(RequestsHandoff::OpenSettings(SettingsSection::Network), cx);
                }))
                .into_any_element(),
            ),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx).requests().editor_network_idle_timeout(),
            Some(SharedString::from(format!(
                "{}: {} ms",
                crate::ui::tr(cx).requests().editor_network_global(),
                global.timeout_ms
            ))),
            None,
            h_row()
                .gap(px(6.0))
                .child(inherit_pill(NetworkField::TimeoutMs, "inh-to", cx))
                .child(number_field(&view.net_timeout_input, Some("ms"), 104.0, cx)),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx)
                .requests()
                .editor_network_total_timeout_2(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .requests()
                    .editor_network_leave_blank_for_no_total(),
            )),
            number_field(&view.net_total_timeout_input, Some("ms"), 120.0, cx),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx).requests().editor_network_retries(),
            Some(SharedString::from(format!(
                "{}: {}",
                crate::ui::tr(cx).requests().editor_network_global(),
                global.retry_count
            ))),
            None,
            h_row()
                .gap(px(6.0))
                .child(inherit_pill(NetworkField::RetryCount, "inh-ret", cx))
                .child(number_field(&view.net_retry_input, None, 78.0, cx)),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx).requests().editor_network_http_version(),
            Some(SharedString::from(format!(
                "{}: {:?}",
                crate::ui::tr(cx).requests().editor_network_global(),
                global.http_version
            ))),
            None,
            h_row()
                .gap(px(6.0))
                .child(inherit_pill(NetworkField::HttpVersion, "inh-hv", cx))
                .child(
                    tab_bar_segmented("net-hv-seg")
                        .selected_index(hv_idx)
                        .child(tab_item("Auto"))
                        .child(tab_item("1.0"))
                        .child(tab_item("1.1"))
                        .child(tab_item("2"))
                        .on_click(move |idx, _, cx| {
                            ent_hv.update(cx, |this, cx| {
                                if let Some(t) = this.active_req_mut() {
                                    t.draft.network_options.http_version = match *idx {
                                        1 => HttpVersionPreference::Http10,
                                        2 => HttpVersionPreference::Http11,
                                        3 => HttpVersionPreference::Http2,
                                        _ => HttpVersionPreference::Auto,
                                    };
                                    t.recompute_dirty();
                                }
                                cx.notify();
                            });
                        }),
                ),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx)
                .requests()
                .editor_network_follow_redirects_2(),
            Some(SharedString::from(format!(
                "{}: {}",
                crate::ui::tr(cx).requests().editor_network_global(),
                if global.follow_redirects {
                    crate::ui::tr(cx).requests().editor_network_on()
                } else {
                    crate::ui::tr(cx).requests().editor_network_off()
                }
            ))),
            None,
            h_row()
                .gap(px(6.0))
                .child(inherit_pill(NetworkField::FollowRedirects, "inh-red", cx))
                .child(
                    toggle_switch("net-follow-red", net.follow_redirects).on_change(cx.listener(
                        |this, c: &bool, _, cx| {
                            if let Some(t) = this.active_req_mut() {
                                t.draft.network_options.follow_redirects = *c;
                                t.recompute_dirty();
                            }
                            cx.notify();
                        },
                    )),
                ),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx)
                .requests()
                .editor_network_max_redirects_2(),
            Some(SharedString::from(format!(
                "{}: {}",
                crate::ui::tr(cx).requests().editor_network_global(),
                global.max_redirects
            ))),
            None,
            h_row()
                .gap(px(6.0))
                .child(inherit_pill(NetworkField::MaxRedirects, "inh-maxr", cx))
                .child(number_field(&view.net_max_redir_input, None, 78.0, cx)),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx).requests().editor_network_verify_tls_2(),
            Some(SharedString::from(format!(
                "{}: {}",
                crate::ui::tr(cx).requests().editor_network_global(),
                if global.verify_tls {
                    crate::ui::tr(cx).requests().editor_network_on()
                } else {
                    crate::ui::tr(cx).requests().editor_network_off()
                }
            ))),
            None,
            h_row()
                .gap(px(6.0))
                .child(inherit_pill(NetworkField::VerifyTls, "inh-tls", cx))
                .child(
                    toggle_switch("net-verify-tls", net.verify_tls).on_change(cx.listener(
                        |this, c: &bool, _, cx| {
                            if let Some(t) = this.active_req_mut() {
                                t.draft.network_options.verify_tls = *c;
                                t.recompute_dirty();
                            }
                            cx.notify();
                        },
                    )),
                ),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx).requests().editor_network_proxy_url(),
            Some(SharedString::from(format!(
                "{}: {}",
                crate::ui::tr(cx).requests().editor_network_global(),
                if global.proxy_url.trim().is_empty() {
                    crate::ui::tr(cx).requests().editor_network_system()
                } else {
                    global.proxy_url.as_str()
                }
            ))),
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .requests()
                    .editor_network_leave_blank_when_overriding_for(),
            )),
            h_row()
                .gap(px(6.0))
                .child(inherit_pill(NetworkField::ProxyUrl, "inh-prx", cx))
                .child(div().w(px(220.0)).child(text_input(&view.net_proxy_input))),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx).requests().editor_network_send_cookies(),
            None,
            None,
            toggle_switch("net-cookies", net.send_cookies).on_change(cx.listener(
                |this, c: &bool, _, cx| {
                    if let Some(t) = this.active_req_mut() {
                        t.draft.network_options.send_cookies = *c;
                        t.recompute_dirty();
                    }
                    cx.notify();
                },
            )),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx)
                .requests()
                .editor_network_auto_encode_url(),
            None,
            None,
            toggle_switch("net-encode", net.encode_url_automatically).on_change(cx.listener(
                |this, c: &bool, _, cx| {
                    if let Some(t) = this.active_req_mut() {
                        t.draft.network_options.encode_url_automatically = *c;
                        t.recompute_dirty();
                    }
                    cx.notify();
                },
            )),
            cx,
        ))
        .child(
            div()
                .pt(px(8.0))
                .text_size(px(11.0))
                .text_color(to_hsla(p.faint))
                .child(
                    crate::ui::tr(cx)
                        .requests()
                        .editor_network_forwarding_authorization_across_redirects(),
                ),
        )
        .into_any_element()
}
