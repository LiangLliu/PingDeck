//! Native anchored popovers & context meter for the Chat composer:
//! Skills (`/`), Permission mode, Reasoning effort, searchable Model selector, and Context meter.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::agent::model::ReasoningEffort;
use spanbay_desktop::core::app::agent_sandbox_test_impl;
use spanbay_desktop::core::model::settings::ToolApproval;
use spanbay_desktop::state::SettingsSection;

use crate::ui::{
    Anchor, ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, h_row, icon_xs, list_row,
    palette, pill_btn, popover, search_input, section_heading, to_hsla, v_col,
};

use super::ChatView;
use super::types::ChatHandoff;

pub fn render_skills_popover(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let skills = view.available_skills.clone();
    let selected = view.selected_skills.clone();
    let chat_ent = cx.entity().clone();
    popover("chat-skills-popover")
        .anchor(Anchor::BottomLeft)
        .trigger(pill_btn(
            "chat-skills-trigger",
            if selected.is_empty() {
                crate::ui::tr(cx)
                    .chat()
                    .composer_popovers_skills()
                    .to_owned()
            } else {
                format!("/ {}", selected.len())
            },
            None,
            !selected.is_empty(),
        ))
        .content(move |_, _, pop_cx| {
            let pp = palette(pop_cx);
            let mut m = v_col().w(px(260.0)).gap(px(3.0)).child(section_heading(
                crate::ui::tr(pop_cx).chat().composer_popovers_skills_2(),
                pop_cx,
            ));
            if skills.is_empty() {
                m = m.child(
                    div()
                        .py(px(8.0))
                        .text_xs()
                        .text_color(to_hsla(pp.muted))
                        .child(
                            crate::ui::tr(pop_cx)
                                .chat()
                                .composer_popovers_no_skills_found_configure_in(),
                        ),
                );
            } else {
                for sk in skills.iter().filter(|s| s.enabled) {
                    let is_sel = selected.contains(&sk.name);
                    let sname = sk.name.clone();
                    let ent = chat_ent.clone();
                    let pop_h = pop_cx.entity().clone();
                    m = m.child(
                        list_row(
                            SharedString::from(format!("chat-sk-opt-{}", sk.name)),
                            is_sel,
                            pop_cx,
                            move |_, w, cx| {
                                ent.update(cx, |this, cx| {
                                    if let Some(pos) =
                                        this.selected_skills.iter().position(|x| x == &sname)
                                    {
                                        this.selected_skills.remove(pos);
                                    } else {
                                        this.selected_skills.push(sname.clone());
                                    }
                                    cx.notify();
                                });
                                pop_h.update(cx, |s, cx| s.dismiss(w, cx));
                            },
                        )
                        .justify_between()
                        .child(
                            v_col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(to_hsla(pp.ink))
                                        .child(format!("/{}", sk.name)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(to_hsla(pp.muted))
                                        .truncate()
                                        .child(sk.description.clone()),
                                ),
                        )
                        .children(is_sel.then(|| icon_xs(UiIcon::Check))),
                    );
                }
            }
            m
        })
}

pub fn render_perm_popover(mode: ToolApproval, cx: &mut Context<ChatView>) -> impl IntoElement {
    let (label, glyph) = match mode {
        ToolApproval::Ask => (
            crate::ui::tr(cx).chat().composer_popovers_ask(),
            UiIcon::Shield,
        ),
        ToolApproval::AutoReadOnly => (
            crate::ui::tr(cx).chat().composer_popovers_auto_reads(),
            UiIcon::ShieldCheck,
        ),
        ToolApproval::AutoAll => (
            crate::ui::tr(cx).chat().composer_perm_full(),
            UiIcon::ShieldAlert,
        ),
    };
    let chat_ref = cx.entity().clone();
    let options: [(ToolApproval, &str, &str); 3] = [
        (
            ToolApproval::Ask,
            crate::ui::tr(cx).chat().composer_perm_ask(),
            crate::ui::tr(cx)
                .chat()
                .composer_popovers_confirm_before_every_tool_call(),
        ),
        (
            ToolApproval::AutoReadOnly,
            crate::ui::tr(cx).chat().composer_popovers_auto_read_only(),
            crate::ui::tr(cx)
                .chat()
                .composer_popovers_auto_allow_reads_ask_for(),
        ),
        (
            ToolApproval::AutoAll,
            crate::ui::tr(cx).chat().composer_perm_full(),
            crate::ui::tr(cx)
                .chat()
                .composer_popovers_auto_run_tools_hard_blocks(),
        ),
    ];

    popover("chat-perm-popover")
        .anchor(Anchor::BottomLeft)
        .trigger(pill_btn(
            "chat-perm-trigger",
            label,
            Some(glyph),
            mode == ToolApproval::AutoAll,
        ))
        .content(move |_st, _window, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut box_el = v_col().w(px(276.0)).gap(px(4.0)).child(
                div().px(px(6.0)).pb(px(2.0)).child(section_heading(
                    crate::ui::tr(pop_cx)
                        .chat()
                        .composer_popovers_permission_mode(),
                    pop_cx,
                )),
            );
            for (opt, title, desc) in options {
                let selected = opt == mode;
                let entity = chat_ref.clone();
                let pop_handle = pop_cx.entity().clone();
                box_el = box_el.child(
                    list_row(
                        SharedString::from(format!("perm-opt-{opt:?}")),
                        selected,
                        pop_cx,
                        move |_, window, cx| {
                            entity.update(cx, |this, cx| this.request_permission_mode(opt, cx));
                            pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .child(
                        v_col()
                            .flex_1()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(pop_p.ink))
                                    .child(title),
                            )
                            .child(div().text_xs().text_color(to_hsla(pop_p.muted)).child(desc)),
                    )
                    .children(selected.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            let ent_link = chat_ref.clone();
            let pop_link = pop_cx.entity().clone();
            let sb = agent_sandbox_test_impl();
            box_el.child(
                v_col()
                    .gap(px(4.0))
                    .pt(px(5.0))
                    .border_t_1()
                    .border_color(to_hsla(pop_p.line.composite_over(pop_p.surface)))
                    .child(
                        h_row()
                            .justify_between()
                            .px(px(6.0))
                            .child(
                                div().text_xs().text_color(to_hsla(pop_p.muted)).child(
                                    crate::ui::tr(pop_cx).chat().inspector_tabs_os_sandbox(),
                                ),
                            )
                            .child(badge(format!("{:?}", sb.tier), Tone::Accent, pop_cx)),
                    )
                    .child(
                        btn_ghost(
                            "chat-perm-eval-order",
                            crate::ui::tr(pop_cx)
                                .chat()
                                .composer_popovers_evaluation_order(),
                        )
                        .on_click(move |_, w, cx| {
                            ent_link.update(cx, |this, cx| {
                                this.pending_handoff.set(Some(ChatHandoff::OpenSettings(
                                    SettingsSection::Permissions,
                                )));
                                cx.notify();
                            });
                            pop_link.update(cx, |st, cx| st.dismiss(w, cx));
                        }),
                    ),
            )
        })
}

pub fn render_effort_popover(
    effort: Option<ReasoningEffort>,
    missing_reasoning_quirk: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let label = match effort {
        None => crate::ui::tr(cx).chat().composer_popovers_effort_auto(),
        Some(ReasoningEffort::Minimal) => {
            crate::ui::tr(cx).chat().composer_popovers_effort_minimal()
        }
        Some(ReasoningEffort::Low) => crate::ui::tr(cx).chat().composer_popovers_effort_low(),
        Some(ReasoningEffort::Medium) => crate::ui::tr(cx).chat().composer_popovers_effort_medium(),
        Some(ReasoningEffort::High) => crate::ui::tr(cx).chat().composer_popovers_effort_high(),
        Some(ReasoningEffort::Max) => crate::ui::tr(cx).chat().composer_popovers_effort_max(),
    };
    let chat_ref = cx.entity().clone();
    let opts: [(Option<ReasoningEffort>, &str); 6] = [
        (
            None,
            crate::ui::tr(cx)
                .chat()
                .composer_popovers_auto_model_default(),
        ),
        (
            Some(ReasoningEffort::Minimal),
            crate::ui::tr(cx).chat().composer_popovers_minimal(),
        ),
        (
            Some(ReasoningEffort::Low),
            crate::ui::tr(cx).chat().composer_popovers_low(),
        ),
        (
            Some(ReasoningEffort::Medium),
            crate::ui::tr(cx).chat().composer_popovers_medium(),
        ),
        (
            Some(ReasoningEffort::High),
            crate::ui::tr(cx).chat().composer_popovers_high(),
        ),
        (
            Some(ReasoningEffort::Max),
            crate::ui::tr(cx).chat().composer_popovers_max(),
        ),
    ];

    popover("chat-effort-popover")
        .anchor(Anchor::BottomLeft)
        .trigger(pill_btn(
            "chat-effort-trigger",
            label,
            Some(UiIcon::Brain),
            effort.is_some(),
        ))
        .content(move |_st, _window, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col().w(px(230.0)).gap(px(2.0));
            for (val, text) in opts {
                let selected = val == effort;
                let entity = chat_ref.clone();
                let pop_handle = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("effort-opt-{val:?}")),
                        selected,
                        pop_cx,
                        move |_, window, cx| {
                            entity.update(cx, |this, cx| this.set_reasoning_effort(val, cx));
                            pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(text))
                    .children(selected.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            if missing_reasoning_quirk {
                list = list.child(
                    div()
                        .mt(px(4.0))
                        .pt(px(6.0))
                        .px(px(6.0))
                        .border_t_1()
                        .border_color(to_hsla(pop_p.line.composite_over(pop_p.surface)))
                        .text_size(px(11.0))
                        .text_color(to_hsla(pop_p.muted))
                        .child(
                            crate::ui::tr(pop_cx)
                                .chat()
                                .composer_popovers_custom_provider_has_no_reasoning(),
                        ),
                );
            }
            list
        })
}

pub fn render_model_popover(
    view: &ChatView,
    model_label: String,
    highlight_missing: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let models = view.available_models.clone();
    let current_target = view.session.as_ref().and_then(|s| s.target.clone());
    let chat_ref = cx.entity().clone();
    let search_ent = view.model_search_input.clone();

    popover("chat-model-popover")
        .anchor(Anchor::BottomRight)
        .trigger(
            pill_btn(
                "chat-model-trigger",
                SharedString::from(model_label),
                Some(if highlight_missing {
                    UiIcon::Warning
                } else {
                    UiIcon::Sparkles
                }),
                highlight_missing,
            )
            .max_w(px(280.0)),
        )
        .content(move |_st, _window, pop_cx| {
            let pop_p = palette(pop_cx);
            let q = search_ent.read(pop_cx).value().trim().to_ascii_lowercase();
            let filtered: Vec<_> = models
                .iter()
                .filter(|m| {
                    q.is_empty()
                        || m.model_id.to_ascii_lowercase().contains(&q)
                        || m.provider_name.to_ascii_lowercase().contains(&q)
                })
                .collect();

            let header = v_col()
                .gap(px(6.0))
                .px(px(6.0))
                .pb(px(6.0))
                .border_b_1()
                .border_color(to_hsla(pop_p.line.composite_over(pop_p.surface)))
                .child(section_heading(
                    crate::ui::tr(pop_cx)
                        .chat()
                        .composer_popovers_select_model(),
                    pop_cx,
                ))
                .child(search_input(&search_ent, pop_cx));

            let body: gpui_kit::AnyElement = if filtered.is_empty() {
                div()
                    .px(px(8.0))
                    .py(px(12.0))
                    .text_xs()
                    .text_color(to_hsla(pop_p.muted))
                    .child(
                        crate::ui::tr(pop_cx)
                            .chat()
                            .composer_popovers_no_matching_models_add_one(),
                    )
                    .into_any_element()
            } else {
                let scroll_h = ((filtered.len() as f32) * 32.0 + 8.0).clamp(64.0, 300.0);
                let mut rows = v_col().w_full().gap(px(2.0)).py(px(4.0));
                for (i, m) in filtered.iter().enumerate() {
                    let target = m.target();
                    let selected = current_target.as_ref() == Some(&target);
                    let entity = chat_ref.clone();
                    let pop_handle = pop_cx.entity().clone();
                    rows = rows.child(
                        list_row(
                            SharedString::from(format!("model-opt-{i}")),
                            selected,
                            pop_cx,
                            move |_, window, cx| {
                                let t = target.clone();
                                entity.update(cx, |this, cx| this.select_model(t, cx));
                                pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                            },
                        )
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            h_row()
                                .flex_1()
                                .min_w_0()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(to_hsla(pop_p.muted))
                                        .child(SharedString::from(format!(
                                            "{} /",
                                            m.provider_name
                                        ))),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .font_weight(if selected {
                                            gpui_kit::FontWeight::SEMIBOLD
                                        } else {
                                            gpui_kit::FontWeight::MEDIUM
                                        })
                                        .text_color(to_hsla(pop_p.ink))
                                        .truncate()
                                        .child(SharedString::from(m.model_id.clone())),
                                ),
                        )
                        .children(selected.then(|| {
                            div()
                                .flex_shrink_0()
                                .text_color(to_hsla(pop_p.accent))
                                .child(icon_xs(UiIcon::Check))
                        })),
                    );
                }
                div()
                    .w_full()
                    .h(px(scroll_h))
                    .overflow_y_scrollbar()
                    .id("chat-model-pop-scroll")
                    .child(rows)
                    .into_any_element()
            };

            v_col().w(px(340.0)).gap(px(2.0)).child(header).child(body)
        })
}
