//! Request Auth editor: all 8 schemes, Inheritance preview, and OAuth 2.0 Token Card.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::request_model_service::AuthSource;
use pingdeck_desktop::core::model::{
    ApiKeyLocation, AuthConfig, OAuth2ClientAuth, OAuth2GrantType,
};

use crate::ui::{
    ScrollableElement as _, Tone, badge, btn_ghost, btn_secondary, card, field_row, h_row, palette,
    section_heading, tab_bar_segmented, tab_item, text_input, to_hsla, v_col,
};

use super::model::AuthKind;
use super::{RequestsView, default_auth_for_kind};

pub fn render_auth_tab(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::AnyElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().into_any_element();
    };
    let kind = AuthKind::from_auth(&tab.draft.auth);
    let ent = cx.entity().clone();

    let seg = tab_bar_segmented("req-auth-kind-seg")
        .selected_index(kind.index())
        .child(tab_item(if zh { "继承" } else { "Inherit" }))
        .child(tab_item(if zh { "无" } else { "None" }))
        .child(tab_item("Bearer"))
        .child(tab_item("Basic"))
        .child(tab_item("API Key"))
        .child(tab_item("OAuth 2.0"))
        .child(tab_item("Digest"))
        .child(tab_item("SigV4"))
        .on_click(move |idx, window, cx| {
            let next = AuthKind::from_index(*idx);
            ent.update(cx, |this, cx| {
                this.snapshot_active_tab(cx);
                let next_cfg = default_auth_for_kind(next);
                if let Some(t) = this.active_req_mut() {
                    t.draft.auth = next_cfg.clone();
                    t.recompute_dirty();
                }
                this.hydrate_auth_inputs(&next_cfg, window, cx);
                this.refresh_effective_auth(cx);
                if next == AuthKind::OAuth2 {
                    this.oauth_refresh_status(cx);
                }
                cx.notify();
            });
        });

    let mut col = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(10.0))
        .overflow_y_scrollbar()
        .child(h_row().flex_wrap().gap(px(8.0)).child(seg));

    // Live effective-auth & masked preview card.
    if let Some(ea) = &tab.effective_auth {
        let src_lbl = match &ea.source {
            AuthSource::Collection { name, .. } => {
                if zh {
                    format!("继承自集合「{name}」")
                } else {
                    format!("Inherited from collection \"{name}\"")
                }
            }
            AuthSource::Folder { name, .. } => {
                if zh {
                    format!("继承自文件夹「{name}」")
                } else {
                    format!("Inherited from folder \"{name}\"")
                }
            }
            AuthSource::Request => if zh {
                "本请求独立配置"
            } else {
                "Defined on this request"
            }
            .into(),
            AuthSource::None => if zh {
                "当前无生效认证"
            } else {
                "No effective auth"
            }
            .into(),
        };
        if kind == AuthKind::Inherit || ea.preview.is_some() {
            let preview_str = ea
                .preview
                .as_ref()
                .map(|pr| format!("{}: {}", pr.name, pr.value))
                .unwrap_or_else(|| {
                    if zh {
                        "不发送认证头"
                    } else {
                        "No auth header"
                    }
                    .into()
                });
            col = col.child(
                card(cx).p(px(10.0)).child(
                    v_col()
                        .gap(px(4.0))
                        .child(
                            h_row()
                                .justify_between()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child(SharedString::from(src_lbl)),
                                )
                                .child(badge(
                                    if ea.preview.is_some() {
                                        "Active"
                                    } else {
                                        "None"
                                    },
                                    if ea.preview.is_some() {
                                        Tone::Success
                                    } else {
                                        Tone::Neutral
                                    },
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(SharedString::from(preview_str)),
                        ),
                ),
            );
        }
    }

    match &tab.draft.auth {
        AuthConfig::Inherit | AuthConfig::None => {}
        AuthConfig::Bearer { .. } => {
            col = col.child(field_row(
                "Bearer Token",
                Some(SharedString::from(if zh {
                    "推荐使用 {{token}} 引用环境机密变量"
                } else {
                    "Use {{token}} to reference a secret variable"
                })),
                div().w(px(360.0)).child(text_input(&view.auth_f1)),
                cx,
            ));
        }
        AuthConfig::Basic { .. } | AuthConfig::Digest { .. } => {
            col = col
                .child(field_row(
                    if zh { "用户名" } else { "Username" },
                    None,
                    div().w(px(320.0)).child(text_input(&view.auth_f1)),
                    cx,
                ))
                .child(field_row(
                    if zh {
                        "密码 / 引用"
                    } else {
                        "Password / ref"
                    },
                    None,
                    div().w(px(320.0)).child(text_input(&view.auth_f2)),
                    cx,
                ));
        }
        AuthConfig::ApiKey { location, .. } => {
            let loc_idx = usize::from(*location == ApiKeyLocation::Query);
            let ent_l = cx.entity().clone();
            col = col
                .child(field_row(
                    if zh { "参数名" } else { "Key name" },
                    None,
                    div().w(px(300.0)).child(text_input(&view.auth_f1)),
                    cx,
                ))
                .child(field_row(
                    if zh {
                        "值 / {{变量}}"
                    } else {
                        "Value / {{var}}"
                    },
                    None,
                    div().w(px(300.0)).child(text_input(&view.auth_f2)),
                    cx,
                ))
                .child(field_row(
                    if zh { "传递位置" } else { "Add to" },
                    None,
                    tab_bar_segmented("apikey-loc-seg")
                        .selected_index(loc_idx)
                        .child(tab_item("Header"))
                        .child(tab_item("Query"))
                        .on_click(move |idx, _, cx| {
                            ent_l.update(cx, |this, cx| {
                                if let Some(t) = this.active_req_mut()
                                    && let AuthConfig::ApiKey { location, .. } = &mut t.draft.auth
                                {
                                    *location = if *idx == 1 {
                                        ApiKeyLocation::Query
                                    } else {
                                        ApiKeyLocation::Header
                                    };
                                    t.recompute_dirty();
                                }
                                this.refresh_effective_auth(cx);
                                cx.notify();
                            });
                        }),
                    cx,
                ));
        }
        AuthConfig::OAuth2 {
            grant_type,
            client_auth,
            ..
        } => {
            let g_idx = match grant_type {
                OAuth2GrantType::ClientCredentials => 0,
                OAuth2GrantType::Password => 1,
                OAuth2GrantType::AuthorizationCodePkce => 2,
            };
            let ca_idx = usize::from(*client_auth == OAuth2ClientAuth::Basic);
            let ent_g = cx.entity().clone();
            let ent_ca = cx.entity().clone();

            col = col
                .child(field_row(
                    if zh { "授权类型" } else { "Grant Type" },
                    None,
                    tab_bar_segmented("oauth-grant-seg")
                        .selected_index(g_idx)
                        .child(tab_item("Client Credentials"))
                        .child(tab_item("Password"))
                        .child(tab_item("Auth Code PKCE"))
                        .on_click(move |idx, _, cx| {
                            ent_g.update(cx, |this, cx| {
                                if let Some(t) = this.active_req_mut()
                                    && let AuthConfig::OAuth2 { grant_type, .. } = &mut t.draft.auth
                                {
                                    *grant_type = match *idx {
                                        1 => OAuth2GrantType::Password,
                                        2 => OAuth2GrantType::AuthorizationCodePkce,
                                        _ => OAuth2GrantType::ClientCredentials,
                                    };
                                    t.recompute_dirty();
                                }
                                cx.notify();
                            });
                        }),
                    cx,
                ))
                .child(field_row(
                    "Access Token URL",
                    None,
                    div().w(px(360.0)).child(text_input(&view.auth_f2)),
                    cx,
                ))
                .child(field_row(
                    "Client ID",
                    None,
                    div().w(px(300.0)).child(text_input(&view.auth_f3)),
                    cx,
                ))
                .child(field_row(
                    "Client Secret",
                    None,
                    div().w(px(300.0)).child(text_input(&view.auth_f4)),
                    cx,
                ))
                .child(field_row(
                    "Scope",
                    None,
                    div().w(px(300.0)).child(text_input(&view.auth_f5)),
                    cx,
                ))
                .children(
                    (*grant_type == OAuth2GrantType::AuthorizationCodePkce).then(|| {
                        field_row(
                            "Authorization URL",
                            None,
                            div().w(px(360.0)).child(text_input(&view.auth_f6)),
                            cx,
                        )
                    }),
                )
                .children((*grant_type == OAuth2GrantType::Password).then(|| {
                    field_row(
                        if zh {
                            "用户名:密码"
                        } else {
                            "user:password"
                        },
                        None,
                        div().w(px(300.0)).child(text_input(&view.auth_f1)),
                        cx,
                    )
                }))
                .child(field_row(
                    if zh {
                        "客户端凭据位置"
                    } else {
                        "Client Auth"
                    },
                    None,
                    tab_bar_segmented("oauth-ca-seg")
                        .selected_index(ca_idx)
                        .child(tab_item("Body"))
                        .child(tab_item("Basic Header"))
                        .on_click(move |idx, _, cx| {
                            ent_ca.update(cx, |this, cx| {
                                if let Some(t) = this.active_req_mut()
                                    && let AuthConfig::OAuth2 { client_auth, .. } =
                                        &mut t.draft.auth
                                {
                                    *client_auth = if *idx == 1 {
                                        OAuth2ClientAuth::Basic
                                    } else {
                                        OAuth2ClientAuth::Body
                                    };
                                    t.recompute_dirty();
                                }
                                cx.notify();
                            });
                        }),
                    cx,
                ))
                // OAuth 2.0 Token Card
                .child(render_oauth_token_card(view, zh, cx));
        }
        AuthConfig::AwsSignatureV4 { .. } => {
            col = col
                .child(field_row(
                    "Access Key ID",
                    None,
                    div().w(px(320.0)).child(text_input(&view.auth_f1)),
                    cx,
                ))
                .child(field_row(
                    "Secret Access Key",
                    None,
                    div().w(px(320.0)).child(text_input(&view.auth_f2)),
                    cx,
                ))
                .child(field_row(
                    "AWS Region",
                    None,
                    div().w(px(220.0)).child(text_input(&view.auth_f3)),
                    cx,
                ))
                .child(field_row(
                    "Service Name",
                    None,
                    div().w(px(220.0)).child(text_input(&view.auth_f4)),
                    cx,
                ))
                .child(field_row(
                    "Session Token (optional)",
                    None,
                    div().w(px(320.0)).child(text_input(&view.auth_f5)),
                    cx,
                ));
        }
    }

    col.into_any_element()
}

fn render_oauth_token_card(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return div();
    };
    let busy = tab.oauth_busy;
    let info = tab.oauth_token_info.as_ref();

    card(cx).p(px(10.0)).child(
        v_col()
            .gap(px(8.0))
            .child(
                h_row()
                    .justify_between()
                    .child(section_heading(
                        if zh {
                            "OAuth 2.0 令牌卡片（存本机 0600 保险库）"
                        } else {
                            "OAuth 2.0 Token Card"
                        },
                        cx,
                    ))
                    .child(match info {
                        Some(ti) => badge(
                            format!("{} …{}", ti.token_type, ti.last4),
                            Tone::Success,
                            cx,
                        ),
                        None => badge(if zh { "未获取" } else { "No token" }, Tone::Neutral, cx),
                    }),
            )
            .child(
                h_row()
                    .gap(px(6.0))
                    .flex_wrap()
                    .child(
                        btn_secondary(
                            "oauth-fetch",
                            if busy {
                                if zh { "获取中…" } else { "Fetching…" }
                            } else if zh {
                                "获取新令牌"
                            } else {
                                "Get token"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.oauth_fetch_token(false, cx))),
                    )
                    .children(info.is_some_and(|i| i.has_refresh).then(|| {
                        btn_ghost("oauth-refresh", if zh { "刷新令牌" } else { "Refresh" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.oauth_fetch_token(true, cx);
                            }))
                    }))
                    .children(info.is_some().then(|| {
                        btn_ghost("oauth-clear", if zh { "清除" } else { "Clear" })
                            .on_click(cx.listener(|this, _, _, cx| this.oauth_clear_token(cx)))
                    }))
                    .children(info.is_some().then(|| {
                        h_row()
                            .gap(px(4.0))
                            .child(div().w(px(130.0)).child(text_input(&view.oauth_var_input)))
                            .child(
                                btn_ghost(
                                    "oauth-store-var",
                                    if zh {
                                        "存为环境变量"
                                    } else {
                                        "Store as var"
                                    },
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.oauth_store_token_as_var(cx);
                                    },
                                )),
                            )
                    })),
            )
            .children(info.filter(|i| !i.scope.is_empty()).map(|i| {
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(format!("Scope: {}", i.scope)))
            })),
    )
}
