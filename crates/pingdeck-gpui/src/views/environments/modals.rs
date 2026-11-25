//! Modal dialogs for `EnvironmentsView`: Create/Delete Env, Import/Export Env, Edit Variable, and resource modals.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};

use crate::ui::{
    Disableable as _, Tone, UiIcon, alert_banner, btn_danger, btn_primary, btn_secondary, checkbox,
    code_editor, h_row, icon_xs, modal_dialog, palette, setting_row, text_input, to_hsla,
    toggle_switch, v_col,
};

use super::modals_resources::{
    render_add_cert_modal, render_add_cookie_modal, render_delete_cert_modal,
};
use super::state::{EnvModal, EnvironmentsView};

pub fn render_modal(
    view: &EnvironmentsView,
    window: &Window,
    cx: &mut Context<EnvironmentsView>,
) -> Option<AnyElement> {
    let modal = view.active_modal.clone()?;
    let p = palette(cx);
    let entity = cx.entity().clone();
    let on_close = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        entity.update(cx, |this, cx| {
            this.active_modal = None;
            cx.notify();
        });
    };

    let el = match modal {
        EnvModal::CreateEnv => {
            let body = v_col()
                .w_full()
                .gap(px(10.0))
                .child(setting_row(
                    view.t("环境名称", "Environment Name"),
                    None,
                    Some(SharedString::from(view.t(
                        "例如 dev / staging / production，可在保存后配置变量与安全护栏。",
                        "e.g. dev, staging, or production; configure variables & guards after creating.",
                    ))),
                    div().w(px(260.0)).child(text_input(&view.new_env_name_input)),
                    cx,
                ))
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-new-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary("env-new-confirm", view.t("创建环境", "Create"))
                        .on_click(cx.listener(|this, _, _, cx| this.create_env_from_modal(cx))),
                )
                .into_any_element();
            modal_dialog(
                "env-create-modal",
                view.t("新建环境", "New Environment"),
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        EnvModal::DeleteEnv { id, name } => {
            let id_go = id.clone();
            let name_go = name.clone();
            let body = div()
                .text_sm()
                .text_color(to_hsla(p.ink))
                .child(if view.is_zh() {
                    format!("确定要删除环境「{name}」及其所有变量与机密吗？删除后 60 秒内支持撤销恢复。")
                } else {
                    format!("Delete environment \"{name}\" and its secrets? Undo remains available for 60 seconds.")
                })
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-del-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_danger("env-del-confirm", view.t("删除环境", "Delete")).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.delete_env(id_go.clone(), name_go.clone(), cx);
                        }),
                    ),
                )
                .into_any_element();
            modal_dialog(
                "env-delete-modal",
                view.t("删除环境", "Delete Environment"),
                430.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        EnvModal::ImportEnv => {
            let body = v_col()
                .w_full()
                .gap(px(10.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(view.t(
                            "粘贴 Postman 环境 JSON，或从磁盘载入文件。",
                            "Paste Postman environment JSON or load a file.",
                        )))
                        .child(
                            btn_secondary("env-imp-file", view.t("选择文件…", "Load File…"))
                                .icon(icon_xs(UiIcon::Upload))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.load_import_file(window, cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(220.0))
                        .child(code_editor(&view.import_editor)),
                )
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-imp-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary("env-imp-go", view.t("导入环境", "Import"))
                        .on_click(cx.listener(|this, _, _, cx| this.import_env_from_editor(cx))),
                )
                .into_any_element();
            modal_dialog(
                "env-import-modal",
                view.t("导入环境", "Import Environment"),
                580.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        EnvModal::ExportEnv {
            id,
            name,
            include_secrets,
            ack_secrets,
        } => {
            let id_t = id.clone();
            let name_t = name.clone();
            let id_ack = id.clone();
            let name_ack = name.clone();
            let mut body_col = v_col().w_full().gap(px(10.0)).child(setting_row(
                view.t(
                    "在导出文件中包含机密明文",
                    "Include secret plaintext values",
                ),
                None,
                Some(SharedString::from(view.t(
                    "默认关闭以防止凭据外泄，机密键名会以空值保留。",
                    "Off by default to prevent secret leaks; secret keys are exported empty.",
                ))),
                toggle_switch("env-exp-sec", include_secrets).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.active_modal = Some(EnvModal::ExportEnv {
                            id: id_t.clone(),
                            name: name_t.clone(),
                            include_secrets: !include_secrets,
                            ack_secrets: false,
                        });
                        cx.notify();
                    },
                )),
                cx,
            ));
            if include_secrets {
                body_col = body_col
                    .child(alert_banner(
                        Tone::Danger,
                        view.t(
                            "警告：导出的 JSON 将以未加密明文包含本环境的机密值，切勿提交到版本库或公开分享。",
                            "Warning: Exported JSON will contain plaintext secret values. Never commit or share publicly.",
                        ),
                        None,
                        cx,
                    ))
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .child(checkbox("env-exp-ack", ack_secrets).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.active_modal = Some(EnvModal::ExportEnv {
                                        id: id_ack.clone(),
                                        name: name_ack.clone(),
                                        include_secrets: true,
                                        ack_secrets: !ack_secrets,
                                    });
                                    cx.notify();
                                },
                            )))
                            .child(div().text_xs().text_color(to_hsla(p.ink)).child(view.t(
                                "我已知晓导出文件含明文凭据并自行妥善保管",
                                "I understand the exported file contains plaintext secrets",
                            ))),
                    );
            }
            let can_export = !include_secrets || ack_secrets;
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-exp-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary("env-exp-save", view.t("导出 JSON…", "Export JSON…"))
                        .disabled(!can_export)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !can_export {
                                return;
                            }
                            this.export_env(id.clone(), name.clone(), include_secrets, cx);
                        })),
                )
                .into_any_element();
            modal_dialog(
                "env-export-modal",
                view.t("导出环境", "Export Environment"),
                480.0,
                body_col.into_any_element(),
                footer,
                window,
                cx,
                on_close,
            )
        }
        EnvModal::EditVar {
            is_global,
            index,
            is_secret,
        } => {
            let body = v_col()
                .w_full()
                .gap(px(10.0))
                .child(setting_row(
                    view.t("变量名 Key", "Key"),
                    None,
                    None,
                    div().w(px(280.0)).child(text_input(&view.edit_var_key)),
                    cx,
                ))
                .child(setting_row(
                    if is_secret {
                        view.t(
                            "新机密值（留空保留原值）",
                            "New Secret (blank keeps stored)",
                        )
                    } else {
                        view.t("变量值 Value", "Value")
                    },
                    None,
                    None,
                    div().w(px(280.0)).child(text_input(&view.edit_var_val)),
                    cx,
                ))
                .child(setting_row(
                    view.t("备注 Note", "Note"),
                    None,
                    None,
                    div().w(px(280.0)).child(text_input(&view.edit_var_note)),
                    cx,
                ))
                .child(setting_row(
                    view.t("设为机密变量", "Secret Variable"),
                    None,
                    None,
                    toggle_switch("env-ed-sec", is_secret).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.active_modal = Some(EnvModal::EditVar {
                                is_global,
                                index,
                                is_secret: !is_secret,
                            });
                            cx.notify();
                        },
                    )),
                    cx,
                ))
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-ed-cancel", view.t("取消", "Cancel")).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        },
                    )),
                )
                .child(
                    btn_primary("env-ed-apply", view.t("应用更改", "Apply")).on_click(cx.listener(
                        move |this, _, _, cx| {
                            let key = this.edit_var_key.read(cx).value().trim().to_owned();
                            let val = this.edit_var_val.read(cx).value().to_string();
                            let note = this.edit_var_note.read(cx).value().trim().to_owned();
                            if key.is_empty() {
                                return;
                            }
                            let list = if is_global {
                                &mut this.globals_draft
                            } else {
                                &mut this.env_draft_vars
                            };
                            if let Some(row) = list.get_mut(index) {
                                row.key = key;
                                row.note = note;
                                row.is_secret = is_secret;
                                if is_secret {
                                    if !val.is_empty() {
                                        row.value = Some(val);
                                        row.has_secret = true;
                                    }
                                } else {
                                    row.value = Some(val);
                                    row.has_secret = false;
                                    row.secret_hint = None;
                                }
                                if is_global {
                                    this.globals_dirty = true;
                                } else {
                                    this.env_dirty = true;
                                }
                            }
                            this.active_modal = None;
                            cx.notify();
                        },
                    )),
                )
                .into_any_element();
            modal_dialog(
                "env-edit-var-modal",
                view.t("编辑变量", "Edit Variable"),
                500.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        EnvModal::AddCookie { secure, http_only } => {
            render_add_cookie_modal(view, secure, http_only, window, cx, on_close)
        }
        EnvModal::AddCert { kind } => render_add_cert_modal(view, kind, window, cx, on_close),
        EnvModal::DeleteCert { id, label } => {
            render_delete_cert_modal(view, id, label, window, cx, on_close)
        }
    };
    Some(el)
}
