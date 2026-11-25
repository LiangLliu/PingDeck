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
                .child(section_header(view.t("流程名称", "Flow Name"), None, cx))
                .child(text_input(&view.new_flow_name_input))
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("flow-new-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary("flow-new-go", view.t("创建流程", "Create")).on_click(cx.listener(
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
                view.t("新建流程", "New Flow"),
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
                .child(if view.is_zh() {
                    format!("确定要永久删除流程「{name}」吗？历史运行记录将一并移除。")
                } else {
                    format!("Permanently delete flow \"{name}\" and its stored run summaries?")
                })
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("flow-del-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_danger("flow-del-confirm", view.t("删除流程", "Delete Flow")).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.delete_flow(id.clone(), cx);
                        }),
                    ),
                )
                .into_any_element();
            modal_dialog(
                "flow-delete-modal",
                view.t("删除流程", "Delete Flow"),
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
                    btn_secondary("flow-guard-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_danger("flow-guard-go", view.t("确认继续执行", "Confirm & Run")).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.run_flow(max_steps, only_step, Some(true), cx);
                        }),
                    ),
                )
                .into_any_element();
            modal_dialog(
                "flow-guard-modal",
                view.t("生产环境执行确认", "Production Environment Guard"),
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
                    view.t("流程内独立 Cookie 会话", "Isolated Run Cookie Session"),
                    None,
                    Some(SharedString::from(view.t(
                        "前序步骤 Set-Cookie 将在后续步骤间传递，但绝不写入工作区 Cookie 罐。",
                        "Carries cookies across steps within one run without writing to the workspace jar.",
                    ))),
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
                .child(section_header(view.t("初始种子变量", "Initial Seed Variables"), None, cx))
                .child(seeds_col)
                .child(
                    h_row()
                        .w_full()
                        .gap(px(6.0))
                        .child(div().w(px(150.0)).child(text_input(&view.seed_key_input)))
                        .child(div().flex_1().min_w_0().child(text_input(&view.seed_val_input)))
                        .child(
                            btn_secondary("flow-seed-add", view.t("+ 添加", "+ Add")).on_click(
                                cx.listener(|this, _, window, cx| {
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
                                }),
                            ),
                        ),
                )
                .into_any_element();
            let footer = btn_primary("flow-opt-done", view.t("完成", "Done"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                }))
                .into_any_element();
            modal_dialog(
                "flow-options-modal",
                view.t("流程运行选项", "Flow Run Options"),
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
                    view.t("工作区暂无已保存的请求", "No saved requests in workspace"),
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
            let footer = btn_secondary("flow-pick-close", view.t("取消", "Cancel"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.active_modal = None;
                    cx.notify();
                }))
                .into_any_element();
            modal_dialog(
                "flow-pick-req-modal",
                if is_relink {
                    view.t("为当前步骤关联已保存请求", "Link Step to Saved Request")
                } else {
                    view.t("从已保存请求添加步骤", "Add Step from Saved Request")
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
