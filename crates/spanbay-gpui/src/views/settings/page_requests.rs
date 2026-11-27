//! Settings › Request Defaults page: HTTP transport defaults, editor/viewer preferences,
//! and response size / preview thresholds — all live-validated and resettable per section.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, px};
use spanbay_desktop::core::model::models::HttpVersionPreference;
use spanbay_desktop::core::model::settings::SettingsSection as CoreSection;

use crate::ui::{
    Disableable as _, Tone, UiIcon, alert_banner, btn_ghost, btn_secondary, choice_chip, h_row,
    icon_xs, number_field, setting_row, settings_section, tab_bar_segmented, tab_item,
    toggle_switch, v_col,
};

use super::SettingsView;

pub fn render_requests(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
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
            crate::ui::tr(cx).settings().page_requests_default_timeout(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_requests_inherited_by_new_requests_1(),
            )),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.req_timeout_ms,
                    Some("ms"),
                    100.0,
                    cx,
                ))
                .child(
                    btn_secondary(
                        "req-save-timeout",
                        crate::ui::tr(cx).common().kv_bulk_apply(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        let v = this.inputs.req_timeout_ms.read(cx).value().to_string();
                        this.commit_u64(
                            &v,
                            1,
                            3_600_000,
                            CoreSection::Network,
                            |n| serde_json::json!({ "timeout_ms": n }),
                            cx,
                        );
                    })),
                ),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx)
                .settings()
                .page_requests_transport_retries(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_requests_automatic_retries_after_transport_level(),
            )),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.req_retry_count,
                    Some(crate::ui::tr(cx).settings().page_advanced_x()),
                    72.0,
                    cx,
                ))
                .child(
                    btn_secondary(
                        "req-save-retries",
                        crate::ui::tr(cx).common().kv_bulk_apply(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        let v = this.inputs.req_retry_count.read(cx).value().to_string();
                        this.commit_u64(
                            &v,
                            0,
                            10,
                            CoreSection::Network,
                            |n| serde_json::json!({ "retry_count": n }),
                            cx,
                        );
                    })),
                ),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx)
                .settings()
                .page_requests_preferred_http_version(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_requests_auto_negotiates_via_alpn_by(),
            )),
            tab_bar_segmented("req-http-ver-seg")
                .selected_index(http_idx)
                .child(tab_item(
                    crate::ui::tr(cx).settings().opt_network_http_version_auto(),
                ))
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
            crate::ui::tr(cx)
                .settings()
                .page_requests_follow_redirects(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_requests_automatically_follow_http_3xx_location(),
            )),
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
            crate::ui::tr(cx)
                .settings()
                .page_requests_max_redirect_hops(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_requests_maximum_redirect_hops_allowed_per(),
            )),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.req_max_redirects, None, 72.0, cx))
                .child(
                    btn_secondary("req-save-redir", crate::ui::tr(cx).common().kv_bulk_apply())
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
            crate::ui::tr(cx)
                .settings()
                .page_requests_verify_tls_certificates(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_requests_keep_enabled_whenever_possible_for(),
            )),
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
        crate::ui::tr(cx)
            .settings()
            .page_requests_http_request_defaults(),
        Some(
            btn_ghost(
                "req-reset-net",
                crate::ui::tr(cx).settings().page_defaults_reset(),
            )
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
            crate::ui::tr(cx)
                .settings()
                .page_requests_global_tls_verification_is_disabled(),
            None,
            cx,
        ));
    }

    let editor_sec = settings_section(
        crate::ui::tr(cx)
            .settings()
            .page_requests_editor_syntax_highlighting(),
        Some(
            btn_ghost(
                "req-reset-editor",
                crate::ui::tr(cx).settings().page_defaults_reset(),
            )
            .icon(icon_xs(UiIcon::RotateCcw))
            .on_click(cx.listener(|this, _, _, cx| {
                this.reset_section(CoreSection::Editor, cx);
            }))
            .into_any_element(),
        ),
        v_col()
            .w_full()
            .child(setting_row(
                crate::ui::tr(cx).settings().page_requests_tab_size(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_requests_number_of_spaces_inserted_by(),
                )),
                h_row()
                    .gap(px(6.0))
                    .children([2u8, 4, 8].into_iter().map(|spaces| {
                        choice_chip(
                            SharedString::from(format!("ed-tab-{spaces}")),
                            crate::ui::tr(cx).settings().page_requests_spaces(spaces),
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
                crate::ui::tr(cx).settings().f_editor_word_wrap(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_requests_wrap_long_lines_automatically_inside(),
                )),
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
                crate::ui::tr(cx)
                    .settings()
                    .page_requests_editor_highlight_limit(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_requests_request_bodies_above_this_size(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.editor_highlight_kib,
                        Some("KiB"),
                        96.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("ed-save-hl", crate::ui::tr(cx).common().kv_bulk_apply())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let v = this
                                    .inputs
                                    .editor_highlight_kib
                                    .read(cx)
                                    .value()
                                    .to_string();
                                this.commit_kib(
                                    &v,
                                    1,
                                    65_536,
                                    CoreSection::Editor,
                                    "editor_highlight_limit_bytes",
                                    cx,
                                );
                            })),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_requests_viewer_highlight_limit(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_requests_response_bodies_larger_than_this(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.viewer_highlight_kib,
                        Some("KiB"),
                        96.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("vw-save-hl", crate::ui::tr(cx).common().kv_bulk_apply())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let v = this
                                    .inputs
                                    .viewer_highlight_kib
                                    .read(cx)
                                    .value()
                                    .to_string();
                                this.commit_kib(
                                    &v,
                                    1,
                                    65_536,
                                    CoreSection::Editor,
                                    "viewer_highlight_limit_bytes",
                                    cx,
                                );
                            })),
                    ),
                cx,
            )),
        cx,
    );

    let response_sec = settings_section(
        crate::ui::tr(cx)
            .settings()
            .page_requests_response_large_body_thresholds(),
        Some(
            btn_ghost(
                "req-reset-resp",
                crate::ui::tr(cx).settings().page_defaults_reset(),
            )
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
                crate::ui::tr(cx)
                    .settings()
                    .page_requests_response_body_cap(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_requests_maximum_bytes_buffered_for_a(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.resp_body_cap_mib,
                        Some("MiB"),
                        84.0,
                        cx,
                    ))
                    .child(
                        btn_secondary("resp-save-cap", crate::ui::tr(cx).common().kv_bulk_apply())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let v = this.inputs.resp_body_cap_mib.read(cx).value().to_string();
                                this.commit_mib(
                                    &v,
                                    1,
                                    512,
                                    CoreSection::Data,
                                    "response_body_cap_bytes",
                                    cx,
                                );
                            })),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_requests_inline_body_threshold(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_requests_responses_up_to_this_size(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.resp_inline_kib,
                        Some("KiB"),
                        96.0,
                        cx,
                    ))
                    .child(
                        btn_secondary(
                            "resp-save-inline",
                            crate::ui::tr(cx).common().kv_bulk_apply(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let v = this.inputs.resp_inline_kib.read(cx).value().to_string();
                            this.commit_kib(
                                &v,
                                1,
                                65_536,
                                CoreSection::Data,
                                "inline_threshold_bytes",
                                cx,
                            );
                        })),
                    ),
                cx,
            ))
            .child(setting_row(
                crate::ui::tr(cx)
                    .settings()
                    .page_requests_large_body_preview_size(),
                None,
                Some(SharedString::from(
                    crate::ui::tr(cx)
                        .settings()
                        .page_requests_initial_preview_chunk_shown_for(),
                )),
                h_row()
                    .gap(px(6.0))
                    .child(number_field(
                        &view.inputs.resp_preview_kib,
                        Some("KiB"),
                        96.0,
                        cx,
                    ))
                    .child(
                        btn_secondary(
                            "resp-save-preview",
                            crate::ui::tr(cx).common().kv_bulk_apply(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let v = this.inputs.resp_preview_kib.read(cx).value().to_string();
                            this.commit_kib(&v, 1, 65_536, CoreSection::Data, "preview_bytes", cx);
                        })),
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
