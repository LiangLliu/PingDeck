//! Modal dialogs for `FlowsView`: Delete Flow, Production Guard confirmation, Flow Run Options, and Pick Saved Request.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div, px,
};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, btn_danger, btn_icon, btn_primary,
    btn_secondary, data_row, empty_inline, h_row, list_row, method_badge, modal_dialog, palette,
    search_input, section_header, setting_row, text_input, to_hsla, toggle_switch, v_col,
};

use super::state::{FlowModal, FlowsView};

pub fn render_modal(
    view: &FlowsView,
    window: &Window,
    cx: &mut Context<FlowsView>,
) -> Option<AnyElement> {
    let modal = view.active_modal.clone()?;
    let entity = cx.entity().clone();
    let on_close = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        entity.update(cx, |this, cx| {
            this.active_modal = None;
            cx.notify();
        });
    };

    let el = match modal {
        FlowModal::CreateFlow => {
            let body = v_col()
                .w_full()
                .gap(px(8.0))
                .child(section_header(
                    crate::ui::Tr(view.lang).flows().modals_flow_name(),
                    None,
                    cx,
                ))
                .child(text_input(&view.new_flow_name_input))
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary(
                        "flow-new-cancel",
                        crate::ui::Tr(view.lang).common().cancel(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = None;
                        cx.notify();
                    })),
                )
                .child(
                    btn_primary(
                        "flow-new-go",
                        crate::ui::Tr(view.lang).flows().modals_create(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        let name = this.new_flow_name_input.read(cx).value().trim().to_owned();
                        this.active_modal = None;
                        this.create_flow_named((!name.is_empty()).then_some(name), cx);
                    })),
                )
                .into_any_element();
            modal_dialog(
                "flow-create-modal",
                crate::ui::Tr(view.lang).flows().flows_new_flow(),
                440.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        FlowModal::DeleteFlow { id, name } => {
            let p = palette(cx);
            let body = div()
                .text_sm()
                .text_color(to_hsla(p.ink))
                .child(
                    crate::ui::Tr(view.lang)
                        .flows()
                        .modals_permanently_delete_flow_and_its(name),
                )
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary(
                        "flow-del-cancel",
                        crate::ui::Tr(view.lang).common().cancel(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = None;
                        cx.notify();
                    })),
                )
                .child(
                    btn_danger(
                        "flow-del-confirm",
                        crate::ui::Tr(view.lang).flows().modals_delete_flow(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_flow(id.clone(), cx);
                    })),
                )
                .into_any_element();
            modal_dialog(
                "flow-delete-modal",
                crate::ui::Tr(view.lang).flows().modals_delete_flow(),
                430.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        FlowModal::ConfirmRun {
            reason,
            max_steps,
            only_step,
        } => {
            let body = v_col()
                .gap(px(10.0))
                .child(alert_banner(Tone::Warning, reason, None, cx))
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary(
                        "flow-guard-cancel",
                        crate::ui::Tr(view.lang).common().cancel(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = None;
                        cx.notify();
                    })),
                )
                .child(
                    btn_danger(
                        "flow-guard-go",
                        crate::ui::Tr(view.lang).flows().modals_confirm_run(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_flow(max_steps, only_step, Some(true), cx);
                    })),
                )
                .into_any_element();
            modal_dialog(
                "flow-guard-modal",
                crate::ui::Tr(view.lang)
                    .flows()
                    .modals_production_environment_guard(),
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        FlowModal::Options => {
            let p = palette(cx);
            let cookie_session = view.draft.as_ref().is_none_or(|f| f.options.cookie_session);
            let seeds: Vec<(String, String)> = view
                .draft
                .as_ref()
                .map(|f| {
                    f.options
                        .initial_variables
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let mut seeds_col = v_col().w_full().gap(px(6.0));
            for (k, v) in seeds {
                let k_rm = k.clone();
                seeds_col = seeds_col.child(
                    data_row(cx)
                        .child(
                            div()
                                .w(px(140.0))
                                .text_xs()
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(k),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .truncate()
                                .child(v),
                        )
                        .child(
                            btn_icon(
                                SharedString::from(format!("flow-seed-rm-{k_rm}")),
                                UiIcon::Trash,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Some(f) = this.draft.as_mut() {
                                        f.options.initial_variables.remove(&k_rm);
                                        this.dirty = true;
                                        cx.notify();
                                    }
                                },
                            )),
                        ),
                );
            }
            let body = v_col()
                .w_full()
                .gap(px(12.0))
                .child(setting_row(
                    crate::ui::Tr(view.lang)
                        .flows()
                        .modals_isolated_run_cookie_session(),
                    None,
                    Some(SharedString::from(
                        crate::ui::Tr(view.lang)
                            .flows()
                            .modals_carries_cookies_across_steps_within(),
                    )),
                    toggle_switch("flow-opt-cookie", cookie_session).on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(f) = this.draft.as_mut() {
                                f.options.cookie_session = !f.options.cookie_session;
                                this.dirty = true;
                                cx.notify();
                            }
                        },
                    )),
                    cx,
                ))
                .child(section_header(
                    crate::ui::Tr(view.lang)
                        .flows()
                        .modals_initial_seed_variables(),
                    None,
                    cx,
                ))
                .child(seeds_col)
                .child(
                    h_row()
                        .w_full()
                        .gap(px(6.0))
                        .child(div().w(px(150.0)).child(text_input(&view.seed_key_input)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(text_input(&view.seed_val_input)),
                        )
                        .child(
                            btn_secondary(
                                "flow-seed-add",
                                crate::ui::Tr(view.lang).flows().modals_add(),
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    let k = this.seed_key_input.read(cx).value().trim().to_owned();
                                    let v = this.seed_val_input.read(cx).value().to_string();
                                    if k.is_empty() {
                                        return;
                                    }
                                    if let Some(f) = this.draft.as_mut() {
                                        f.options.initial_variables.insert(k, v);
                                        this.dirty = true;
                                    }
                                    this.seed_key_input
                                        .update(cx, |s, cx| s.set_value("", window, cx));
                                    this.seed_val_input
                                        .update(cx, |s, cx| s.set_value("", window, cx));
                                    cx.notify();
                                },
                            )),
                        ),
                )
                .into_any_element();
            let footer = btn_primary(
                "flow-opt-done",
                crate::ui::Tr(view.lang).flows().modals_done(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.active_modal = None;
                cx.notify();
            }))
            .into_any_element();
            modal_dialog(
                "flow-options-modal",
                crate::ui::Tr(view.lang).flows().modals_flow_run_options(),
                520.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        FlowModal::AddSavedStep | FlowModal::PickStepRequest => {
            let is_relink = matches!(modal, FlowModal::PickStepRequest);
            let p = palette(cx);
            let q = view
                .req_picker_search
                .read(cx)
                .value()
                .trim()
                .to_lowercase();
            let mut list = v_col().w_full().gap(px(4.0));
            let matched: Vec<_> = view
                .requests
                .iter()
                .filter(|r| {
                    q.is_empty()
                        || r.name.to_lowercase().contains(&q)
                        || r.url.to_lowercase().contains(&q)
                })
                .collect();
            if matched.is_empty() {
                list = list.child(empty_inline(
                    crate::ui::Tr(view.lang)
                        .flows()
                        .modals_no_saved_requests_in_workspace(),
                    None,
                    None,
                    cx,
                ));
            } else {
                for req in matched {
                    let req_clone = req.clone();
                    let method_s = format!("{:?}", req.method).to_uppercase();
                    list = list.child(
                        list_row(
                            SharedString::from(format!("flow-pick-{}", req.id.as_str())),
                            false,
                            cx,
                            cx.listener(move |this, _, _, cx| {
                                if is_relink {
                                    this.relink_selected_step_request(&req_clone, cx);
                                } else {
                                    this.add_saved_request_step(&req_clone, cx);
                                }
                            }),
                        )
                        .child(method_badge(&method_s, cx))
                        .child(
                            div()
                                .w(px(160.0))
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(req.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(req.url.clone()),
                        ),
                    );
                }
            }
            let body = v_col()
                .w_full()
                .gap(px(8.0))
                .child(search_input(&view.req_picker_search, cx))
                .child(
                    div()
                        .id("flow-req-picker-scroll")
                        .w_full()
                        .h(px(280.0))
                        .overflow_y_scrollbar()
                        .child(list),
                )
                .into_any_element();
            let footer = btn_secondary(
                "flow-pick-close",
                crate::ui::Tr(view.lang).common().cancel(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.active_modal = None;
                cx.notify();
            }))
            .into_any_element();
            modal_dialog(
                "flow-pick-req-modal",
                if is_relink {
                    crate::ui::Tr(view.lang)
                        .flows()
                        .modals_link_step_to_saved_request()
                } else {
                    crate::ui::Tr(view.lang)
                        .flows()
                        .modals_add_step_from_saved_request()
                },
                560.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
    };
    Some(el)
}
