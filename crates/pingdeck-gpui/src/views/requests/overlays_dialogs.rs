//! Extract Variable and Multi-Model Compare modal contents for `RequestsView`.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::AiCompareEstimateDto;

use crate::ui::{
    Disableable as _, ScrollableElement as _, Tone, alert_banner, btn_primary, checkbox, data_row,
    empty_inline, h_row, palette, section_heading, text_input, to_hsla, v_col,
};

use super::RequestsView;
use super::model::ActiveModal;
use super::overlays::cancel_btn;

pub(super) fn render_extract_var(
    view: &RequestsView,
    value: String,
    is_secret: bool,
    use_globals: bool,
    cx: &mut Context<RequestsView>,
) -> (String, f32, gpui_kit::AnyElement, gpui_kit::Div) {
    let p = palette(cx);
    let body = v_col()
        .gap(px(12.0))
        .child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(
                    crate::ui::tr(cx)
                        .requests()
                        .overlays_dialogs_variable_name(),
                    cx,
                ))
                .child(text_input(&view.modal_name_input)),
        )
        .child(
            div()
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.sunken)))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .truncate()
                .child(SharedString::from(
                    crate::ui::tr(cx).requests().overlays_value_preview(&value),
                )),
        )
        .child(
            h_row()
                .gap(px(16.0))
                .child(
                    checkbox("ext-secret", is_secret)
                        .label(
                            crate::ui::tr(cx)
                                .requests()
                                .overlays_dialogs_store_as_secret(),
                        )
                        .on_click(cx.listener(|this, ch: &bool, _, cx| {
                            if let Some(ActiveModal::ExtractVariable { is_secret, .. }) =
                                &mut this.active_modal
                            {
                                *is_secret = *ch;
                            }
                            cx.notify();
                        })),
                )
                .child(
                    checkbox("ext-global", use_globals)
                        .label(
                            crate::ui::tr(cx)
                                .requests()
                                .overlays_dialogs_save_to_globals(),
                        )
                        .on_click(cx.listener(|this, ch: &bool, _, cx| {
                            if let Some(ActiveModal::ExtractVariable { use_globals, .. }) =
                                &mut this.active_modal
                            {
                                *use_globals = *ch;
                            }
                            cx.notify();
                        })),
                ),
        );

    let footer = h_row().gap(px(8.0)).child(cancel_btn(cx)).child(
        btn_primary(
            "ext-ok",
            crate::ui::tr(cx)
                .requests()
                .overlays_dialogs_save_variable(),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            let key = this.modal_name_input.read(cx).value().to_string();
            this.commit_extract_variable(key, value.clone(), is_secret, use_globals, cx);
        })),
    );
    (
        crate::ui::tr(cx)
            .requests()
            .overlays_dialogs_extract_value_to_variable()
            .into(),
        480.0,
        body.into_any_element(),
        footer,
    )
}

pub(super) fn render_compare_modal(
    view: &RequestsView,
    selected: Vec<(String, String)>,
    estimate: Option<AiCompareEstimateDto>,
    error: Option<String>,
    cx: &mut Context<RequestsView>,
) -> (String, f32, gpui_kit::AnyElement, gpui_kit::Div) {
    let mut list = v_col().h(px(220.0)).gap(px(4.0)).overflow_y_scrollbar();
    let mut model_count = 0usize;

    for pv in &view.providers {
        if !pv.provider.enabled {
            continue;
        }
        for m in view
            .models
            .iter()
            .filter(|m| m.provider_id == pv.provider.id && m.enabled)
        {
            model_count += 1;
            let checked = selected
                .iter()
                .any(|(pid, mid)| *pid == pv.provider.id && *mid == m.model_id);
            let pid_c = pv.provider.id.clone();
            let mid_c = m.model_id.clone();
            list = list.child(
                data_row(cx)
                    .child(
                        checkbox(
                            SharedString::from(format!("cmp-m-{}-{}", pv.provider.id, m.model_id)),
                            checked,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_compare_model(pid_c.clone(), mid_c.clone(), cx);
                        })),
                    )
                    .child(div().text_xs().child(SharedString::from(format!(
                        "{} / {}",
                        pv.provider.name, m.model_id
                    )))),
            );
        }
    }
    if model_count == 0 {
        list = list.child(empty_inline(
            crate::ui::tr(cx)
                .requests()
                .overlays_dialogs_no_enabled_models(),
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .requests()
                    .overlays_dialogs_add_a_provider_and_enable(),
            )),
            None,
            cx,
        ));
    }

    let est_str = estimate.map(|e| {
        let caps: Vec<String> = e
            .at_most
            .iter()
            .map(|(cur, micro)| format!("{cur} {:.5}", (*micro as f64) / 1_000_000.0))
            .collect();
        let r = crate::ui::tr(cx).requests();
        r.overlays_compare_est(
            e.input_tokens,
            if caps.is_empty() {
                r.overlays_unknown_price().into()
            } else {
                caps.join(" + ")
            },
        )
    });

    let can_run = !selected.is_empty();
    let body = v_col()
        .gap(px(10.0))
        .child(section_heading(
            crate::ui::tr(cx)
                .requests()
                .overlays_dialogs_select_models_to_compare_12(selected.len()),
            cx,
        ))
        .child(list)
        .children(est_str.map(|s| alert_banner(Tone::Warning, s, None, cx)))
        .children(error.map(|e| alert_banner(Tone::Danger, e, None, cx)));

    let footer = h_row().gap(px(8.0)).child(cancel_btn(cx)).child(
        btn_primary(
            "cmp-start",
            crate::ui::tr(cx).requests().overlays_dialogs_run_compare(),
        )
        .disabled(!can_run)
        .on_click(cx.listener(|this, _, _, cx| this.start_ai_compare(cx))),
    );
    (
        crate::ui::tr(cx)
            .requests()
            .overlays_dialogs_compare_on_multiple_models()
            .into(),
        560.0,
        body.into_any_element(),
        footer,
    )
}
