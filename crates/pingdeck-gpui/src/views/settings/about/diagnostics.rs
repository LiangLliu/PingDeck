//! Redacted diagnostics copy & selectable inline preview section for Settings › About.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    div, px,
};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::i18n::{t, t_args};

use crate::ui::{
    ScrollableElement as _, StatusShape, UiIcon, btn_ghost, btn_secondary, card, h_row, icon_xs,
    markdown_view, palette, section_heading, status_indicator, to_hsla, v_col,
};

use super::AboutSettingsView;

pub(super) fn render_diagnostics_section(
    view: &AboutSettingsView,
    lang: Language,
    zh: bool,
    cx: &mut Context<AboutSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);

    let copy_btn = if view.diag_copied {
        btn_secondary("about-diag-copy", t(lang, "set.about.copied").to_owned())
            .icon(icon_xs(UiIcon::Check))
            .text_color(to_hsla(p.ok))
            .on_click(cx.listener(|this, _, _, cx| this.copy_diagnostics(cx)))
    } else {
        let label = if view.diag_copying {
            if zh {
                "正在生成…"
            } else {
                "Preparing…"
            }
            .to_owned()
        } else {
            t(lang, "set.about.copy").to_owned()
        };
        btn_secondary("about-diag-copy", label)
            .icon(icon_xs(UiIcon::Copy))
            .on_click(cx.listener(|this, _, _, cx| this.copy_diagnostics(cx)))
    };

    let preview_btn = btn_ghost(
        "about-diag-preview-toggle",
        if view.diag_open {
            if zh { "收起预览" } else { "Hide preview" }
        } else if zh {
            "查看内容"
        } else {
            "Preview"
        },
    )
    .icon(icon_xs(if view.diag_open {
        UiIcon::ChevronUp
    } else {
        UiIcon::Eye
    }))
    .on_click(cx.listener(|this, _, _, cx| this.toggle_diag_preview(cx)));

    let mut card_col = v_col().child(
        h_row()
            .justify_between()
            .items_start()
            .gap(px(16.0))
            .p(px(14.0))
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink))
                            .child(t(lang, "set.about.diag").to_owned()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(t(lang, "set.about.copy.d").to_owned()),
                    ),
            )
            .child(
                h_row()
                    .gap(px(6.0))
                    .flex_shrink_0()
                    .child(preview_btn)
                    .child(copy_btn),
            ),
    );

    if let Some(err) = &view.diag_error {
        let msg = t_args(lang, "set.about.copyFailed", &[("reason", err.as_str())]);
        card_col = card_col.child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .px(px(14.0))
                .py(px(8.0))
                .bg(to_hsla(p.bad_soft))
                .border_t_1()
                .border_color(to_hsla(p.bad_line.composite_over(p.bad_soft)))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(6.0))
                        .child(status_indicator(StatusShape::Bad, cx))
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(to_hsla(p.bad))
                                .child(SharedString::from(msg)),
                        ),
                )
                .child(
                    btn_ghost("about-diag-err-dismiss", "×").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.diag_error = None;
                            cx.notify();
                        },
                    )),
                ),
        );
    }

    if view.diag_open {
        card_col = card_col.child(render_preview_panel(view, zh, cx));
    }

    v_col()
        .gap(px(8.0))
        .child(section_heading(
            t(lang, "set.sec.about.diagnostics").to_owned(),
            cx,
        ))
        .child(card(cx).child(card_col))
}

fn render_preview_panel(
    view: &AboutSettingsView,
    zh: bool,
    cx: &mut Context<AboutSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let preview_bar = h_row()
        .justify_between()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(6.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .child(
            h_row()
                .gap(px(6.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(icon_xs(UiIcon::ShieldCheck))
                .child(if zh {
                    "已脱敏 · 仅本机生成，可直接划选或整份复制"
                } else {
                    "Redacted · generated locally, selectable or copy whole report"
                }),
        )
        .child(
            btn_ghost("about-diag-refresh", if zh { "刷新" } else { "Refresh" })
                .icon(icon_xs(UiIcon::Refresh))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.load_diagnostics_preview(cx);
                })),
        );

    let preview_body = if view.diag_loading && view.diag_text.is_none() {
        h_row()
            .p(px(14.0))
            .gap(px(8.0))
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(status_indicator(StatusShape::Run, cx))
            .child(if zh {
                "正在生成诊断信息…"
            } else {
                "Generating diagnostic report…"
            })
            .into_any_element()
    } else if let Some(text) = &view.diag_text {
        let fenced = fence_plain_text(text.trim_end());
        div()
            .id("about-diag-scroll")
            .w_full()
            .max_h(px(300.0))
            .overflow_y_scrollbar()
            .px(px(12.0))
            .py(px(8.0))
            .child(markdown_view("about-diag-md", fenced))
            .into_any_element()
    } else {
        div().into_any_element()
    };

    v_col()
        .bg(to_hsla(p.sunken))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .rounded_b(px(7.0))
        .child(preview_bar)
        .child(preview_body)
}

fn fence_plain_text(text: &str) -> String {
    let mut fence = String::from("````");
    while text.contains(&fence) {
        fence.push('`');
    }
    format!("{fence}text\n{text}\n{fence}")
}
