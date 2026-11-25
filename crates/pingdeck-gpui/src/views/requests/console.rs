//! Collapsible `⌘J` Console Drawer and bottom status footer bar for `RequestsView`.

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::model::script_result::ConsoleLevel;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_icon, h_row, icon_xs, kbd_chip,
    palette, to_hsla, v_col,
};

use super::model::{OpenTab, format_time_hms};
use super::{RequestsView, model};

pub fn render_console_drawer(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> Option<gpui_kit::Div> {
    if !view.console_open {
        return None;
    }
    let p = palette(cx);
    let total = view.console_entries.len();

    let mut rows = v_col()
        .flex_1()
        .min_h_0()
        .w_full()
        .gap(px(2.0))
        .p(px(8.0))
        .overflow_y_scrollbar();

    if view.console_entries.is_empty() {
        rows = rows.child(
            div()
                .px(px(8.0))
                .py(px(12.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(if zh {
                    "暂无控制台日志。发送请求或运行脚本后的输出会显示在这里。"
                } else {
                    "No console entries yet. Send a request or run scripts to capture logs here."
                }),
        );
    } else {
        for entry in view.console_entries.iter().rev().take(200) {
            let tone = match entry.level {
                ConsoleLevel::Error => Tone::Danger,
                ConsoleLevel::Warn => Tone::Warning,
                ConsoleLevel::Info => Tone::Success,
                ConsoleLevel::Log => Tone::Accent,
                ConsoleLevel::Debug => Tone::Neutral,
            };
            let mut line = h_row()
                .w_full()
                .gap(px(8.0))
                .px(px(6.0))
                .py(px(3.0))
                .rounded(px(4.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.faint))
                        .flex_shrink_0()
                        .child(SharedString::from(format_time_hms(entry.ts_ms))),
                )
                .child(badge(entry.tag, tone, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(SharedString::from(entry.summary.clone())),
                );
            if let Some(detail) = &entry.detail {
                line = line.child(
                    div()
                        .max_w(px(260.0))
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .truncate()
                        .child(SharedString::from(detail.clone())),
                );
            }
            rows = rows.child(line);
        }
    }

    Some(
        v_col()
            .w_full()
            .h(px(180.0))
            .flex_shrink_0()
            .bg(to_hsla(p.surface))
            .border_t_1()
            .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
            .child(
                h_row()
                    .justify_between()
                    .h(px(32.0))
                    .px(px(12.0))
                    .bg(to_hsla(p.pane))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.pane)))
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .child(icon_xs(UiIcon::Terminal))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .child(if zh { "控制台" } else { "Console" }),
                            )
                            .child(badge(
                                if zh {
                                    format!("{total} 条记录")
                                } else {
                                    format!("{total} entries")
                                },
                                Tone::Neutral,
                                cx,
                            )),
                    )
                    .child(
                        h_row()
                            .gap(px(4.0))
                            .child(
                                btn_ghost("req-console-clear", if zh { "清空" } else { "Clear" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.console_entries.clear();
                                        cx.notify();
                                    })),
                            )
                            .child(btn_icon("req-console-close", UiIcon::Close).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.console_open = false;
                                    cx.notify();
                                }),
                            )),
                    ),
            )
            .child(rows),
    )
}

pub fn render_footer_bar(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let console_count = view.console_entries.len();

    let url_summary = match view.active_tab() {
        Some(OpenTab::Request(t)) => {
            if let Some(resp) = &t.last_http {
                format!(
                    "{} {} → {}",
                    model::method_str(t.draft.method),
                    resp.url,
                    resp.status_code
                )
            } else if t.is_ai() {
                let proto = t
                    .draft
                    .ai
                    .as_ref()
                    .map(|a| model::ai_protocol_label(a.protocol))
                    .unwrap_or("AI");
                format!("{proto} · {}", t.draft.url)
            } else {
                format!("{} {}", model::method_str(t.draft.method), t.draft.url)
            }
        }
        Some(OpenTab::Runner(r)) => {
            if zh {
                format!("集合运行：{}", r.collection_name)
            } else {
                format!("Runner: {}", r.collection_name)
            }
        }
        Some(OpenTab::Container(c)) => c.name.clone(),
        None => String::new(),
    };

    let unresolved_count = view.active_unresolved_var_names(cx).len();

    h_row()
        .w_full()
        .h(px(28.0))
        .flex_shrink_0()
        .px(px(10.0))
        .gap(px(10.0))
        .bg(to_hsla(p.pane))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            div()
                .id("req-foot-console-btn")
                .flex()
                .flex_row()
                .items_center()
                .gap(px(5.0))
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.console_open = !this.console_open;
                    cx.notify();
                }))
                .child(icon_xs(UiIcon::Terminal))
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(if zh {
                            format!("控制台 {console_count}")
                        } else {
                            format!("Console {console_count}")
                        })),
                )
                .child(kbd_chip("⌘J", cx)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .truncate()
                .child(SharedString::from(url_summary)),
        )
        .children((unresolved_count > 0).then(|| {
            badge(
                if zh {
                    format!("{unresolved_count} 个未解析变量")
                } else {
                    format!("{unresolved_count} unresolved")
                },
                Tone::Warning,
                cx,
            )
        }))
}
