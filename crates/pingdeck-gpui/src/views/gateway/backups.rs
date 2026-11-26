//! Managed client config backups ledger & inline client policy editor
//! (extracted from `gateway_clients.rs`).

use gpui_kit::{Context, Div, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::llm::store::GatewayClient;

use crate::ui::{
    PaginationBar, Tone, badge, btn_ghost, btn_primary, btn_secondary, card, choice_chip, h_row,
    page_slice, pagination_bar, palette, section_heading, text_input, to_hsla, toggle_switch,
    v_col,
};

use super::GatewayView;

pub fn render_client_policy_editor(
    view: &GatewayView,
    client: &GatewayClient,
    cx: &mut Context<GatewayView>,
) -> Div {
    let p = palette(cx);
    let cid = client.id.clone();
    let save_cid = cid.clone();
    let cur_allow_all = client.allow_all;
    let cur_unknown = client.unknown_model_to_default;
    let mut box_el = v_col()
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
                        .child(
                            div().text_xs().child(
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_gateway_backups_allow_all_models(),
                            ),
                        )
                        .child(
                            toggle_switch(
                                SharedString::from(format!("cl-allow-all-{cid}")),
                                cur_allow_all,
                            )
                            .on_click(cx.listener({
                                let cid2 = cid.clone();
                                move |this, _, _, cx| {
                                    this.toggle_client_allow_all(cid2.clone(), cx);
                                }
                            })),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            div().text_xs().child(
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_gateway_backups_map_unknown_bare_models_to(),
                            ),
                        )
                        .child(
                            toggle_switch(
                                SharedString::from(format!("cl-unknown-{cid}")),
                                cur_unknown,
                            )
                            .on_click(cx.listener({
                                let cid2 = cid.clone();
                                move |this, _, _, cx| {
                                    this.toggle_client_unknown_default(
                                        cid2.clone(),
                                        !cur_unknown,
                                        cx,
                                    );
                                }
                            })),
                        ),
                ),
        );

    if !cur_allow_all {
        const CLIENT_MODELS_PAGE_SIZE: usize = 40;
        let enabled_models: Vec<_> = view.models.iter().filter(|m| m.model.enabled).collect();
        let (page_models, clamped_page, _) = page_slice(
            &enabled_models,
            view.client_models_page,
            CLIENT_MODELS_PAGE_SIZE,
        );
        let mut chips = h_row().gap(px(4.0)).flex_wrap();
        for (i, mv) in page_models.iter().enumerate() {
            let mref = format!("{}/{}", mv.model.provider_id, mv.model.model_id);
            let disp = format!(
                "{}/{}",
                view.provider_name(&mv.model.provider_id),
                mv.model.model_id
            );
            let active = client.allowed_models.contains(&mref);
            let cid_m = cid.clone();
            chips = chips.child(
                choice_chip(
                    SharedString::from(format!("cl-am-{cid}-{clamped_page}-{i}")),
                    disp,
                    active,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_client_allowed_model(cid_m.clone(), mref.clone(), cx);
                })),
            );
        }
        let mut section = v_col()
            .gap(px(4.0))
            .child(
                div().text_xs().text_color(to_hsla(p.muted)).child(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_gateway_backups_allowed_models_for_this_client(),
                ),
            )
            .child(chips);
        if enabled_models.len() > CLIENT_MODELS_PAGE_SIZE {
            section = section.child(div().pt(px(4.0)).child(pagination_bar(
                PaginationBar {
                    id_prefix: "cl-am-pg",
                    page: clamped_page,
                    page_size: CLIENT_MODELS_PAGE_SIZE,
                    total: enabled_models.len(),
                    page_sizes: &[],
                    loading: false,
                    compact: false,
                },
                |this: &mut GatewayView, next_page, cx| {
                    this.client_models_page = next_page;
                    cx.notify();
                },
                None::<fn(&mut GatewayView, usize, &mut Context<GatewayView>)>,
                cx,
            )));
        }
        box_el = box_el.child(section);
    }

    box_el.child(
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
                    .child(
                        div().text_xs().text_color(to_hsla(p.muted)).child(
                            crate::ui::tr(cx)
                                .providers()
                                .prov_gateway_backups_max_conc(),
                        ),
                    )
                    .child(div().w(px(72.0)).child(text_input(&view.client_conc_input))),
            )
            .child(
                btn_primary(
                    "cl-save-pol",
                    crate::ui::tr(cx).providers().gwa_gateway_limits_save(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.save_client_limits(save_cid.clone(), cx);
                })),
            ),
    )
}

pub fn render_managed_backups(view: &GatewayView, cx: &mut Context<GatewayView>) -> Option<Div> {
    if view.managed_tools.is_empty() {
        return None;
    }
    let p = palette(cx);
    let mut ledger_box = v_col().gap(px(8.0)).child(section_heading(
        crate::ui::tr(cx)
            .providers()
            .prov_gateway_backups_files_modified_by_pingdeck_backups(),
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
                                        crate::ui::tr(cx)
                                            .providers()
                                            .prov_gateway_backups_backups(tool.kept.len()),
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
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_gateway_backups_restore_prev(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.restore_client_tool(app, false, cx);
                                    },
                                )),
                            )
                            .children(tool.has_original.then(|| {
                                btn_ghost(
                                    SharedString::from(format!("led-orig-{}", app.as_str())),
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_gateway_backups_restore_original(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.restore_client_tool(app, true, cx);
                                    },
                                ))
                            }))
                            .children((!tool.kept.is_empty()).then(|| {
                                btn_ghost(
                                    SharedString::from(format!("led-purge-{}", app.as_str())),
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_gateway_backups_purge_backups(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.purge_client_backups(app, cx);
                                    },
                                ))
                            })),
                    ),
            ),
        );
    }
    Some(ledger_box)
}
