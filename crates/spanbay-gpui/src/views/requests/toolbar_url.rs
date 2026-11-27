//! HTTP Method/URL/Environment/Send bar, environment selector popover, and variables inspector.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::model::{HttpMethod, VariableResolutionSource};

use crate::ui::{
    Anchor, Popover, ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_danger,
    btn_icon, btn_primary, empty_inline, h_row, icon_xs, menu_option, method_badge, method_pill,
    palette, pill_btn, popover, popover_header, select_pill, status_indicator, text_input, to_hsla,
    toolbar_strip, v_col,
};

use super::RequestsView;
use super::model::{ALL_METHODS, RequestsHandoff, method_str};

pub fn render_http_url_strip(view: &RequestsView, cx: &mut Context<RequestsView>) -> gpui_kit::Div {
    let Some(tab) = view.active_req() else {
        return div();
    };
    let cur_method = tab.draft.method;
    let sending = tab.sending;
    let view_ref_m = cx.entity().clone();

    let method_desc = |m: HttpMethod, pop_cx: &gpui_kit::App| -> &'static str {
        let r = crate::ui::tr(pop_cx).requests();
        match m {
            HttpMethod::Get => r.method_get_desc(),
            HttpMethod::Post => r.method_post_desc(),
            HttpMethod::Put => r.method_put_desc(),
            HttpMethod::Patch => r.method_patch_desc(),
            HttpMethod::Delete => r.method_delete_desc(),
            HttpMethod::Head => r.method_head_desc(),
            HttpMethod::Options => r.method_options_desc(),
        }
    };

    let method_picker = popover("req-method-pop")
        .p(px(6.0))
        .rounded(px(10.0))
        .anchor(Anchor::TopLeft)
        .trigger(method_pill(
            "req-method-trigger",
            method_str(cur_method),
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col().w(px(196.0)).gap(px(2.0));
            for m in ALL_METHODS {
                let sel = m == cur_method;
                let ent = view_ref_m.clone();
                let pop_h = pop_cx.entity().clone();
                list = list.child(
                    menu_option(
                        SharedString::from(format!("m-pick-{m:?}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            ent.update(cx, |this, cx| {
                                if let Some(t) = this.active_req_mut() {
                                    t.draft.method = m;
                                    t.recompute_dirty();
                                }
                                cx.notify();
                            });
                            pop_h.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .child(method_badge(method_str(m), pop_cx))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(to_hsla(pop_p.muted))
                            .child(method_desc(m, pop_cx)),
                    ),
                );
            }
            list
        });

    let send_btn: gpui_kit::AnyElement = if sending {
        btn_danger("req-stop-btn", crate::ui::tr(cx).requests().send_stop())
            .icon(icon_xs(UiIcon::Stop))
            .on_click(cx.listener(|this, _, _, cx| this.stop_active_request(cx)))
            .into_any_element()
    } else {
        btn_primary("req-send-btn", crate::ui::tr(cx).requests().send_send())
            .icon(icon_xs(UiIcon::Send))
            .on_click(cx.listener(|this, _, window, cx| {
                this.send_active_request(None, None, window, cx);
            }))
            .into_any_element()
    };

    toolbar_strip(cx)
        .py(px(9.0))
        .child(method_picker)
        .child(div().flex_1().min_w_0().child(text_input(&view.url_input)))
        .child(render_env_picker(view, cx))
        .child(send_btn)
        .child(
            btn_icon("req-send-save-file", UiIcon::Download)
                .tooltip(
                    crate::ui::tr(cx)
                        .requests()
                        .toolbar_url_send_and_save_response_to(),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.send_and_save_to_file(window, cx);
                })),
        )
}

pub fn render_env_picker(view: &RequestsView, cx: &mut Context<RequestsView>) -> Popover {
    let view_ref_e = cx.entity().clone();
    let envs = view.environments.clone();
    let active_env = view.active_env_id.clone();
    let active_dto = active_env
        .as_deref()
        .and_then(|id| envs.iter().find(|e| e.id == id));
    let env_label = active_dto
        .map(|e| {
            if e.confirm_before_send {
                crate::ui::tr(cx).requests().env_guarded_label(&e.name)
            } else {
                e.name.clone()
            }
        })
        .unwrap_or_else(|| crate::ui::tr(cx).requests().toolbar_url_no_env().into());

    popover("req-env-pop")
        .p(px(6.0))
        .rounded(px(10.0))
        .anchor(Anchor::TopRight)
        .trigger(select_pill(
            "req-env-trigger",
            SharedString::from(env_label),
            active_env.is_some(),
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col().w(px(244.0)).gap(px(2.0));
            let ent_m = view_ref_e.clone();
            let ph_m = pop_cx.entity().clone();
            list = list.child(popover_header(
                crate::ui::tr(pop_cx).requests().toolbar_url_environment(),
                Some((
                    "req-env-manage".into(),
                    SharedString::from(crate::ui::tr(pop_cx).requests().toolbar_url_manage()),
                    Box::new(move |_, window, cx| {
                        ent_m.update(cx, |this, cx| {
                            this.emit_handoff(RequestsHandoff::OpenEnvironments, cx);
                        });
                        ph_m.update(cx, |s, cx| s.dismiss(window, cx));
                    }),
                )),
                pop_cx,
            ));

            let ent0 = view_ref_e.clone();
            let ph0 = pop_cx.entity().clone();
            list = list.child(
                menu_option(
                    "env-pick-none",
                    active_env.is_none(),
                    pop_cx,
                    move |_, window, cx| {
                        ent0.update(cx, |this, cx| {
                            this.active_env_id = None;
                            this.reload_resolved_vars(cx);
                            cx.notify();
                        });
                        ph0.update(cx, |st, cx| st.dismiss(window, cx));
                    },
                )
                .gap(px(6.0))
                .child(status_indicator(StatusShape::Idle, pop_cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(pop_p.muted))
                        .child(crate::ui::tr(pop_cx).requests().code_no_environment()),
                ),
            );

            for env in &envs {
                let eid = env.id.clone();
                let sel = active_env.as_deref() == Some(env.id.as_str());
                let ent = view_ref_e.clone();
                let ph = pop_cx.entity().clone();
                let shape = if env.confirm_before_send {
                    StatusShape::Warn
                } else {
                    StatusShape::Ok
                };
                list = list.child(
                    menu_option(
                        SharedString::from(format!("env-pick-{eid}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            ent.update(cx, |this, cx| {
                                this.active_env_id = Some(eid.clone());
                                this.reload_resolved_vars(cx);
                                cx.notify();
                            });
                            ph.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .min_w_0()
                            .child(status_indicator(shape, pop_cx))
                            .child(
                                v_col()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_xs()
                                            .truncate()
                                            .child(SharedString::from(env.name.clone())),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(to_hsla(pop_p.muted))
                                            .child(SharedString::from(
                                                crate::ui::tr(pop_cx)
                                                    .requests()
                                                    .toolbar_url_vars(env.variables.len()),
                                            )),
                                    ),
                            ),
                    )
                    .children(env.confirm_before_send.then(|| {
                        badge(
                            crate::ui::tr(pop_cx).requests().toolbar_url_guard(),
                            Tone::Warning,
                            pop_cx,
                        )
                    })),
                );
            }
            list
        })
}

pub fn render_variables_popover(view: &RequestsView, cx: &mut Context<RequestsView>) -> Popover {
    let vars = view.resolved_vars.clone();
    let active_env_name = view
        .active_env_id
        .as_deref()
        .and_then(|id| view.environments.iter().find(|e| e.id == id))
        .map(|e| e.name.clone());
    let has_secrets = vars.iter().any(|v| v.is_secret);
    let ent = cx.entity().clone();
    popover("req-vars-pop")
        .anchor(Anchor::TopRight)
        .trigger(pill_btn(
            "req-vars-btn",
            crate::ui::tr(cx).requests().toolbar_url_vars_2(vars.len()),
            Some(UiIcon::Eye),
            false,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut col = v_col().w(px(344.0)).max_h(px(340.0)).gap(px(4.0));
            let ent_m = ent.clone();
            let ph = pop_cx.entity().clone();
            let r = crate::ui::tr(pop_cx).requests();
            let hdr_title = match &active_env_name {
                Some(nm) => r.vars_header_env(nm),
                None => r.vars_header_default().into(),
            };
            col = col.child(popover_header(
                SharedString::from(hdr_title),
                Some((
                    "req-vars-manage".into(),
                    SharedString::from(crate::ui::tr(pop_cx).requests().toolbar_url_edit()),
                    Box::new(move |_, window, cx| {
                        ent_m.update(cx, |this, cx| {
                            this.emit_handoff(RequestsHandoff::OpenEnvironments, cx);
                        });
                        ph.update(cx, |s, cx| s.dismiss(window, cx));
                    }),
                )),
                pop_cx,
            ));

            let mut rows = v_col().gap(px(3.0)).overflow_y_scrollbar();
            if vars.is_empty() {
                rows = rows.child(empty_inline(
                    crate::ui::tr(pop_cx)
                        .requests()
                        .toolbar_url_no_variables_defined_in_active(),
                    None,
                    None,
                    pop_cx,
                ));
            } else {
                for v in &vars {
                    let src = match v.source {
                        VariableResolutionSource::Environment => {
                            crate::ui::tr(pop_cx).requests().toolbar_url_env()
                        }
                        VariableResolutionSource::Collection => {
                            crate::ui::tr(pop_cx).requests().toolbar_url_col()
                        }
                        VariableResolutionSource::Global => {
                            crate::ui::tr(pop_cx).requests().editor_network_global()
                        }
                        _ => "—",
                    };
                    let val_disp = if v.is_secret {
                        "•••••• (0600)".to_owned()
                    } else {
                        v.value.clone().unwrap_or_else(|| "—".into())
                    };
                    rows = rows.child(
                        h_row()
                            .justify_between()
                            .gap(px(8.0))
                            .px(px(8.0))
                            .py(px(5.0))
                            .rounded(px(6.0))
                            .bg(to_hsla(pop_p.pane))
                            .child(
                                v_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .truncate()
                                            .child(SharedString::from(v.name.clone())),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(to_hsla(pop_p.muted))
                                            .truncate()
                                            .child(SharedString::from(val_disp)),
                                    ),
                            )
                            .child(badge(
                                src,
                                if v.is_secret {
                                    Tone::Warning
                                } else {
                                    Tone::Neutral
                                },
                                pop_cx,
                            )),
                    );
                }
            }
            col.child(rows).children(has_secrets.then(|| {
                div()
                    .px(px(6.0))
                    .pt(px(4.0))
                    .border_t_1()
                    .border_color(to_hsla(pop_p.line.composite_over(pop_p.surface)))
                    .text_size(px(10.5))
                    .text_color(to_hsla(pop_p.warn))
                    .child(
                        crate::ui::tr(pop_cx)
                            .requests()
                            .toolbar_url_secrets_inject_at_send_time(),
                    )
            }))
        })
}
