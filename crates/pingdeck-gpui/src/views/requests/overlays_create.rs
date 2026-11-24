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
    zh: bool,
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
        .child(tab_item(if zh { "集合" } else { "Collection" }))
        .child(tab_item(if zh { "文件夹" } else { "Folder" }))
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
    let hint = match (kind, zh) {
        (NewItemKind::Collection, true) => "集合用于组织接口、共享认证、集合变量和前置/后置脚本。",
        (NewItemKind::Folder, true) => "文件夹在所属集合下分组接口，支持多层嵌套与认证继承。",
        (NewItemKind::AiApi, true) => {
            "直接调试 OpenAI / Anthropic / Gemini / Embeddings 协议并检视 SSE 帧。"
        }
        (NewItemKind::Graphql, true) => "打开 GraphQL Query 与 Variables JSON 分栏编辑器。",
        (_, true) => "打开新的请求标签页，按 ⌘S 可随时保存到集合或未归档。",
        (NewItemKind::Collection, false) => {
            "Group endpoints with shared auth, variables, and scripts."
        }
        (NewItemKind::Folder, false) => "Organize endpoints in nested folders inside a collection.",
        (NewItemKind::AiApi, false) => {
            "Debug OpenAI, Anthropic, Gemini, or Embeddings endpoints with live SSE."
        }
        (NewItemKind::Graphql, false) => "Open a split GraphQL Query + Variables editor tab.",
        (_, false) => "Opens a new request tab; press ⌘S anytime to save into a collection.",
    };

    let mut body = v_col()
        .gap(px(12.0))
        .child(seg)
        .child(div().text_xs().text_color(to_hsla(p.muted)).child(hint))
        .child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(if zh { "名称" } else { "Name" }, cx))
                .child(text_input(&view.modal_name_input)),
        );

    if folder_needs_col {
        body = body.child(alert_banner(
            Tone::Warning,
            if zh {
                "当前工作区还没有集合，请先切换到「集合」创建一个集合后再建文件夹。"
            } else {
                "No collections exist yet. Switch to Collection and create one first."
            },
            None,
            cx,
        ));
    } else if kind != NewItemKind::Collection && !view.collections.is_empty() {
        let mut col_row = h_row().gap(px(6.0)).flex_wrap();
        if kind != NewItemKind::Folder {
            col_row = col_row.child(
                choice_chip(
                    "new-col-pick-none",
                    if zh { "未归档" } else { "Unfiled" },
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
                    if zh { "所属集合" } else { "Collection" },
                    cx,
                ))
                .child(col_row),
        );
    }

    let tc = target_col.clone();
    let tf = target_fld.clone();
    let footer = h_row().gap(px(8.0)).child(cancel_btn(zh, cx)).child(
        btn_primary("new-item-create", if zh { "创建" } else { "Create" })
            .disabled(folder_needs_col)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.confirm_new_item_modal(kind, tc.clone(), tf.clone(), window, cx);
            })),
    );

    (
        if zh {
            "新建请求、集合或文件夹"
        } else {
            "New Request, Collection or Folder"
        }
        .into(),
        560.0,
        body.into_any_element(),
        footer,
    )
}

pub(super) fn render_code_snippets(
    view: &RequestsView,
    values_mode: bool,
    zh: bool,
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
        .child(tab_item(if zh { "展开环境值" } else { "Values" }))
        .child(tab_item(if zh { "保留 {{模板}}" } else { "Template" }))
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
                .child(if zh {
                    "秘密变量与字面量凭据已自动替换为占位符或环境变量，防止复制泄漏。"
                } else {
                    "Secrets and literal credentials are automatically redacted from generated snippets."
                }),
        );

    let footer = h_row().gap(px(8.0)).child(cancel_btn(zh, cx)).child(
        btn_primary("snip-copy-ok", if zh { "复制代码" } else { "Copy snippet" })
            .icon(icon_xs(UiIcon::Copy))
            .on_click(cx.listener(|this, _, _, cx| {
                let code = this.snippet_editor.read(cx).value().to_string();
                let _ = clipboard::set_text(&code);
                this.status_msg = Some((Tone::Success, "已复制代码片段".into()));
                this.active_modal = None;
                cx.notify();
            })),
    );

    (
        if zh {
            "生成请求代码片段"
        } else {
            "Generate Code Snippet"
        }
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
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> (String, f32, gpui_kit::AnyElement, gpui_kit::Div) {
    let mut col_row = h_row().gap(px(6.0)).flex_wrap().child(
        choice_chip(
            "saveas-unfiled",
            if zh { "未归档" } else { "Unfiled" },
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

    let body = v_col()
        .gap(px(12.0))
        .child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(
                    if zh { "请求名称" } else { "Request name" },
                    cx,
                ))
                .child(text_input(&view.modal_name_input)),
        )
        .child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(if zh { "保存位置" } else { "Save to" }, cx))
                .child(col_row),
        );

    let footer = h_row().gap(px(8.0)).child(cancel_btn(zh, cx)).child(
        btn_primary("saveas-ok", if zh { "保存" } else { "Save" }).on_click(cx.listener(
            move |this, _, window, cx| {
                let nm = this.modal_name_input.read(cx).value().to_string();
                this.save_active_as(nm, target_col.clone(), target_fld.clone(), window, cx);
            },
        )),
    );
    (
        if zh {
            "保存请求到…"
        } else {
            "Save Request As"
        }
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
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> (String, f32, gpui_kit::AnyElement, gpui_kit::Div) {
    let mut row = h_row().gap(px(6.0)).flex_wrap().child(
        choice_chip(
            "mv-unfiled",
            if zh { "未归档" } else { "Unfiled" },
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
    let body = v_col()
        .gap(px(8.0))
        .child(section_heading(
            if zh {
                "目标集合"
            } else {
                "Target collection"
            },
            cx,
        ))
        .child(row);
    let footer = h_row().gap(px(8.0)).child(cancel_btn(zh, cx)).child(
        btn_primary("mv-ok", if zh { "移动" } else { "Move" }).on_click(cx.listener(
            move |this, _, _, cx| {
                this.move_request_to(
                    request_id.clone(),
                    target_col.clone(),
                    target_fld.clone(),
                    cx,
                );
            },
        )),
    );
    (
        if zh { "移动请求" } else { "Move Request" }.into(),
        460.0,
        body.into_any_element(),
        footer,
    )
}
