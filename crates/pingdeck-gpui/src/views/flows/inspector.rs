//! Step Inspector for `FlowsView`: Request overrides, Extractions, and Assertions tabs.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::model::items::{FlowBodyKind, FlowExtractionSource, FlowStep};
use pingdeck_desktop::core::model::{Assertion, HttpMethod, KeyValue};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_icon, btn_secondary, card, checkbox,
    choice_chip, code_editor, data_row, empty_inline, empty_state, h_row, meta_pill, palette,
    section_header, setting_row, subtab_strip, tab_bar_underline, tab_item, text_input, to_hsla,
    toggle_switch, v_col,
};

use super::inspector_exec::{render_control_tab, render_output_tab};
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
    let zh = view.is_zh();
    let Some(step) = view.selected_step().cloned() else {
        return empty_state(
            UiIcon::Workflow,
            view.t("选择或添加一个步骤", "Select or add a step"),
            view.t(
                "在右侧配置请求覆盖、变量提取（JSONPath/Header/Regex/Cookie）、断言及失败重试。",
                "Configure request overrides, variable extractions, assertions, conditions, and retries.",
            ),
            cx,
        )
        .into_any_element();
    };

    let mut tabs = tab_bar_underline("flow-step-tabs")
        .selected_index(view.step_tab.index())
        .on_click(cx.listener(|this, idx: &usize, _, cx| {
            this.snapshot_inputs(cx);
            this.step_tab = FlowStepTab::from_index(*idx);
            cx.notify();
        }));
    for tab in FlowStepTab::ALL {
        tabs = tabs.child(tab_item(tab.label(zh)));
    }

    let body = match view.step_tab {
        FlowStepTab::Request => render_request_tab(view, &step, cx),
        FlowStepTab::Extractions => render_extractions_tab(view, &step, cx),
        FlowStepTab::Assertions => render_assertions_tab(view, &step, cx),
        FlowStepTab::Control => render_control_tab(view, &step, cx),
        FlowStepTab::Output => render_output_tab(view, &step, cx),
    };

    v_col()
        .size_full()
        .min_h_0()
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
            .child(view.t("Body 类型:", "Body Kind:")),
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
                        .child(view.t("关联请求:", "Linked:")),
                )
                .child(meta_pill(
                    linked.map(|r| r.name.clone()).unwrap_or_else(|| {
                        view.t("内联步骤（无关联请求）", "Inline step").to_owned()
                    }),
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(4.0))
                .child(
                    btn_secondary("flow-pick-req", view.t("选择请求…", "Pick Request…")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = Some(FlowModal::AddSavedStep);
                            cx.notify();
                        }),
                    ),
                )
                .children(linked.is_some().then(|| {
                    btn_icon("flow-unlink-req", UiIcon::Close)
                        .tooltip(view.t("转为内联步骤", "Detach into inline step"))
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
        view.t("请求头覆盖", "Header Overrides"),
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
                btn_secondary("flow-h-add", view.t("+ 请求头", "+ Header")).on_click(cx.listener(
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
    let p = palette(cx);
    let mut box_el = card(cx).p(px(12.0)).gap(px(8.0)).child(section_header(
        view.t(
            "响应变量提取（供后续步骤 {{var}} 使用）",
            "Variable Extractions",
        ),
        Some(badge(step.extractions.len().to_string(), Tone::Neutral, cx).into_any_element()),
        cx,
    ));

    if step.extractions.is_empty() {
        box_el = box_el.child(empty_inline(
            view.t("尚未配置提取规则", "No extractions configured"),
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
                            .then(|| badge(view.t("机密", "Secret"), Tone::Warning, cx)),
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
        choice_chip(
            "flow-esec",
            view.t("标记机密", "Secret"),
            view.new_ext_secret,
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.new_ext_secret = !this.new_ext_secret;
            cx.notify();
        })),
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
                    btn_secondary("flow-ext-add", view.t("+ 提取", "+ Add")).on_click(
                        cx.listener(|this, _, window, cx| this.add_extraction(window, cx)),
                    ),
                ),
        )
        .into_any_element()
}

fn render_assertions_tab(
    view: &FlowsView,
    step: &FlowStep,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let p = palette(cx);
    let kinds = [
        (0, view.t("状态码等于", "Status ==")),
        (1, view.t("响应头存在", "Header Exists")),
        (2, view.t("JSONPath 存在", "JSONPath Exists")),
        (3, view.t("正文包含", "Body Contains")),
        (4, view.t("耗时小于(ms)", "Time < ms")),
    ];

    let mut box_el = card(cx)
        .p(px(12.0))
        .gap(px(8.0))
        .child(section_header(
            view.t("步骤断言", "Step Assertions"),
            None,
            cx,
        ))
        .child(setting_row(
            view.t(
                "忽略关联请求自带的断言",
                "Ignore linked request's own assertions",
            ),
            None,
            Some(SharedString::from(view.t(
                "仅按当前步骤配置的断言判定通过（如删除后期望 404）。",
                "Judge this step only by its own assertions (e.g. expecting 404 after delete).",
            ))),
            toggle_switch("flow-ign-assert", step.ignore_request_assertions).on_click(cx.listener(
                |this, _, _, cx| {
                    if let Some(st) = this.selected_step_mut() {
                        st.ignore_request_assertions = !st.ignore_request_assertions;
                        this.dirty = true;
                        cx.notify();
                    }
                },
            )),
            cx,
        ));

    for (idx, a) in step.assertions.iter().enumerate() {
        box_el = box_el.child(
            data_row(cx)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(format_assertion(a)),
                )
                .child(
                    btn_icon(
                        SharedString::from(format!("flow-as-rm-{idx}")),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(st) = this.selected_step_mut()
                            && idx < st.assertions.len()
                        {
                            st.assertions.remove(idx);
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                ),
        );
    }

    let mut kind_row = h_row().gap(px(4.0)).flex_wrap();
    for (idx, lbl) in kinds {
        kind_row = kind_row.child(
            choice_chip(
                SharedString::from(format!("flow-ak-{idx}")),
                lbl,
                view.new_assert_kind_idx == idx,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.new_assert_kind_idx = idx;
                cx.notify();
            })),
        );
    }

    box_el
        .child(kind_row)
        .child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .child(
                    div()
                        .w(px(160.0))
                        .child(text_input(&view.assert_path_input)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(text_input(&view.assert_val_input)),
                )
                .child(
                    btn_secondary("flow-as-add", view.t("+ 断言", "+ Add")).on_click(
                        cx.listener(|this, _, window, cx| this.add_quick_assertion(window, cx)),
                    ),
                ),
        )
        .into_any_element()
}

fn format_assertion(a: &Assertion) -> String {
    match a {
        Assertion::StatusEquals { value } => format!("Status == {value}"),
        Assertion::StatusIn { values } => format!("Status in {values:?}"),
        Assertion::HeaderExists { key } => format!("Header exists: {key}"),
        Assertion::HeaderEquals { key, value } => format!("Header {key} == {value}"),
        Assertion::JsonPathExists { path } => format!("JSONPath exists: {path}"),
        Assertion::JsonPathEquals { path, value } => format!("JSONPath {path} == {value}"),
        Assertion::BodyContains { value } => format!("Body contains \"{value}\""),
        Assertion::ResponseTimeLessThanMs { value } => format!("Elapsed < {value} ms"),
        Assertion::BodySizeLessThanBytes { value } => format!("Size < {value} B"),
        other => format!("{other:?}"),
    }
}
