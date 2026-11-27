//! Band 2 of `Settings › Appearance`: Side-by-Side Code Syntax Highlighting Studio.
//!
//! Left column displays 6 light/dark-paired curated syntax theme cards with 5 live semantic token
//! dots (`key`, `string`, `number`, `keyword`, `tag`). Right column renders an authentic live
//! Tree-sitter highlighted preview (`JSON` / `XML` / `Diff`) through PingDeck's unified
//! `payload_block` primitive so every click previews real code rendering in a single frame.

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::response_viewer::ResponseBodyFormat;
use pingdeck_desktop::theme::{SyntaxThemeId, resolve_syntax_palette};

use crate::ui::{
    PayloadBlockOptions, Tone, UiIcon, badge, h_row, icon_xs, meta_pill, palette, payload_block,
    settings_section_padded, settings_sub_well, tab_bar_segmented, tab_item, to_hsla, v_col,
};

use super::super::SettingsView;
use super::{SyntaxPreviewSample, syntax_theme_name, syntax_theme_sub};

const SAMPLE_JSON: &str = r#"{
  "service": "pingdeck-gateway",
  "status": "healthy",
  "latency_ms": 42,
  "stream": true,
  "model": {
    "id": "gpt-4.1-mini",
    "max_tokens": 8192,
    "cached": false
  },
  "headers": ["x-request-id", "x-ratelimit-remaining"]
}"#;

const SAMPLE_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<invoice id="inv_9f2a" currency="USD">
  <status>open</status>
  <total cents="4200">42.00</total>
  <item sku="pd-pro" qty="1">API Workbench License</item>
</invoice>"#;

const SAMPLE_DIFF: &str = r#"@@ -14,5 +14,6 @@ fn resolve_route(req: &Request) -> Target {
-    let timeout_ms = 30_000;
+    let timeout_ms = 15_000;
+    let retries = 2;
     Target::new(req.host(), timeout_ms, retries)
 }"#;

pub(super) fn render_syntax_studio(
    view: &SettingsView,
    is_dark: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let tr_set = crate::ui::tr(cx).settings();
    let active_st = view.theme_prefs.syntax_theme;

    let mut grid = v_col().flex_1().min_w(px(276.0)).gap(px(8.0));
    for chunk in SyntaxThemeId::ALL.chunks(2) {
        let mut row = h_row().w_full().gap(px(8.0));
        for &st in chunk {
            let syn = resolve_syntax_palette(st, &p);
            let selected = active_st == st;
            let border = if selected {
                to_hsla(p.accent)
            } else {
                to_hsla(p.line_strong.composite_over(p.pane))
            };
            let mut dots = h_row()
                .w_full()
                .gap(px(5.0))
                .px(px(7.0))
                .py(px(5.0))
                .rounded(px(5.0))
                .bg(to_hsla(p.sunken))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.sunken)));
            for c in syn.swatch_dots() {
                dots = dots.child(div().flex_1().h(px(6.0)).rounded(px(3.0)).bg(to_hsla(c)));
            }

            row = row.child(
                div()
                    .id(SharedString::from(format!("app-syntax-{}", st.id())))
                    .flex_1()
                    .min_w(px(132.0))
                    .p(px(9.0))
                    .rounded(px(9.0))
                    .bg(to_hsla(if selected { p.accent_soft } else { p.pane }))
                    .border_2()
                    .border_color(border)
                    .cursor_pointer()
                    .hover(|s| s.border_color(to_hsla(p.accent)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose_syntax_theme(st, cx);
                    }))
                    .child(
                        v_col()
                            .gap(px(6.0))
                            .child(
                                h_row()
                                    .justify_between()
                                    .gap(px(4.0))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .text_color(to_hsla(if selected {
                                                p.accent_ink
                                            } else {
                                                p.ink
                                            }))
                                            .truncate()
                                            .child(syntax_theme_name(st, cx)),
                                    )
                                    .child(if selected {
                                        div()
                                            .text_color(to_hsla(p.accent))
                                            .child(icon_xs(UiIcon::Check))
                                    } else {
                                        div()
                                            .text_size(px(10.0))
                                            .text_color(to_hsla(p.faint))
                                            .child(st.variant_pair(is_dark))
                                    }),
                            )
                            .child(dots)
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(syntax_theme_sub(st, cx)),
                            ),
                    ),
            );
        }
        grid = grid.child(row);
    }

    let sample_idx = match view.syntax_preview {
        SyntaxPreviewSample::Json => 0,
        SyntaxPreviewSample::Xml => 1,
        SyntaxPreviewSample::Diff => 2,
    };
    let (sample_text, sample_fmt) = match view.syntax_preview {
        SyntaxPreviewSample::Json => (SAMPLE_JSON, ResponseBodyFormat::Json),
        SyntaxPreviewSample::Xml => (SAMPLE_XML, ResponseBodyFormat::Xml),
        SyntaxPreviewSample::Diff => (SAMPLE_DIFF, ResponseBodyFormat::Text),
    };
    let ent_sample = cx.entity().clone();
    let preview_panel = settings_sub_well(cx)
        .flex_1()
        .min_w(px(290.0))
        .gap(px(8.0))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(tr_set.appearance_syntax_preview_title()),
                        )
                        .child(badge(
                            SharedString::from(format!(
                                "{} · {}",
                                syntax_theme_name(active_st, cx),
                                active_st.variant_pair(is_dark)
                            )),
                            Tone::Accent,
                            cx,
                        )),
                )
                .child(
                    tab_bar_segmented("app-syntax-sample-seg")
                        .selected_index(sample_idx)
                        .child(tab_item("JSON"))
                        .child(tab_item("XML"))
                        .child(tab_item("Diff"))
                        .on_click(move |idx, _, cx| {
                            let next = match *idx {
                                1 => SyntaxPreviewSample::Xml,
                                2 => SyntaxPreviewSample::Diff,
                                _ => SyntaxPreviewSample::Json,
                            };
                            ent_sample.update(cx, |this, cx| {
                                this.syntax_preview = next;
                                cx.notify();
                            });
                        }),
                ),
        )
        .child(payload_block(
            SharedString::from(format!(
                "app-syn-prev-{}-{}-{sample_idx}",
                active_st.id(),
                u8::from(is_dark)
            )),
            sample_text,
            PayloadBlockOptions {
                format_override: Some(sample_fmt),
                pretty: false,
                max_height_px: Some(218.0),
                show_header: false,
                show_copy: false,
                ..Default::default()
            },
            cx,
        ));

    let body = v_col()
        .w_full()
        .gap(px(12.0))
        .child(
            h_row()
                .gap(px(6.0))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(to_hsla(p.muted))
                        .child(tr_set.appearance_syntax_desc()),
                )
                .child(meta_pill(tr_set.page_defaults_local_only(), cx)),
        )
        .child(
            h_row()
                .w_full()
                .items_stretch()
                .gap(px(14.0))
                .child(grid)
                .child(preview_panel),
        );

    settings_section_padded(tr_set.appearance_sec_syntax(), None, body, cx)
}
