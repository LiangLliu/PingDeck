//! Redacted diagnostics copy & selectable inline preview section for Settings › About.

use gpui_kit::{Context, IntoElement, ParentElement as _, Styled as _, div, px};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::core::response_viewer::ResponseBodyFormat;

use crate::ui::{
    Disableable as _, PayloadBlockOptions, StatusShape, Tone, UiIcon, btn_ghost, btn_secondary,
    dismissible_banner, h_row, icon_xs, palette, payload_block, settings_section_flush,
    status_indicator, to_hsla, v_col,
};

use super::AboutSettingsView;

pub(super) fn render_diagnostics_section(
    view: &AboutSettingsView,
    lang: Language,
    cx: &mut Context<AboutSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);

    let copy_btn = if view.diag_copied {
        btn_secondary(
            "about-diag-copy",
            crate::ui::Tr(lang).settings().about_copied().to_owned(),
        )
        .icon(icon_xs(UiIcon::Check))
        .text_color(to_hsla(p.ok))
        .on_click(cx.listener(|this, _, _, cx| this.copy_diagnostics(cx)))
    } else {
        let label = if view.diag_copying {
            crate::ui::tr(cx)
                .settings()
                .diagnostics_preparing()
                .to_owned()
        } else {
            crate::ui::Tr(lang).settings().about_copy().to_owned()
        };
        btn_secondary("about-diag-copy", label)
            .icon(icon_xs(UiIcon::Copy))
            .disabled(view.diag_copying)
            .on_click(cx.listener(|this, _, _, cx| this.copy_diagnostics(cx)))
    };

    let preview_btn = btn_ghost(
        "about-diag-preview-toggle",
        if view.diag_open {
            crate::ui::tr(cx).settings().diagnostics_hide_preview()
        } else {
            crate::ui::tr(cx).settings().diagnostics_preview()
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
            .p(px(16.0))
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.0))
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink))
                            .child(crate::ui::Tr(lang).settings().about_diag().to_owned()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(crate::ui::Tr(lang).settings().about_copy_d().to_owned()),
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
        let msg = crate::ui::Tr(lang).settings().about_copy_failed(err);
        card_col = card_col.child(div().px(px(16.0)).pb(px(12.0)).child(dismissible_banner(
            "about-diag-err-dismiss",
            Tone::Danger,
            msg,
            cx.listener(|this, _, _, cx| {
                this.diag_error = None;
                cx.notify();
            }),
            cx,
        )));
    }

    if view.diag_open {
        card_col = card_col.child(render_preview_panel(view, cx));
    }

    settings_section_flush(
        crate::ui::Tr(lang)
            .settings()
            .sec_about_diagnostics()
            .to_owned(),
        None,
        card_col,
        cx,
    )
}

fn render_preview_panel(
    view: &AboutSettingsView,
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
                .child(
                    crate::ui::tr(cx)
                        .settings()
                        .diagnostics_redacted_generated_locally_selectable_or(),
                ),
        )
        .child(
            btn_ghost("about-diag-refresh", crate::ui::tr(cx).common().refresh())
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
            .child(
                crate::ui::tr(cx)
                    .settings()
                    .diagnostics_generating_diagnostic_report(),
            )
            .into_any_element()
    } else if let Some(text) = &view.diag_text {
        div()
            .w_full()
            .p(px(10.0))
            .child(payload_block(
                "about-diag-payload",
                text.trim_end(),
                PayloadBlockOptions {
                    format_override: Some(ResponseBodyFormat::Text),
                    max_height_px: Some(300.0),
                    show_header: false,
                    show_copy: false,
                    ..Default::default()
                },
                cx,
            ))
            .into_any_element()
    } else {
        div().into_any_element()
    };

    v_col()
        .bg(to_hsla(p.sunken))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .child(preview_bar)
        .child(preview_body)
}
