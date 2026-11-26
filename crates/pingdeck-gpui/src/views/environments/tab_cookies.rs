//! Workspace Cookie Jar management tab for `EnvironmentsView`.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};

use crate::ui::{
    PaginationBar, ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_danger,
    btn_ghost, btn_icon, btn_primary, btn_secondary, card, choice_chip, empty_inline, empty_state,
    h_row, icon_xs, kv_line, list_row, meta_pill, page_slice, pagination_bar, palette,
    search_input, section_header, setting_row, text_input, to_hsla, toggle_switch, toolbar_strip,
    v_col,
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
                    crate::ui::Tr(view.lang)
                        .environments()
                        .tab_cookies_cookies(view.cookies.len()),
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .child(
                    btn_primary(
                        "ck-add-btn",
                        crate::ui::Tr(view.lang)
                            .environments()
                            .tab_cookies_add_cookie(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = Some(EnvModal::AddCookie {
                            secure: true,
                            http_only: false,
                        });
                        cx.notify();
                    })),
                )
                .children((expired_count > 0).then(|| {
                    btn_secondary(
                        "ck-rm-exp",
                        crate::ui::Tr(view.lang)
                            .environments()
                            .tab_cookies_clean_expired(expired_count),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.delete_expired_cookies(cx)))
                }))
                .children((!view.cookies.is_empty()).then(|| {
                    btn_danger(
                        "ck-clear-all",
                        crate::ui::Tr(view.lang)
                            .environments()
                            .tab_cookies_clear_all(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.clear_all_cookies(cx)))
                }))
                .child(
                    btn_ghost(
                        "ck-import",
                        crate::ui::Tr(view.lang).environments().tab_cookies_import(),
                    )
                    .icon(icon_xs(UiIcon::Upload))
                    .on_click(cx.listener(|this, _, _, cx| this.import_cookies_file(cx))),
                )
                .child(
                    btn_ghost(
                        "ck-export",
                        crate::ui::Tr(view.lang).environments().tab_cookies_export(),
                    )
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

    const COOKIES_PAGE_SIZE: usize = 50;
    let (page_rows, clamped_page, _) = page_slice(&filtered, view.cookies_page, COOKIES_PAGE_SIZE);
    let mut list = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if filtered.is_empty() {
        list = list.child(empty_inline(
            crate::ui::Tr(view.lang)
                .environments()
                .tab_cookies_cookie_jar_is_empty(),
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .environments()
                    .tab_cookies_response_set_cookie_headers_populate(),
            )),
            None,
            cx,
        ));
    } else {
        for ck in page_rows {
            let cid = ck.id.as_str().to_owned();
            let active = view.selected_cookie_id.as_deref() == Some(ck.id.as_str());
            list = list.child(
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
                                    badge(
                                        crate::ui::Tr(view.lang)
                                            .environments()
                                            .tab_cookies_expired(),
                                        Tone::Danger,
                                        cx,
                                    )
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
                                    btn_danger(
                                        "ck-del-btn",
                                        crate::ui::Tr(view.lang).common().delete(),
                                    )
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
                                        btn_primary(
                                            "ck-save-val",
                                            crate::ui::Tr(view.lang)
                                                .environments()
                                                .tab_cookies_save_value(),
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.save_cookie_value(cid_save.clone(), cx);
                                            }),
                                        ),
                                    ),
                            )
                            .child(kv_line(
                                crate::ui::Tr(view.lang).environments().tab_cookies_scope(),
                                if ck.host_only {
                                    format!("{} (Host-Only)", ck.domain)
                                } else {
                                    format!(".{} (Subdomains)", ck.domain)
                                },
                                120.0,
                                cx,
                            ))
                            .child(kv_line(
                                crate::ui::Tr(view.lang)
                                    .environments()
                                    .tab_cookies_expires(),
                                ck.expires.clone().unwrap_or_else(|| {
                                    crate::ui::Tr(view.lang)
                                        .environments()
                                        .tab_cookies_session()
                                        .to_owned()
                                }),
                                120.0,
                                cx,
                            )),
                    )
                    .child(
                        card(cx)
                            .p(px(14.0))
                            .child(section_header(
                                crate::ui::Tr(view.lang)
                                    .environments()
                                    .tab_cookies_security_flags(),
                                None,
                                cx,
                            ))
                            .child(setting_row(
                                "Secure",
                                None,
                                Some(SharedString::from(
                                    crate::ui::Tr(view.lang)
                                        .environments()
                                        .tab_cookies_sent_only_over_https(),
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
            crate::ui::Tr(view.lang)
                .environments()
                .tab_cookies_select_a_cookie(),
            crate::ui::Tr(view.lang)
                .environments()
                .tab_cookies_inspect_and_edit_cookie_values(),
            cx,
        )
        .into_any_element()
    };

    let mut page = v_col().size_full().min_h_0().child(bar);
    if view.deleted_cookies_undo.is_some() {
        page = page.child(
            div().px(px(12.0)).pt(px(8.0)).child(alert_banner(
                Tone::Warning,
                crate::ui::Tr(view.lang)
                    .environments()
                    .tab_cookies_deleted_cookie_undo_to_restore(),
                Some(
                    btn_secondary("ck-undo-btn", crate::ui::Tr(view.lang).common().undo())
                        .icon(icon_xs(UiIcon::RotateCcw))
                        .on_click(cx.listener(|this, _, _, cx| this.undo_delete_cookies(cx)))
                        .into_any_element(),
                ),
                cx,
            )),
        );
    }

    let mut left_pane = v_col()
        .w(px(300.0))
        .flex_shrink_0()
        .h_full()
        .bg(to_hsla(p.pane))
        .border_r_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            div()
                .id("ck-list-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(list),
        );
    if filtered.len() > COOKIES_PAGE_SIZE {
        left_pane = left_pane.child(
            div()
                .flex_shrink_0()
                .px(px(10.0))
                .py(px(7.0))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(pagination_bar(
                    PaginationBar {
                        id_prefix: "ck-pg",
                        page: clamped_page,
                        page_size: COOKIES_PAGE_SIZE,
                        total: filtered.len(),
                        page_sizes: &[],
                        loading: false,
                        compact: true,
                    },
                    |this: &mut EnvironmentsView, next_page, cx| {
                        this.cookies_page = next_page;
                        cx.notify();
                    },
                    None::<fn(&mut EnvironmentsView, usize, &mut Context<EnvironmentsView>)>,
                    cx,
                )),
        );
    }

    page.child(
        h_row()
            .flex_1()
            .min_h_0()
            .child(left_pane)
            .child(div().flex_1().min_w_0().h_full().child(detail)),
    )
    .into_any_element()
}
