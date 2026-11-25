//! Routes sub-tab: route list, method/path/mode editor, CRUD bucket configuration, and response branch editor.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::app::factory_service::mock_crud_endpoints;
use pingdeck_desktop::core::model::items::{MockResponseMode, MockRouteType};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_primary, btn_secondary,
    card, checkbox, choice_chip, data_row, empty_inline, empty_state, h_row, icon_xs, list_row,
    meta_pill, method_badge, palette, search_input, section_header, table_header, text_input,
    to_hsla, v_col,
};

use super::state::MockView;
use super::tab_route_detail::render_response_branches;
use super::tab_route_strip::{render_live_traffic_strip, render_try_it_card};

const HTTP_METHODS: [&str; 8] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "ANY",
];

pub fn render_routes_tab(view: &MockView, cx: &mut Context<MockView>) -> AnyElement {
    let p = palette(cx);
    let Some(srv) = &view.draft else {
        return empty_state(
            UiIcon::Server,
            crate::ui::Tr(view.lang)
                .mock()
                .tab_routes_no_server_selected(),
            "",
            cx,
        )
        .into_any_element();
    };

    // Left route list column
    let route_q = view
        .route_search_input
        .read(cx)
        .value()
        .trim()
        .to_lowercase();
    let enabled_cnt = srv.routes.iter().filter(|r| r.enabled).count();
    let filtered_routes: Vec<_> = srv
        .routes
        .iter()
        .filter(|r| {
            route_q.is_empty()
                || r.path.to_lowercase().contains(&route_q)
                || r.method.to_lowercase().contains(&route_q)
                || r.description.to_lowercase().contains(&route_q)
        })
        .collect();
    let mut route_list = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if filtered_routes.is_empty() {
        route_list = route_list.child(empty_inline(
            if srv.routes.is_empty() {
                crate::ui::Tr(view.lang).mock().tab_routes_no_routes_yet()
            } else {
                crate::ui::Tr(view.lang)
                    .mock()
                    .tab_routes_no_matching_routes()
            },
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .mock()
                    .tab_routes_add_an_http_or_crud(),
            )),
            None,
            cx,
        ));
    } else {
        for route in filtered_routes {
            let rid = route.id.clone();
            let rid_toggle = route.id.clone();
            let rid_dup = route.id.clone();
            let rid_del = route.id.clone();
            let rid_test = route.id.clone();
            let active = view.selected_route_id.as_deref() == Some(route.id.as_str());
            let is_crud = route.route_type == MockRouteType::Crud;

            route_list = route_list.child(
                list_row(
                    SharedString::from(format!("mock-route-{}", route.id)),
                    active,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.snapshot_inputs(cx);
                        this.selected_route_id = Some(rid.clone());
                        this.selected_response_id = None;
                        this.pending_sync = true;
                        cx.notify();
                    }),
                )
                .py(px(6.0))
                .child(
                    checkbox(
                        SharedString::from(format!("mock-rt-en-{}", route.id)),
                        route.enabled,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if let Some(srv) = this.draft.as_mut()
                            && let Some(r) = srv.routes.iter_mut().find(|x| x.id == rid_toggle)
                        {
                            r.enabled = !r.enabled;
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                )
                .child(if is_crud {
                    badge("CRUD", Tone::Accent, cx)
                } else {
                    method_badge(&route.method, cx)
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(if route.enabled {
                            to_hsla(p.ink)
                        } else {
                            to_hsla(p.muted)
                        })
                        .truncate()
                        .child(route.path.clone()),
                )
                .child(
                    h_row()
                        .gap(px(2.0))
                        .flex_shrink_0()
                        .child(
                            btn_icon(
                                SharedString::from(format!("mock-rt-test-{}", route.id)),
                                UiIcon::Send,
                            )
                            .tooltip(
                                crate::ui::Tr(view.lang)
                                    .mock()
                                    .tab_routes_test_in_requests(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.test_route_in_requests(&rid_test, cx);
                                },
                            )),
                        )
                        .child(
                            btn_icon(
                                SharedString::from(format!("mock-rt-dup-{}", route.id)),
                                UiIcon::Copy,
                            )
                            .tooltip(crate::ui::Tr(view.lang).mock().tab_routes_duplicate_route())
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.duplicate_route(rid_dup.clone(), cx);
                                },
                            )),
                        )
                        .child(
                            btn_icon(
                                SharedString::from(format!("mock-rt-del-{}", route.id)),
                                UiIcon::Trash,
                            )
                            .tooltip(crate::ui::Tr(view.lang).mock().route_delete())
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.delete_route(rid_del.clone(), cx);
                                },
                            )),
                        ),
                ),
            );
        }
    }

    let left_col = v_col()
        .w(px(280.0))
        .flex_shrink_0()
        .h_full()
        .bg(to_hsla(p.pane))
        .border_r_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(crate::ui::tr(cx).mock().tab_routes_routes(srv.routes.len())),
                )
                .child(
                    h_row()
                        .gap(px(4.0))
                        .child(
                            btn_secondary("mock-add-http-rt", "+ HTTP")
                                .on_click(cx.listener(|this, _, _, cx| this.add_route("GET", cx))),
                        )
                        .child(
                            btn_ghost("mock-add-crud-rt", "+ CRUD")
                                .on_click(cx.listener(|this, _, _, cx| this.add_route("CRUD", cx))),
                        ),
                ),
        )
        .child(
            div()
                .w_full()
                .px(px(8.0))
                .py(px(6.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(search_input(&view.route_search_input, cx)),
        )
        .child(
            div()
                .id("mock-routes-list-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(route_list),
        )
        .child(
            h_row()
                .w_full()
                .justify_between()
                .px(px(10.0))
                .py(px(6.0))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .child(
                            crate::ui::tr(cx)
                                .mock()
                                .tab_routes_enabled_127_0_0_1(enabled_cnt, srv.routes.len()),
                        ),
                ),
        );

    // Right route editor column + docked Live Traffic Strip
    let right_main = if let Some(route) = view.selected_route() {
        render_route_editor(view, srv, route, cx)
    } else {
        empty_state(
            UiIcon::Branch,
            crate::ui::Tr(view.lang)
                .mock()
                .tab_routes_select_or_create_a_route(),
            crate::ui::Tr(view.lang)
                .mock()
                .tab_routes_configure_http_response_branches_matching(),
            cx,
        )
        .into_any_element()
    };

    let mut right_pane = v_col()
        .flex_1()
        .min_w_0()
        .h_full()
        .child(div().flex_1().min_h_0().w_full().child(right_main));
    if !view.logs.is_empty() {
        right_pane = right_pane.child(render_live_traffic_strip(view, srv, cx));
    }

    h_row()
        .size_full()
        .min_h_0()
        .child(left_col)
        .child(right_pane)
        .into_any_element()
}

fn render_route_editor(
    view: &MockView,
    srv: &pingdeck_desktop::core::model::items::MockServer,
    route: &pingdeck_desktop::core::model::items::MockRoute,
    cx: &mut Context<MockView>,
) -> AnyElement {
    let p = palette(cx);
    let is_crud = route.route_type == MockRouteType::Crud;
    let route_id_test = route.id.clone();
    let route_id_try = route.id.clone();

    let mut content = v_col().w_full().p(px(16.0)).gap(px(14.0));

    // Route definition card
    let mut def_card = card(cx).p(px(14.0)).gap(px(10.0)).child(section_header(
        if is_crud {
            crate::ui::Tr(view.lang).mock().tab_routes_crud_data_route()
        } else {
            crate::ui::Tr(view.lang)
                .mock()
                .tab_routes_http_route_definition()
        },
        Some(
            h_row()
                .gap(px(6.0))
                .child(
                    btn_primary(
                        "mock-try-route-btn",
                        if view.try_it_busy {
                            crate::ui::Tr(view.lang).mock().tab_routes_trying()
                        } else {
                            crate::ui::Tr(view.lang).mock().tab_routes_try_it()
                        },
                    )
                    .icon(icon_xs(UiIcon::Play))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.try_route_inline(&route_id_try, cx);
                    })),
                )
                .child(
                    btn_secondary(
                        "mock-test-route-btn",
                        crate::ui::Tr(view.lang)
                            .mock()
                            .tab_routes_open_in_requests(),
                    )
                    .icon(icon_xs(UiIcon::Send))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.test_route_in_requests(&route_id_test, cx);
                    })),
                )
                .into_any_element(),
        ),
        cx,
    ));

    if let Some((code, ms, url, body)) = &view.try_it_result {
        content = content.child(render_try_it_card(view, *code, *ms, url, body, cx));
    }

    if !is_crud {
        let mut method_row = h_row().gap(px(4.0)).flex_wrap();
        for m in HTTP_METHODS {
            let selected = route.method.eq_ignore_ascii_case(m);
            method_row = method_row.child(
                choice_chip(SharedString::from(format!("mock-m-{m}")), m, selected, cx).on_click(
                    cx.listener(move |this, _, _, cx| {
                        if let Some(r) = this.selected_route_mut() {
                            r.method = m.to_owned();
                            this.dirty = true;
                            cx.notify();
                        }
                    }),
                ),
            );
        }
        def_card = def_card.child(method_row);
    }

    def_card = def_card.child(
        h_row()
            .w_full()
            .gap(px(10.0))
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(
                        div().text_xs().text_color(to_hsla(p.muted)).child(
                            crate::ui::Tr(view.lang)
                                .mock()
                                .tab_routes_path_supports_id_params(),
                        ),
                    )
                    .child(text_input(&view.route_path_input)),
            )
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(crate::ui::Tr(view.lang).shared().description()),
                    )
                    .child(text_input(&view.route_desc_input)),
            ),
    );

    if is_crud {
        // Bucket picker + ID key + Generated CRUD endpoints preview
        let mut bucket_row = h_row().gap(px(6.0)).flex_wrap();
        if srv.data_buckets.is_empty() {
            bucket_row = bucket_row.child(meta_pill(
                crate::ui::Tr(view.lang)
                    .mock()
                    .tab_routes_add_a_bucket_in_data(),
                cx,
            ));
        } else {
            for b in &srv.data_buckets {
                let bid = b.id.clone();
                let selected = route.data_bucket_id == b.id;
                bucket_row = bucket_row.child(
                    choice_chip(
                        SharedString::from(format!("mock-crud-b-{}", b.id)),
                        b.name.clone(),
                        selected,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(r) = this.selected_route_mut() {
                            r.data_bucket_id = bid.clone();
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                );
            }
        }
        def_card = def_card.child(
            h_row()
                .w_full()
                .gap(px(12.0))
                .items_end()
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(4.0))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(crate::ui::Tr(view.lang).mock().tab_routes_data_bucket()),
                        )
                        .child(bucket_row),
                )
                .child(
                    v_col()
                        .w(px(140.0))
                        .gap(px(4.0))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(crate::ui::Tr(view.lang).mock().tab_routes_id_field()),
                        )
                        .child(text_input(&view.route_crud_id_input)),
                ),
        );
        content = content.child(def_card);

        if let Ok(endpoints) = mock_crud_endpoints(srv, route) {
            let mut ep_card = card(cx)
                .p(px(14.0))
                .gap(px(6.0))
                .child(section_header(
                    crate::ui::Tr(view.lang)
                        .mock()
                        .tab_routes_generated_crud_endpoints(),
                    None,
                    cx,
                ))
                .child(table_header(
                    &[
                        (crate::ui::Tr(view.lang).mock().target_method(), Some(64.0)),
                        (
                            crate::ui::Tr(view.lang).mock().tab_routes_path(),
                            Some(180.0),
                        ),
                        (crate::ui::Tr(view.lang).mock().tab_routes_behavior(), None),
                    ],
                    cx,
                ));
            for ep in endpoints {
                ep_card = ep_card.child(
                    data_row(cx)
                        .child(div().w(px(64.0)).child(method_badge(&ep.method, cx)))
                        .child(
                            div()
                                .w(px(180.0))
                                .text_xs()
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(ep.path),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(ep.description),
                        ),
                );
            }
            content = content.child(ep_card);
        }
    } else {
        // Response selection mode chips
        let mut mode_row = h_row().gap(px(4.0)).flex_wrap().child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .mr(px(4.0))
                .child(crate::ui::Tr(view.lang).mock().tab_routes_mode()),
        );
        let m_tr = crate::ui::Tr(view.lang).mock();
        let response_modes = [
            (
                MockResponseMode::FirstMatch,
                "first",
                m_tr.rmode_first_match(),
            ),
            (MockResponseMode::Sequential, "seq", m_tr.rmode_sequential()),
            (MockResponseMode::Random, "rand", m_tr.rmode_random()),
            (
                MockResponseMode::Fallback,
                "fallback",
                m_tr.rmode_fallback(),
            ),
            (
                MockResponseMode::DisableRules,
                "disable",
                m_tr.rmode_disable_rules(),
            ),
        ];
        for (mode, tag, lbl) in response_modes {
            let selected = route.response_mode == mode;
            mode_row = mode_row.child(
                choice_chip(
                    SharedString::from(format!("mock-rmode-{tag}")),
                    lbl,
                    selected,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(r) = this.selected_route_mut() {
                        r.response_mode = mode;
                        this.dirty = true;
                        cx.notify();
                    }
                })),
            );
        }
        def_card = def_card.child(mode_row);
        content = content.child(def_card);
        content = content.child(render_response_branches(view, srv, route, cx));
    }

    div()
        .id("mock-route-editor-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(content)
        .into_any_element()
}
