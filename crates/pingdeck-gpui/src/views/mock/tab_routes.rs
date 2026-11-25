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

const RESPONSE_MODES: [(MockResponseMode, &str, &str); 5] = [
    (MockResponseMode::FirstMatch, "首条匹配", "First Match"),
    (MockResponseMode::Sequential, "顺序轮询", "Sequential"),
    (MockResponseMode::Random, "随机分支", "Random"),
    (MockResponseMode::Fallback, "回退模式", "Fallback"),
    (MockResponseMode::DisableRules, "忽略规则", "Disable Rules"),
];

pub fn render_routes_tab(view: &MockView, cx: &mut Context<MockView>) -> AnyElement {
    let p = palette(cx);
    let zh = view.is_zh();
    let Some(srv) = &view.draft else {
        return empty_state(
            UiIcon::Server,
            view.t("未选择服务", "No server selected"),
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
                view.t("暂无路由规则", "No routes yet")
            } else {
                view.t("没有匹配的路由", "No matching routes")
            },
            Some(SharedString::from(view.t(
                "添加 HTTP 路由或 CRUD 数据路由开始模拟接口。",
                "Add an HTTP or CRUD route to start mocking endpoints.",
            ))),
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
                            .tooltip(view.t("在请求中测试", "Test in Requests"))
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
                            .tooltip(view.t("复制路由", "Duplicate route"))
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
                            .tooltip(view.t("删除路由", "Delete route"))
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
                        .child(if zh {
                            format!("路由列表 ({})", srv.routes.len())
                        } else {
                            format!("Routes ({})", srv.routes.len())
                        }),
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
                        .child(if zh {
                            format!("{enabled_cnt}/{} 启用 · 127.0.0.1", srv.routes.len())
                        } else {
                            format!("{enabled_cnt}/{} enabled · 127.0.0.1", srv.routes.len())
                        }),
                ),
        );

    // Right route editor column + docked Live Traffic Strip
    let right_main = if let Some(route) = view.selected_route() {
        render_route_editor(view, srv, route, cx)
    } else {
        empty_state(
            UiIcon::Branch,
            view.t("选择或新建一条路由", "Select or create a route"),
            view.t(
                "支持 HTTP 响应分支、规则匹配、SSE 流式推送与 CRUD 数据桶。",
                "Configure HTTP response branches, matching rules, SSE streams, or CRUD buckets.",
            ),
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
    let zh = view.is_zh();
    let is_crud = route.route_type == MockRouteType::Crud;
    let route_id_test = route.id.clone();
    let route_id_try = route.id.clone();

    let mut content = v_col().w_full().p(px(16.0)).gap(px(14.0));

    // Route definition card
    let mut def_card = card(cx).p(px(14.0)).gap(px(10.0)).child(section_header(
        if is_crud {
            view.t("CRUD 数据路由", "CRUD Data Route")
        } else {
            view.t("HTTP 路由定义", "HTTP Route Definition")
        },
        Some(
            h_row()
                .gap(px(6.0))
                .child(
                    btn_primary(
                        "mock-try-route-btn",
                        if view.try_it_busy {
                            view.t("请求中…", "Trying…")
                        } else {
                            view.t("试一下", "Try It")
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
                        view.t("在请求中测试", "Open in Requests"),
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

    def_card =
        def_card.child(
            h_row()
                .w_full()
                .gap(px(10.0))
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(4.0))
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(
                            view.t("路径 Path（支持 :id 参数）", "Path (supports :id params)"),
                        ))
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
                                .child(view.t("说明", "Description")),
                        )
                        .child(text_input(&view.route_desc_input)),
                ),
        );

    if is_crud {
        // Bucket picker + ID key + Generated CRUD endpoints preview
        let mut bucket_row = h_row().gap(px(6.0)).flex_wrap();
        if srv.data_buckets.is_empty() {
            bucket_row = bucket_row.child(meta_pill(
                view.t(
                    "请先在「数据桶」页签添加数据桶",
                    "Add a bucket in Data tab first",
                ),
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
                                .child(view.t("绑定数据桶", "Data Bucket")),
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
                                .child(view.t("主键字段", "ID Field")),
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
                    view.t("自动生成的 CRUD 端点", "Generated CRUD Endpoints"),
                    None,
                    cx,
                ))
                .child(table_header(
                    &[
                        (view.t("方法", "Method"), Some(64.0)),
                        (view.t("路径", "Path"), Some(180.0)),
                        (view.t("行为", "Behavior"), None),
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
                .child(view.t("分支策略:", "Mode:")),
        );
        for (mode, zh_l, en_l) in RESPONSE_MODES {
            let selected = route.response_mode == mode;
            mode_row = mode_row.child(
                choice_chip(
                    SharedString::from(format!("mock-rmode-{en_l}")),
                    if zh { zh_l } else { en_l },
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
