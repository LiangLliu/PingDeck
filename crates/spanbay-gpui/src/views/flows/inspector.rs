//! Step Inspector for `FlowsView`: Request overrides, Extractions, and Assertions tabs.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use spanbay_desktop::core::model::items::{FlowBodyKind, FlowExtractionSource, FlowStep};
use spanbay_desktop::core::model::{HttpMethod, KeyValue};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_icon, btn_secondary, card, checkbox,
    choice_chip, code_editor, data_row, empty_inline, empty_state, h_row, meta_pill, palette,
    section_header, subtab_strip, tab_bar_underline, tab_item, text_input, to_hsla, v_col,
};

use super::inspector_exec::{render_assertions_tab, render_control_tab, render_output_tab};
use super::inspector_topology::render_topology_tab;
use super::state::{FlowModal, FlowStepTab, FlowsView};

const EXTRACTION_SOURCES: [(FlowExtractionSource, &str); 6] = [
    (FlowExtractionSource::BodyJsonPath, "JSONPath"),
    (FlowExtractionSource::Header, "Header"),
    (FlowExtractionSource::StatusCode, "Status"),
    (FlowExtractionSource::BodyRegex, "Regex"),
    (FlowExtractionSource::Cookie, "Cookie"),
    (FlowExtractionSource::ResponseBody, "Full Body"),
];

const BODY_KINDS: [(Option<FlowBodyKind>, &str); 5] = [
    (None, "Inherit"),
    (Some(FlowBodyKind::Json), "JSON"),
    (Some(FlowBodyKind::Text), "Text"),
    (Some(FlowBodyKind::Xml), "XML"),
    (Some(FlowBodyKind::FormUrlencoded), "Form"),
];

pub fn render_step_inspector(view: &FlowsView, cx: &mut Context<FlowsView>) -> AnyElement {
    let p = palette(cx);
    let fl = crate::ui::Tr(view.lang).flows();
    let Some(step) = view.selected_step().cloned() else {
        return empty_state(
            UiIcon::Workflow,
            fl.inspector_select_or_add_a_step(),
            fl.inspector_configure_request_overrides_variable_extra(),
            cx,
        )
        .into_any_element();
    };
    let step_idx = view
        .draft
        .as_ref()
        .and_then(|f| f.steps.iter().position(|s| s.id == step.id))
        .unwrap_or(0);
    let sid_dup = step.id.clone();
    let sid_del = step.id.clone();

    let header = h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(6.0))
        .px(px(12.0))
        .py(px(8.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(6.0))
                .child(meta_pill(format!("#{}", step_idx + 1), cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(SharedString::from(step.name.clone())),
                ),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(2.0))
                .child(
                    btn_icon("flow-insp-run1", UiIcon::Play)
                        .tooltip(fl.steps_pane_run_only_this_step())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.run_flow(None, Some(step_idx), None, cx);
                        })),
                )
                .child(
                    btn_icon("flow-insp-upto", UiIcon::Zap)
                        .tooltip(fl.steps_pane_run_up_to_this_step())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.run_flow(Some(step_idx + 1), None, None, cx);
                        })),
                )
                .child(
                    btn_icon("flow-insp-dup", UiIcon::Copy)
                        .tooltip(fl.insp_duplicate())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.duplicate_step(sid_dup.clone(), cx);
                        })),
                )
                .child(
                    btn_icon("flow-insp-del", UiIcon::Trash)
                        .tooltip(fl.insp_delete())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_step(sid_del.clone(), cx);
                        })),
                )
                .child(
                    btn_icon("flow-insp-close", UiIcon::Close)
                        .tooltip(fl.flows_hide_inspector())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.select_step(None, cx);
                        })),
                ),
        );

    let mut tabs = tab_bar_underline("flow-step-tabs")
        .selected_index(view.step_tab.index())
        .on_click(cx.listener(|this, idx: &usize, _, cx| {
            this.snapshot_inputs(cx);
            this.step_tab = FlowStepTab::from_index(*idx);
            cx.notify();
        }));
    for tab in FlowStepTab::ALL {
        tabs = tabs.child(tab_item(tab.label(view.lang)));
    }

    let body = match view.step_tab {
        FlowStepTab::Request => render_request_tab(view, &step, cx),
        FlowStepTab::Extractions => render_extractions_tab(view, &step, cx),
        FlowStepTab::Assertions => render_assertions_tab(view, &step, cx),
        FlowStepTab::Control => render_control_tab(view, &step, cx),
        FlowStepTab::Topology => render_topology_tab(view, &step, cx),
        FlowStepTab::Output => render_output_tab(view, &step, cx),
    };

    v_col()
        .size_full()
        .min_h_0()
        .bg(to_hsla(p.surface))
        .border_l_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(header)
        .child(subtab_strip(tabs, None, cx))
        .child(
            div()
                .id("flow-inspector-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(v_col().w_full().p(px(14.0)).gap(px(12.0)).child(body)),
        )
        .into_any_element()
}

fn render_request_tab(
    view: &FlowsView,
    step: &FlowStep,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let fl = crate::ui::Tr(view.lang).flows();
    let p = palette(cx);
    let linked = view.request_of(&step.request_id);
    let methods = [
        (None, "Inherit"),
        (Some(HttpMethod::Get), "GET"),
        (Some(HttpMethod::Post), "POST"),
        (Some(HttpMethod::Put), "PUT"),
        (Some(HttpMethod::Patch), "PATCH"),
        (Some(HttpMethod::Delete), "DELETE"),
    ];

    let mut m_row = h_row().gap(px(4.0)).flex_wrap();
    for (m, lbl) in methods {
        m_row = m_row.child(
            choice_chip(
                SharedString::from(format!("flow-sm-{lbl}")),
                lbl,
                step.method_override == m,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(st) = this.selected_step_mut() {
                    st.method_override = m;
                    this.dirty = true;
                    cx.notify();
                }
            })),
        );
    }

    let mut bk_row = h_row().gap(px(4.0)).flex_wrap().child(
        div()
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(fl.inspector_body_kind()),
    );
    for (bk, lbl) in BODY_KINDS {
        bk_row = bk_row.child(
            choice_chip(
                SharedString::from(format!("flow-bk-{lbl}")),
                lbl,
                step.body_override_kind == bk,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(st) = this.selected_step_mut() {
                    st.body_override_kind = bk;
                    this.dirty = true;
                    cx.notify();
                }
            })),
        );
    }

    let source_bar = h_row()
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .child(
            h_row()
                .gap(px(6.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(fl.inspector_linked()),
                )
                .child(meta_pill(
                    linked
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| fl.inspector_inline_step().to_owned()),
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(4.0))
                .children(linked.map(|r| {
                    let rid_open = r.id.as_str().to_owned();
                    crate::ui::btn_ghost("flow-open-linked", fl.inspector_open_request()).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.pending_handoff.set(Some(
                                super::state::FlowsHandoff::OpenRequest(rid_open.clone()),
                            ));
                            cx.notify();
                        }),
                    )
                }))
                .child(
                    btn_secondary("flow-pick-req", fl.inspector_pick_request()).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = Some(FlowModal::PickStepRequest);
                            cx.notify();
                        }),
                    ),
                )
                .children(linked.is_some().then(|| {
                    btn_icon("flow-unlink-req", UiIcon::Close)
                        .tooltip(fl.inspector_detach_into_inline_step())
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(st) = this.selected_step_mut() {
                                st.request_id.clear();
                                this.dirty = true;
                                cx.notify();
                            }
                        }))
                })),
        );

    let mut hdr_card = card(cx).p(px(12.0)).gap(px(6.0)).child(section_header(
        fl.inspector_header_overrides(),
        Some(badge(step.headers_override.len().to_string(), Tone::Neutral, cx).into_any_element()),
        cx,
    ));
    for (idx, kv) in step.headers_override.iter().enumerate() {
        hdr_card = hdr_card.child(
            data_row(cx)
                .child(
                    checkbox(SharedString::from(format!("flow-h-en-{idx}")), kv.enabled).on_click(
                        cx.listener(move |this, _, _, cx| {
                            if let Some(st) = this.selected_step_mut()
                                && let Some(h) = st.headers_override.get_mut(idx)
                            {
                                h.enabled = !h.enabled;
                                this.dirty = true;
                                cx.notify();
                            }
                        }),
                    ),
                )
                .child(
                    div()
                        .w(px(150.0))
                        .text_xs()
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
                        SharedString::from(format!("flow-h-rm-{idx}")),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(st) = this.selected_step_mut()
                            && idx < st.headers_override.len()
                        {
                            st.headers_override.remove(idx);
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                ),
        );
    }
    hdr_card = hdr_card.child(
        h_row()
            .w_full()
            .gap(px(6.0))
            .child(div().w(px(150.0)).child(text_input(&view.kv_key_input)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(text_input(&view.kv_val_input)),
            )
            .child(
                btn_secondary("flow-h-add", fl.inspector_header()).on_click(cx.listener(
                    |this, _, window, cx| {
                        let k = this.kv_key_input.read(cx).value().trim().to_owned();
                        let v = this.kv_val_input.read(cx).value().to_string();
                        if k.is_empty() {
                            return;
                        }
                        if let Some(st) = this.selected_step_mut() {
                            st.headers_override.push(KeyValue {
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
                    },
                )),
            ),
    );

    v_col()
        .w_full()
        .gap(px(12.0))
        .child(
            card(cx)
                .p(px(12.0))
                .gap(px(10.0))
                .child(source_bar)
                .child(
                    h_row()
                        .w_full()
                        .gap(px(8.0))
                        .child(div().w(px(180.0)).child(text_input(&view.step_name_input)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(text_input(&view.step_url_input)),
                        ),
                )
                .child(m_row)
                .child(bk_row)
                .child(
                    div()
                        .w_full()
                        .h(px(130.0))
                        .child(code_editor(&view.step_body_editor)),
                ),
        )
        .child(hdr_card)
        .into_any_element()
}

fn render_extractions_tab(
    view: &FlowsView,
    step: &FlowStep,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let fl = crate::ui::Tr(view.lang).flows();
    let p = palette(cx);
    let mut box_el = card(cx).p(px(12.0)).gap(px(8.0)).child(section_header(
        fl.inspector_variable_extractions(),
        Some(badge(step.extractions.len().to_string(), Tone::Neutral, cx).into_any_element()),
        cx,
    ));

    if step.extractions.is_empty() {
        box_el = box_el.child(empty_inline(
            fl.inspector_no_extractions_configured(),
            None,
            None,
            cx,
        ));
    } else {
        for (idx, ext) in step.extractions.iter().enumerate() {
            box_el = box_el.child(
                data_row(cx)
                    .child(
                        div()
                            .w(px(130.0))
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.accent_ink))
                            .truncate()
                            .child(format!("{{{{{}}}}}", ext.variable_name)),
                    )
                    .child(meta_pill(ext.source.as_str(), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(ext.expression.clone()),
                    )
                    .children(
                        ext.is_secret()
                            .then(|| badge(fl.insp_ext_secret(), Tone::Warning, cx)),
                    )
                    .child(
                        btn_icon(
                            SharedString::from(format!("flow-ext-rm-{idx}")),
                            UiIcon::Trash,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(st) = this.selected_step_mut()
                                && idx < st.extractions.len()
                            {
                                st.extractions.remove(idx);
                                this.dirty = true;
                                cx.notify();
                            }
                        })),
                    ),
            );
        }
    }

    let mut src_row = h_row().gap(px(4.0)).flex_wrap();
    for (src, lbl) in EXTRACTION_SOURCES {
        src_row = src_row.child(
            choice_chip(
                SharedString::from(format!("flow-esrc-{lbl}")),
                lbl,
                view.new_ext_source == src,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.new_ext_source = src;
                cx.notify();
            })),
        );
    }
    src_row = src_row.child(
        choice_chip("flow-esec", fl.inspector_secret(), view.new_ext_secret, cx).on_click(
            cx.listener(|this, _, _, cx| {
                this.new_ext_secret = !this.new_ext_secret;
                cx.notify();
            }),
        ),
    );

    box_el
        .child(src_row)
        .child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .child(div().w(px(150.0)).child(text_input(&view.ext_var_input)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(text_input(&view.ext_expr_input)),
                )
                .child(
                    btn_secondary("flow-ext-add", fl.inspector_add()).on_click(
                        cx.listener(|this, _, window, cx| this.add_extraction(window, cx)),
                    ),
                ),
        )
        .into_any_element()
}
