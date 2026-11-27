//! Branch response headers and matching rules editors for HTTP mock routes.

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use spanbay_desktop::core::model::KeyValue;
use spanbay_desktop::core::model::items::{
    MockResponse, MockRuleOperator, MockRuleTarget, MockRulesOperator,
};

use crate::ui::{
    Tone, UiIcon, badge, btn_icon, btn_secondary, checkbox, choice_chip, data_row, h_row, icon_xs,
    meta_pill, palette, section_header, text_input, to_hsla, v_col,
};

use super::state::MockView;

const RULE_TARGETS: [(MockRuleTarget, &str); 8] = [
    (MockRuleTarget::Query, "Query"),
    (MockRuleTarget::Header, "Header"),
    (MockRuleTarget::PathParam, "Param"),
    (MockRuleTarget::Body, "Body JSON"),
    (MockRuleTarget::BodyRaw, "Body Raw"),
    (MockRuleTarget::Cookie, "Cookie"),
    (MockRuleTarget::Method, "Method"),
    (MockRuleTarget::RequestNumber, "Req #"),
];

const RULE_OPS: [(MockRuleOperator, &str); 8] = [
    (MockRuleOperator::Equals, "="),
    (MockRuleOperator::Contains, "contains"),
    (MockRuleOperator::StartsWith, "startsWith"),
    (MockRuleOperator::Regex, "regex"),
    (MockRuleOperator::RegexIgnoreCase, "regex/i"),
    (MockRuleOperator::Exists, "exists"),
    (MockRuleOperator::Null, "null"),
    (MockRuleOperator::EmptyArray, "empty[]"),
];

pub fn render_resp_headers(
    view: &MockView,
    headers: &[KeyValue],
    cx: &mut Context<MockView>,
) -> AnyElement {
    let p = palette(cx);
    let mut box_el = v_col().w_full().gap(px(6.0)).child(section_header(
        crate::ui::Tr(view.lang)
            .mock()
            .tab_route_rules_response_headers(),
        Some(badge(headers.len().to_string(), Tone::Neutral, cx).into_any_element()),
        cx,
    ));

    for (idx, h) in headers.iter().enumerate() {
        box_el = box_el.child(
            data_row(cx)
                .child(
                    checkbox(SharedString::from(format!("mock-rh-en-{idx}")), h.enabled).on_click(
                        cx.listener(move |this, _, _, cx| {
                            if let Some(rp) = this.selected_response_mut()
                                && let Some(item) = rp.headers.get_mut(idx)
                            {
                                item.enabled = !item.enabled;
                                this.dirty = true;
                                cx.notify();
                            }
                        }),
                    ),
                )
                .child(
                    div()
                        .w(px(160.0))
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(h.key.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .truncate()
                        .child(h.value.clone()),
                )
                .child(
                    btn_icon(
                        SharedString::from(format!("mock-rh-rm-{idx}")),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(rp) = this.selected_response_mut()
                            && idx < rp.headers.len()
                        {
                            rp.headers.remove(idx);
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                ),
        );
    }

    box_el = box_el.child(
        h_row()
            .w_full()
            .gap(px(6.0))
            .child(div().w(px(160.0)).child(text_input(&view.kv_key_input)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(text_input(&view.kv_val_input)),
            )
            .child(
                btn_secondary(
                    "mock-rh-add",
                    crate::ui::Tr(view.lang).mock().tab_route_rules_header(),
                )
                .icon(icon_xs(UiIcon::Plus))
                .on_click(cx.listener(|this, _, window, cx| {
                    let k = this.kv_key_input.read(cx).value().trim().to_owned();
                    let v = this.kv_val_input.read(cx).value().to_string();
                    if k.is_empty() {
                        return;
                    }
                    if let Some(rp) = this.selected_response_mut() {
                        rp.headers.push(KeyValue {
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
                })),
            ),
    );
    box_el.into_any_element()
}

pub fn render_matching_rules(
    view: &MockView,
    resp: &MockResponse,
    cx: &mut Context<MockView>,
) -> AnyElement {
    let p = palette(cx);
    let op_toggle = h_row()
        .gap(px(4.0))
        .child(
            choice_chip(
                "mock-rop-and",
                crate::ui::tr(cx).mock().tab_route_rules_match_all_and(),
                resp.rules_operator == MockRulesOperator::And,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(rp) = this.selected_response_mut() {
                    rp.rules_operator = MockRulesOperator::And;
                    this.dirty = true;
                    cx.notify();
                }
            })),
        )
        .child(
            choice_chip(
                "mock-rop-or",
                crate::ui::tr(cx).mock().tab_route_rules_match_any_or(),
                resp.rules_operator == MockRulesOperator::Or,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(rp) = this.selected_response_mut() {
                    rp.rules_operator = MockRulesOperator::Or;
                    this.dirty = true;
                    cx.notify();
                }
            })),
        )
        .into_any_element();

    let mut box_el = v_col().w_full().gap(px(6.0)).child(section_header(
        crate::ui::Tr(view.lang)
            .mock()
            .tab_route_rules_matching_rules(),
        Some(op_toggle),
        cx,
    ));

    for (idx, rule) in resp.rules.iter().enumerate() {
        box_el = box_el.child(
            data_row(cx)
                .child(meta_pill(rule.target.as_str(), cx))
                .child(
                    div()
                        .w(px(120.0))
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(rule.modifier.clone()),
                )
                .children(rule.invert.then(|| badge("NOT", Tone::Warning, cx)))
                .child(meta_pill(rule.operator.as_str(), cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .truncate()
                        .child(rule.value.clone()),
                )
                .child(
                    btn_icon(
                        SharedString::from(format!("mock-rule-rm-{idx}")),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(rp) = this.selected_response_mut()
                            && idx < rp.rules.len()
                        {
                            rp.rules.remove(idx);
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                ),
        );
    }

    let mut target_row = h_row().gap(px(4.0)).flex_wrap();
    for (t, label) in RULE_TARGETS {
        target_row = target_row.child(
            choice_chip(
                SharedString::from(format!("mock-rtgt-{label}")),
                label,
                view.new_rule_target == t,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.new_rule_target = t;
                cx.notify();
            })),
        );
    }
    let mut ops_row = h_row().gap(px(4.0)).flex_wrap();
    for (op, label) in RULE_OPS {
        ops_row = ops_row.child(
            choice_chip(
                SharedString::from(format!("mock-rop-{label}")),
                label,
                view.new_rule_op == op,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.new_rule_op = op;
                cx.notify();
            })),
        );
    }
    ops_row = ops_row.child(
        choice_chip("mock-rinv", "NOT", view.new_rule_invert, cx).on_click(cx.listener(
            |this, _, _, cx| {
                this.new_rule_invert = !this.new_rule_invert;
                cx.notify();
            },
        )),
    );

    box_el = box_el.child(target_row).child(ops_row).child(
        h_row()
            .w_full()
            .gap(px(6.0))
            .child(div().w(px(160.0)).child(text_input(&view.rule_mod_input)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(text_input(&view.rule_val_input)),
            )
            .child(
                btn_secondary(
                    "mock-rule-add",
                    crate::ui::Tr(view.lang).mock().tab_route_rules_rule(),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.add_rule_to_response(window, cx);
                })),
            ),
    );
    box_el.into_any_element()
}
