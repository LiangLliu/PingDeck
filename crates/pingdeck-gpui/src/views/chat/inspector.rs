//! Collapsible right-hand Details inspector for AI Chat using native underline `TabBar`:
//! `Progress` (todos & sub-agents), `Changes` (file edits), `Context` (tokens & budget),
//! and `Options` (per-chat instructions, web/pin toggles, archive/delete).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::model::{AgentSessionPatch, AgentTodoStatus};
use pingdeck_desktop::core::agent::tools::FileChangeKind;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_danger, btn_primary,
    btn_secondary, card, field_row, h_row, icon_xs, multiline_input, palette, progress_bar,
    section_heading, status_indicator, tab_bar_underline, tab_item, to_hsla, toggle_switch, v_col,
};

use super::{ChatView, DetailsTab};

pub fn render_inspector(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let chat_entity = cx.entity().clone();

    let tabs = tab_bar_underline("chat-details-tabs")
        .selected_index(view.details_tab.index())
        .child(tab_item(if zh { "进度" } else { "Progress" }))
        .child(tab_item(if zh { "变更" } else { "Changes" }))
        .child(tab_item(if zh { "上下文" } else { "Context" }))
        .child(tab_item(if zh { "选项" } else { "Options" }))
        .on_click(move |idx, _, cx| {
            let next = DetailsTab::from_index(*idx);
            chat_entity.update(cx, |this, cx| {
                this.details_tab = next;
                cx.notify();
            });
        });

    let body = match view.details_tab {
        DetailsTab::Progress => render_progress_tab(view, zh, cx).into_any_element(),
        DetailsTab::Changes => render_changes_tab(view, zh, cx).into_any_element(),
        DetailsTab::Context => render_context_tab(view, zh, cx).into_any_element(),
        DetailsTab::Options => render_options_tab(view, zh, cx).into_any_element(),
    };

    v_col()
        .size_full()
        .bg(to_hsla(p.pane))
        .child(
            div()
                .px(px(8.0))
                .pt(px(6.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(tabs),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .p(px(12.0))
                .overflow_y_scrollbar()
                .child(body),
        )
}

fn render_progress_tab(view: &ChatView, zh: bool, cx: &gpui_kit::App) -> gpui_kit::Div {
    let p = palette(cx);
    let todos = &view.thread.todos;
    let total = todos.len();
    let done = todos
        .iter()
        .filter(|t| t.status == AgentTodoStatus::Completed)
        .count();

    let mut col = v_col().gap(px(12.0));

    if let Some(run) = &view.thread.active_run {
        col =
            col.child(
                card(cx).p(px(10.0)).child(
                    v_col()
                        .gap(px(4.0))
                        .child(
                            h_row()
                                .justify_between()
                                .child(section_heading(
                                    if zh { "当前运行" } else { "Active run" },
                                    cx,
                                ))
                                .child(badge(format!("{:?}", run.status), Tone::Accent, cx)),
                        )
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(
                            SharedString::from(format!(
                                "{} · {} step(s)",
                                view.format_raw_model_label(&run.model_label),
                                run.iterations
                            )),
                        )),
                ),
            );
    }

    col = col.child(
        h_row()
            .justify_between()
            .child(section_heading(if zh { "任务清单" } else { "Tasks" }, cx))
            .children((total > 0).then(|| badge(format!("{done}/{total}"), Tone::Neutral, cx))),
    );

    if total > 0 {
        let pct = (done as f32 / total as f32) * 100.0;
        col = col.child(progress_bar("chat-todos-prog", pct));
        for item in todos {
            let shape = match item.status {
                AgentTodoStatus::Completed => StatusShape::Ok,
                AgentTodoStatus::InProgress => StatusShape::Run,
                AgentTodoStatus::Cancelled => StatusShape::Bad,
                AgentTodoStatus::Pending => StatusShape::Idle,
            };
            col = col.child(
                h_row()
                    .gap(px(8.0))
                    .py(px(3.0))
                    .child(status_indicator(shape, cx))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(if item.status == AgentTodoStatus::Completed {
                                to_hsla(p.muted)
                            } else {
                                to_hsla(p.ink)
                            })
                            .child(SharedString::from(item.content.clone())),
                    ),
            );
        }
    } else {
        col = col.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "智能体制定多步计划后，任务进度会显示在这里。"
        } else {
            "Multi-step task progress appears here once the agent creates a plan."
        }));
    }

    if !view.thread.subagents.is_empty() {
        col = col.child(div().pt(px(6.0)).child(section_heading(
            if zh { "子智能体" } else { "Sub-agents" },
            cx,
        )));
        for sub in view.thread.subagents.values() {
            col = col.child(
                card(cx).p(px(8.0)).child(
                    h_row()
                        .justify_between()
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(status_indicator(
                                    if sub.finished {
                                        StatusShape::Ok
                                    } else {
                                        StatusShape::Run
                                    },
                                    cx,
                                ))
                                .child(div().text_xs().text_color(to_hsla(p.ink)).child(
                                    SharedString::from(
                                        view.format_raw_model_label(&sub.summary.model_label),
                                    ),
                                )),
                        )
                        .child(badge(
                            format!("{} tools", sub.active_tools),
                            Tone::Neutral,
                            cx,
                        )),
                ),
            );
        }
    }

    col
}

fn render_changes_tab(view: &ChatView, zh: bool, cx: &gpui_kit::App) -> gpui_kit::Div {
    let p = palette(cx);
    let mut col = v_col().gap(px(8.0)).child(section_heading(
        if zh { "文件改动" } else { "Modified files" },
        cx,
    ));

    if view.thread.file_changes.is_empty() {
        return col.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "本次运行暂无文件改动。"
        } else {
            "No file edits recorded in this run."
        }));
    }

    for ch in &view.thread.file_changes {
        let kind_label = match ch.change {
            FileChangeKind::Create => {
                if zh {
                    "新建"
                } else {
                    "New"
                }
            }
            FileChangeKind::Edit => {
                if zh {
                    "修改"
                } else {
                    "Edit"
                }
            }
        };
        col = col.child(
            card(cx).p(px(8.0)).child(
                v_col()
                    .gap(px(4.0))
                    .child(
                        h_row()
                            .justify_between()
                            .gap(px(6.0))
                            .child(
                                h_row()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(5.0))
                                    .child(icon_xs(UiIcon::FileDiff))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_xs()
                                            .text_color(to_hsla(p.ink))
                                            .truncate()
                                            .child(SharedString::from(ch.path.clone())),
                                    ),
                            )
                            .child(badge(kind_label, Tone::Neutral, cx)),
                    )
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .text_xs()
                            .child(
                                div()
                                    .text_color(to_hsla(p.ok))
                                    .child(SharedString::from(format!("+{}", ch.added))),
                            )
                            .child(
                                div()
                                    .text_color(to_hsla(p.bad))
                                    .child(SharedString::from(format!("-{}", ch.removed))),
                            ),
                    ),
            ),
        );
    }
    col
}

fn render_context_tab(view: &ChatView, zh: bool, cx: &gpui_kit::App) -> gpui_kit::Div {
    let p = palette(cx);
    let used = view.thread.context_used_tokens;
    let limit = view.thread.context_limit_tokens;
    let cost_usd = view.thread.cost_micro_usd as f64 / 1_000_000.0;
    let currency = view.thread.cost_currency.as_deref().unwrap_or("USD");

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
                })),
        ),
    );

    if let Some(sess) = &view.session {
        let in_tok = sess.total_usage.input_tokens.unwrap_or(0);
        let out_tok = sess.total_usage.output_tokens.unwrap_or(0);
        let cached = sess.total_usage.cached_input_tokens.unwrap_or(0);
        let cache_write = sess.total_usage.cache_write_tokens.unwrap_or(0);
        let reasoning_tok = sess.total_usage.reasoning_tokens.unwrap_or(0);
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
                    .children((cache_write > 0).then(|| {
                        metric_line(
                            if zh { "缓存写入" } else { "Cache write" },
                            super::metrics::format_tokens_int(cache_write),
                            cx,
                        )
                    }))
                    .children((reasoning_tok > 0).then(|| {
                        metric_line(
                            if zh {
                                "思考 Tokens"
                            } else {
                                "Reasoning tokens"
                            },
                            super::metrics::format_tokens_int(reasoning_tok),
                            cx,
                        )
                    }))
                    .child(metric_line(
                        if zh { "估算费用" } else { "Estimated cost" },
                        format!("{currency} {cost_usd:.4}"),
                        cx,
                    )),
            ),
        );

        if let Some(dir) = &sess.workspace_dir {
            col = col.child(
                card(cx).p(px(10.0)).child(
                    v_col()
                        .gap(px(4.0))
                        .child(section_heading(
                            if zh {
                                "绑定本地目录"
                            } else {
                                "Bound folder"
                            },
                            cx,
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(SharedString::from(dir.clone())),
                        ),
                ),
            );
        }
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

fn render_options_tab(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> gpui_kit::Div {
    let web_on = view.session.as_ref().is_some_and(|s| s.web_enabled);
    let pinned = view
        .session
        .as_ref()
        .is_some_and(|s| s.pinned_at_ms.is_some());
    let archived = view
        .session
        .as_ref()
        .is_some_and(|s| s.archived_at_ms.is_some());

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
            v_col()
                .gap(px(6.0))
                .pt(px(4.0))
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
                .pt(px(8.0))
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
                        .on_click(cx.listener(|this, _, _, cx| this.delete_current_session(cx))),
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
