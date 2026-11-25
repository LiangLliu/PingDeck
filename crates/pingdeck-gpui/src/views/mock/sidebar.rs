//! Master sidebar pane listing workspace Mock servers with status, port, hit counts, and search.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_icon, btn_primary, empty_inline,
    h_row, icon_xs, list_row, master_pane, meta_pill, palette, pane_footer, pane_header,
    search_input, status_indicator, to_hsla, v_col,
};

use super::state::MockView;

pub fn render_sidebar(view: &MockView, cx: &mut Context<MockView>) -> AnyElement {
    let p = palette(cx);
    let zh = view.is_zh();
    let query = view.search_input.read(cx).value().trim().to_lowercase();

    let filtered: Vec<_> = view
        .servers
        .iter()
        .filter(|srv| {
            query.is_empty()
                || srv.name.to_lowercase().contains(&query)
                || srv.port.to_string().contains(&query)
                || srv.endpoint_prefix.to_lowercase().contains(&query)
        })
        .collect();

    let running_count = view
        .servers
        .iter()
        .filter(|s| view.is_running(&s.id))
        .count();

    let header_actions = h_row()
        .gap(px(4.0))
        .child(
            btn_icon("mock-import-btn", UiIcon::Upload)
                .tooltip(view.t(
                    "导入 Mock / OpenAPI / Mockoon",
                    "Import Mock / OpenAPI / Mockoon",
                ))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_import_modal(window, cx);
                })),
        )
        .child(
            btn_primary("mock-new-btn", view.t("新建", "New"))
                .icon(icon_xs(UiIcon::Plus))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_create_server_modal(window, cx);
                })),
        )
        .into_any_element();

    let header = pane_header(
        view.t("Mock 服务", "Mock Servers"),
        Some(view.servers.len()),
        Some(header_actions),
        Some(search_input(&view.search_input, cx).into_any_element()),
        cx,
    );

    let mut list = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if filtered.is_empty() {
        list = list.child(empty_inline(
            if view.servers.is_empty() {
                view.t("暂无 Mock 服务", "No mock servers yet")
            } else {
                view.t("没有匹配的 Mock 服务", "No matching mock servers")
            },
            Some(SharedString::from(view.t(
                "可新建本地服务，或从 OpenAPI / Mockoon 导入。",
                "Create a local mock server or import from OpenAPI / Mockoon.",
            ))),
            None,
            cx,
        ));
    } else {
        for srv in filtered {
            let sid = srv.id.clone();
            let active = view.selected_id.as_deref() == Some(srv.id.as_str());
            let st = view.status_of(&srv.id);
            let running = st.is_some_and(|s| s.running);
            let hits = st.map_or(0, |s| s.hit_count);
            let recording = st.is_some_and(|s| s.recording);
            let port_str = format!(":{}", srv.port);
            let sub = if zh {
                format!(
                    "{} 条路由{}",
                    srv.routes.len(),
                    if running {
                        format!(" · {hits} 次命中")
                    } else {
                        String::new()
                    }
                )
            } else {
                format!(
                    "{} routes{}",
                    srv.routes.len(),
                    if running {
                        format!(" · {hits} hits")
                    } else {
                        String::new()
                    }
                )
            };

            let mut row = list_row(
                SharedString::from(format!("mock-srv-{}", srv.id)),
                active,
                cx,
                cx.listener(move |this, _, _, cx| {
                    this.select_server(sid.clone(), cx);
                }),
            )
            .items_start()
            .py(px(7.0))
            .child(div().pt(px(3.0)).child(status_indicator(
                if running {
                    StatusShape::Live
                } else {
                    StatusShape::Idle
                },
                cx,
            )))
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
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(srv.name.clone()),
                            )
                            .child(meta_pill(port_str, cx)),
                    )
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(sub),
                            )
                            .children(
                                recording.then(|| badge(view.t("录制中", "REC"), Tone::Danger, cx)),
                            ),
                    ),
            );
            if active && view.dirty {
                row = row.child(
                    div()
                        .size(px(6.0))
                        .mt(px(5.0))
                        .rounded(px(999.0))
                        .bg(to_hsla(p.accent))
                        .flex_shrink_0(),
                );
            }
            list = list.child(row);
        }
    }

    let footer_text = if zh {
        format!("{running_count} 个运行中 · 共 {} 个", view.servers.len())
    } else {
        format!("{running_count} running · {} total", view.servers.len())
    };
    let footer = pane_footer(
        div()
            .text_size(px(11.0))
            .text_color(to_hsla(p.muted))
            .child(footer_text),
        Some(
            btn_icon("mock-refresh-btn", UiIcon::Refresh)
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(ws) = this.workspace_id.clone() {
                        this.reload(ws, cx);
                    }
                }))
                .into_any_element(),
        ),
        cx,
    );

    master_pane(
        header,
        div()
            .id("mock-sidebar-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(list),
        Some(footer.into_any_element()),
        cx,
    )
    .into_any_element()
}
