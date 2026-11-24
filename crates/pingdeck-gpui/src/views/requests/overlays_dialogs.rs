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
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> (String, f32, gpui_kit::AnyElement, gpui_kit::Div) {
    let p = palette(cx);
    let ent = cx.entity().clone();
    let top_seg = tab_bar_segmented("io-tab-seg")
        .selected_index(usize::from(tab == ImportExportTab::Export))
        .child(tab_item(if zh { "导入 (预览)" } else { "Import" }))
        .child(tab_item(if zh {
            "导出 Postman JSON"
        } else {
            "Export Postman"
        }))
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
                            if zh {
                                "选择文件…"
                            } else {
                                "Pick file…"
                            },
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
                        btn_secondary("imp-preview-btn", if zh { "解析预览" } else { "Preview" })
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
                            .child(tab_item(if zh { "更新" } else { "Update" }))
                            .child(tab_item(if zh { "保留两者" } else { "Keep both" }))
                            .child(tab_item(if zh { "替换" } else { "Replace" }))
                            .child(tab_item(if zh { "跳过" } else { "Skip" }))
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

        let footer = h_row().gap(px(8.0)).child(cancel_btn(zh, cx)).child(
            btn_primary("imp-apply-btn", if zh { "确认导入" } else { "Import" })
                .on_click(cx.listener(|this, _, window, cx| this.import_apply(window, cx))),
        );
        return (
            if zh {
                "导入与导出"
            } else {
                "Import & Export"
            }
            .into(),
            680.0,
            body.into_any_element(),
            footer,
        );
    }

    // Export tab
    let mut col_picks = h_row().gap(px(6.0)).flex_wrap();
    if view.collections.is_empty() {
        col_picks = col_picks.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "当前工作区还没有可导出的集合。"
        } else {
            "No collections to export in this workspace."
        }));
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
                    if zh {
                        "选择要导出的集合"
                    } else {
                        "Collection to export"
                    },
                    cx,
                ))
                .child(col_picks),
        )
        .child(
            checkbox("exp-inc-sec", include_secrets)
                .label(if zh {
                    "包含字面量凭据（明文导出，默认留空以保安全）"
                } else {
                    "Include literal credentials (plaintext)"
                })
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
                .label(if zh {
                    "我确认导出文件包含明文凭据，不会提交到公开仓库。"
                } else {
                    "I understand the export contains plaintext credentials."
                })
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
    let footer = h_row().gap(px(8.0)).child(cancel_btn(zh, cx)).child(
        btn_primary(
            "exp-go-btn",
            if zh {
                "导出 JSON…"
            } else {
                "Export JSON…"
            },
        )
        .disabled(!can_export)
        .on_click(cx.listener(move |this, _, _, cx| {
            if can_export && let Some(cid) = cid_go.clone() {
                this.export_collection_postman(cid, include_secrets, cx);
            }
        })),
    );
    (
        if zh {
            "导入与导出"
        } else {
            "Import & Export"
        }
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
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> (String, f32, gpui_kit::AnyElement, gpui_kit::Div) {
    let p = palette(cx);
    let body = v_col()
        .gap(px(12.0))
        .child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(
                    if zh { "变量名称" } else { "Variable name" },
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
                .child(SharedString::from(format!("值预览: {value}"))),
        )
        .child(
            h_row()
                .gap(px(16.0))
                .child(
                    checkbox("ext-secret", is_secret)
                        .label(if zh {
                            "存为机密 (0600 保险库)"
                        } else {
                            "Store as secret"
                        })
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
                        .label(if zh {
                            "写入工作区全局变量"
                        } else {
                            "Save to Globals"
                        })
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

    let footer = h_row().gap(px(8.0)).child(cancel_btn(zh, cx)).child(
        btn_primary("ext-ok", if zh { "写入变量" } else { "Save variable" }).on_click(cx.listener(
            move |this, _, _, cx| {
                let key = this.modal_name_input.read(cx).value().to_string();
                this.commit_extract_variable(key, value.clone(), is_secret, use_globals, cx);
            },
        )),
    );
    (
        if zh {
            "提取响应值为环境变量"
        } else {
            "Extract Value to Variable"
        }
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
    zh: bool,
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
            if zh {
                "没有已启用的服务商模型"
            } else {
                "No enabled models"
            },
            Some(SharedString::from(if zh {
                "请先在「服务商」页面添加服务商并启用模型后再进行多模型对比。"
            } else {
                "Add a provider and enable models in the Providers view first."
            })),
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
        format!(
            "预估输入 ~{} tok · 上限: {}",
            e.input_tokens,
            if caps.is_empty() {
                "未知价格".into()
            } else {
                caps.join(" + ")
            }
        )
    });

    let can_run = !selected.is_empty();
    let body = v_col()
        .gap(px(10.0))
        .child(section_heading(
            if zh {
                format!("选择要对比的模型（已选 {}/12）", selected.len())
            } else {
                format!("Select models to compare ({}/12)", selected.len())
            },
            cx,
        ))
        .child(list)
        .children(est_str.map(|s| alert_banner(Tone::Warning, s, None, cx)))
        .children(error.map(|e| alert_banner(Tone::Danger, e, None, cx)));

    let footer = h_row().gap(px(8.0)).child(cancel_btn(zh, cx)).child(
        btn_primary(
            "cmp-start",
            if zh {
                "开始对比运行"
            } else {
                "Run compare"
            },
        )
        .disabled(!can_run)
        .on_click(cx.listener(|this, _, _, cx| this.start_ai_compare(cx))),
    );
    (
        if zh {
            "多模型对比运行"
        } else {
            "Compare on Multiple Models"
        }
        .into(),
        560.0,
        body.into_any_element(),
        footer,
    )
}
