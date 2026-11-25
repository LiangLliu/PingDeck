//! Rendering body helpers for `ConnectClientSheet` (file-writer diff preview & snippet blocks).

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::{SnippetTone, VerifyOutcome};
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    Tone, UiIcon, badge, btn_ghost, btn_icon, btn_primary, btn_secondary, card, diff_lines_preview,
    h_row, palette, section_heading, to_hsla, v_col,
};

use super::connect_sheet::{ConnectClientSheet, ConnectToolKind};

pub fn render_connect_details(
    sheet: &ConnectClientSheet,
    cx: &mut Context<ConnectClientSheet>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut out = v_col().gap(px(12.0));

    match sheet.tool {
        ConnectToolKind::File(_) => {
            if let Some(plan) = &sheet.plan {
                let mut diffs = v_col().gap(px(10.0)).child(section_heading(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_connect_sheet_body_2_config_diff_preview_auto(),
                    cx,
                ));
                for f in &plan.files {
                    let tag = if f.is_new {
                        badge(
                            crate::ui::tr(cx).providers().connect_file_new(),
                            Tone::Accent,
                            cx,
                        )
                    } else if f.changed {
                        badge(
                            crate::ui::tr(cx)
                                .providers()
                                .prov_connect_sheet_body_modified(),
                            Tone::Warning,
                            cx,
                        )
                    } else {
                        badge(
                            crate::ui::tr(cx)
                                .providers()
                                .prov_connect_sheet_body_unchanged(),
                            Tone::Neutral,
                            cx,
                        )
                    };
                    let preview_txt = if f.diff.trim().is_empty() {
                        &f.after
                    } else {
                        &f.diff
                    };
                    diffs = diffs.child(
                        card(cx).p(px(10.0)).child(
                            v_col()
                                .gap(px(6.0))
                                .child(
                                    h_row()
                                        .justify_between()
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
                                    badge(
                                        crate::ui::tr(cx)
                                            .providers()
                                            .prov_connect_sheet_body_inside_git_repo_do_not(g),
                                        Tone::Warning,
                                        cx,
                                    )
                                }))
                                .child(diff_lines_preview(preview_txt, 28, cx)),
                        ),
                    );
                }
                out = out.child(diffs);
            }

            let mut actions = h_row().gap(px(8.0)).flex_wrap().child(
                btn_primary(
                    "conn-apply-btn",
                    if sheet.busy {
                        crate::ui::tr(cx)
                            .providers()
                            .prov_connect_sheet_body_writing()
                    } else {
                        crate::ui::tr(cx)
                            .providers()
                            .prov_connect_sheet_body_backup_write_config()
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if !this.busy {
                        this.apply_connect(cx);
                    }
                })),
            );
            if sheet.connect_result.is_some() {
                actions = actions
                    .child(
                        btn_secondary(
                            "conn-verify-btn",
                            crate::ui::tr(cx).providers().gwa_gateway_writer_test(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.busy {
                                this.run_verify(cx);
                            }
                        })),
                    )
                    .child(
                        btn_ghost(
                            "conn-restore-btn",
                            crate::ui::tr(cx)
                                .providers()
                                .prov_connect_sheet_body_restore_previous(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.restore_prev(cx))),
                    );
            }
            out = out.child(actions);

            if let Some(vr) = &sheet.verify_result {
                let (tone, txt) = match vr.outcome {
                    VerifyOutcome::Served => (
                        Tone::Success,
                        crate::ui::tr(cx)
                            .providers()
                            .prov_connect_sheet_body_test_passed_ms(
                                vr.target.as_deref().unwrap_or(&vr.alias),
                                vr.ttft_ms.unwrap_or(vr.latency_ms),
                            ),
                    ),
                    other => (
                        Tone::Danger,
                        format!("{other:?}: {}", vr.message.as_deref().unwrap_or("")),
                    ),
                };
                out = out.child(card(cx).p(px(10.0)).child(badge(txt, tone, cx)));
            }
        }
        ConnectToolKind::Snippet(_) => {
            out = out.child(
                h_row().gap(px(8.0)).child(
                    btn_primary(
                        "conn-mint-snip",
                        crate::ui::tr(cx)
                            .providers()
                            .prov_connect_sheet_body_mint_token_fill_snippets(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.mint_snippet_token(cx))),
                ),
            );
            if let Some(set) = &sheet.snippets {
                for note in &set.notes {
                    let tone = match note.tone {
                        SnippetTone::Warn => Tone::Warning,
                        SnippetTone::Info => Tone::Neutral,
                    };
                    out = out.child(
                        card(cx).p(px(9.0)).child(
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
                        card(cx).p(px(10.0)).child(
                            v_col()
                                .gap(px(6.0))
                                .child(
                                    h_row()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
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
