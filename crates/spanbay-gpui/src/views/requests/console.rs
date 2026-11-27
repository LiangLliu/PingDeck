//! Collapsible `⌘J` Console Drawer and bottom status footer bar for `RequestsView`.

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::model::script_result::ConsoleLevel;

use crate::ui::{
    PaginationBar, ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_icon, h_row,
    icon_xs, kbd_chip, pagination_bar, palette, to_hsla, v_col,
};

use super::model::{OpenTab, format_time_hms};
use super::{RequestsView, model};

pub fn render_console_drawer(
    view: &RequestsView,
    cx: &mut Context<RequestsView>,
) -> Option<gpui_kit::Div> {
    if !view.console_open {
        return None;
    }
    let p = palette(cx);
    const CONSOLE_PAGE_SIZE: usize = 50;
    let total = view.console_entries.len();
    let total_pages = total.div_ceil(CONSOLE_PAGE_SIZE).max(1);
    let clamped_page = view.console_page.min(total_pages - 1);
    let skip = clamped_page.saturating_mul(CONSOLE_PAGE_SIZE);

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
                .child(
                    crate::ui::tr(cx)
                        .requests()
                        .console_no_console_entries_yet_send(),
                ),
        );
    } else {
        for entry in view
            .console_entries
            .iter()
            .rev()
            .skip(skip)
            .take(CONSOLE_PAGE_SIZE)
        {
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
                                    .child(crate::ui::tr(cx).requests().foot_console()),
                            )
                            .child(badge(
                                crate::ui::tr(cx).requests().console_entries(total),
                                Tone::Neutral,
                                cx,
                            )),
                    )
                    .child(
                        h_row()
                            .gap(px(4.0))
                            .child(
                                btn_ghost(
                                    "req-console-clear",
                                    crate::ui::tr(cx).requests().console_clear(),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.console_entries.clear();
                                        this.console_page = 0;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(btn_icon("req-console-close", UiIcon::Close).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.console_open = false;
                                    cx.notify();
                                }),
                            )),
                    ),
            )
            .child(rows)
            .children((total > CONSOLE_PAGE_SIZE).then(|| {
                div()
                    .flex_shrink_0()
                    .px(px(12.0))
                    .py(px(4.0))
                    .bg(to_hsla(p.pane))
                    .border_t_1()
                    .border_color(to_hsla(p.line.composite_over(p.pane)))
                    .child(pagination_bar(
                        PaginationBar {
                            id_prefix: "req-console-pg",
                            page: clamped_page,
                            page_size: CONSOLE_PAGE_SIZE,
                            total,
                            page_sizes: &[],
                            loading: false,
                            compact: true,
                        },
                        |this: &mut RequestsView, next_page, cx| {
                            this.console_page = next_page;
                            cx.notify();
                        },
                        None::<fn(&mut RequestsView, usize, &mut Context<RequestsView>)>,
                        cx,
                    ))
            })),
    )
}

pub fn render_footer_bar(view: &RequestsView, cx: &mut Context<RequestsView>) -> gpui_kit::Div {
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
        Some(OpenTab::Runner(r)) => crate::ui::tr(cx)
            .requests()
            .console_runner(&r.collection_name),
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
                        .child(SharedString::from(
                            crate::ui::tr(cx).requests().console_console(console_count),
                        )),
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
                crate::ui::tr(cx)
                    .requests()
                    .console_unresolved(unresolved_count),
                Tone::Warning,
                cx,
            )
        }))
}
