//! AI API Request URL/Protocol/Provider-Model/Custom-URL/Compare/Send top bar.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::model::ai_request::{AI_PROTOCOLS, AiTarget};

use crate::ui::{
    Anchor, Popover, ScrollableElement as _, UiIcon, btn_danger, btn_primary, btn_secondary, h_row,
    icon_xs, list_row, palette, popover, select_pill, tab_bar_segmented, tab_item, text_input,
    to_hsla, toolbar_strip, v_col,
};

use super::RequestsView;
use super::model::ai_protocol_label;

pub fn render_ai_url_strip(view: &RequestsView, cx: &mut Context<RequestsView>) -> gpui_kit::Div {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return div();
    };
    let ai = tab.draft.ai.as_ref();
    let cur_proto = ai
        .map(|a| a.protocol)
        .unwrap_or(spanbay_desktop::core::llm::wire::Protocol::OpenaiChat);
    let is_provider = matches!(ai.map(|a| &a.target), Some(AiTarget::Provider { .. }));
    let sending = tab.sending;

    let ent_p = cx.entity().clone();
    let proto_picker = popover("ai-proto-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            "ai-proto-trigger",
            ai_protocol_label(cur_proto),
            true,
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let mut col = v_col().w(px(190.0)).gap(px(2.0));
            for proto in AI_PROTOCOLS {
                let sel = proto == cur_proto;
                let ent = ent_p.clone();
                let ph = pop_cx.entity().clone();
                col = col.child(
                    list_row(
                        SharedString::from(format!("ai-proto-{proto:?}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            ent.update(cx, |this, cx| this.set_ai_protocol(proto, window, cx));
                            ph.update(cx, |s, cx| s.dismiss(window, cx));
                        },
                    )
                    .child(div().text_xs().child(ai_protocol_label(proto))),
                );
            }
            col
        });

    let ent_tgt = cx.entity().clone();
    let target_seg = tab_bar_segmented("ai-target-kind-seg")
        .selected_index(usize::from(!is_provider))
        .child(tab_item(
            crate::ui::tr(cx).requests().ai_url_provider_menu(),
        ))
        .child(tab_item(crate::ui::tr(cx).requests().ai_url_custom()))
        .on_click(move |idx, window, cx| {
            ent_tgt.update(cx, |this, cx| {
                if *idx == 0 {
                    if let Some((pid, pname, base, mid)) = this.first_enabled_model_tuple() {
                        this.set_ai_target_provider(pid, pname, base, mid, window, cx);
                    }
                } else {
                    this.set_ai_target_custom(window, cx);
                }
            });
        });

    let target_control: gpui_kit::AnyElement = if is_provider {
        render_provider_model_picker(view, cx).into_any_element()
    } else {
        h_row()
            .flex_1()
            .min_w_0()
            .gap(px(6.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(text_input(&view.ai_custom_url_input)),
            )
            .child(
                div()
                    .w(px(180.0))
                    .child(text_input(&view.ai_custom_model_input)),
            )
            .into_any_element()
    };

    let send_btn: gpui_kit::AnyElement = if sending {
        btn_danger(
            "ai-stop-btn",
            crate::ui::tr(cx).requests().editor_ai_bar_stop_ai_run(),
        )
        .icon(icon_xs(UiIcon::Stop))
        .on_click(cx.listener(|this, _, _, cx| this.stop_active_request(cx)))
        .into_any_element()
    } else {
        btn_primary("ai-send-btn", crate::ui::tr(cx).requests().send_send())
            .icon(icon_xs(UiIcon::Send))
            .on_click(cx.listener(|this, _, window, cx| {
                this.send_active_ai_request(None, window, cx);
            }))
            .into_any_element()
    };

    let _ = p;
    toolbar_strip(cx)
        .py(px(9.0))
        .child(proto_picker)
        .child(target_control)
        .child(target_seg)
        .child(
            btn_secondary(
                "ai-compare-btn",
                crate::ui::tr(cx).requests().editor_ai_bar_compare(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.open_compare_models_modal(cx))),
        )
        .child(send_btn)
}

fn render_provider_model_picker(view: &RequestsView, cx: &mut Context<RequestsView>) -> Popover {
    let cur_label = view
        .active_req()
        .and_then(|t| t.draft.ai.as_ref())
        .map(|a| match &a.target {
            AiTarget::Provider {
                provider_name,
                model,
                ..
            } => {
                if model.is_empty() {
                    crate::ui::tr(cx)
                        .requests()
                        .editor_ai_bar_choose_model()
                        .into()
                } else {
                    format!("{provider_name} / {model}")
                }
            }
            AiTarget::Custom { model } => model.clone(),
        })
        .unwrap_or_else(|| {
            crate::ui::tr(cx)
                .requests()
                .editor_ai_bar_choose_model_2()
                .into()
        });

    let mut choices: Vec<(String, String, String, String)> = Vec::new();
    for pv in &view.providers {
        if !pv.provider.enabled {
            continue;
        }
        for m in view
            .models
            .iter()
            .filter(|m| m.provider_id == pv.provider.id && m.enabled)
        {
            choices.push((
                pv.provider.id.clone(),
                pv.provider.name.clone(),
                pv.provider.base_url.clone(),
                m.model_id.clone(),
            ));
        }
    }
    let ent = cx.entity().clone();

    popover("ai-model-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            "ai-model-trigger",
            SharedString::from(format!(
                "{cur_label}  ·  {}",
                crate::ui::tr(cx).requests().editor_ai_bar_core_key()
            )),
            true,
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col()
                .w(px(320.0))
                .max_h(px(320.0))
                .gap(px(2.0))
                .overflow_y_scrollbar();
            if choices.is_empty() {
                list = list.child(
                    div()
                        .p(px(10.0))
                        .text_xs()
                        .text_color(to_hsla(pop_p.muted))
                        .child(
                            crate::ui::tr(pop_cx)
                                .requests()
                                .editor_ai_bar_no_enabled_models_add_a(),
                        ),
                );
            } else {
                for (i, (pid, pname, base, mid)) in choices.iter().take(80).enumerate() {
                    let ent_c = ent.clone();
                    let (pid_c, pname_c, base_c, mid_c) =
                        (pid.clone(), pname.clone(), base.clone(), mid.clone());
                    let ph = pop_cx.entity().clone();
                    list = list.child(
                        list_row(
                            SharedString::from(format!("ai-mdl-opt-{i}")),
                            false,
                            pop_cx,
                            move |_, window, cx| {
                                ent_c.update(cx, |this, cx| {
                                    this.set_ai_target_provider(
                                        pid_c.clone(),
                                        pname_c.clone(),
                                        base_c.clone(),
                                        mid_c.clone(),
                                        window,
                                        cx,
                                    );
                                });
                                ph.update(cx, |s, cx| s.dismiss(window, cx));
                            },
                        )
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .child(SharedString::from(format!("{pname} / {mid}"))),
                        ),
                    );
                }
            }
            list
        })
}
