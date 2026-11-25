//! Control & Retry and Step Output inspector tabs for `FlowsView`.

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::model::items::{FlowRetryOn, FlowStep};

use crate::ui::{
    Tone, alert_banner, badge, card, choice_chip, data_row, empty_inline, h_row, kv_line,
    meta_pill, number_field, palette, section_header, setting_row, text_input, to_hsla,
    toggle_switch, v_col,
};

use super::state::FlowsView;

pub fn render_control_tab(
    view: &FlowsView,
    step: &FlowStep,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let retry_targets = [
        (FlowRetryOn::Network, view.t("网络错误", "Network")),
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
            view.t("条件控制与重试策略", "Condition & Retry"),
            None,
            cx,
        ))
        .child(setting_row(
            view.t("执行条件表达式", "Condition Expression"),
            None,
            Some(SharedString::from(view.t(
                "留空无条件执行；如 {{env}} == prod 或 {{has_more}} != false，为假时跳过此步。",
                "Blank runs always; e.g. {{status}} == active. Skips step when false.",
            ))),
            div().w(px(280.0)).child(text_input(&view.step_cond_input)),
            cx,
        ))
        .child(setting_row(
            view.t("步前延迟", "Pre-step Delay"),
            None,
            None,
            number_field(&view.step_delay_input, Some("ms"), 90.0, cx),
            cx,
        ))
        .child(setting_row(
            view.t("单次尝试超时", "Attempt Timeout"),
            None,
            Some(SharedString::from(
                view.t("留空继承请求超时。", "Blank inherits request timeout."),
            )),
            number_field(&view.step_timeout_input, Some("ms"), 90.0, cx),
            cx,
        ))
        .child(setting_row(
            view.t("失败后继续后续步骤", "Continue on Error"),
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
            view.t("最大重试次数（0–5）与间隔", "Retries (0–5) & Wait"),
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
            view.t("触发重试的失败类型", "Retry On"),
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
        return empty_inline(
            view.t("尚未运行此步骤", "No output for this step yet"),
            Some(SharedString::from(view.t(
                "点击顶部「运行流程」或步骤行的播放图标执行后查看输出。",
                "Run the flow or this step to inspect response details.",
            ))),
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
                view.t("已跳过", "Skipped").to_owned()
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
            view.t("实发请求", "Sent"),
            format!("{} {}", exec.method, exec.url),
            100.0,
            cx,
        ));
    }

    if !res.extracted_variables.is_empty() {
        let mut ext_card = card(cx).p(px(10.0)).gap(px(4.0)).child(section_header(
            view.t("本步提取变量", "Extracted Variables"),
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
            view.t("断言结果", "Assertions"),
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
        box_el = box_el.child(
            div()
                .w_full()
                .p(px(10.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(res.response_body_preview.clone()),
        );
    }

    box_el.into_any_element()
}
