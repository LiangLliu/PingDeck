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

pub(super) fn render_context_tab(
    view: &ChatView,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
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
                    if zh {
                        "上下文窗口"
                    } else {
                        "Context window"
                    },
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
                        if zh {
                            "立即压缩上下文"
                        } else {
                            "Compact context now"
                        },
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
                        if zh { "累计消耗" } else { "Session totals" },
                        cx,
                    ))
                    .child(metric_line(
                        if zh { "输入 Tokens" } else { "Input tokens" },
                        super::metrics::format_tokens_int(in_tok),
                        cx,
                    ))
                    .child(metric_line(
                        if zh { "输出 Tokens" } else { "Output tokens" },
                        super::metrics::format_tokens_int(out_tok),
                        cx,
                    ))
                    .child(metric_line(
                        if zh { "缓存命中" } else { "Cached input" },
                        super::metrics::format_tokens_int(cached),
                        cx,
                    ))
                    .child(metric_line(
                        if zh { "估算费用" } else { "Estimated cost" },
                        format!("{currency} {cost_usd:.4}"),
                        cx,
                    )),
            ),
        );
    }

    if !view.thread.notices.is_empty() {
        col = col.child(section_heading(if zh { "系统提示" } else { "Notices" }, cx));
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

pub(super) fn render_options_tab(
    view: &ChatView,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
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
            if zh {
                "联网搜索与抓取"
            } else {
                "Web access"
            },
            None,
            toggle_switch("chat-opt-web", web_on)
                .on_click(cx.listener(|this, _, _, cx| this.toggle_web_enabled(cx))),
            cx,
        ))
        .child(field_row(
            if zh {
                "置顶本对话"
            } else {
                "Pin conversation"
            },
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
                                if zh {
                                    "OS 沙箱与权限"
                                } else {
                                    "OS Sandbox"
                                },
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
                            if zh {
                                "查看 6 层权限顺序 ↗"
                            } else {
                                "View 6-layer permissions ↗"
                            },
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
                    if zh {
                        "本对话附加指令"
                    } else {
                        "Chat instructions"
                    },
                    cx,
                ))
                .child(multiline_input(&view.instructions_input))
                .child(
                    h_row().justify_end().child(
                        btn_primary("chat-opt-save-inst", if zh { "保存指令" } else { "Save" })
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
                            if zh { "取消归档" } else { "Unarchive" }
                        } else if zh {
                            "归档"
                        } else {
                            "Archive"
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
                    btn_danger("chat-opt-del", if zh { "删除对话" } else { "Delete" })
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
