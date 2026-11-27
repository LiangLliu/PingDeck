//! Declarative Assertions builder: HTTP checks, JSON Schema validation, and AI assertions.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::model::models::{Assertion, AssertionOp, AssertionTarget};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_icon, btn_secondary, data_row, h_row, palette,
    tab_bar_segmented, tab_item, text_input, to_hsla, v_col,
};

use super::RequestsView;
use super::model::assertion_short_label;

pub fn render_assertions_tab(
    view: &RequestsView,
    is_ai: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::AnyElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().into_any_element();
    };
    let last_results = if is_ai {
        tab.last_ai.as_ref().map(|r| r.assertion_results.as_slice())
    } else {
        tab.last_http
            .as_ref()
            .map(|r| r.assertion_results.as_slice())
    }
    .unwrap_or(&[]);

    let ent_t = cx.entity().clone();
    let ent_o = cx.entity().clone();

    let target_seg = if is_ai {
        tab_bar_segmented("assert-ai-target-seg")
            .selected_index(view.assert_target_kind.min(5))
            .child(tab_item("NoError"))
            .child(tab_item("Reply Contains"))
            .child(tab_item("Reply Regex"))
            .child(tab_item("TTFT < ms"))
            .child(tab_item("Tokens <"))
            .child(tab_item("JSON Schema"))
            .on_click(move |idx, _, cx| {
                ent_t.update(cx, |this, cx| {
                    this.assert_target_kind = *idx;
                    cx.notify();
                });
            })
    } else {
        tab_bar_segmented("assert-http-target-seg")
            .selected_index(view.assert_target_kind.min(5))
            .child(tab_item("Status"))
            .child(tab_item("JSONPath"))
            .child(tab_item("Header"))
            .child(tab_item("Body"))
            .child(tab_item("Time ms"))
            .child(tab_item("JSON Schema"))
            .on_click(move |idx, _, cx| {
                ent_t.update(cx, |this, cx| {
                    this.assert_target_kind = *idx;
                    cx.notify();
                });
            })
    };

    let op_seg = tab_bar_segmented("assert-op-seg")
        .selected_index(view.assert_op_idx.min(6))
        .child(tab_item("=="))
        .child(tab_item("!="))
        .child(tab_item("exists"))
        .child(tab_item("contains"))
        .child(tab_item("<"))
        .child(tab_item(">"))
        .child(tab_item("matches"))
        .on_click(move |idx, _, cx| {
            ent_o.update(cx, |this, cx| {
                this.assert_op_idx = *idx;
                cx.notify();
            });
        });

    let needs_value = !((is_ai && view.assert_target_kind == 0)
        || (!is_ai && view.assert_target_kind < 5 && view.assert_op_idx == 2));

    let mut root = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(8.0))
        .overflow_y_scrollbar()
        .child(
            h_row()
                .flex_wrap()
                .gap(px(6.0))
                .child(target_seg)
                .children((!is_ai && view.assert_target_kind < 5).then_some(op_seg)),
        )
        .child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .children(
                    (!is_ai && matches!(view.assert_target_kind, 1 | 2 | 5)).then(|| {
                        div()
                            .w(px(200.0))
                            .child(text_input(&view.assert_target_input))
                    }),
                )
                .children(needs_value.then(|| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(text_input(&view.assert_val_input))
                }))
                .children((!needs_value).then(|| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(
                            crate::ui::tr(cx)
                                .requests()
                                .editor_assertions_no_expected_value_needed_for(),
                        )
                }))
                .child(
                    btn_secondary(
                        "assert-add-btn",
                        crate::ui::tr(cx).requests().editor_assertions_add(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let target_txt =
                            this.assert_target_input.read(cx).value().trim().to_owned();
                        let val_txt = this.assert_val_input.read(cx).value().trim().to_owned();
                        let assertion = build_assertion(
                            is_ai,
                            this.assert_target_kind,
                            this.assert_op_idx,
                            &target_txt,
                            &val_txt,
                            crate::ui::active_language(cx),
                        );
                        match assertion {
                            Ok(a) => {
                                if let Err(err) = a.validate() {
                                    this.status_msg = Some((Tone::Danger, err.to_string()));
                                } else if let Some(t) = this.active_req_mut() {
                                    t.draft.assertions.push(a);
                                    t.recompute_dirty();
                                    this.assert_val_input
                                        .update(cx, |i, cx| i.set_value("", window, cx));
                                }
                            }
                            Err(msg) => this.status_msg = Some((Tone::Danger, msg)),
                        }
                        cx.notify();
                    })),
                ),
        );

    for (idx, a) in tab.draft.assertions.iter().enumerate() {
        let (target_s, op_s, val_s) = assertion_short_label(a);
        let res = last_results.iter().find(|r| r.index == idx);
        root = root.child(
            data_row(cx)
                .child(
                    div()
                        .w(px(180.0))
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .truncate()
                        .child(SharedString::from(target_s)),
                )
                .child(badge(op_s, Tone::Neutral, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .truncate()
                        .child(SharedString::from(val_s)),
                )
                .children(res.map(|r| {
                    badge(
                        if r.passed {
                            crate::ui::tr(cx).requests().editor_assertions_pass()
                        } else {
                            crate::ui::tr(cx).requests().editor_assertions_fail()
                        },
                        if r.passed {
                            Tone::Success
                        } else {
                            Tone::Danger
                        },
                        cx,
                    )
                }))
                .child(
                    btn_icon(
                        SharedString::from(format!("assert-del-{idx}")),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(t) = this.active_req_mut()
                            && idx < t.draft.assertions.len()
                        {
                            t.draft.assertions.remove(idx);
                            t.recompute_dirty();
                        }
                        cx.notify();
                    })),
                ),
        );
    }

    root.into_any_element()
}

fn build_assertion(
    is_ai: bool,
    kind: usize,
    op_idx: usize,
    target_txt: &str,
    val_txt: &str,
    lang: spanbay_desktop::core::model::settings::Language,
) -> Result<Assertion, String> {
    if is_ai {
        return Ok(match kind {
            0 => Assertion::AiNoError {},
            1 => Assertion::AiReplyContains {
                value: val_txt.to_owned(),
            },
            2 => Assertion::AiReplyMatches {
                pattern: val_txt.to_owned(),
            },
            3 => Assertion::AiTtftLessThanMs {
                value: val_txt.parse().unwrap_or(1500),
            },
            4 => Assertion::AiTokensLessThan {
                value: val_txt.parse().unwrap_or(4096),
            },
            _ => {
                let schema = serde_json::from_str(val_txt).map_err(|e| {
                    crate::ui::Tr(lang)
                        .requests()
                        .editor_assertions_invalid_schema(e)
                })?;
                Assertion::AiJsonSchemaValid { schema }
            }
        });
    }
    if kind == 5 {
        let schema = serde_json::from_str(val_txt).map_err(|e| {
            crate::ui::Tr(lang)
                .requests()
                .editor_assertions_invalid_schema(e)
        })?;
        return Ok(Assertion::JsonSchemaValid {
            schema,
            path: (!target_txt.is_empty()).then(|| target_txt.to_owned()),
        });
    }
    let target = match kind {
        0 => AssertionTarget::Status,
        1 => AssertionTarget::JsonPath {
            path: target_txt.to_owned(),
        },
        2 => AssertionTarget::Header {
            name: target_txt.to_owned(),
        },
        3 => AssertionTarget::Body,
        _ => AssertionTarget::ResponseTimeMs,
    };
    let op = match op_idx {
        1 => AssertionOp::NotEquals,
        2 => AssertionOp::Exists,
        3 => AssertionOp::Contains,
        4 => AssertionOp::LessThan,
        5 => AssertionOp::GreaterThan,
        6 => AssertionOp::Matches,
        _ => AssertionOp::Equals,
    };
    let value = if op == AssertionOp::Exists {
        serde_json::Value::Null
    } else {
        serde_json::from_str(val_txt)
            .unwrap_or_else(|_| serde_json::Value::String(val_txt.to_owned()))
    };
    Ok(Assertion::Check { target, op, value })
}
