//! Workspace Cookie Jar management tab for `EnvironmentsView`.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_danger, btn_ghost, btn_icon,
    btn_primary, btn_secondary, card, choice_chip, empty_inline, empty_state, h_row, icon_xs,
    kv_line, list_row, meta_pill, palette, search_input, section_header, setting_row, text_input,
    to_hsla, toggle_switch, toolbar_strip, v_col,
};

use super::state::{EnvModal, EnvironmentsView};

pub fn render_cookies_tab(
    view: &EnvironmentsView,
    cx: &mut Context<EnvironmentsView>,
) -> AnyElement {
    let p = palette(cx);
    let q = view
        .cookie_search_input
        .read(cx)
        .value()
        .trim()
        .to_lowercase();
    let filtered: Vec<_> = view
        .cookies
        .iter()
        .filter(|c| {
            q.is_empty()
                || c.domain.to_lowercase().contains(&q)
                || c.name.to_lowercase().contains(&q)
                || c.path.to_lowercase().contains(&q)
        })
        .collect();
    let expired_count = view.cookies.iter().filter(|c| c.expired).count();

    let bar = toolbar_strip(cx)
        .justify_between()
        .child(
            h_row()
                .gap(px(8.0))
                .child(
                    div()
                        .w(px(220.0))
                        .child(search_input(&view.cookie_search_input, cx)),
                )
                .child(meta_pill(
                    if view.is_zh() {
                        format!("共 {} 条", view.cookies.len())
                    } else {
                        format!("{} cookies", view.cookies.len())
                    },
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .child(
                    btn_primary("ck-add-btn", view.t("+ 添加 Cookie", "+ Add Cookie")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = Some(EnvModal::AddCookie {
                                secure: true,
                                http_only: false,
                            });
                            cx.notify();
                        }),
                    ),
                )
                .children((expired_count > 0).then(|| {
                    btn_secondary(
                        "ck-rm-exp",
                        if view.is_zh() {
                            format!("清理过期 ({expired_count})")
                        } else {
                            format!("Clean Expired ({expired_count})")
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.delete_expired_cookies(cx)))
                }))
                .child(
                    btn_ghost("ck-import", view.t("导入", "Import"))
                        .icon(icon_xs(UiIcon::Upload))
                        .on_click(cx.listener(|this, _, _, cx| this.import_cookies_file(cx))),
                )
                .child(
                    btn_ghost("ck-export", view.t("导出", "Export"))
                        .icon(icon_xs(UiIcon::Download))
                        .on_click(cx.listener(|this, _, _, cx| this.export_cookies_file(cx))),
                )
                .child(
                    btn_icon("ck-refresh", UiIcon::Refresh).on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(ws) = this.workspace_id.clone() {
                                this.reload_cookies(ws, cx);
                            }
                        },
                    )),
                ),
        );

    let mut list = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if filtered.is_empty() {
        list = list.child(empty_inline(
            view.t("Cookie 罐为空", "Cookie jar is empty"),
            Some(SharedString::from(view.t(
                "响应 Set-Cookie 会自动存入此处，也可手动添加或导入 Netscape cookies.txt / JSON。",
                "Response Set-Cookie headers populate here automatically, or import cookies.txt / JSON.",
            ))),
            None,
            cx,
        ));
    } else {
        for ck in &filtered {
            let cid = ck.id.as_str().to_owned();
            let active = view.selected_cookie_id.as_deref() == Some(ck.id.as_str());
            list =
                list.child(
                    list_row(
                        SharedString::from(format!("ck-row-{}", ck.id.as_str())),
                        active,
                        cx,
                        cx.listener(move |this, _, _, cx| {
                            this.selected_cookie_id = Some(cid.clone());
                            this.pending_sync = true;
                            cx.notify();
                        }),
                    )
                    .items_start()
                    .py(px(7.0))
                    .child(
                        v_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(
                                h_row()
                                    .w_full()
                                    .justify_between()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .text_color(to_hsla(p.ink))
                                            .truncate()
                                            .child(ck.name.clone()),
                                    )
                                    .children(ck.expired.then(|| {
                                        badge(view.t("已过期", "Expired"), Tone::Danger, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(format!("{}{}", ck.domain, ck.path)),
                            ),
                    ),
                );
        }
    }

    let selected = view
        .selected_cookie_id
        .as_deref()
        .and_then(|id| view.cookies.iter().find(|c| c.id.as_str() == id))
        .cloned();

    let detail = if let Some(ck) = selected {
        let cid_save = ck.id.as_str().to_owned();
        let cid_sec = ck.id.as_str().to_owned();
        let cid_http = ck.id.as_str().to_owned();
        let cid_del = ck.id.as_str().to_owned();
        let cur_sec = ck.secure;
        let cur_http = ck.http_only;
        let cur_ss = ck.same_site.clone();
        let cur_ss_http = ck.same_site.clone();
        let ss_opts: [(Option<&str>, &str); 4] = [
            (None, "Default"),
            (Some("Lax"), "Lax"),
            (Some("Strict"), "Strict"),
            (Some("None"), "None"),
        ];
        let mut ss_row = h_row().gap(px(4.0));
        for (opt, lbl) in ss_opts {
            let cid_ss = ck.id.as_str().to_owned();
            let active = cur_ss.as_deref() == opt;
            let next_ss = opt.map(str::to_owned);
            ss_row = ss_row.child(
                choice_chip(SharedString::from(format!("ck-ss-{lbl}")), lbl, active, cx).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.update_cookie_flags(
                            cid_ss.clone(),
                            cur_sec,
                            cur_http,
                            next_ss.clone(),
                            cx,
                        );
                    }),
                ),
            );
        }

        div()
            .id("ck-detail-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .p(px(16.0))
            .child(
                v_col()
                    .w_full()
                    .gap(px(12.0))
                    .child(
                        card(cx)
                            .p(px(14.0))
                            .gap(px(8.0))
                            .child(section_header(
                                format!("{} ({}{})", ck.name, ck.domain, ck.path),
                                Some(
                                    btn_danger("ck-del-btn", view.t("删除", "Delete"))
                                        .icon(icon_xs(UiIcon::Trash))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.delete_cookie(cid_del.clone(), cx);
                                        }))
                                        .into_any_element(),
                                ),
                                cx,
                            ))
                            .child(
                                h_row()
                                    .w_full()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(text_input(&view.cookie_edit_val)),
                                    )
                                    .child(
                                        btn_primary("ck-save-val", view.t("更新值", "Save Value"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.save_cookie_value(cid_save.clone(), cx);
                                            })),
                                    ),
                            )
                            .child(kv_line(
                                view.t("作用域", "Scope"),
                                if ck.host_only {
                                    format!("{} (Host-Only)", ck.domain)
                                } else {
                                    format!(".{} (Subdomains)", ck.domain)
                                },
                                120.0,
                                cx,
                            ))
                            .child(kv_line(
                                view.t("过期时间", "Expires"),
                                ck.expires.clone().unwrap_or_else(|| {
                                    view.t("会话级 (Session)", "Session").to_owned()
                                }),
                                120.0,
                                cx,
                            )),
                    )
                    .child(
                        card(cx)
                            .p(px(14.0))
                            .child(section_header(
                                view.t("安全标志 Flags", "Security Flags"),
                                None,
                                cx,
                            ))
                            .child(setting_row(
                                "Secure",
                                None,
                                Some(SharedString::from(
                                    view.t("仅在 HTTPS 连接发送。", "Sent only over HTTPS."),
                                )),
                                toggle_switch("ck-flag-sec", cur_sec).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.update_cookie_flags(
                                            cid_sec.clone(),
                                            !cur_sec,
                                            cur_http,
                                            cur_ss.clone(),
                                            cx,
                                        );
                                    },
                                )),
                                cx,
                            ))
                            .child(setting_row(
                                "HttpOnly",
                                None,
                                None,
                                toggle_switch("ck-flag-http", cur_http).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.update_cookie_flags(
                                            cid_http.clone(),
                                            cur_sec,
                                            !cur_http,
                                            cur_ss_http.clone(),
                                            cx,
                                        );
                                    },
                                )),
                                cx,
                            ))
                            .child(setting_row("SameSite", None, None, ss_row, cx)),
                    ),
            )
            .into_any_element()
    } else {
        empty_state(
            UiIcon::Cookie,
            view.t("选择一个 Cookie", "Select a Cookie"),
            view.t(
                "查看和修改 Cookie 值、Secure / HttpOnly / SameSite 属性。",
                "Inspect and edit cookie values and security flags.",
            ),
            cx,
        )
        .into_any_element()
    };

    let mut page = v_col().size_full().min_h_0().child(bar);
    if view.deleted_cookies_undo.is_some() {
        page = page.child(
            div().px(px(12.0)).pt(px(8.0)).child(alert_banner(
                Tone::Warning,
                view.t(
                    "已删除 Cookie，可立即撤销恢复。",
                    "Deleted cookie — undo to restore.",
                ),
                Some(
                    btn_secondary("ck-undo-btn", view.t("撤销", "Undo"))
                        .icon(icon_xs(UiIcon::RotateCcw))
                        .on_click(cx.listener(|this, _, _, cx| this.undo_delete_cookies(cx)))
                        .into_any_element(),
                ),
                cx,
            )),
        );
    }

    page.child(
        h_row()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .id("ck-list-scroll")
                    .w(px(300.0))
                    .flex_shrink_0()
                    .h_full()
                    .bg(to_hsla(p.pane))
                    .border_r_1()
                    .border_color(to_hsla(p.line.composite_over(p.pane)))
                    .overflow_y_scrollbar()
                    .child(list),
            )
            .child(div().flex_1().min_w_0().h_full().child(detail)),
    )
    .into_any_element()
}
