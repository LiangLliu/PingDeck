//! Creation, Code Snippets, Save As, and Move Request modal contents for `RequestsView`.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::snippet_service::SnippetLanguage;
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    Disableable as _, Tone, UiIcon, alert_banner, btn_primary, choice_chip, code_editor, h_row,
    icon_xs, palette, section_heading, tab_bar_segmented, tab_item, text_input, to_hsla, v_col,
};

use super::RequestsView;
use super::model::{ActiveModal, NewItemKind};
use super::overlays::cancel_btn;

pub(super) fn render_new_item(
    view: &RequestsView,
    kind: NewItemKind,
    target_col: Option<String>,
    target_fld: Option<String>,
    cx: &mut Context<RequestsView>,
) -> (String, f32, gpui_kit::AnyElement, gpui_kit::Div) {
    let p = palette(cx);
    let ent = cx.entity().clone();
    let seg = tab_bar_segmented("new-item-kind-seg")
        .selected_index(kind.index())
        .child(tab_item("HTTP GET"))
        .child(tab_item("HTTP POST"))
        .child(tab_item("GraphQL"))
        .child(tab_item("AI API"))
        .child(tab_item(crate::ui::tr(cx).requests().palette_collection()))
        .child(tab_item(
            crate::ui::tr(cx).requests().overlays_create_folder(),
        ))
        .on_click(move |idx, _, cx| {
            let next = NewItemKind::from_index(*idx);
            ent.update(cx, |this, cx| {
                if let Some(ActiveModal::NewItem { kind, .. }) = &mut this.active_modal {
                    *kind = next;
                }
                cx.notify();
            });
        });

    let folder_needs_col = kind == NewItemKind::Folder && view.collections.is_empty();
    let r = crate::ui::tr(cx).requests();
    let hint = match kind {
        NewItemKind::Collection => r.overlays_hint_collection(),
        NewItemKind::Folder => r.overlays_hint_folder(),
        NewItemKind::AiApi => r.overlays_hint_ai(),
        NewItemKind::Graphql => r.overlays_hint_graphql(),
        _ => r.overlays_hint_http(),
    };

    let mut body = v_col()
        .gap(px(12.0))
        .child(seg)
        .child(div().text_xs().text_color(to_hsla(p.muted)).child(hint))
        .child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(
                    crate::ui::tr(cx).requests().tree_name_label(),
                    cx,
                ))
                .child(text_input(&view.modal_name_input)),
        );

    if folder_needs_col {
        body = body.child(alert_banner(
            Tone::Warning,
            crate::ui::tr(cx)
                .requests()
                .overlays_create_no_collections_exist_yet_switch(),
            None,
            cx,
        ));
    } else if kind != NewItemKind::Collection && !view.collections.is_empty() {
        let mut col_row = h_row().gap(px(6.0)).flex_wrap();
        if kind != NewItemKind::Folder {
            col_row = col_row.child(
                choice_chip(
                    "new-col-pick-none",
                    crate::ui::tr(cx).requests().tree_unfiled(),
                    target_col.is_none(),
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(ActiveModal::NewItem {
                        target_collection_id,
                        target_folder_id,
                        ..
                    }) = &mut this.active_modal
                    {
                        *target_collection_id = None;
                        *target_folder_id = None;
                    }
                    cx.notify();
                })),
            );
        }
        for c in &view.collections {
            let sel = target_col.as_deref() == Some(c.id.as_str());
            let cid = c.id.clone();
            col_row = col_row.child(
                choice_chip(
                    SharedString::from(format!("new-col-pick-{cid}")),
                    c.name.clone(),
                    sel,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(ActiveModal::NewItem {
                        target_collection_id,
                        target_folder_id,
                        ..
                    }) = &mut this.active_modal
                    {
                        *target_collection_id = Some(cid.clone());
                        *target_folder_id = None;
                    }
                    cx.notify();
                })),
            );
        }
        body = body.child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(
                    crate::ui::tr(cx).requests().overlays_create_collection(),
                    cx,
                ))
                .child(col_row),
        );
        if let Some(cid_sel) = target_col.as_deref() {
            let flds: Vec<_> = view
                .folders
                .iter()
                .filter(|f| f.collection_id.as_str() == cid_sel)
                .collect();
            if !flds.is_empty() {
                let mut f_row = h_row().gap(px(6.0)).flex_wrap().child(
                    choice_chip(
                        "new-fld-root",
                        crate::ui::tr(cx).requests().overlays_create_root(),
                        target_fld.is_none(),
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(ActiveModal::NewItem {
                            target_folder_id, ..
                        }) = &mut this.active_modal
                        {
                            *target_folder_id = None;
                        }
                        cx.notify();
                    })),
                );
                for f in flds {
                    let fid = f.id.as_str().to_owned();
                    let sel = target_fld.as_deref() == Some(fid.as_str());
                    f_row = f_row.child(
                        choice_chip(
                            SharedString::from(format!("new-fld-{fid}")),
                            f.name.clone(),
                            sel,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(ActiveModal::NewItem {
                                target_folder_id, ..
                            }) = &mut this.active_modal
                            {
                                *target_folder_id = Some(fid.clone());
                            }
                            cx.notify();
                        })),
                    );
                }
                body = body.child(
                    v_col()
                        .gap(px(6.0))
                        .child(section_heading(
                            crate::ui::tr(cx)
                                .requests()
                                .overlays_create_folder_optional(),
                            cx,
                        ))
                        .child(f_row),
                );
            }
        }
    }

    let tc = target_col.clone();
    let tf = target_fld.clone();
    let footer = h_row().gap(px(8.0)).child(cancel_btn(cx)).child(
        btn_primary(
            "new-item-create",
            crate::ui::tr(cx).requests().tree_create(),
        )
        .disabled(folder_needs_col)
        .on_click(cx.listener(move |this, _, window, cx| {
            this.confirm_new_item_modal(kind, tc.clone(), tf.clone(), window, cx);
        })),
    );

    (
        crate::ui::tr(cx)
            .requests()
            .overlays_create_new_request_collection_or_folder()
            .into(),
        560.0,
        body.into_any_element(),
        footer,
    )
}

pub(super) fn render_code_snippets(
    view: &RequestsView,
    values_mode: bool,
    cx: &mut Context<RequestsView>,
) -> (String, f32, gpui_kit::AnyElement, gpui_kit::Div) {
    let p = palette(cx);
    let mut lang_row = h_row().gap(px(5.0)).flex_wrap();
    for lang in SnippetLanguage::ALL {
        let sel = lang == view.snippet_lang;
        lang_row = lang_row.child(
            choice_chip(
                SharedString::from(format!("snip-lang-{}", lang.id())),
                lang.label(),
                sel,
                cx,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.refresh_snippet(lang, values_mode, window, cx);
            })),
        );
    }

    let ent_m = cx.entity().clone();
    let mode_seg = tab_bar_segmented("snip-mode-seg")
        .selected_index(usize::from(!values_mode))
        .child(tab_item(
            crate::ui::tr(cx).requests().overlays_create_values(),
        ))
        .child(tab_item(
            crate::ui::tr(cx).requests().overlays_create_template(),
        ))
        .on_click(move |idx, window, cx| {
            let vm = *idx == 0;
            ent_m.update(cx, |this, cx| {
                if let Some(ActiveModal::CodeSnippets { values_mode }) = &mut this.active_modal {
                    *values_mode = vm;
                }
                this.refresh_snippet(this.snippet_lang, vm, window, cx);
            });
        });

    let body = v_col()
        .gap(px(10.0))
        .child(
            h_row()
                .justify_between()
                .items_start()
                .gap(px(10.0))
                .child(div().flex_1().min_w_0().child(lang_row))
                .child(div().flex_shrink_0().child(mode_seg)),
        )
        .child(div().h(px(300.0)).child(code_editor(&view.snippet_editor)))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .child(
                    crate::ui::tr(cx)
                        .requests()
                        .overlays_create_secrets_and_literal_credentials_are(),
                ),
        );

    let footer = h_row().gap(px(8.0)).child(cancel_btn(cx)).child(
        btn_primary(
            "snip-copy-ok",
            crate::ui::tr(cx).requests().overlays_create_copy_snippet(),
        )
        .icon(icon_xs(UiIcon::Copy))
        .on_click(cx.listener(|this, _, _, cx| {
            let code = this.snippet_editor.read(cx).value().to_string();
            let _ = clipboard::set_text(&code);
            this.status_msg = Some((
                Tone::Success,
                crate::ui::tr(cx)
                    .requests()
                    .overlays_copied_snippet()
                    .into(),
            ));
            this.active_modal = None;
            cx.notify();
        })),
    );

    (
        crate::ui::tr(cx)
            .requests()
            .overlays_create_generate_code_snippet()
            .into(),
        720.0,
        body.into_any_element(),
        footer,
    )
}

pub(super) fn render_save_as(
    view: &RequestsView,
    target_col: Option<String>,
    target_fld: Option<String>,
    cx: &mut Context<RequestsView>,
) -> (String, f32, gpui_kit::AnyElement, gpui_kit::Div) {
    let mut col_row = h_row().gap(px(6.0)).flex_wrap().child(
        choice_chip(
            "saveas-unfiled",
            crate::ui::tr(cx).requests().tree_unfiled(),
            target_col.is_none(),
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(ActiveModal::SaveAs {
                target_collection_id,
                target_folder_id,
            }) = &mut this.active_modal
            {
                *target_collection_id = None;
                *target_folder_id = None;
            }
            cx.notify();
        })),
    );
    for c in &view.collections {
        let sel = target_col.as_deref() == Some(c.id.as_str());
        let cid = c.id.clone();
        col_row = col_row.child(
            choice_chip(
                SharedString::from(format!("saveas-col-{cid}")),
                c.name.clone(),
                sel,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(ActiveModal::SaveAs {
                    target_collection_id,
                    target_folder_id,
                }) = &mut this.active_modal
                {
                    *target_collection_id = Some(cid.clone());
                    *target_folder_id = None;
                }
                cx.notify();
            })),
        );
    }

    let mut body = v_col()
        .gap(px(12.0))
        .child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(
                    crate::ui::tr(cx).requests().overlays_create_request_name(),
                    cx,
                ))
                .child(text_input(&view.modal_name_input)),
        )
        .child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(
                    crate::ui::tr(cx).requests().overlays_create_save_to(),
                    cx,
                ))
                .child(col_row),
        );
    if let Some(cid_sel) = target_col.as_deref() {
        let flds: Vec<_> = view
            .folders
            .iter()
            .filter(|f| f.collection_id.as_str() == cid_sel)
            .collect();
        if !flds.is_empty() {
            let mut f_row = h_row().gap(px(6.0)).flex_wrap().child(
                choice_chip(
                    "saveas-fld-root",
                    crate::ui::tr(cx).requests().overlays_create_root_2(),
                    target_fld.is_none(),
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(ActiveModal::SaveAs {
                        target_folder_id, ..
                    }) = &mut this.active_modal
                    {
                        *target_folder_id = None;
                    }
                    cx.notify();
                })),
            );
            for f in flds {
                let fid = f.id.as_str().to_owned();
                let sel = target_fld.as_deref() == Some(fid.as_str());
                f_row = f_row.child(
                    choice_chip(
                        SharedString::from(format!("saveas-fld-{fid}")),
                        f.name.clone(),
                        sel,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ActiveModal::SaveAs {
                            target_folder_id, ..
                        }) = &mut this.active_modal
                        {
                            *target_folder_id = Some(fid.clone());
                        }
                        cx.notify();
                    })),
                );
            }
            body = body.child(
                v_col()
                    .gap(px(6.0))
                    .child(section_heading(
                        crate::ui::tr(cx).requests().overlays_create_folder_2(),
                        cx,
                    ))
                    .child(f_row),
            );
        }
    }

    let footer = h_row().gap(px(8.0)).child(cancel_btn(cx)).child(
        btn_primary("saveas-ok", crate::ui::tr(cx).common().save()).on_click(cx.listener(
            move |this, _, window, cx| {
                let nm = this.modal_name_input.read(cx).value().to_string();
                this.save_active_as(nm, target_col.clone(), target_fld.clone(), window, cx);
            },
        )),
    );
    (
        crate::ui::tr(cx)
            .requests()
            .overlays_create_save_request_as()
            .into(),
        480.0,
        body.into_any_element(),
        footer,
    )
}

pub(super) fn render_move_request(
    view: &RequestsView,
    request_id: String,
    target_col: Option<String>,
    target_fld: Option<String>,
    cx: &mut Context<RequestsView>,
) -> (String, f32, gpui_kit::AnyElement, gpui_kit::Div) {
    let mut row = h_row().gap(px(6.0)).flex_wrap().child(
        choice_chip(
            "mv-unfiled",
            crate::ui::tr(cx).requests().tree_unfiled(),
            target_col.is_none(),
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(ActiveModal::MoveRequest {
                target_collection_id,
                target_folder_id,
                ..
            }) = &mut this.active_modal
            {
                *target_collection_id = None;
                *target_folder_id = None;
            }
            cx.notify();
        })),
    );
    for c in &view.collections {
        let sel = target_col.as_deref() == Some(c.id.as_str());
        let cid = c.id.clone();
        row = row.child(
            choice_chip(
                SharedString::from(format!("mv-col-{cid}")),
                c.name.clone(),
                sel,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(ActiveModal::MoveRequest {
                    target_collection_id,
                    target_folder_id,
                    ..
                }) = &mut this.active_modal
                {
                    *target_collection_id = Some(cid.clone());
                    *target_folder_id = None;
                }
                cx.notify();
            })),
        );
    }
    let mut body = v_col()
        .gap(px(8.0))
        .child(section_heading(
            crate::ui::tr(cx)
                .requests()
                .overlays_create_target_collection(),
            cx,
        ))
        .child(row);
    if let Some(cid_sel) = target_col.as_deref() {
        let flds: Vec<_> = view
            .folders
            .iter()
            .filter(|f| f.collection_id.as_str() == cid_sel)
            .collect();
        if !flds.is_empty() {
            let mut f_row = h_row().gap(px(6.0)).flex_wrap().child(
                choice_chip(
                    "mv-fld-root",
                    crate::ui::tr(cx).requests().overlays_create_root_2(),
                    target_fld.is_none(),
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(ActiveModal::MoveRequest {
                        target_folder_id, ..
                    }) = &mut this.active_modal
                    {
                        *target_folder_id = None;
                    }
                    cx.notify();
                })),
            );
            for f in flds {
                let fid = f.id.as_str().to_owned();
                let sel = target_fld.as_deref() == Some(fid.as_str());
                f_row = f_row.child(
                    choice_chip(
                        SharedString::from(format!("mv-fld-{fid}")),
                        f.name.clone(),
                        sel,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ActiveModal::MoveRequest {
                            target_folder_id, ..
                        }) = &mut this.active_modal
                        {
                            *target_folder_id = Some(fid.clone());
                        }
                        cx.notify();
                    })),
                );
            }
            body = body
                .child(section_heading(
                    crate::ui::tr(cx).requests().overlays_create_folder_2(),
                    cx,
                ))
                .child(f_row);
        }
    }
    let footer = h_row().gap(px(8.0)).child(cancel_btn(cx)).child(
        btn_primary("mv-ok", crate::ui::tr(cx).requests().tree_move_submit()).on_click(
            cx.listener(move |this, _, _, cx| {
                this.move_request_to(
                    request_id.clone(),
                    target_col.clone(),
                    target_fld.clone(),
                    cx,
                );
            }),
        ),
    );
    (
        crate::ui::tr(cx)
            .requests()
            .overlays_create_move_request()
            .into(),
        460.0,
        body.into_any_element(),
        footer,
    )
}
