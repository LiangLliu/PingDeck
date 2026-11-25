//! Context & Options tabs for the AI Chat right-hand Details inspector.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::model::AgentSessionPatch;
use pingdeck_desktop::core::app::agent_sandbox_test_impl;
use pingdeck_desktop::state::SettingsSection;

use crate::ui::{
    Disableable as _, Tone, badge, btn_danger, btn_ghost, btn_primary, btn_secondary, card,
    field_row, h_row, multiline_input, palette, progress_bar, section_heading, to_hsla,
    toggle_switch, v_col,
};

use super::ChatView;
use super::types::ChatHandoff;

pub(super) fn render_context_tab(view: &ChatView, cx: &mut Context<ChatView>) -> gpui_kit::Div {
    let p = palette(cx);
    let used = view.thread.context_used_tokens;
    let limit = view.thread.context_limit_tokens;
    let cost_usd = view.thread.cost_micro_usd as f64 / 1_000_000.0;
    let currency = view.thread.cost_currency.as_deref().unwrap_or("USD");
    let can_compact = !view.thread.session_id.is_empty() && !view.is_run_active();

    let mut col = v_col().gap(px(10.0)).child(
        card(cx).p(px(10.0)).child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(
                    crate::ui::tr(cx).chat().inspector_tabs_context_window(),
                    cx,
                ))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(match limit {
                            Some(l) if l > 0 => format!("{used} / {l} tokens"),
                            _ => format!("{used} tokens"),
                        })),
                )
                .children(limit.filter(|&l| l > 0).map(|l| {
                    let pct = ((used as f64 / l as f64) * 100.0).min(100.0) as f32;
                    progress_bar("ctx-detail-bar", pct)
                }))
                .child(
                    btn_secondary(
                        "chat-ctx-compact",
                        crate::ui::tr(cx)
                            .chat()
                            .inspector_tabs_compact_context_now(),
                    )
                    .disabled(!can_compact)
                    .on_click(cx.listener(|this, _, _, cx| this.compact_context(cx))),
                ),
        ),
    );

    if let Some(sess) = &view.session {
        let in_tok = sess.total_usage.input_tokens.unwrap_or(0);
        let out_tok = sess.total_usage.output_tokens.unwrap_or(0);
        let cached = sess.total_usage.cached_input_tokens.unwrap_or(0);
        col = col.child(
            card(cx).p(px(10.0)).child(
                v_col()
                    .gap(px(5.0))
                    .child(section_heading(
                        crate::ui::tr(cx).chat().inspector_tabs_session_totals(),
                        cx,
                    ))
                    .child(metric_line(
                        crate::ui::tr(cx).chat().inspector_tabs_input_tokens(),
                        super::metrics::format_tokens_int(in_tok),
                        cx,
                    ))
                    .child(metric_line(
                        crate::ui::tr(cx).chat().inspector_tabs_output_tokens(),
                        super::metrics::format_tokens_int(out_tok),
                        cx,
                    ))
                    .child(metric_line(
                        crate::ui::tr(cx).chat().inspector_tabs_cached_input(),
                        super::metrics::format_tokens_int(cached),
                        cx,
                    ))
                    .child(metric_line(
                        crate::ui::tr(cx).chat().inspector_tabs_estimated_cost(),
                        format!("{currency} {cost_usd:.4}"),
                        cx,
                    )),
            ),
        );
    }

    if !view.thread.notices.is_empty() {
        col = col.child(section_heading(
            crate::ui::tr(cx).chat().inspector_tabs_notices(),
            cx,
        ));
        for n in &view.thread.notices {
            col = col.child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(n.clone())),
            );
        }
    }
    col
}

pub(super) fn render_options_tab(view: &ChatView, cx: &mut Context<ChatView>) -> gpui_kit::Div {
    let p = palette(cx);
    let web_on = view.session.as_ref().is_some_and(|s| s.web_enabled);
    let pinned = view
        .session
        .as_ref()
        .is_some_and(|s| s.pinned_at_ms.is_some());
    let archived = view
        .session
        .as_ref()
        .is_some_and(|s| s.archived_at_ms.is_some());
    let sb = agent_sandbox_test_impl();

    v_col()
        .gap(px(10.0))
        .child(field_row(
            crate::ui::tr(cx).chat().inspector_tabs_web_access(),
            None,
            toggle_switch("chat-opt-web", web_on)
                .on_click(cx.listener(|this, _, _, cx| this.toggle_web_enabled(cx))),
            cx,
        ))
        .child(field_row(
            crate::ui::tr(cx).chat().inspector_tabs_pin_conversation(),
            None,
            toggle_switch("chat-opt-pin", pinned)
                .on_click(cx.listener(|this, _, _, cx| this.toggle_pinned(cx))),
            cx,
        ))
        .child(
            card(cx).p(px(10.0)).child(
                v_col()
                    .gap(px(4.0))
                    .child(
                        h_row()
                            .justify_between()
                            .child(section_heading(
                                crate::ui::tr(cx).chat().inspector_tabs_os_sandbox(),
                                cx,
                            ))
                            .child(badge(format!("{:?}", sb.tier), Tone::Accent, cx)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(sb.detail),
                    )
                    .child(
                        btn_ghost(
                            "chat-opt-perm-link",
                            crate::ui::tr(cx)
                                .chat()
                                .inspector_tabs_view_6_layer_permissions(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pending_handoff.set(Some(ChatHandoff::OpenSettings(
                                SettingsSection::Permissions,
                            )));
                            cx.notify();
                        })),
                    ),
            ),
        )
        .child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(
                    crate::ui::tr(cx).chat().inspector_tabs_chat_instructions(),
                    cx,
                ))
                .child(multiline_input(&view.instructions_input))
                .child(
                    h_row().justify_end().child(
                        btn_primary(
                            "chat-opt-save-inst",
                            crate::ui::tr(cx).chat().inspector_tabs_save(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.save_instructions(cx))),
                    ),
                ),
        )
        .child(
            h_row()
                .pt(px(6.0))
                .gap(px(6.0))
                .child(
                    btn_secondary(
                        "chat-opt-arch",
                        if archived {
                            crate::ui::tr(cx).chat().pane_menu_unarchive()
                        } else {
                            crate::ui::tr(cx).chat().pane_menu_archive()
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.patch_session(
                            AgentSessionPatch {
                                archived: Some(!archived),
                                ..Default::default()
                            },
                            cx,
                        );
                    })),
                )
                .child(
                    btn_danger(
                        "chat-opt-del",
                        crate::ui::tr(cx).chat().inspector_tabs_delete(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.request_delete_current(cx))),
                ),
        )
}

fn metric_line(label: &'static str, value: String, cx: &gpui_kit::App) -> gpui_kit::Div {
    let p = palette(cx);
    h_row()
        .justify_between()
        .text_xs()
        .child(div().text_color(to_hsla(p.muted)).child(label))
        .child(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink))
                .child(SharedString::from(value)),
        )
}
