//! Import/Export, Extract Variable, and Multi-Model Compare modal contents for `RequestsView`.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::{AiCompareEstimateDto, ImportConflict, ImportPreviewDto};

use crate::ui::{
    Disableable as _, ScrollableElement as _, Tone, alert_banner, badge, btn_primary,
    btn_secondary, card, checkbox, choice_chip, data_row, empty_inline, h_row, multiline_input,
    palette, section_heading, tab_bar_segmented, tab_item, text_input, to_hsla, v_col,
};

use super::RequestsView;
use super::model::{ActiveModal, ImportExportTab};
use super::overlays::cancel_btn;

#[allow(clippy::too_many_arguments)]
pub(super) fn render_import_export(
    view: &RequestsView,
    tab: ImportExportTab,
    format: String,
    preview: Option<ImportPreviewDto>,
    conflict: ImportConflict,
    export_col: Option<String>,
    include_secrets: bool,
    ack_secrets: bool,
    cx: &mut Context<RequestsView>,
) -> (String, f32, gpui_kit::AnyElement, gpui_kit::Div) {
    let p = palette(cx);
    let ent = cx.entity().clone();
    let top_seg = tab_bar_segmented("io-tab-seg")
        .selected_index(usize::from(tab == ImportExportTab::Export))
        .child(tab_item(
            crate::ui::tr(cx).requests().overlays_dialogs_import(),
        ))
        .child(tab_item(
            crate::ui::tr(cx)
                .requests()
                .overlays_dialogs_export_postman(),
        ))
        .on_click(move |idx, _, cx| {
            ent.update(cx, |this, cx| {
                if let Some(ActiveModal::ImportExport { tab, .. }) = &mut this.active_modal {
                    *tab = if *idx == 1 {
                        ImportExportTab::Export
                    } else {
                        ImportExportTab::Import
                    };
                }
                cx.notify();
            });
        });

    let mut body = v_col().gap(px(12.0)).child(top_seg);

    if tab == ImportExportTab::Import {
        let fmts = [
            ("curl", "cURL"),
            ("postman", "Postman"),
            ("openapi", "OpenAPI"),
            ("har", "HAR"),
            ("insomnia", "Insomnia"),
            ("bruno", "Bruno"),
            ("hoppscotch", "Hoppscotch"),
            ("environment", "Environment"),
        ];
        let mut frow = h_row().gap(px(5.0)).flex_wrap();
        for (key, label) in fmts {
            let sel = format == key;
            let f_s = key.to_owned();
            frow = frow.child(
                choice_chip(SharedString::from(format!("imp-fmt-{key}")), label, sel, cx).on_click(
                    cx.listener(move |this, _, _, cx| {
                        if let Some(ActiveModal::ImportExport { format, .. }) =
                            &mut this.active_modal
                        {
                            *format = f_s.clone();
                        }
                        cx.notify();
                    }),
                ),
            );
        }
        body = body
            .child(
                h_row()
                    .justify_between()
                    .items_start()
                    .gap(px(8.0))
                    .child(div().flex_1().min_w_0().child(frow))
                    .child(
                        btn_secondary(
                            "imp-pick-file",
                            crate::ui::tr(cx).requests().overlays_dialogs_pick_file(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.import_pick_file(window, cx);
                        })),
                    ),
            )
            .child(
                div()
                    .h(px(140.0))
                    .child(multiline_input(&view.modal_text_input)),
            )
            .child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        btn_secondary(
                            "imp-preview-btn",
                            crate::ui::tr(cx).requests().overlays_dialogs_preview(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.import_run_preview(cx))),
                    )
                    .child({
                        let c_idx = match conflict {
                            ImportConflict::Update => 0,
                            ImportConflict::KeepBoth => 1,
                            ImportConflict::Replace => 2,
                            ImportConflict::Skip => 3,
                        };
                        let ent_c = cx.entity().clone();
                        tab_bar_segmented("imp-conflict-seg")
                            .selected_index(c_idx)
                            .child(tab_item(
                                crate::ui::tr(cx).requests().overlays_dialogs_update(),
                            ))
                            .child(tab_item(
                                crate::ui::tr(cx).requests().overlays_dialogs_keep_both(),
                            ))
                            .child(tab_item(
                                crate::ui::tr(cx).requests().overlays_dialogs_replace(),
                            ))
                            .child(tab_item(
                                crate::ui::tr(cx).requests().overlays_dialogs_skip(),
                            ))
                            .on_click(move |idx, _, cx| {
                                ent_c.update(cx, |this, cx| {
                                    if let Some(ActiveModal::ImportExport { conflict, .. }) =
                                        &mut this.active_modal
                                    {
                                        *conflict = match *idx {
                                            1 => ImportConflict::KeepBoth,
                                            2 => ImportConflict::Replace,
                                            3 => ImportConflict::Skip,
                                            _ => ImportConflict::Update,
                                        };
                                    }
                                    cx.notify();
                                });
                            })
                    }),
            );

        if let Some(prev) = preview {
            body = body.child(
                card(cx).p(px(10.0)).child(
                    v_col()
                        .gap(px(6.0))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(badge(prev.format.clone(), Tone::Accent, cx))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child(SharedString::from(
                                            prev.name.unwrap_or_else(|| "Unnamed".into()),
                                        )),
                                )
                                .children(
                                    prev.requests
                                        .map(|n| badge(format!("{n} req"), Tone::Neutral, cx)),
                                )
                                .children(
                                    prev.folders
                                        .map(|n| badge(format!("{n} folders"), Tone::Neutral, cx)),
                                ),
                        )
                        .children((!prev.warnings.is_empty()).then(|| {
                            alert_banner(Tone::Warning, prev.warnings.join(" · "), None, cx)
                        })),
                ),
            );
        }

        let footer = h_row().gap(px(8.0)).child(cancel_btn(cx)).child(
            btn_primary(
                "imp-apply-btn",
                crate::ui::tr(cx).requests().overlays_dialogs_import_2(),
            )
            .on_click(cx.listener(|this, _, window, cx| this.import_apply(window, cx))),
        );
        return (
            crate::ui::tr(cx)
                .requests()
                .overlays_dialogs_import_export()
                .into(),
            680.0,
            body.into_any_element(),
            footer,
        );
    }

    // Export tab
    let mut col_picks = h_row().gap(px(6.0)).flex_wrap();
    if view.collections.is_empty() {
        col_picks = col_picks.child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                crate::ui::tr(cx)
                    .requests()
                    .overlays_dialogs_no_collections_to_export_in(),
            ),
        );
    } else {
        for c in &view.collections {
            let sel = export_col.as_deref() == Some(c.id.as_str());
            let cid = c.id.clone();
            col_picks = col_picks.child(
                choice_chip(
                    SharedString::from(format!("exp-col-{cid}")),
                    c.name.clone(),
                    sel,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(ActiveModal::ImportExport {
                        export_collection_id,
                        ..
                    }) = &mut this.active_modal
                    {
                        *export_collection_id = Some(cid.clone());
                    }
                    cx.notify();
                })),
            );
        }
    }

    body = body
        .child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(
                    crate::ui::tr(cx)
                        .requests()
                        .overlays_dialogs_collection_to_export(),
                    cx,
                ))
                .child(col_picks),
        )
        .child(
            checkbox("exp-inc-sec", include_secrets)
                .label(
                    crate::ui::tr(cx)
                        .requests()
                        .overlays_dialogs_include_literal_credentials_plaintext(),
                )
                .on_click(cx.listener(|this, ch: &bool, _, cx| {
                    if let Some(ActiveModal::ImportExport {
                        export_include_secrets,
                        export_ack_secrets,
                        ..
                    }) = &mut this.active_modal
                    {
                        *export_include_secrets = *ch;
                        if !*ch {
                            *export_ack_secrets = false;
                        }
                    }
                    cx.notify();
                })),
        )
        .children(include_secrets.then(|| {
            checkbox("exp-ack-sec", ack_secrets)
                .label(
                    crate::ui::tr(cx)
                        .requests()
                        .overlays_dialogs_i_understand_the_export_contains(),
                )
                .on_click(cx.listener(|this, ch: &bool, _, cx| {
                    if let Some(ActiveModal::ImportExport {
                        export_ack_secrets, ..
                    }) = &mut this.active_modal
                    {
                        *export_ack_secrets = *ch;
                    }
                    cx.notify();
                }))
        }));

    let can_export = export_col.is_some() && (!include_secrets || ack_secrets);
    let cid_go = export_col.clone();
    let footer = h_row().gap(px(8.0)).child(cancel_btn(cx)).child(
        btn_primary(
            "exp-go-btn",
            crate::ui::tr(cx).requests().overlays_dialogs_export_json(),
        )
        .disabled(!can_export)
        .on_click(cx.listener(move |this, _, _, cx| {
            if can_export && let Some(cid) = cid_go.clone() {
                this.export_collection_postman(cid, include_secrets, cx);
            }
        })),
    );
    (
        crate::ui::tr(cx)
            .requests()
            .overlays_dialogs_import_export()
            .into(),
        660.0,
        body.into_any_element(),
        footer,
    )
}

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
