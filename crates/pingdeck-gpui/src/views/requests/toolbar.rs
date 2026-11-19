//! Document tab bar and combined URL/Method/Environment strip for `RequestsView`.

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::ui::{
    Anchor, StatusShape, UiIcon, badge, btn_danger, btn_icon, btn_primary, btn_secondary, h_row,
    icon_xs, list_row, method_badge, palette, pill_btn, popover, status_indicator, text_input,
    to_hsla, v_col,
};

use super::{ALL_METHODS, RequestsView};

pub fn render_doc_tabbar(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut bar = h_row()
        .h(px(34.0))
        .px(px(8.0))
        .gap(px(4.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)));

    if view.open_tabs.is_empty() {
        bar = bar.child(
            h_row()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(method_badge(&format!("{:?}", view.method), cx))
                .child(div().text_xs().text_color(to_hsla(p.ink)).child(if zh {
                    "草稿请求"
                } else {
                    "Draft request"
                })),
        );
    } else {
        for tab in &view.open_tabs {
            let active = view.selected_id.as_deref() == Some(tab.id.as_str());
            let tid = tab.id.clone();
            let tid_close = tab.id.clone();
            let req_match = view
                .requests
                .iter()
                .find(|r| r.id.as_str() == tab.id.as_str())
                .cloned();
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("doc-tab-{tid}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(9.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .bg(if active {
                        to_hsla(p.surface)
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .border_1()
                    .border_color(if active {
                        to_hsla(p.line_strong.composite_over(p.surface))
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(r) = &req_match {
                            this.select_request(r, window, cx);
                        }
                    }))
                    .child(method_badge(&format!("{:?}", tab.method), cx))
                    .child(
                        div()
                            .max_w(px(130.0))
                            .text_xs()
                            .text_color(if active {
                                to_hsla(p.ink)
                            } else {
                                to_hsla(p.muted)
                            })
                            .truncate()
                            .child(SharedString::from(tab.name.clone())),
                    )
                    .child(
                        btn_icon(
                            SharedString::from(format!("tab-x-{tid_close}")),
                            UiIcon::Close,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_tab(&tid_close, cx);
                        })),
                    ),
            );
        }
    }

    bar.child(
        btn_icon("doc-tab-plus", UiIcon::Plus)
            .tooltip(if zh { "新建请求" } else { "New request" })
            .on_click(cx.listener(|this, _, window, cx| this.new_blank_draft(window, cx))),
    )
}

pub fn render_url_strip(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let view_ref_m = cx.entity().clone();
    let view_ref_e = cx.entity().clone();
    let cur_method = view.method;
    let envs = view.environments.clone();
    let active_env = view.active_env_id.clone();
    let env_label = active_env
        .as_deref()
        .and_then(|id| envs.iter().find(|e| e.id == id))
        .map(|e| e.name.clone())
        .unwrap_or_else(|| {
            if zh {
                "无环境".into()
            } else {
                "No Env".into()
            }
        });

    v_col()
        .gap(px(8.0))
        .px(px(14.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .gap(px(8.0))
                .child(
                    popover("req-method-pop")
                        .anchor(Anchor::TopLeft)
                        .trigger(pill_btn(
                            "req-method-trigger",
                            SharedString::from(format!("{:?}", cur_method).to_uppercase()),
                            None,
                            true,
                        ))
                        .content(move |_, _, pop_cx| {
                            let mut list = v_col().w(px(130.0)).gap(px(2.0));
                            for m in ALL_METHODS {
                                let sel = m == cur_method;
                                let ent = view_ref_m.clone();
                                let pop_h = pop_cx.entity().clone();
                                list = list.child(
                                    list_row(
                                        SharedString::from(format!("m-pick-{m:?}")),
                                        sel,
                                        pop_cx,
                                        move |_, window, cx| {
                                            ent.update(cx, |this, cx| {
                                                this.method = m;
                                                cx.notify();
                                            });
                                            pop_h.update(cx, |st, cx| st.dismiss(window, cx));
                                        },
                                    )
                                    .child(method_badge(&format!("{m:?}"), pop_cx)),
                                );
                            }
                            list
                        }),
                )
                .child(div().flex_1().min_w_0().child(text_input(&view.url_input)))
                .child(
                    popover("req-env-pop")
                        .anchor(Anchor::TopRight)
                        .trigger(pill_btn(
                            "req-env-trigger",
                            SharedString::from(env_label),
                            Some(UiIcon::Layers),
                            active_env.is_some(),
                        ))
                        .content(move |_, _, pop_cx| {
                            let pop_p = palette(pop_cx);
                            let mut list = v_col().w(px(200.0)).gap(px(2.0));
                            let ent0 = view_ref_e.clone();
                            let pop_h0 = pop_cx.entity().clone();
                            list = list.child(
                                list_row(
                                    "env-pick-none",
                                    active_env.is_none(),
                                    pop_cx,
                                    move |_, window, cx| {
                                        ent0.update(cx, |this, cx| {
                                            this.active_env_id = None;
                                            cx.notify();
                                        });
                                        pop_h0.update(cx, |st, cx| st.dismiss(window, cx));
                                    },
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(to_hsla(pop_p.muted))
                                        .child(if zh {
                                            "不使用环境"
                                        } else {
                                            "No environment"
                                        }),
                                ),
                            );
                            for env in &envs {
                                let eid = env.id.clone();
                                let sel = active_env.as_deref() == Some(env.id.as_str());
                                let ent = view_ref_e.clone();
                                let pop_h = pop_cx.entity().clone();
                                list = list.child(
                                    list_row(
                                        SharedString::from(format!("env-pick-{eid}")),
                                        sel,
                                        pop_cx,
                                        move |_, window, cx| {
                                            ent.update(cx, |this, cx| {
                                                this.active_env_id = Some(eid.clone());
                                                cx.notify();
                                            });
                                            pop_h.update(cx, |st, cx| st.dismiss(window, cx));
                                        },
                                    )
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_xs()
                                            .truncate()
                                            .child(SharedString::from(env.name.clone())),
                                    )
                                    .children(sel.then(|| {
                                        div()
                                            .text_color(to_hsla(pop_p.accent))
                                            .child(icon_xs(UiIcon::Check))
                                    })),
                                );
                            }
                            list
                        }),
                )
                .child(
                    btn_primary(
                        "req-send",
                        if view.sending {
                            if zh { "发送中…" } else { "Sending…" }
                        } else if zh {
                            "发送"
                        } else {
                            "Send"
                        },
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.send_current(window, cx))),
                )
                .child(
                    btn_secondary("req-save", if zh { "保存" } else { "Save" })
                        .on_click(cx.listener(|this, _, _, cx| this.save_current(cx))),
                ),
        )
        .child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .child(
                    h_row()
                        .gap(px(8.0))
                        .child(div().w(px(240.0)).child(text_input(&view.name_input)))
                        .children(view.sending.then(|| status_indicator(StatusShape::Run, cx)))
                        .children(
                            view.status_msg
                                .clone()
                                .map(|(tone, msg)| badge(msg, tone, cx)),
                        ),
                )
                .children(view.selected_id.is_some().then(|| {
                    btn_danger("req-del", if zh { "删除" } else { "Delete" })
                        .on_click(cx.listener(|this, _, _, cx| this.delete_selected(cx)))
                })),
        )
}
