//! Bands 3 & 4 of `Settings › Appearance`:
//! - Band 3 (**Typography**): Side-by-side UI & Code font scale sliders paired with a live API
//!   workbench specimen reflecting font sizes, density, palette, accent, and syntax tokens.
//! - Band 4 (**Workspace Layout & Motion**): 3-column horizontal visual domain cards for Density,
//!   Sidebar Layout, and Reduced Motion (zero vertical stacking).

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::settings::{
    Density, ReduceMotion, SettingsSection as CoreSection, SidebarMode,
};

use crate::ui::{
    Tone, badge, h_row, meta_pill, palette, settings_section_padded, settings_sub_well, slider,
    syntax_palette, tab_bar_segmented, tab_item, to_hsla, v_col,
};

use super::super::SettingsView;

pub(super) fn render_typography_section(
    view: &SettingsView,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let tr_set = crate::ui::tr(cx).settings();
    let app = &view.settings.appearance;
    let ui_font_val = view.inputs.ui_font_slider.read(cx).value().end().round() as u8;
    let mono_font_val = view.inputs.mono_font_slider.read(cx).value().end().round() as u8;

    let slider_block =
        |label: &'static str, desc: &'static str, val: u8, control: gpui_kit::Div| {
            v_col()
                .w_full()
                .gap(px(6.0))
                .p(px(10.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.pane))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    h_row()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .child(label),
                                )
                                .child(meta_pill(tr_set.page_defaults_local_only(), cx)),
                        )
                        .child(badge(
                            SharedString::from(format!("{val} px")),
                            Tone::Accent,
                            cx,
                        )),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .child(desc),
                )
                .child(control)
        };

    let left_controls = v_col()
        .flex_1()
        .min_w(px(260.0))
        .gap(px(10.0))
        .child(slider_block(
            tr_set.appearance_ui_font_size(),
            tr_set.appearance_proportionally_scales_global_interface_typ(),
            ui_font_val,
            div().w_full().child(slider(&view.inputs.ui_font_slider)),
        ))
        .child(slider_block(
            tr_set.appearance_monospace_font_size(),
            tr_set.appearance_used_for_request_editors_response(),
            mono_font_val,
            div().w_full().child(slider(&view.inputs.mono_font_slider)),
        ));

    let right_specimen = render_live_specimen(ui_font_val, mono_font_val, app.density, cx);

    settings_section_padded(
        tr_set.appearance_sec_typography(),
        None,
        h_row()
            .w_full()
            .items_stretch()
            .gap(px(14.0))
            .child(left_controls)
            .child(right_specimen),
        cx,
    )
}

fn render_live_specimen(
    ui_px: u8,
    mono_px: u8,
    density: Density,
    cx: &gpui_kit::App,
) -> gpui_kit::Div {
    let p = palette(cx);
    let syn = syntax_palette(cx);
    let tr_set = crate::ui::tr(cx).settings();
    let compact = density == Density::Compact;

    let tok_span = |text: &'static str, color: pingdeck_desktop::theme::RgbaColor| {
        div()
            .text_size(px(f32::from(mono_px)))
            .text_color(to_hsla(color))
            .child(text)
    };

    settings_sub_well(cx)
        .flex_1()
        .min_w(px(280.0))
        .justify_between()
        .gap(px(if compact { 7.0 } else { 11.0 }))
        .child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(tr_set.appearance_specimen_title()),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .child(tr_set.appearance_148_ms_live_preview()),
                ),
        )
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(if compact { 5.0 } else { 8.0 }))
                .rounded(px(6.0))
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .min_w_0()
                        .child(badge("GET", Tone::Success, cx))
                        .child(
                            div()
                                .text_size(px(f32::from(ui_px)))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child("/v1/invoices?status=open"),
                        ),
                )
                .child(badge("200 OK", Tone::Success, cx)),
        )
        .child(
            h_row()
                .w_full()
                .h(px(6.0))
                .rounded(px(3.0))
                .overflow_hidden()
                .bg(to_hsla(p.surface))
                .child(div().w(px(20.0)).h_full().bg(to_hsla(p.ph_dns)))
                .child(div().w(px(30.0)).h_full().bg(to_hsla(p.ph_connect)))
                .child(div().w(px(34.0)).h_full().bg(to_hsla(p.ph_tls)))
                .child(div().flex_1().h_full().bg(to_hsla(p.ph_wait)))
                .child(div().w(px(46.0)).h_full().bg(to_hsla(p.ph_recv))),
        )
        .child(
            v_col()
                .w_full()
                .gap(px(2.0))
                .px(px(10.0))
                .py(px(if compact { 6.0 } else { 9.0 }))
                .rounded(px(6.0))
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(
                    h_row()
                        .flex_wrap()
                        .child(tok_span("{ ", syn.punctuation))
                        .child(tok_span("\"id\"", syn.key))
                        .child(tok_span(": ", syn.punctuation))
                        .child(tok_span("\"inv_9f2a\"", syn.string))
                        .child(tok_span(", ", syn.punctuation))
                        .child(tok_span("\"paid\"", syn.key))
                        .child(tok_span(": ", syn.punctuation))
                        .child(tok_span("true", syn.boolean))
                        .child(tok_span(", ", syn.punctuation))
                        .child(tok_span("\"total\"", syn.key))
                        .child(tok_span(": ", syn.punctuation))
                        .child(tok_span("4200", syn.number))
                        .child(tok_span(" }", syn.punctuation)),
                ),
        )
}

pub(super) fn render_layout_motion_section(
    view: &SettingsView,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let tr_set = crate::ui::tr(cx).settings();
    let app = &view.settings.appearance;

    let density_idx = usize::from(app.density == Density::Compact);
    let sidebar_idx = match app.sidebar {
        SidebarMode::Auto => 0,
        SidebarMode::Expanded => 1,
        SidebarMode::Rail => 2,
    };
    let motion_idx = match app.reduce_motion {
        ReduceMotion::System => 0,
        ReduceMotion::On => 1,
        ReduceMotion::Off => 2,
    };

    let ent_density = cx.entity().clone();
    let ent_sidebar = cx.entity().clone();
    let ent_motion = cx.entity().clone();

    let row_h = if app.density == Density::Compact {
        5.0
    } else {
        8.0
    };
    let row_gap = if app.density == Density::Compact {
        3.0
    } else {
        5.0
    };
    let density_schematic = v_col()
        .w_full()
        .h(px(42.0))
        .justify_center()
        .gap(px(row_gap))
        .px(px(8.0))
        .rounded(px(6.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            div()
                .w_full()
                .h(px(row_h))
                .rounded(px(2.5))
                .bg(to_hsla(p.accent_soft)),
        )
        .child(
            div()
                .w(px(96.0))
                .h(px(row_h))
                .rounded(px(2.5))
                .bg(to_hsla(p.line_strong.composite_over(p.surface))),
        )
        .child(
            div()
                .w(px(72.0))
                .h(px(row_h))
                .rounded(px(2.5))
                .bg(to_hsla(p.line.composite_over(p.surface))),
        );

    let sidebar_w = match app.sidebar {
        SidebarMode::Rail => 14.0,
        SidebarMode::Auto => 26.0,
        SidebarMode::Expanded => 40.0,
    };
    let sidebar_schematic = h_row()
        .w_full()
        .h(px(42.0))
        .rounded(px(6.0))
        .overflow_hidden()
        .bg(to_hsla(p.canvas))
        .p(px(4.0))
        .gap(px(4.0))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.canvas)))
        .child(
            v_col()
                .w(px(sidebar_w))
                .h_full()
                .rounded(px(4.0))
                .bg(to_hsla(p.pane))
                .p(px(4.0))
                .gap(px(3.0))
                .child(
                    div()
                        .w_full()
                        .h(px(5.0))
                        .rounded(px(2.0))
                        .bg(to_hsla(p.accent)),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(4.0))
                        .rounded(px(2.0))
                        .bg(to_hsla(p.faint)),
                ),
        )
        .child(
            div()
                .flex_1()
                .h_full()
                .rounded(px(4.0))
                .bg(to_hsla(p.surface)),
        );

    let motion_reduced = app.reduce_motion == ReduceMotion::On;
    let motion_schematic = h_row()
        .w_full()
        .h(px(42.0))
        .px(px(10.0))
        .gap(px(6.0))
        .rounded(px(6.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            div()
                .size(px(10.0))
                .rounded(px(999.0))
                .bg(to_hsla(if motion_reduced {
                    p.faint
                } else {
                    p.accent_soft
                })),
        )
        .child(
            div()
                .size(px(10.0))
                .rounded(px(999.0))
                .bg(to_hsla(if motion_reduced {
                    p.muted
                } else {
                    p.accent_mark
                })),
        )
        .child(
            div()
                .size(px(12.0))
                .rounded(px(999.0))
                .bg(to_hsla(p.accent)),
        );

    let domain_card = |title: &'static str,
                       desc: &'static str,
                       schematic: gpui_kit::Div,
                       control: crate::ui::TabBar| {
        settings_sub_well(cx)
            .flex_1()
            .min_w(px(190.0))
            .justify_between()
            .gap(px(9.0))
            .child(schematic)
            .child(
                v_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.muted))
                            .child(desc),
                    ),
            )
            .child(control)
    };

    let cards = h_row()
        .w_full()
        .items_stretch()
        .gap(px(12.0))
        .child(domain_card(
            tr_set.appearance_density(),
            tr_set.appearance_compact_tightens_row_padding_for(),
            density_schematic,
            tab_bar_segmented("appearance-density-seg")
                .selected_index(density_idx)
                .child(tab_item(tr_set.opt_appearance_density_comfortable()))
                .child(tab_item(tr_set.opt_appearance_density_compact()))
                .on_click(move |idx, _, cx| {
                    let density = if *idx == 1 { "compact" } else { "comfortable" };
                    ent_density.update(cx, |this, cx| {
                        this.patch_section(
                            CoreSection::Appearance,
                            serde_json::json!({ "density": density }),
                            cx,
                        );
                    });
                }),
        ))
        .child(domain_card(
            tr_set.appearance_sidebar_layout(),
            tr_set.appearance_auto_remembers_your_b_toggle(),
            sidebar_schematic,
            tab_bar_segmented("appearance-sidebar-seg")
                .selected_index(sidebar_idx)
                .child(tab_item(tr_set.opt_network_http_version_auto()))
                .child(tab_item(tr_set.appearance_expanded()))
                .child(tab_item(tr_set.appearance_rail()))
                .on_click(move |idx, _, cx| {
                    let val = match *idx {
                        1 => "expanded",
                        2 => "rail",
                        _ => "auto",
                    };
                    ent_sidebar.update(cx, |this, cx| {
                        this.patch_section(
                            CoreSection::Appearance,
                            serde_json::json!({ "sidebar": val }),
                            cx,
                        );
                    });
                }),
        ))
        .child(domain_card(
            tr_set.f_appearance_reduce_motion(),
            tr_set.appearance_minimize_transitions_collapsible_animation(),
            motion_schematic,
            tab_bar_segmented("appearance-motion-seg")
                .selected_index(motion_idx)
                .child(tab_item(tr_set.appearance_system()))
                .child(tab_item(tr_set.opt_appearance_reduce_motion_on()))
                .child(tab_item(tr_set.notify_off()))
                .on_click(move |idx, _, cx| {
                    let val = match *idx {
                        1 => "on",
                        2 => "off",
                        _ => "system",
                    };
                    ent_motion.update(cx, |this, cx| {
                        this.patch_section(
                            CoreSection::Appearance,
                            serde_json::json!({ "reduce_motion": val }),
                            cx,
                        );
                    });
                }),
        ));

    settings_section_padded(tr_set.appearance_sec_layout(), None, cards, cx)
}
