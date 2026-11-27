//! HTTP Request Body editor: all 7 modes (None, JSON, Raw, Form, Multipart, GraphQL, Binary).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::model::{
    KeyValue, RawLanguage, RequestBody, RequestBodyFileValidationStatus,
};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_ghost, btn_icon, btn_secondary,
    checkbox, code_editor, data_row, empty_inline, h_row, palette, section_heading,
    tab_bar_segmented, tab_item, table_header, text_input, to_hsla, v_col,
};

use super::RequestsView;
use super::model::{BodyKind, raw_lang_label};

pub(super) fn render_body_tab(
    view: &RequestsView,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col();
    };
    let kind = BodyKind::from_body(&tab.draft.body);
    let ent = cx.entity().clone();

    let seg = tab_bar_segmented("req-body-kind-seg")
        .selected_index(kind.index())
        .child(tab_item(crate::ui::tr(cx).requests().editor_body_none_2()))
        .child(tab_item("JSON"))
        .child(tab_item("Raw"))
        .child(tab_item("Form"))
        .child(tab_item("Multipart"))
        .child(tab_item("GraphQL"))
        .child(tab_item("Binary"))
        .on_click(move |idx, window, cx| {
            let next = BodyKind::from_index(*idx);
            ent.update(cx, |this, cx| {
                this.snapshot_active_tab(cx);
                if let Some(t) = this.active_req_mut() {
                    let prev_txt = match &t.draft.body {
                        RequestBody::Json { content } | RequestBody::RawText { content, .. } => {
                            content.clone()
                        }
                        _ => String::new(),
                    };
                    t.draft.body = match next {
                        BodyKind::None => RequestBody::None,
                        BodyKind::Json => RequestBody::Json { content: prev_txt },
                        BodyKind::Raw => RequestBody::RawText {
                            content: prev_txt,
                            language: Some(RawLanguage::Text),
                        },
                        BodyKind::Form => RequestBody::FormUrlEncoded { fields: Vec::new() },
                        BodyKind::Multipart => RequestBody::Multipart { parts: Vec::new() },
                        BodyKind::Graphql => RequestBody::Graphql {
                            query: "query {\n  \n}".into(),
                            variables_json: Some("{}".into()),
                            operation_name: None,
                        },
                        BodyKind::Binary => RequestBody::Binary {
                            file_path: String::new(),
                            content_type: None,
                        },
                    };
                    t.recompute_dirty();
                }
                this.hydrate_active_tab(window, cx);
                cx.notify();
            });
        });

    let mut top_bar = h_row().w_full().justify_between().gap(px(8.0)).child(seg);
    if kind == BodyKind::Json {
        top_bar = top_bar.child(
            btn_ghost(
                "req-body-fmt",
                crate::ui::tr(cx).requests().editor_body_prettify(),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                let raw = this.body_editor.read(cx).value().to_string();
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
                    && let Ok(pretty) = serde_json::to_string_pretty(&val)
                {
                    this.body_editor
                        .update(cx, |e, cx| e.set_value(pretty, window, cx));
                    this.mark_active_dirty(cx);
                }
            })),
        );
    } else if let RequestBody::RawText { language, .. } = &tab.draft.body {
        let cur_lang = *language;
        let ent_l = cx.entity().clone();
        top_bar = top_bar.child(
            btn_secondary("req-raw-lang-cycle", raw_lang_label(cur_lang)).on_click(
                move |_, window, cx| {
                    let next = match cur_lang.unwrap_or(RawLanguage::Text) {
                        RawLanguage::Text => RawLanguage::Xml,
                        RawLanguage::Xml => RawLanguage::Html,
                        RawLanguage::Html => RawLanguage::Javascript,
                        RawLanguage::Javascript => RawLanguage::Text,
                    };
                    ent_l.update(cx, |this, cx| {
                        this.snapshot_active_tab(cx);
                        if let Some(t) = this.active_req_mut()
                            && let RequestBody::RawText { language, .. } = &mut t.draft.body
                        {
                            *language = Some(next);
                            t.recompute_dirty();
                        }
                        this.hydrate_active_tab(window, cx);
                        cx.notify();
                    });
                },
            ),
        );
    }

    let detail: gpui_kit::AnyElement = match &tab.draft.body {
        RequestBody::None => empty_inline(
            crate::ui::tr(cx).requests().editor_body_no_request_body(),
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .requests()
                    .editor_body_select_json_raw_form_multipart(),
            )),
            None,
            cx,
        )
        .into_any_element(),
        RequestBody::Json { .. } | RequestBody::RawText { .. } => v_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(code_editor(&view.body_editor))
            .into_any_element(),
        RequestBody::FormUrlEncoded { fields } => {
            let mut col = v_col()
                .flex_1()
                .min_h_0()
                .gap(px(6.0))
                .overflow_y_scrollbar()
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(div().w(px(180.0)).child(text_input(&view.kv_key_input)))
                        .child(div().flex_1().child(text_input(&view.kv_val_input)))
                        .child(
                            btn_secondary(
                                "form-kv-add",
                                crate::ui::tr(cx).requests().editor_body_add_field(),
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    let k = this.kv_key_input.read(cx).value().trim().to_owned();
                                    let v = this.kv_val_input.read(cx).value().to_string();
                                    if k.is_empty() {
                                        return;
                                    }
                                    if let Some(t) = this.active_req_mut()
                                        && let RequestBody::FormUrlEncoded { fields } =
                                            &mut t.draft.body
                                    {
                                        fields.push(KeyValue {
                                            key: k,
                                            value: v,
                                            enabled: true,
                                            description: None,
                                        });
                                        t.recompute_dirty();
                                    }
                                    this.kv_key_input
                                        .update(cx, |i, cx| i.set_value("", window, cx));
                                    this.kv_val_input
                                        .update(cx, |i, cx| i.set_value("", window, cx));
                                    cx.notify();
                                },
                            )),
                        ),
                );
            if !fields.is_empty() {
                col = col.child(table_header(
                    &[
                        ("", Some(22.0)),
                        (
                            crate::ui::tr(cx).requests().editor_body_field(),
                            Some(180.0),
                        ),
                        (crate::ui::tr(cx).common().secret_value_placeholder(), None),
                        ("", Some(28.0)),
                    ],
                    cx,
                ));
            }
            for (i, f) in fields.iter().enumerate() {
                col = col.child(
                    data_row(cx)
                        .child(
                            checkbox(SharedString::from(format!("form-chk-{i}")), f.enabled)
                                .on_click(cx.listener(move |this, c: &bool, _, cx| {
                                    if let Some(t) = this.active_req_mut()
                                        && let RequestBody::FormUrlEncoded { fields } =
                                            &mut t.draft.body
                                        && let Some(r) = fields.get_mut(i)
                                    {
                                        r.enabled = *c;
                                        t.recompute_dirty();
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .w(px(180.0))
                                .text_xs()
                                .child(SharedString::from(f.key.clone())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .child(SharedString::from(f.value.clone())),
                        )
                        .child(
                            btn_icon(SharedString::from(format!("form-del-{i}")), UiIcon::Trash)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(t) = this.active_req_mut()
                                        && let RequestBody::FormUrlEncoded { fields } =
                                            &mut t.draft.body
                                        && i < fields.len()
                                    {
                                        fields.remove(i);
                                        t.recompute_dirty();
                                    }
                                    cx.notify();
                                })),
                        ),
                );
            }
            col.into_any_element()
        }
        RequestBody::Multipart { parts } => {
            let mut col = v_col()
                .flex_1()
                .min_h_0()
                .gap(px(6.0))
                .overflow_y_scrollbar()
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(div().w(px(140.0)).child(text_input(&view.kv_key_input)))
                        .child(div().flex_1().child(text_input(&view.kv_val_input)))
                        .child(div().w(px(180.0)).child(text_input(&view.kv_file_input)))
                        .child(
                            btn_ghost(
                                "mp-pick-file",
                                crate::ui::tr(cx).requests().editor_body_file(),
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| this.pick_multipart_file(window, cx),
                            )),
                        )
                        .child(
                            btn_secondary(
                                "mp-add",
                                crate::ui::tr(cx).requests().container_settings_add(),
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| this.add_multipart_part(window, cx),
                            )),
                        ),
                );
            if !parts.is_empty() {
                col = col.child(table_header(
                    &[
                        ("", Some(22.0)),
                        (
                            crate::ui::tr(cx).requests().editor_body_field(),
                            Some(140.0),
                        ),
                        (
                            crate::ui::tr(cx).requests().editor_auth_token_type(),
                            Some(56.0),
                        ),
                        (crate::ui::tr(cx).requests().editor_body_value_file(), None),
                        ("", Some(28.0)),
                    ],
                    cx,
                ));
            }
            for (i, part) in parts.iter().enumerate() {
                let is_file = part.file_path.is_some();
                col = col.child(
                    data_row(cx)
                        .child(
                            checkbox(SharedString::from(format!("mp-chk-{i}")), part.enabled)
                                .on_click(cx.listener(move |this, c: &bool, _, cx| {
                                    if let Some(t) = this.active_req_mut()
                                        && let RequestBody::Multipart { parts } = &mut t.draft.body
                                        && let Some(r) = parts.get_mut(i)
                                    {
                                        r.enabled = *c;
                                        t.recompute_dirty();
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .w(px(140.0))
                                .text_xs()
                                .child(SharedString::from(part.key.clone())),
                        )
                        .child(badge(
                            if is_file { "File" } else { "Text" },
                            Tone::Neutral,
                            cx,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .truncate()
                                .child(SharedString::from(
                                    part.file_path.clone().unwrap_or_else(|| part.value.clone()),
                                )),
                        )
                        .child(
                            btn_icon(SharedString::from(format!("mp-del-{i}")), UiIcon::Trash)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(t) = this.active_req_mut()
                                        && let RequestBody::Multipart { parts } = &mut t.draft.body
                                        && i < parts.len()
                                    {
                                        parts.remove(i);
                                        t.recompute_dirty();
                                    }
                                    cx.notify();
                                })),
                        ),
                );
            }
            col.into_any_element()
        }
        RequestBody::Graphql { .. } => h_row()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap(px(8.0))
            .child(
                v_col()
                    .flex_1()
                    .h_full()
                    .gap(px(4.0))
                    .child(section_heading("Query", cx))
                    .child(
                        v_col()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .child(code_editor(&view.body_editor)),
                    ),
            )
            .child(
                v_col()
                    .w(px(280.0))
                    .h_full()
                    .gap(px(4.0))
                    .child(section_heading("Variables (JSON)", cx))
                    .child(
                        v_col()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .child(code_editor(&view.gql_vars_editor)),
                    ),
            )
            .into_any_element(),
        RequestBody::Binary { .. } => {
            let status_el = tab.binary_validation.as_ref().map(|v| {
                let tone = if v.status == RequestBodyFileValidationStatus::Ready {
                    Tone::Success
                } else {
                    Tone::Warning
                };
                alert_banner(tone, v.message.clone(), None, cx)
            });
            v_col()
                .gap(px(10.0))
                .p(px(8.0))
                .child(
                    h_row()
                        .gap(px(8.0))
                        .child(div().flex_1().child(text_input(&view.binary_path_input)))
                        .child(
                            btn_secondary(
                                "bin-pick",
                                crate::ui::tr(cx).requests().editor_body_browse(),
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.pick_binary_body_file(window, cx);
                                },
                            )),
                        )
                        .child(
                            btn_ghost(
                                "bin-check",
                                crate::ui::tr(cx).requests().editor_body_validate(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.validate_binary_body_file(cx);
                            })),
                        ),
                )
                .children(status_el)
                .into_any_element()
        }
    };

    v_col()
        .size_full()
        .p(px(10.0))
        .gap(px(8.0))
        .child(top_bar)
        .child(detail)
}
