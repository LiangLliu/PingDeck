//! Full-window modal overlay dispatcher and confirmation dialogs for `RequestsView`,
//! backed by PingDeck's unified `ui::modal_dialog` (`deferred(anchored)`).

use gpui_kit::{
    AnyElement, Context, Focusable as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};

use crate::ui::{
    Tone, alert_banner, btn_danger, btn_primary, btn_secondary, h_row, modal_dialog, palette,
    section_heading, text_input, to_hsla, v_col,
};

use super::RequestsView;
use super::model::{ActiveModal, NewItemKind};
use super::overlays_create::{
    render_code_snippets, render_move_request, render_new_item, render_save_as,
};
use super::overlays_dialogs::{render_compare_modal, render_extract_var, render_import_export};

impl RequestsView {
    /// Opens the New Item modal, pre-selects defaults, and focuses the name input immediately.
    pub fn open_new_item_modal(
        &mut self,
        kind: NewItemKind,
        target_collection_id: Option<String>,
        target_folder_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal_name_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.modal_name_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        self.active_modal = Some(ActiveModal::NewItem {
            kind,
            target_collection_id: target_collection_id
                .or_else(|| self.collections.first().map(|c| c.id.clone())),
            target_folder_id,
        });
        cx.notify();
    }

    /// Focuses `modal_name_input` after prefilling a modal with text (e.g. Rename / Save As).
    pub fn focus_modal_name_input(
        &mut self,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal_name_input
            .update(cx, |i, cx| i.set_value(value, window, cx));
        self.modal_name_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    }

    /// Submits the active modal when pressing Enter in `modal_name_input`.
    pub fn submit_active_modal_on_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.active_modal.clone() else {
            return;
        };
        let name = self.modal_name_input.read(cx).value().trim().to_owned();
        match modal {
            ActiveModal::NewItem {
                kind,
                target_collection_id,
                target_folder_id,
            } => {
                self.confirm_new_item_modal(
                    kind,
                    target_collection_id,
                    target_folder_id,
                    window,
                    cx,
                );
            }
            ActiveModal::Rename { kind, id } if !name.is_empty() => {
                self.rename_tree_item(kind, id, name, cx);
            }
            ActiveModal::SaveAs {
                target_collection_id,
                target_folder_id,
            } => {
                self.save_active_as(name, target_collection_id, target_folder_id, window, cx);
            }
            ActiveModal::ExtractVariable {
                value,
                is_secret,
                use_globals,
            } if !name.is_empty() => {
                self.commit_extract_variable(name, value, is_secret, use_globals, cx);
            }
            _ => {}
        }
    }

    /// Commits creation from the New Item modal (shared by click & Enter submit).
    pub fn confirm_new_item_modal(
        &mut self,
        kind: NewItemKind,
        target_collection_id: Option<String>,
        target_folder_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.modal_name_input.read(cx).value().trim().to_owned();
        match kind {
            NewItemKind::Collection => {
                self.create_collection(
                    if name.is_empty() {
                        "New Collection".into()
                    } else {
                        name
                    },
                    cx,
                );
            }
            NewItemKind::Folder => {
                if let Some(cid) =
                    target_collection_id.or_else(|| self.collections.first().map(|c| c.id.clone()))
                {
                    self.create_folder(
                        cid,
                        target_folder_id,
                        if name.is_empty() {
                            "New Folder".into()
                        } else {
                            name
                        },
                        cx,
                    );
                } else {
                    self.status_msg = Some((Tone::Warning, "请先创建一个集合".into()));
                    cx.notify();
                }
            }
            other => {
                self.active_modal = None;
                self.open_new_request_tab(
                    other,
                    target_collection_id,
                    target_folder_id,
                    window,
                    cx,
                );
                if !name.is_empty()
                    && let Some(t) = self.active_req_mut()
                {
                    t.draft.name = name.clone();
                    self.name_input
                        .update(cx, |i, cx| i.set_value(name, window, cx));
                }
                cx.notify();
            }
        }
    }
}

pub fn render_active_modal(
    view: &RequestsView,
    zh: bool,
    window: &Window,
    cx: &mut Context<RequestsView>,
) -> Option<AnyElement> {
    let modal = view.active_modal.clone()?;
    let p = palette(cx);

    let (title, width, body, footer): (String, f32, AnyElement, gpui_kit::Div) = match modal {
        ActiveModal::NewItem {
            kind,
            target_collection_id,
            target_folder_id,
        } => render_new_item(view, kind, target_collection_id, target_folder_id, zh, cx),
        ActiveModal::CodeSnippets { values_mode } => {
            render_code_snippets(view, values_mode, zh, cx)
        }
        ActiveModal::ImportExport {
            tab,
            format,
            preview,
            conflict,
            export_collection_id,
            export_include_secrets,
            export_ack_secrets,
        } => render_import_export(
            view,
            tab,
            format,
            preview,
            conflict,
            export_collection_id,
            export_include_secrets,
            export_ack_secrets,
            zh,
            cx,
        ),
        ActiveModal::SaveAs {
            target_collection_id,
            target_folder_id,
        } => render_save_as(view, target_collection_id, target_folder_id, zh, cx),
        ActiveModal::Rename { kind, id } => {
            let id_c = id.clone();
            (
                if zh { "重命名" } else { "Rename" }.into(),
                420.0,
                v_col()
                    .gap(px(8.0))
                    .child(section_heading(if zh { "新名称" } else { "New name" }, cx))
                    .child(text_input(&view.modal_name_input))
                    .into_any_element(),
                h_row().gap(px(8.0)).child(cancel_btn(zh, cx)).child(
                    btn_primary("modal-rename-ok", if zh { "保存" } else { "Save" }).on_click(
                        cx.listener(move |this, _, _, cx| {
                            let nm = this.modal_name_input.read(cx).value().to_string();
                            this.rename_tree_item(kind, id_c.clone(), nm, cx);
                        }),
                    ),
                ),
            )
        }
        ActiveModal::MoveRequest {
            request_id,
            target_collection_id,
            target_folder_id,
        } => render_move_request(
            view,
            request_id,
            target_collection_id,
            target_folder_id,
            zh,
            cx,
        ),
        ActiveModal::ExtractVariable {
            value,
            is_secret,
            use_globals,
        } => render_extract_var(view, value, is_secret, use_globals, zh, cx),
        ActiveModal::ConfirmCloseDirty { tab_id, name } => {
            let tid_save = tab_id.clone();
            let tid_disc = tab_id.clone();
            (
                if zh {
                    "保存未保存的更改？"
                } else {
                    "Save changes?"
                }
                .into(),
                440.0,
                div()
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(if zh {
                        format!("「{name}」有未保存的更改。不保存就关闭将丢失当前编辑。")
                    } else {
                        format!(
                            "\"{name}\" has unsaved changes. Closing without saving will discard them."
                        )
                    }))
                    .into_any_element(),
                h_row()
                    .gap(px(8.0))
                    .child(cancel_btn(zh, cx))
                    .child(
                        btn_danger("dirty-discard", if zh { "不保存" } else { "Discard" })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.active_modal = None;
                                this.force_close_tab(&tid_disc, window, cx);
                            })),
                    )
                    .child(
                        btn_primary("dirty-save", if zh { "保存并关闭" } else { "Save & close" })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.active_modal = None;
                                this.save_active_tab(false, window, cx);
                                this.force_close_tab(&tid_save, window, cx);
                            })),
                    ),
            )
        }
        ActiveModal::ConfirmGuard {
            tab_id,
            message,
            is_ai,
            is_runner,
            save_to_file,
        } => (
            if zh {
                "发送前安全确认"
            } else {
                "Confirm before sending"
            }
            .into(),
            480.0,
            v_col()
                .gap(px(10.0))
                .child(alert_banner(
                    Tone::Warning,
                    if zh {
                        "受保护环境或凭据安全策略要求发送前显式确认"
                    } else {
                        "Protected environment or credential guard requires explicit confirmation"
                    },
                    None,
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(message)),
                )
                .into_any_element(),
            h_row().gap(px(8.0)).child(cancel_btn(zh, cx)).child(
                btn_danger(
                    "guard-confirm-send",
                    if zh { "确认发送" } else { "Confirm & send" },
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.active_modal = None;
                    if is_runner {
                        this.start_collection_run(tab_id.clone(), Some(true), cx);
                    } else if is_ai {
                        this.send_active_ai_request(Some(true), window, cx);
                    } else {
                        this.send_active_request(Some(true), save_to_file.clone(), window, cx);
                    }
                })),
            ),
        ),
        ActiveModal::StaleWriteConfirm { tab_id: _, name } => (
            if zh {
                "检测到并发修改"
            } else {
                "Concurrent edit detected"
            }
            .into(),
            460.0,
            alert_banner(
                Tone::Warning,
                if zh {
                    format!(
                        "「{name}」在你打开后已被其他来源（如智能体或同步）修改。是否覆盖保存？"
                    )
                } else {
                    format!(
                        "\"{name}\" was modified since you opened it. Overwrite with your draft?"
                    )
                },
                None,
                cx,
            )
            .into_any_element(),
            h_row().gap(px(8.0)).child(cancel_btn(zh, cx)).child(
                btn_danger("stale-overwrite", if zh { "覆盖保存" } else { "Overwrite" }).on_click(
                    cx.listener(|this, _, window, cx| {
                        this.active_modal = None;
                        this.save_active_tab(true, window, cx);
                    }),
                ),
            ),
        ),
        ActiveModal::CompareModels {
            tab_id: _,
            selected,
            concurrency: _,
            estimate,
            error,
        } => render_compare_modal(view, selected, estimate, error, zh, cx),
    };

    let ent = cx.entity().clone();
    Some(modal_dialog(
        "req-modal",
        title,
        width,
        body,
        footer.into_any_element(),
        window,
        cx,
        move |_, _, cx| {
            ent.update(cx, |this, cx| {
                this.active_modal = None;
                cx.notify();
            });
        },
    ))
}

pub(super) fn cancel_btn(zh: bool, cx: &mut Context<RequestsView>) -> impl IntoElement {
    btn_secondary("req-modal-cancel", if zh { "取消" } else { "Cancel" }).on_click(cx.listener(
        |this, _, _, cx| {
            this.active_modal = None;
            cx.notify();
        },
    ))
}
