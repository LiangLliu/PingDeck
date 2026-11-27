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
                    crate::ui::Tr(view.lang)
                        .environments()
                        .modals_environment_name(),
                    None,
                    Some(SharedString::from(
                        crate::ui::Tr(view.lang)
                            .environments()
                            .modals_e_g_dev_staging_or(),
                    )),
                    div()
                        .w(px(260.0))
                        .child(text_input(&view.new_env_name_input)),
                    cx,
                ))
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-new-cancel", crate::ui::Tr(view.lang).common().cancel())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        })),
                )
                .child(
                    btn_primary(
                        "env-new-confirm",
                        crate::ui::Tr(view.lang).environments().modals_create(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.create_env_from_modal(cx))),
                )
                .into_any_element();
            modal_dialog(
                "env-create-modal",
                crate::ui::Tr(view.lang)
                    .environments()
                    .environments_new_environment(),
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
                .child(
                    crate::ui::Tr(view.lang)
                        .environments()
                        .modals_delete_environment_and_its_secrets(name),
                )
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-del-cancel", crate::ui::Tr(view.lang).common().cancel())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        })),
                )
                .child(
                    btn_danger(
                        "env-del-confirm",
                        crate::ui::Tr(view.lang).environments().modals_delete(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_env(id_go.clone(), name_go.clone(), cx);
                    })),
                )
                .into_any_element();
            modal_dialog(
                "env-delete-modal",
                crate::ui::Tr(view.lang)
                    .environments()
                    .modals_delete_environment(),
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
                        .child(
                            div().text_xs().text_color(to_hsla(p.muted)).child(
                                crate::ui::Tr(view.lang)
                                    .environments()
                                    .modals_paste_postman_environment_json_or(),
                            ),
                        )
                        .child(
                            btn_secondary(
                                "env-imp-file",
                                crate::ui::Tr(view.lang).environments().modals_load_file(),
                            )
                            .icon(icon_xs(UiIcon::Upload))
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.load_import_file(window, cx);
                                },
                            )),
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
                    btn_secondary("env-imp-cancel", crate::ui::Tr(view.lang).common().cancel())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        })),
                )
                .child(
                    btn_primary(
                        "env-imp-go",
                        crate::ui::Tr(view.lang).environments().modals_import(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.import_env_from_editor(cx))),
                )
                .into_any_element();
            modal_dialog(
                "env-import-modal",
                crate::ui::Tr(view.lang)
                    .environments()
                    .modals_import_environment(),
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
                crate::ui::Tr(view.lang)
                    .environments()
                    .modals_include_secret_plaintext_values(),
                None,
                Some(SharedString::from(
                    crate::ui::Tr(view.lang)
                        .environments()
                        .modals_off_by_default_to_prevent(),
                )),
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
                        crate::ui::Tr(view.lang)
                            .environments()
                            .modals_warning_exported_json_will_contain(),
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
                            .child(
                                div().text_xs().text_color(to_hsla(p.ink)).child(
                                    crate::ui::Tr(view.lang)
                                        .environments()
                                        .modals_i_understand_the_exported_file(),
                                ),
                            ),
                    );
            }
            let can_export = !include_secrets || ack_secrets;
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("env-exp-cancel", crate::ui::Tr(view.lang).common().cancel())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        })),
                )
                .child(
                    btn_primary(
                        "env-exp-save",
                        crate::ui::Tr(view.lang).environments().menu_export(),
                    )
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
                crate::ui::Tr(view.lang)
                    .environments()
                    .modals_export_environment(),
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
                    crate::ui::Tr(view.lang).environments().modals_key(),
                    None,
                    None,
                    div().w(px(280.0)).child(text_input(&view.edit_var_key)),
                    cx,
                ))
                .child(setting_row(
                    if is_secret {
                        crate::ui::Tr(view.lang)
                            .environments()
                            .modals_new_secret_blank_keeps_stored()
                    } else {
                        crate::ui::Tr(view.lang).environments().modals_value()
                    },
                    None,
                    None,
                    div().w(px(280.0)).child(text_input(&view.edit_var_val)),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::Tr(view.lang).environments().modals_note(),
                    None,
                    None,
                    div().w(px(280.0)).child(text_input(&view.edit_var_note)),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::Tr(view.lang)
                        .environments()
                        .modals_secret_variable(),
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
                    btn_secondary("env-ed-cancel", crate::ui::Tr(view.lang).common().cancel())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        })),
                )
                .child(
                    btn_primary(
                        "env-ed-apply",
                        crate::ui::Tr(view.lang).environments().modals_apply(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
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
                    })),
                )
                .into_any_element();
            modal_dialog(
                "env-edit-var-modal",
                crate::ui::Tr(view.lang)
                    .environments()
                    .modals_edit_variable(),
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
