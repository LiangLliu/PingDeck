//! Data buckets, Global Headers sub-tabs, and shared header-list card for `MockView`.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::model::KeyValue;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_secondary, card,
    checkbox, code_editor, data_row, empty_inline, empty_state, h_row, icon_xs, list_row,
    meta_pill, palette, section_header, setting_row, text_input, to_hsla, toggle_switch, v_col,
};

use super::state::MockView;

pub fn render_data_tab(view: &MockView, cx: &mut Context<MockView>) -> AnyElement {
    let p = palette(cx);
    let zh = view.is_zh();
    let Some(srv) = &view.draft else {
        return div().into_any_element();
    };
    let running = view.is_running(&srv.id);

    let mut list = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if srv.data_buckets.is_empty() {
        list = list.child(empty_inline(
            view.t("暂无数据桶", "No data buckets"),
            Some(SharedString::from(view.t(
                "在 CRUD 路由或模板 {{data 'name'}} 中引用 JSON 数据桶。",
                "Use JSON buckets in CRUD routes or {{data 'name'}} templates.",
            ))),
            None,
            cx,
        ));
    } else {
        for b in &srv.data_buckets {
            let bid = b.id.clone();
            let bid_del = b.id.clone();
            let active = view.selected_bucket_id.as_deref() == Some(b.id.as_str());
            let live_b = view
                .live_data
                .as_ref()
                .and_then(|ld| ld.buckets.iter().find(|lb| lb.id == b.id));
            list = list.child(
                list_row(
                    SharedString::from(format!("mock-bkt-{}", b.id)),
                    active,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.snapshot_inputs(cx);
                        this.selected_bucket_id = Some(bid.clone());
                        this.pending_sync = true;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(b.name.clone()),
                )
                .children(
                    live_b
                        .filter(|lb| lb.changed)
                        .map(|_| badge(view.t("已变更", "Modified"), Tone::Warning, cx)),
                )
                .child(
                    btn_icon(
                        SharedString::from(format!("mock-bkt-del-{}", b.id)),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.snapshot_inputs(cx);
                        if let Some(s) = this.draft.as_mut() {
                            s.data_buckets.retain(|x| x.id != bid_del);
                            this.dirty = true;
                            this.pending_sync = true;
                            cx.notify();
                        }
                    })),
                ),
            );
        }
    }

    let left = v_col()
        .w(px(240.0))
        .flex_shrink_0()
        .h_full()
        .bg(to_hsla(p.pane))
        .border_r_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .px(px(10.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(view.t("数据桶", "Buckets")),
                )
                .child(
                    btn_secondary("mock-add-bucket", view.t("+ 新建", "+ Add"))
                        .on_click(cx.listener(|this, _, _, cx| this.add_bucket(cx))),
                ),
        )
        .child(
            div()
                .id("mock-bucket-list-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(list),
        );

    let right = if let Some(bucket) = view.selected_bucket() {
        let bid_reset = bucket.id.clone();
        let live_b = view
            .live_data
            .as_ref()
            .and_then(|ld| ld.buckets.iter().find(|lb| lb.id == bucket.id));
        let mut col = v_col().w_full().p(px(16.0)).gap(px(14.0)).child(
            card(cx)
                .p(px(14.0))
                .gap(px(10.0))
                .child(section_header(
                    view.t("初始 JSON 数据", "Saved Seed JSON"),
                    None,
                    cx,
                ))
                .child(
                    h_row()
                        .w_full()
                        .gap(px(10.0))
                        .child(
                            div()
                                .w(px(200.0))
                                .child(text_input(&view.bucket_name_input)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(text_input(&view.bucket_doc_input)),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(220.0))
                        .child(code_editor(&view.bucket_json_editor)),
                ),
        );
        if running {
            let mut live_card = card(cx).p(px(14.0)).gap(px(8.0)).child(section_header(
                view.t("运行时内存数据（CRUD 实时状态）", "Live Runtime Data"),
                Some(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            btn_ghost("mock-live-refresh", view.t("刷新", "Refresh"))
                                .icon(icon_xs(UiIcon::Refresh))
                                .on_click(cx.listener(|this, _, _, cx| this.refresh_live_data(cx))),
                        )
                        .child(
                            btn_secondary(
                                "mock-live-reset-one",
                                view.t("重置此桶", "Reset Bucket"),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.reset_live_data(Some(bid_reset.clone()), cx);
                                },
                            )),
                        )
                        .child(
                            btn_ghost("mock-live-reset-all", view.t("重置全部", "Reset All"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.reset_live_data(None, cx);
                                })),
                        )
                        .into_any_element(),
                ),
                cx,
            ));
            if let Some(lb) = live_b {
                let meta = if zh {
                    format!(
                        "{}{}",
                        lb.items
                            .map_or("对象".to_owned(), |n| format!("{n} 条记录")),
                        if lb.changed {
                            " · 与初始数据不同"
                        } else {
                            " · 未变更"
                        }
                    )
                } else {
                    format!(
                        "{}{}",
                        lb.items
                            .map_or("Object".to_owned(), |n| format!("{n} items")),
                        if lb.changed {
                            " · Modified"
                        } else {
                            " · Unchanged"
                        }
                    )
                };
                live_card = live_card.child(meta_pill(meta, cx)).child(
                    div()
                        .w_full()
                        .p(px(10.0))
                        .rounded(px(6.0))
                        .bg(to_hsla(p.sunken))
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(lb.value_json.clone()),
                );
            }
            col = col.child(live_card);
        }
        div()
            .id("mock-bucket-detail-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(col)
            .into_any_element()
    } else {
        empty_state(
            UiIcon::Layers,
            view.t("选择或新建数据桶", "Select or create a data bucket"),
            view.t(
                "数据桶为 CRUD 路由和模板提供内存态 JSON 数据集。",
                "Buckets back CRUD routes and template helpers.",
            ),
            cx,
        )
        .into_any_element()
    };

    h_row()
        .size_full()
        .min_h_0()
        .child(left)
        .child(div().flex_1().min_w_0().h_full().child(right))
        .into_any_element()
}

pub fn render_headers_tab(view: &MockView, cx: &mut Context<MockView>) -> AnyElement {
    let Some(srv) = &view.draft else {
        return div().into_any_element();
    };
    let cors = srv.cors_enabled;
    let body = v_col()
        .w_full()
        .p(px(18.0))
        .gap(px(14.0))
        .child(card(cx).p(px(14.0)).child(setting_row(
            view.t("自动添加 CORS 响应头", "Enable CORS automatically"),
            None,
            Some(SharedString::from(view.t(
                "响应 OPTIONS 预检请求并附带 Access-Control-Allow-* 头。",
                "Answers OPTIONS preflight requests and injects Access-Control-Allow-* headers.",
            ))),
            toggle_switch("mock-hdr-cors", cors).on_click(cx.listener(|this, _, _, cx| {
                if let Some(s) = this.draft.as_mut() {
                    s.cors_enabled = !s.cors_enabled;
                    this.dirty = true;
                    cx.notify();
                }
            })),
            cx,
        )))
        .child(render_kv_card(
            view,
            view.t("全局默认响应头", "Global Default Response Headers"),
            &srv.headers,
            KvListTarget::Global,
            cx,
        ));
    div()
        .id("mock-headers-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(body)
        .into_any_element()
}

#[derive(Clone, Copy)]
pub(super) enum KvListTarget {
    Global,
    Callback,
    ProxyReq,
    ProxyRes,
}

pub(super) fn render_kv_card(
    view: &MockView,
    title: &'static str,
    rows: &[KeyValue],
    target: KvListTarget,
    cx: &mut Context<MockView>,
) -> AnyElement {
    let p = palette(cx);
    let prefix = match target {
        KvListTarget::Global => "gh",
        KvListTarget::Callback => "cbh",
        KvListTarget::ProxyReq => "prq",
        KvListTarget::ProxyRes => "prs",
    };
    let mut box_el = card(cx).p(px(14.0)).gap(px(6.0)).child(section_header(
        title,
        Some(badge(rows.len().to_string(), Tone::Neutral, cx).into_any_element()),
        cx,
    ));

    for (idx, kv) in rows.iter().enumerate() {
        box_el = box_el.child(
            data_row(cx)
                .child(
                    checkbox(
                        SharedString::from(format!("mock-{prefix}-en-{idx}")),
                        kv.enabled,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(list) = kv_list_mut(this, target)
                            && let Some(item) = list.get_mut(idx)
                        {
                            item.enabled = !item.enabled;
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                )
                .child(
                    div()
                        .w(px(180.0))
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(kv.key.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .truncate()
                        .child(kv.value.clone()),
                )
                .child(
                    btn_icon(
                        SharedString::from(format!("mock-{prefix}-rm-{idx}")),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(list) = kv_list_mut(this, target)
                            && idx < list.len()
                        {
                            list.remove(idx);
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                ),
        );
    }

    box_el = box_el.child(
        h_row()
            .w_full()
            .gap(px(6.0))
            .child(div().w(px(180.0)).child(text_input(&view.kv_key_input)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(text_input(&view.kv_val_input)),
            )
            .child(
                btn_secondary(
                    SharedString::from(format!("mock-{prefix}-add")),
                    view.t("+ 添加", "+ Add"),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    let k = this.kv_key_input.read(cx).value().trim().to_owned();
                    let v = this.kv_val_input.read(cx).value().to_string();
                    if k.is_empty() {
                        return;
                    }
                    if let Some(list) = kv_list_mut(this, target) {
                        list.push(KeyValue {
                            key: k,
                            value: v,
                            enabled: true,
                            description: None,
                        });
                        this.dirty = true;
                    }
                    this.kv_key_input
                        .update(cx, |s, cx| s.set_value("", window, cx));
                    this.kv_val_input
                        .update(cx, |s, cx| s.set_value("", window, cx));
                    cx.notify();
                })),
            ),
    );
    box_el.into_any_element()
}

fn kv_list_mut(view: &mut MockView, target: KvListTarget) -> Option<&mut Vec<KeyValue>> {
    match target {
        KvListTarget::Global => view.draft.as_mut().map(|s| &mut s.headers),
        KvListTarget::Callback => view.selected_callback_mut().map(|c| &mut c.headers),
        KvListTarget::ProxyReq => view.draft.as_mut().map(|s| &mut s.proxy_req_headers),
        KvListTarget::ProxyRes => view.draft.as_mut().map(|s| &mut s.proxy_res_headers),
    }
}
