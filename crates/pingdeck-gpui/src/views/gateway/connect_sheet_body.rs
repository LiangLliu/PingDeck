//! Rendering helpers for `ConnectClientSheet`: grouped tool picker, diff/snippet preview, and
//! modal footer.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::app::{OnExisting, SnippetTone, VerifyOutcome};
use pingdeck_desktop::core::client_config::{ClientApp, SnippetClient};
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    Tone, UiIcon, alert_banner, badge, btn_ghost, btn_icon, btn_primary, btn_secondary, card,
    choice_chip, diff_lines_preview, field_row, h_row, icon_xs, palette, settings_sub_well,
    tab_bar_segmented, tab_item, text_input, to_hsla, toggle_switch, v_col,
};

use super::connect_sheet::{ConnectClientSheet, ConnectToolKind};

pub fn render_tool_picker(
    sheet: &ConnectClientSheet,
    cx: &mut Context<ConnectClientSheet>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let lang = crate::ui::active_language(cx);

    let auto_tools = [
        ClientApp::ClaudeCode,
        ClientApp::Codex,
        ClientApp::OpenCode,
        ClientApp::GeminiCli,
    ];
    let manual_tools = [
        SnippetClient::Cursor,
        SnippetClient::Zed,
        SnippetClient::CherryStudio,
        SnippetClient::GenericEnv,
    ];

    let mut auto_row = h_row().gap(px(6.0)).flex_wrap();
    for (i, app) in auto_tools.into_iter().enumerate() {
        let kind = ConnectToolKind::File(app);
        let installed = sheet.installed_apps.contains(&app);
        let lbl = if installed {
            format!("{} ✓", kind.label(lang))
        } else {
            kind.label(lang).to_owned()
        };
        auto_row = auto_row.child(
            choice_chip(
                SharedString::from(format!("conn-auto-{i}")),
                lbl,
                sheet.tool == kind,
                cx,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_tool(kind, window, cx);
            })),
        );
    }

    let mut manual_row = h_row().gap(px(6.0)).flex_wrap();
    for (i, sc) in manual_tools.into_iter().enumerate() {
        let kind = ConnectToolKind::Snippet(sc);
        let installed = sheet.installed_snippets.contains(&sc);
        let lbl = if installed {
            format!("{} ✓", kind.label(lang))
        } else {
            kind.label(lang).to_owned()
        };
        manual_row = manual_row.child(
            choice_chip(
                SharedString::from(format!("conn-man-{i}")),
                lbl,
                sheet.tool == kind,
                cx,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_tool(kind, window, cx);
            })),
        );
    }

    h_row()
        .w_full()
        .gap(px(16.0))
        .items_start()
        .flex_wrap()
        .child(
            v_col()
                .flex_1()
                .min_w(px(260.0))
                .gap(px(6.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.muted))
                        .child(pr.prov_connect_modal_group_auto()),
                )
                .child(auto_row),
        )
        .child(
            v_col()
                .flex_1()
                .min_w(px(260.0))
                .gap(px(6.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.muted))
                        .child(pr.prov_connect_modal_group_manual()),
                )
                .child(manual_row),
        )
}

pub fn render_advanced_section(
    sheet: &ConnectClientSheet,
    cx: &mut Context<ConnectClientSheet>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let ent_ex = cx.entity().clone();
    let ex_idx = match sheet.on_existing {
        OnExisting::Rotate | OnExisting::Fail => 0,
        OnExisting::SecondClient => 1,
        OnExisting::ReplaceRevoked => 2,
    };
    let on_existing_seg = tab_bar_segmented("conn-on-existing")
        .selected_index(ex_idx)
        .child(tab_item(pr.prov_connect_sheet_rotate()))
        .child(tab_item(pr.prov_connect_sheet_add_2nd()))
        .child(tab_item(pr.prov_connect_sheet_replace()))
        .on_click(move |idx, _, cx| {
            let next = match *idx {
                1 => OnExisting::SecondClient,
                2 => OnExisting::ReplaceRevoked,
                _ => OnExisting::Rotate,
            };
            ent_ex.update(cx, |this, cx| {
                this.on_existing = next;
                cx.notify();
            });
        });

    let adv_toggle = h_row()
        .id("conn-adv-toggle")
        .gap(px(5.0))
        .cursor_pointer()
        .on_click(cx.listener(|this, _, _, cx| {
            this.show_advanced = !this.show_advanced;
            cx.notify();
        }))
        .child(icon_xs(if sheet.show_advanced {
            UiIcon::ChevronDown
        } else {
            UiIcon::ChevronRight
        }))
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.muted))
                .child(pr.prov_connect_modal_advanced_options()),
        );

    let adv_panel = sheet.show_advanced.then(|| {
        settings_sub_well(cx)
            .child(field_row(
                pr.connect_name(),
                None,
                div()
                    .w(px(220.0))
                    .child(text_input(&sheet.client_name_input)),
                cx,
            ))
            .children(matches!(sheet.tool, ConnectToolKind::File(_)).then(|| {
                field_row(
                    pr.prov_connect_sheet_if_already_connected(),
                    None,
                    on_existing_seg,
                    cx,
                )
            }))
            .children(matches!(sheet.tool, ConnectToolKind::File(_)).then(|| {
                field_row(
                    pr.prov_connect_sheet_map_unknown_model_names(),
                    Some(SharedString::from(pr.prov_connect_unknown_model_desc())),
                    toggle_switch("conn-unknown-sw", sheet.unknown_to_default).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.unknown_to_default = !this.unknown_to_default;
                            cx.notify();
                        }),
                    ),
                    cx,
                )
            }))
    });

    v_col().gap(px(6.0)).child(adv_toggle).children(adv_panel)
}

pub fn render_connect_details(
    sheet: &ConnectClientSheet,
    cx: &mut Context<ConnectClientSheet>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let mut out = v_col().gap(px(10.0));

    match sheet.tool {
        ConnectToolKind::File(_) => {
            if sheet.connect_result.is_some() {
                out = out.child(alert_banner(
                    Tone::Success,
                    pr.prov_connect_modal_done_banner(),
                    None,
                    cx,
                ));
            }
            if let Some(vr) = &sheet.verify_result {
                let (tone, txt) = match vr.outcome {
                    VerifyOutcome::Served => (
                        Tone::Success,
                        pr.prov_connect_sheet_body_test_passed_ms(
                            vr.target.as_deref().unwrap_or(&vr.alias),
                            vr.ttft_ms.unwrap_or(vr.latency_ms),
                        ),
                    ),
                    other => (
                        Tone::Danger,
                        format!("{other:?}: {}", vr.message.as_deref().unwrap_or("")),
                    ),
                };
                out = out.child(alert_banner(tone, txt, None, cx));
            }

            if let Some(plan) = &sheet.plan {
                let mut diff_wrap = v_col().gap(px(8.0)).child(
                    h_row()
                        .justify_between()
                        .gap(px(8.0))
                        .flex_wrap()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.muted))
                                .child(pr.prov_connect_modal_diff_hint()),
                        )
                        .child(badge(
                            pr.prov_connect_modal_auto_token_badge(),
                            Tone::Success,
                            cx,
                        )),
                );
                for f in &plan.files {
                    let tag = if f.is_new {
                        badge(pr.connect_file_new(), Tone::Accent, cx)
                    } else if f.changed {
                        badge(pr.prov_connect_sheet_body_modified(), Tone::Warning, cx)
                    } else {
                        badge(pr.prov_connect_sheet_body_unchanged(), Tone::Neutral, cx)
                    };
                    let preview_txt = if f.diff.trim().is_empty() {
                        &f.after
                    } else {
                        &f.diff
                    };
                    diff_wrap = diff_wrap.child(
                        card(cx).p(px(10.0)).bg(to_hsla(p.pane)).child(
                            v_col()
                                .gap(px(6.0))
                                .child(
                                    h_row()
                                        .justify_between()
                                        .gap(px(8.0))
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                .text_color(to_hsla(p.ink))
                                                .truncate()
                                                .child(SharedString::from(f.path.clone())),
                                        )
                                        .child(tag),
                                )
                                .children(f.git_work_tree.as_ref().map(|g| {
                                    h_row()
                                        .gap(px(5.0))
                                        .child(
                                            div()
                                                .text_color(to_hsla(p.warn))
                                                .child(icon_xs(UiIcon::Warning)),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(11.0))
                                                .text_color(to_hsla(p.warn))
                                                .truncate()
                                                .child(SharedString::from(
                                                    pr.prov_connect_sheet_body_inside_git_repo_do_not(
                                                        g,
                                                    ),
                                                )),
                                        )
                                }))
                                .child(diff_lines_preview(preview_txt, 48, cx)),
                        ),
                    );
                }
                out = out.child(diff_wrap);
            }
        }
        ConnectToolKind::Snippet(_) => {
            out =
                out.child(
                    h_row()
                        .justify_between()
                        .gap(px(8.0))
                        .flex_wrap()
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(pr.prov_connect_modal_snippet_hint()),
                        )
                        .children(sheet.revealed_token.is_some().then(|| {
                            badge(pr.prov_connect_modal_token_ready(), Tone::Success, cx)
                        })),
                );
            if let Some(set) = &sheet.snippets {
                for note in &set.notes {
                    let tone = match note.tone {
                        SnippetTone::Warn => Tone::Warning,
                        SnippetTone::Info => Tone::Neutral,
                    };
                    out = out.child(
                        card(cx).p(px(9.0)).bg(to_hsla(p.pane)).child(
                            h_row()
                                .gap(px(6.0))
                                .child(badge(
                                    if tone == Tone::Warning { "!" } else { "i" },
                                    tone,
                                    cx,
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_xs()
                                        .text_color(to_hsla(p.ink_2))
                                        .child(SharedString::from(note.text.clone())),
                                ),
                        ),
                    );
                }
                for (i, blk) in set.blocks.iter().enumerate() {
                    let content = match &sheet.revealed_token {
                        Some(tok) => blk.text.replace("YOUR_PINGDECK_TOKEN", tok),
                        None => blk.text.clone(),
                    };
                    let copy_txt = content.clone();
                    out = out.child(
                        card(cx).p(px(10.0)).bg(to_hsla(p.pane)).child(
                            v_col()
                                .gap(px(6.0))
                                .child(
                                    h_row()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                .text_color(to_hsla(p.ink))
                                                .child(SharedString::from(blk.title.clone())),
                                        )
                                        .child(btn_icon(("snip-copy", i), UiIcon::Copy).on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                let _ = clipboard::set_text(&copy_txt);
                                                this.flash = Some((
                                                    Tone::Success,
                                                    crate::ui::tr(cx)
                                                        .providers()
                                                        .prov_connect_sheet_body_copied()
                                                        .into(),
                                                ));
                                                cx.notify();
                                            }),
                                        )),
                                )
                                .child(
                                    div()
                                        .p(px(8.0))
                                        .rounded(px(6.0))
                                        .bg(to_hsla(p.sunken))
                                        .text_xs()
                                        .text_color(to_hsla(p.ink))
                                        .child(SharedString::from(content)),
                                ),
                        ),
                    );
                }
            }
        }
    }

    out
}

pub fn render_modal_footer(
    sheet: &ConnectClientSheet,
    cx: &mut Context<ConnectClientSheet>,
) -> AnyElement {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let cm = crate::ui::tr(cx).common();

    let mut right = h_row().gap(px(8.0));
    match sheet.tool {
        ConnectToolKind::File(_) => {
            if sheet.connect_result.is_some() {
                right = right
                    .child(
                        btn_ghost(
                            "conn-restore-btn",
                            pr.prov_connect_sheet_body_restore_previous(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.restore_prev(cx))),
                    )
                    .child(
                        btn_secondary("conn-verify-btn", pr.gwa_gateway_writer_test()).on_click(
                            cx.listener(|this, _, _, cx| {
                                if !this.busy {
                                    this.run_verify(cx);
                                }
                            }),
                        ),
                    )
                    .child(
                        btn_primary("conn-done-btn", pr.prov_connect_modal_done_btn())
                            .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                    );
            } else {
                right = right
                    .child(
                        btn_ghost("conn-cancel-btn", cm.cancel())
                            .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                    )
                    .child(
                        btn_primary(
                            "conn-apply-btn",
                            if sheet.busy {
                                pr.prov_connect_sheet_body_writing()
                            } else {
                                pr.prov_connect_sheet_body_backup_write_config()
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.busy && !this.available_models.is_empty() {
                                this.apply_connect(cx);
                            }
                        })),
                    );
            }
        }
        ConnectToolKind::Snippet(_) => {
            if sheet.revealed_token.is_some() {
                right = right.child(
                    btn_primary("conn-done-snip", pr.prov_connect_modal_done_btn())
                        .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                );
            } else {
                right = right
                    .child(
                        btn_ghost("conn-cancel-snip", cm.cancel())
                            .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                    )
                    .child(
                        btn_primary(
                            "conn-mint-snip",
                            pr.prov_connect_sheet_body_mint_token_fill_snippets(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.mint_snippet_token(cx))),
                    );
            }
        }
    }

    h_row()
        .w_full()
        .justify_between()
        .gap(px(12.0))
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .truncate()
                .child(pr.prov_connect_modal_foot_verify_hint()),
        )
        .child(right)
        .into_any_element()
}
