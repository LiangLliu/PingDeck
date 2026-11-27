//! Modal dialogs for `MockView`: Delete confirmation, Import (SpanBay / Mockoon / OpenAPI), and Export options.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};

use crate::ui::{
    Tone, UiIcon, alert_banner, btn_danger, btn_primary, btn_secondary, choice_chip, code_editor,
    h_row, icon_xs, modal_dialog, number_field, palette, setting_row, text_input, to_hsla,
    toggle_switch, v_col,
};

use super::state::{MockImportFormat, MockModal, MockView};

pub fn render_modal(
    view: &MockView,
    window: &Window,
    cx: &mut Context<MockView>,
) -> Option<AnyElement> {
    let modal = view.active_modal.clone()?;
    let entity = cx.entity().clone();
    let on_close = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        entity.update(cx, |this, cx| {
            this.active_modal = None;
            cx.notify();
        });
    };

    let el = match modal {
        MockModal::CreateServer => {
            let body = v_col()
                .w_full()
                .gap(px(10.0))
                .child(setting_row(
                    crate::ui::Tr(view.lang).mock().modals_server_name(),
                    None,
                    None,
                    div()
                        .w(px(260.0))
                        .child(text_input(&view.new_srv_name_input)),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::Tr(view.lang).mock().modals_listen_port_127_0_0(),
                    None,
                    Some(SharedString::from(
                        crate::ui::Tr(view.lang)
                            .mock()
                            .modals_starts_with_a_default_get(),
                    )),
                    number_field(&view.new_srv_port_input, None, 110.0, cx),
                    cx,
                ))
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary(
                        "mock-new-cancel",
                        crate::ui::Tr(view.lang).common().cancel(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = None;
                        cx.notify();
                    })),
                )
                .child(
                    btn_primary(
                        "mock-new-confirm",
                        crate::ui::Tr(view.lang).mock().modals_create_server(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.create_server_from_modal(cx))),
                )
                .into_any_element();
            modal_dialog(
                "mock-create-modal",
                crate::ui::Tr(view.lang).mock().mock_new_mock_server(),
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        MockModal::DeleteServer { id, name } => {
            let p = palette(cx);
            let body = v_col()
                .gap(px(10.0))
                .child(
                    div().text_sm().text_color(to_hsla(p.ink)).child(
                        crate::ui::Tr(view.lang)
                            .mock()
                            .modals_permanently_delete_mock_server_and(name),
                    ),
                )
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary(
                        "mock-del-cancel",
                        crate::ui::Tr(view.lang).common().cancel(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = None;
                        cx.notify();
                    })),
                )
                .child(
                    btn_danger(
                        "mock-del-confirm",
                        crate::ui::Tr(view.lang).mock().modals_delete_server(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_server(id.clone(), cx);
                    })),
                )
                .into_any_element();
            modal_dialog(
                "mock-delete-modal",
                crate::ui::Tr(view.lang).mock().modals_delete_mock_server(),
                440.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        MockModal::Import { format } => {
            let fmt_row = h_row()
                .gap(px(6.0))
                .child(
                    choice_chip(
                        "mock-imp-pd",
                        "SpanBay JSON",
                        format == MockImportFormat::SpanBayJson,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = Some(MockModal::Import {
                            format: MockImportFormat::SpanBayJson,
                        });
                        cx.notify();
                    })),
                )
                .child(
                    choice_chip(
                        "mock-imp-mk",
                        "Mockoon JSON",
                        format == MockImportFormat::Mockoon,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = Some(MockModal::Import {
                            format: MockImportFormat::Mockoon,
                        });
                        cx.notify();
                    })),
                )
                .child(
                    choice_chip(
                        "mock-imp-oa",
                        "OpenAPI 3 / Swagger",
                        format == MockImportFormat::OpenApi,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = Some(MockModal::Import {
                            format: MockImportFormat::OpenApi,
                        });
                        cx.notify();
                    })),
                )
                .child(div().flex_1())
                .child(
                    btn_secondary(
                        "mock-imp-file",
                        crate::ui::Tr(view.lang).mock().modals_load_file(),
                    )
                    .icon(icon_xs(UiIcon::Upload))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.load_import_from_file(window, cx);
                    })),
                );
            let body = v_col()
                .w_full()
                .gap(px(10.0))
                .child(fmt_row)
                .child(alert_banner(
                    Tone::Neutral,
                    crate::ui::Tr(view.lang)
                        .mock()
                        .modals_imported_servers_start_stopped_with(),
                    None,
                    cx,
                ))
                .child(
                    div()
                        .w_full()
                        .h(px(240.0))
                        .child(code_editor(&view.import_editor)),
                )
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary(
                        "mock-imp-cancel",
                        crate::ui::Tr(view.lang).common().cancel(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = None;
                        cx.notify();
                    })),
                )
                .child(
                    btn_primary(
                        "mock-imp-submit",
                        crate::ui::Tr(view.lang).mock().modals_import(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_import(format, cx);
                    })),
                )
                .into_any_element();
            modal_dialog(
                "mock-import-modal",
                crate::ui::Tr(view.lang).mock().modals_import_mock_server(),
                620.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
        MockModal::ExportOptions {
            include_credentials,
        } => {
            let body = v_col()
                .w_full()
                .gap(px(10.0))
                .child(setting_row(
                    crate::ui::Tr(view.lang)
                        .mock()
                        .modals_include_literal_credential_headers(),
                    None,
                    Some(SharedString::from(
                        crate::ui::Tr(view.lang)
                            .mock()
                            .modals_when_off_sensitive_proxy_and(),
                    )),
                    toggle_switch("mock-exp-creds", include_credentials).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.active_modal = Some(MockModal::ExportOptions {
                                include_credentials: !include_credentials,
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
                    btn_secondary(
                        "mock-exp-cancel",
                        crate::ui::Tr(view.lang).common().cancel(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = None;
                        cx.notify();
                    })),
                )
                .child(
                    btn_primary(
                        "mock-exp-confirm",
                        crate::ui::Tr(view.lang).mock().modals_save_as(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.export_selected(include_credentials, cx);
                    })),
                )
                .into_any_element();
            modal_dialog(
                "mock-export-modal",
                crate::ui::Tr(view.lang).mock().modals_export_mock_server(),
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            )
        }
    };
    Some(el)
}
