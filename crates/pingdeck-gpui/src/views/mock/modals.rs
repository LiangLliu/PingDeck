//! Modal dialogs for `MockView`: Delete confirmation, Import (PingDeck / Mockoon / OpenAPI), and Export options.

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
                    view.t("服务名称", "Server Name"),
                    None,
                    None,
                    div()
                        .w(px(260.0))
                        .child(text_input(&view.new_srv_name_input)),
                    cx,
                ))
                .child(setting_row(
                    view.t(
                        "监听端口（仅绑定 127.0.0.1）",
                        "Listen Port (127.0.0.1 only)",
                    ),
                    None,
                    Some(SharedString::from(view.t(
                        "创建后默认包含一条 GET 路由，可立即启动或热编辑。",
                        "Starts with a default GET route ready to launch or hot-edit.",
                    ))),
                    number_field(&view.new_srv_port_input, None, 110.0, cx),
                    cx,
                ))
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("mock-new-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary("mock-new-confirm", view.t("创建服务", "Create Server"))
                        .on_click(cx.listener(|this, _, _, cx| this.create_server_from_modal(cx))),
                )
                .into_any_element();
            modal_dialog(
                "mock-create-modal",
                view.t("新建 Mock 服务", "New Mock Server"),
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
                    div()
                        .text_sm()
                        .text_color(to_hsla(p.ink))
                        .child(if view.is_zh() {
                            format!("确定要永久删除 Mock 服务「{name}」及其所有路由、数据桶与回调吗？")
                        } else {
                            format!("Permanently delete mock server \"{name}\" and all its routes, buckets, and callbacks?")
                        }),
                )
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("mock-del-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_danger("mock-del-confirm", view.t("删除服务", "Delete Server")).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.delete_server(id.clone(), cx);
                        }),
                    ),
                )
                .into_any_element();
            modal_dialog(
                "mock-delete-modal",
                view.t("删除 Mock 服务", "Delete Mock Server"),
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
                        "PingDeck JSON",
                        format == MockImportFormat::PingDeckJson,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = Some(MockModal::Import {
                            format: MockImportFormat::PingDeckJson,
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
                    btn_secondary("mock-imp-file", view.t("从文件读取…", "Load File…"))
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
                    view.t(
                        "导入的 Mock 服务默认处于停止状态且自动关闭代理回源，防止意外发起外联。",
                        "Imported servers start stopped with proxying disabled for safety.",
                    ),
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
                    btn_secondary("mock-imp-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary("mock-imp-submit", view.t("导入服务", "Import")).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.run_import(format, cx);
                        }),
                    ),
                )
                .into_any_element();
            modal_dialog(
                "mock-import-modal",
                view.t("导入 Mock 服务", "Import Mock Server"),
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
                    view.t("包含代理和回调中的明文凭据头", "Include literal credential headers"),
                    None,
                    Some(SharedString::from(view.t(
                        "关闭时会自动清空 Authorization、X-Api-Key 等敏感请求头的值。",
                        "When off, sensitive proxy and callback header values are cleared automatically.",
                    ))),
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
                    btn_secondary("mock-exp-cancel", view.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary("mock-exp-confirm", view.t("选择保存路径…", "Save As…")).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.export_selected(include_credentials, cx);
                        }),
                    ),
                )
                .into_any_element();
            modal_dialog(
                "mock-export-modal",
                view.t("导出 Mock 服务", "Export Mock Server"),
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
