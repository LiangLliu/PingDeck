//! Managed client config backups ledger & inline client policy editor
//! (extracted from `gateway_clients.rs`).

use gpui_kit::{Context, Div, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::llm::store::GatewayClient;

use crate::ui::{
    Tone, badge, btn_ghost, btn_primary, btn_secondary, card, h_row, palette, section_heading,
    text_input, to_hsla, toggle_switch, v_col,
};

use super::ProvidersView;

pub fn render_client_policy_editor(
    view: &ProvidersView,
    client: &GatewayClient,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> Div {
    let p = palette(cx);
    let cid = client.id.clone();
    let save_cid = cid.clone();
    let cur_allow_all = client.allow_all;
    let cur_unknown = client.unknown_model_to_default;
    v_col()
        .gap(px(8.0))
        .p(px(10.0))
        .rounded(px(7.0))
        .bg(to_hsla(p.pane))
        .child(
            h_row()
                .gap(px(16.0))
                .flex_wrap()
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(div().text_xs().child(if zh {
                            "允许全部模型"
                        } else {
                            "Allow all models"
                        }))
                        .child(
                            toggle_switch(
                                SharedString::from(format!("cl-allow-all-{cid}")),
                                cur_allow_all,
                            )
                            .on_click(cx.listener({
                                let cid2 = cid.clone();
                                move |this, _, _, cx| {
                                    this.toggle_client_allow_all(cid2.clone(), zh, cx);
                                }
                            })),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(div().text_xs().child(if zh {
                            "未知裸模型名回退到默认模型"
                        } else {
                            "Map unknown bare models to default"
                        }))
                        .child(
                            toggle_switch(
                                SharedString::from(format!("cl-unknown-{cid}")),
                                cur_unknown,
                            )
                            .on_click(cx.listener({
                                let cid2 = cid;
                                move |this, _, _, cx| {
                                    this.toggle_client_unknown_default(
                                        cid2.clone(),
                                        !cur_unknown,
                                        zh,
                                        cx,
                                    );
                                }
                            })),
                        ),
                ),
        )
        .child(
            h_row()
                .gap(px(8.0))
                .flex_wrap()
                .child(
                    h_row()
                        .gap(px(4.0))
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child("RPM"))
                        .child(div().w(px(80.0)).child(text_input(&view.client_rpm_input))),
                )
                .child(
                    h_row()
                        .gap(px(4.0))
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                            "最大并发"
                        } else {
                            "Max conc"
                        }))
                        .child(div().w(px(72.0)).child(text_input(&view.client_conc_input))),
                )
                .child(
                    btn_primary("cl-save-pol", if zh { "保存限额" } else { "Save limits" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_client_limits(save_cid.clone(), zh, cx);
                        })),
                ),
        )
}

pub fn render_managed_backups(
    view: &ProvidersView,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> Option<Div> {
    if view.managed_tools.is_empty() {
        return None;
    }
    let p = palette(cx);
    let mut ledger_box = v_col().gap(px(8.0)).child(section_heading(
        if zh {
            "PingDeck 改过的文件与备份"
        } else {
            "Files Modified by PingDeck (Backups)"
        },
        cx,
    ));
    for tool in &view.managed_tools {
        let app = tool.app;
        let file_list: Vec<_> = tool.files.iter().map(|f| f.path.as_str()).collect();
        ledger_box = ledger_box.child(
            card(cx).p(px(11.0)).child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .flex_wrap()
                    .child(
                        v_col()
                            .gap(px(2.0))
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .child(app.display_name()),
                                    )
                                    .child(badge(
                                        if zh {
                                            format!("{} 份备份", tool.kept.len())
                                        } else {
                                            format!("{} backups", tool.kept.len())
                                        },
                                        Tone::Neutral,
                                        cx,
                                    )),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(SharedString::from(file_list.join(", "))),
                            ),
                    )
                    .child(
                        h_row()
                            .gap(px(5.0))
                            .child(
                                btn_secondary(
                                    SharedString::from(format!("led-prev-{}", app.as_str())),
                                    if zh {
                                        "恢复上一次"
                                    } else {
                                        "Restore prev"
                                    },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.restore_client_tool(app, false, zh, cx);
                                    },
                                )),
                            )
                            .children(tool.has_original.then(|| {
                                btn_ghost(
                                    SharedString::from(format!("led-orig-{}", app.as_str())),
                                    if zh {
                                        "恢复原配置"
                                    } else {
                                        "Restore original"
                                    },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.restore_client_tool(app, true, zh, cx);
                                    },
                                ))
                            }))
                            .children((!tool.kept.is_empty()).then(|| {
                                btn_ghost(
                                    SharedString::from(format!("led-purge-{}", app.as_str())),
                                    if zh { "清理副本" } else { "Purge backups" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.purge_client_backups(app, zh, cx);
                                    },
                                ))
                            })),
                    ),
            ),
        );
    }
    Some(ledger_box)
}
