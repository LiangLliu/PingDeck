//! Modal dialogs for `FlowsView`: Create/Delete Flow, Production Guard confirmation, Flow Run
//! Options (with swimlane management), Captured Variables inspector, Insert-on-Edge picker, and
//! Pick Saved Request.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div, px,
};
use pingdeck_desktop::core::model::items::FlowStepKind;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, btn_danger, btn_icon, btn_primary,
    btn_secondary, data_row, empty_inline, h_row, icon_xs, list_row, method_badge, modal_dialog,
    palette, search_input, section_header, setting_row, text_input, to_hsla, toggle_switch, v_col,
};

use super::state::{FlowModal, FlowsView};
use super::toolbar::render_variables_modal;

pub fn render_modal(
    view: &FlowsView,
    window: &Window,
    cx: &mut Context<FlowsView>,
) -> Option<AnyElement> {
    let modal = view.active_modal.clone()?;
    let fl = crate::ui::Tr(view.lang).flows();
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
                .child(section_header(fl.modals_flow_name(), None, cx))
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
                    btn_primary("flow-new-go", fl.modals_create()).on_click(cx.listener(
                        |this, _, _, cx| {
                            let name = this.new_flow_name_input.read(cx).value().trim().to_owned();
                            this.active_modal = None;
                            this.create_flow_named((!name.is_empty()).then_some(name), cx);
                        },
                    )),
                )
                .into_any_element();
            modal_dialog(
                "flow-create-modal",
                fl.flows_new_flow(),
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
                .child(fl.modals_permanently_delete_flow_and_its(name))
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
                    btn_danger("flow-del-confirm", fl.modals_delete_flow()).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.delete_flow(id.clone(), cx);
                        },
                    )),
                )
                .into_any_element();
            modal_dialog(
                "flow-delete-modal",
                fl.modals_delete_flow(),
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
                    btn_danger("flow-guard-go", fl.modals_confirm_run()).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.run_flow(max_steps, only_step, Some(true), cx);
                        },
                    )),
                )
                .into_any_element();
            modal_dialog(
                "flow-guard-modal",
                fl.modals_production_environment_guard(),
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        FlowModal::Options => render_options_modal(view, window, cx, on_close),
        FlowModal::Variables => render_variables_modal(view, window, cx, on_close),
        FlowModal::InsertOnEdge {
            from_step_id,
            to_step_id,
        } => {
            let f1 = from_step_id.clone();
            let t1 = to_step_id.clone();
            let f2 = from_step_id.clone();
            let t2 = to_step_id.clone();
            let f3 = from_step_id.clone();
            let t3 = to_step_id.clone();
            let f4 = from_step_id;
            let t4 = to_step_id;

            let body = v_col()
                .w_full()
                .gap(px(8.0))
                .child(
                    btn_secondary("flow-ins-inline", fl.insert_inline_http())
                        .icon(icon_xs(UiIcon::Send))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.insert_step_between(&f1, &t1, FlowStepKind::Request, None, cx);
                        })),
                )
                .child(
                    btn_secondary("flow-ins-saved", fl.insert_saved_request())
                        .icon(icon_xs(UiIcon::FolderOpen))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.active_modal = Some(FlowModal::AddSavedStep {
                                insert_between: Some((f2.clone(), t2.clone())),
                            });
                            cx.notify();
                        })),
                )
                .child(
                    btn_secondary("flow-ins-cond", fl.insert_condition_node())
                        .icon(icon_xs(UiIcon::Diamond))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.insert_step_between(&f3, &t3, FlowStepKind::Condition, None, cx);
                        })),
                )
                .child(
                    btn_secondary("flow-ins-join", fl.insert_join_node())
                        .icon(icon_xs(UiIcon::Branch))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.insert_step_between(&f4, &t4, FlowStepKind::Join, None, cx);
                        })),
                )
                .into_any_element();
            let footer =
                btn_secondary("flow-ins-close", crate::ui::Tr(view.lang).common().cancel())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = None;
                        cx.notify();
                    }))
                    .into_any_element();
            modal_dialog(
                "flow-insert-edge-modal",
                fl.graph_insert_between(),
                400.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        FlowModal::AddSavedStep { insert_between } => {
            render_request_picker(view, false, insert_between, window, cx, on_close)
        }
        FlowModal::PickStepRequest => render_request_picker(view, true, None, window, cx, on_close),
    };
    Some(el)
}

fn render_options_modal(
    view: &FlowsView,
    window: &Window,
    cx: &mut Context<FlowsView>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let p = palette(cx);
    let fl = crate::ui::Tr(view.lang).flows();
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
    let lanes = view
        .draft
        .as_ref()
        .map(|f| f.options.lanes.clone())
        .unwrap_or_default();

    let mut seeds_col = v_col().w_full().gap(px(5.0));
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(f) = this.draft.as_mut() {
                            f.options.initial_variables.remove(&k_rm);
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                ),
        );
    }

    let mut lanes_col = v_col().w_full().gap(px(5.0));
    for lane in lanes {
        let lid = lane.id.clone();
        lanes_col = lanes_col.child(
            data_row(cx)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(lane.title),
                )
                .child(
                    btn_icon(
                        SharedString::from(format!("flow-lane-rm-{}", lane.id)),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_lane(&lid, cx))),
                ),
        );
    }

    let body = v_col()
        .w_full()
        .gap(px(12.0))
        .child(setting_row(
            fl.modals_isolated_run_cookie_session(),
            None,
            Some(SharedString::from(
                fl.modals_carries_cookies_across_steps_within(),
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
        .child(section_header(fl.modals_initial_seed_variables(), None, cx))
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
                    btn_secondary("flow-seed-add", fl.modals_add()).on_click(cx.listener(
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
        .child(section_header(fl.modals_swimlanes_heading(), None, cx))
        .child(lanes_col)
        .child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(text_input(&view.lane_name_input)),
                )
                .child(
                    btn_secondary("flow-lane-add", fl.modals_add())
                        .on_click(cx.listener(|this, _, window, cx| this.add_lane(window, cx))),
                ),
        )
        .into_any_element();

    let footer = btn_primary("flow-opt-done", fl.modals_done())
        .on_click(cx.listener(|this, _, _, cx| {
            this.active_modal = None;
            cx.notify();
        }))
        .into_any_element();
    modal_dialog(
        "flow-options-modal",
        fl.modals_flow_run_options(),
        540.0,
        body,
        footer,
        window,
        cx,
        on_close,
    )
}

fn render_request_picker(
    view: &FlowsView,
    is_relink: bool,
    insert_between: Option<(String, String)>,
    window: &Window,
    cx: &mut Context<FlowsView>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let p = palette(cx);
    let fl = crate::ui::Tr(view.lang).flows();
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
            q.is_empty() || r.name.to_lowercase().contains(&q) || r.url.to_lowercase().contains(&q)
        })
        .collect();
    if matched.is_empty() {
        list = list.child(empty_inline(
            fl.modals_no_saved_requests_in_workspace(),
            None,
            None,
            cx,
        ));
    } else {
        for req in matched {
            let req_clone = req.clone();
            let between = insert_between.clone();
            let method_s = format!("{:?}", req.method).to_uppercase();
            list = list.child(
                list_row(
                    SharedString::from(format!("flow-pick-{}", req.id.as_str())),
                    false,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        if is_relink {
                            this.relink_selected_step_request(&req_clone, cx);
                        } else if let Some((from_id, to_id)) = &between {
                            this.insert_step_between(
                                from_id,
                                to_id,
                                FlowStepKind::Request,
                                Some(&req_clone),
                                cx,
                            );
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
            fl.modals_link_step_to_saved_request()
        } else {
            fl.modals_add_step_from_saved_request()
        },
        560.0,
        body,
        footer,
        window,
        cx,
        on_close,
    )
}
