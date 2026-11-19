//! Left Collections & Requests tree sidebar for `RequestsView`.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_primary, h_row, icon_xs, list_row,
    method_badge, palette, search_input, section_heading, to_hsla, v_col,
};

use super::RequestsView;

pub fn render_sidebar(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let query = view.search_input.read(cx).value().trim().to_lowercase();

    let matches = |name: &str, url: &str| -> bool {
        if query.is_empty() {
            return true;
        }
        name.to_lowercase().contains(&query) || url.to_lowercase().contains(&query)
    };

    let mut list = v_col()
        .flex_1()
        .min_h_0()
        .gap(px(2.0))
        .p(px(8.0))
        .overflow_y_scrollbar();

    // Render grouped collections first.
    for col in &view.collections {
        let items: Vec<_> = view
            .requests
            .iter()
            .filter(|r| {
                r.collection_id.as_ref().map(|c| c.as_str()) == Some(col.id.as_str())
                    && matches(&r.name, &r.url)
            })
            .collect();
        if items.is_empty() && !query.is_empty() {
            continue;
        }
        list = list.child(
            h_row()
                .justify_between()
                .px(px(8.0))
                .pt(px(8.0))
                .pb(px(3.0))
                .child(
                    h_row()
                        .gap(px(5.0))
                        .text_color(to_hsla(p.muted))
                        .child(icon_xs(UiIcon::Folder))
                        .child(section_heading(col.name.clone(), cx)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(items.len().to_string())),
                ),
        );
        for req in items {
            let req_clone = req.clone();
            let sel = view.selected_id.as_deref() == Some(req.id.as_str());
            list = list.child(
                list_row(
                    SharedString::from(format!("req-item-{}", req.id.as_str())),
                    sel,
                    cx,
                    cx.listener(move |this, _, window, cx| {
                        this.select_request(&req_clone, window, cx);
                    }),
                )
                .pl(px(16.0))
                .child(method_badge(&format!("{:?}", req.method), cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .child(SharedString::from(req.name.clone())),
                ),
            );
        }
    }

    // Ungrouped requests.
    let ungrouped: Vec<_> = view
        .requests
        .iter()
        .filter(|r| r.collection_id.is_none() && matches(&r.name, &r.url))
        .collect();

    if !ungrouped.is_empty() {
        if !view.collections.is_empty() {
            list = list.child(
                div()
                    .px(px(8.0))
                    .pt(px(10.0))
                    .pb(px(3.0))
                    .child(section_heading(
                        if zh { "未分组请求" } else { "Ungrouped" },
                        cx,
                    )),
            );
        }
        for req in ungrouped {
            let req_clone = req.clone();
            let sel = view.selected_id.as_deref() == Some(req.id.as_str());
            list = list.child(
                list_row(
                    SharedString::from(format!("req-item-{}", req.id.as_str())),
                    sel,
                    cx,
                    cx.listener(move |this, _, window, cx| {
                        this.select_request(&req_clone, window, cx);
                    }),
                )
                .child(method_badge(&format!("{:?}", req.method), cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .child(SharedString::from(req.name.clone())),
                ),
            );
        }
    }

    v_col()
        .size_full()
        .bg(to_hsla(p.pane))
        .child(
            v_col()
                .gap(px(8.0))
                .p(px(10.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    h_row()
                        .justify_between()
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .child(if zh { "请求" } else { "Requests" }),
                                )
                                .child(badge(view.requests.len().to_string(), Tone::Neutral, cx)),
                        )
                        .child(
                            btn_primary("req-new", if zh { "新建" } else { "New" })
                                .icon(icon_xs(UiIcon::Plus))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.new_blank_draft(window, cx);
                                })),
                        ),
                )
                .child(search_input(&view.search_input, cx)),
        )
        .child(list)
}
