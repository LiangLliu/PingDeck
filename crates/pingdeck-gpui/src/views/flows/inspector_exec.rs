//! Control & Retry and Step Output inspector tabs for `FlowsView`.

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    px,
};
use pingdeck_desktop::core::app::factory_service::assertion_new;
use pingdeck_desktop::core::model::Assertion;
use pingdeck_desktop::core::model::items::{FlowExtraction, FlowRetryOn, FlowStep};

use crate::ui::{
    PayloadBlockOptions, Tone, UiIcon, alert_banner, badge, btn_icon, btn_secondary, card,
    choice_chip, data_row, empty_inline, h_row, kv_line, meta_pill, number_field, palette,
    payload_block, section_header, setting_row, text_input, to_hsla, toggle_switch, v_col,
};

use super::state::FlowsView;

pub fn render_control_tab(
    view: &FlowsView,
    step: &FlowStep,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let retry_targets = [
        (
            FlowRetryOn::Network,
            crate::ui::Tr(view.lang).flows().inspector_exec_network(),
        ),
        (FlowRetryOn::Status5xx, "5xx"),
        (FlowRetryOn::Status429, "429"),
    ];
    let mut retry_row = h_row().gap(px(4.0)).flex_wrap();
    for (kind, lbl) in retry_targets {
        let on = step.retry_on.contains(&kind);
        retry_row = retry_row.child(
            choice_chip(
                SharedString::from(format!("flow-ron-{kind:?}")),
                lbl,
                on,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(st) = this.selected_step_mut() {
                    if let Some(pos) = st.retry_on.iter().position(|x| *x == kind) {
                        st.retry_on.remove(pos);
                    } else {
                        st.retry_on.push(kind);
                    }
                    this.dirty = true;
                    cx.notify();
                }
            })),
        );
    }

    card(cx)
        .p(px(12.0))
        .child(section_header(
            crate::ui::Tr(view.lang)
                .flows()
                .inspector_exec_condition_retry(),
            None,
            cx,
        ))
        .child(setting_row(
            crate::ui::Tr(view.lang)
                .flows()
                .inspector_exec_condition_expression(),
            None,
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .flows()
                    .inspector_exec_blank_runs_always_e_g(),
            )),
            div().w(px(280.0)).child(text_input(&view.step_cond_input)),
            cx,
        ))
        .child(setting_row(
            crate::ui::Tr(view.lang)
                .flows()
                .inspector_exec_pre_step_delay(),
            None,
            None,
            number_field(&view.step_delay_input, Some("ms"), 90.0, cx),
            cx,
        ))
        .child(setting_row(
            crate::ui::Tr(view.lang)
                .flows()
                .inspector_exec_attempt_timeout(),
            None,
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .flows()
                    .inspector_exec_blank_inherits_request_timeout(),
            )),
            number_field(&view.step_timeout_input, Some("ms"), 90.0, cx),
            cx,
        ))
        .child(setting_row(
            crate::ui::Tr(view.lang)
                .flows()
                .inspector_exec_continue_on_error(),
            None,
            None,
            toggle_switch("flow-cont-err", step.continue_on_error).on_click(cx.listener(
                |this, _, _, cx| {
                    if let Some(st) = this.selected_step_mut() {
                        st.continue_on_error = !st.continue_on_error;
                        this.dirty = true;
                        cx.notify();
                    }
                },
            )),
            cx,
        ))
        .child(setting_row(
            crate::ui::Tr(view.lang)
                .flows()
                .inspector_exec_retries_0_5_wait(),
            None,
            None,
            h_row()
                .gap(px(8.0))
                .child(number_field(&view.step_retries_input, Some("×"), 64.0, cx))
                .child(number_field(
                    &view.step_retry_wait_input,
                    Some("ms"),
                    80.0,
                    cx,
                )),
            cx,
        ))
        .child(setting_row(
            crate::ui::Tr(view.lang).flows().inspector_exec_retry_on(),
            None,
            None,
            retry_row,
            cx,
        ))
        .into_any_element()
}

pub fn render_output_tab(
    view: &FlowsView,
    step: &FlowStep,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let p = palette(cx);
    let Some(res) = view.step_result_of(&step.id) else {
        if let Some(sum) = view.step_summary_of(&step.id) {
            return render_summary_output(view, sum, cx);
        }
        return empty_inline(
            crate::ui::Tr(view.lang)
                .flows()
                .inspector_exec_no_output_for_this_step(),
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .flows()
                    .inspector_exec_run_the_flow_or_this(),
            )),
            None,
            cx,
        )
        .into_any_element();
    };

    let secrets = view
        .last_result
        .as_ref()
        .map_or(&[][..], |r| r.secret_variables.as_slice());

    let mut box_el = v_col().w_full().gap(px(10.0));
    if let Some(exp) = &res.explanation {
        box_el = box_el.child(alert_banner(Tone::Danger, exp.message.clone(), None, cx));
    } else if let Some(err) = &res.error {
        box_el = box_el.child(alert_banner(Tone::Danger, err.clone(), None, cx));
    }

    let mut meta = h_row()
        .gap(px(6.0))
        .flex_wrap()
        .child(badge(
            if res.skipped {
                crate::ui::Tr(view.lang)
                    .common()
                    .status_skipped()
                    .to_owned()
            } else if res.passed {
                format!("HTTP {} · PASS", res.status_code.unwrap_or(0))
            } else {
                format!("HTTP {} · FAIL", res.status_code.unwrap_or(0))
            },
            if res.skipped {
                Tone::Warning
            } else if res.passed {
                Tone::Success
            } else {
                Tone::Danger
            },
            cx,
        ))
        .child(meta_pill(format!("{} ms", res.elapsed_ms), cx))
        .child(meta_pill(format!("attempts {}", res.attempts), cx));
    if let Some(cond) = &res.condition {
        meta = meta.child(meta_pill(format!("cond: {cond}"), cx));
    }
    box_el = box_el.child(meta);

    if let Some(exec) = &res.executed_request {
        box_el = box_el.child(kv_line(
            crate::ui::Tr(view.lang).flows().inspector_exec_sent(),
            format!("{} {}", exec.method, exec.url),
            100.0,
            cx,
        ));
    }

    if !res.extracted_variables.is_empty() {
        let mut ext_card = card(cx).p(px(10.0)).gap(px(4.0)).child(section_header(
            crate::ui::Tr(view.lang)
                .flows()
                .inspector_exec_extracted_variables(),
            None,
            cx,
        ));
        for kv in &res.extracted_variables {
            let masked = secrets.iter().any(|s| s == &kv.key);
            ext_card = ext_card.child(kv_line(
                kv.key.clone(),
                if masked {
                    "••••••••".to_owned()
                } else {
                    kv.value.clone()
                },
                140.0,
                cx,
            ));
        }
        box_el = box_el.child(ext_card);
    }

    if !res.assertion_results.is_empty() {
        let mut as_card = card(cx).p(px(10.0)).gap(px(4.0)).child(section_header(
            crate::ui::Tr(view.lang).flows().inspector_exec_assertions(),
            None,
            cx,
        ));
        for ar in &res.assertion_results {
            as_card = as_card.child(
                data_row(cx)
                    .child(badge(
                        if ar.passed { "PASS" } else { "FAIL" },
                        if ar.passed {
                            Tone::Success
                        } else {
                            Tone::Danger
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .child(ar.message.clone()),
                    ),
            );
        }
        box_el = box_el.child(as_card);
    }

    if !res.response_body_preview.is_empty() {
        box_el = box_el.child(payload_block(
            format!("flow-step-out-{}", step.id),
            &res.response_body_preview,
            PayloadBlockOptions {
                max_height_px: Some(320.0),
                ..Default::default()
            },
            cx,
        ));
    }

    box_el.into_any_element()
}

fn render_summary_output(
    view: &FlowsView,
    sum: &pingdeck_desktop::core::model::items::FlowStepSummary,
    cx: &Context<FlowsView>,
) -> AnyElement {
    let mut box_el = v_col().w_full().gap(px(10.0));
    if let Some(exp) = &sum.explanation {
        box_el = box_el.child(alert_banner(Tone::Danger, exp.message.clone(), None, cx));
    } else if let Some(err) = &sum.error {
        box_el = box_el.child(alert_banner(Tone::Danger, err.clone(), None, cx));
    }
    let mut meta = h_row()
        .gap(px(6.0))
        .flex_wrap()
        .child(badge(
            if sum.skipped {
                crate::ui::Tr(view.lang)
                    .common()
                    .status_skipped()
                    .to_owned()
            } else if let Some(code) = sum.status_code {
                format!("HTTP {code} · {}", if sum.passed { "PASS" } else { "FAIL" })
            } else if sum.passed {
                "PASS".to_owned()
            } else {
                "FAIL".to_owned()
            },
            if sum.skipped {
                Tone::Warning
            } else if sum.passed {
                Tone::Success
            } else {
                Tone::Danger
            },
            cx,
        ))
        .child(meta_pill(format!("{} ms", sum.elapsed_ms), cx))
        .child(meta_pill(format!("attempts {}", sum.attempts), cx));
    if sum.assertions_passed + sum.assertions_failed > 0 {
        meta = meta.child(meta_pill(
            format!("✓ {} / ✗ {}", sum.assertions_passed, sum.assertions_failed),
            cx,
        ));
    }
    if let Some(cond) = &sum.condition {
        meta = meta.child(meta_pill(format!("cond: {cond}"), cx));
    }
    box_el.child(meta).into_any_element()
}

pub fn render_assertions_tab(
    view: &FlowsView,
    step: &FlowStep,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let fl = crate::ui::Tr(view.lang).flows();
    let p = palette(cx);
    let kinds = [
        (0, fl.inspector_status()),
        (1, fl.inspector_status_in()),
        (2, fl.inspector_header_exists()),
        (3, fl.inspector_header_2()),
        (4, fl.inspector_jsonpath_exists()),
        (5, fl.inspector_jsonpath()),
        (6, fl.inspector_body_contains()),
        (7, fl.inspector_time_ms()),
    ];

    let mut box_el = card(cx)
        .p(px(12.0))
        .gap(px(8.0))
        .child(section_header(fl.inspector_step_assertions(), None, cx))
        .child(setting_row(
            fl.inspector_ignore_linked_request_s_own(),
            None,
            Some(SharedString::from(fl.inspector_judge_this_step_only_by())),
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
                .child(btn_secondary("flow-as-add", fl.inspector_add_2()).on_click(
                    cx.listener(|this, _, window, cx| this.add_quick_assertion(window, cx)),
                )),
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

impl FlowsView {
    pub fn add_extraction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let var_name = self.ext_var_input.read(cx).value().trim().to_owned();
        let expr = self.ext_expr_input.read(cx).value().trim().to_owned();
        if var_name.is_empty() {
            return;
        }
        let source = self.new_ext_source;
        let secret = self.new_ext_secret;
        if let Some(step) = self.selected_step_mut() {
            step.extractions.push(FlowExtraction {
                variable_name: var_name,
                source,
                expression: expr,
                secret: Some(secret),
            });
            self.dirty = true;
        }
        self.ext_var_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.ext_expr_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    pub fn add_quick_assertion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let path_or_key = self.assert_path_input.read(cx).value().trim().to_owned();
        let val_str = self.assert_val_input.read(cx).value().trim().to_owned();
        let kind_idx = self.new_assert_kind_idx;
        let assertion = match kind_idx {
            0 => Assertion::StatusEquals {
                value: val_str.parse::<u16>().unwrap_or(200),
            },
            1 => {
                let vals: Vec<u16> = val_str
                    .split(',')
                    .filter_map(|s| s.trim().parse::<u16>().ok())
                    .collect();
                Assertion::StatusIn {
                    values: if vals.is_empty() {
                        vec![200, 201]
                    } else {
                        vals
                    },
                }
            }
            2 => Assertion::HeaderExists {
                key: if path_or_key.is_empty() {
                    "content-type".into()
                } else {
                    path_or_key
                },
            },
            3 => Assertion::HeaderEquals {
                key: if path_or_key.is_empty() {
                    "content-type".into()
                } else {
                    path_or_key
                },
                value: val_str,
            },
            4 => Assertion::JsonPathExists {
                path: if path_or_key.is_empty() {
                    "$.id".into()
                } else {
                    path_or_key
                },
            },
            5 => Assertion::JsonPathEquals {
                path: if path_or_key.is_empty() {
                    "$.ok".into()
                } else {
                    path_or_key
                },
                value: serde_json::from_str::<serde_json::Value>(&val_str)
                    .unwrap_or(serde_json::Value::String(val_str)),
            },
            6 => Assertion::BodyContains { value: val_str },
            7 => Assertion::ResponseTimeLessThanMs {
                value: val_str.parse::<u64>().unwrap_or(1000),
            },
            _ => match assertion_new("StatusEquals") {
                Ok(a) => a,
                Err(_) => return,
            },
        };
        if let Some(step) = self.selected_step_mut() {
            step.assertions.push(assertion);
            self.dirty = true;
        }
        self.assert_path_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.assert_val_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }
}
