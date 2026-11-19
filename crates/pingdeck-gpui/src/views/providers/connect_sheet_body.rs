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
    let zh = sheet.zh;
    let mut out = v_col().gap(px(12.0));

    match sheet.tool {
        ConnectToolKind::File(_) => {
            if let Some(plan) = &sheet.plan {
                let mut diffs = v_col().gap(px(10.0)).child(section_heading(
                    if zh {
                        "2. 写入前差异预览（自动创建备份，随时可还原）"
                    } else {
                        "2. Config diff preview (auto-backed up)"
                    },
                    cx,
                ));
                for f in &plan.files {
                    let tag = if f.is_new {
                        badge(if zh { "新文件" } else { "New file" }, Tone::Accent, cx)
                    } else if f.changed {
                        badge(if zh { "将修改" } else { "Modified" }, Tone::Warning, cx)
                    } else {
                        badge(if zh { "无更改" } else { "Unchanged" }, Tone::Neutral, cx)
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
                                        if zh {
                                            format!("位于 Git 仓库 {g}，请勿提交令牌")
                                        } else {
                                            format!("Inside Git repo {g} — do not commit tokens")
                                        },
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
                        if zh { "正在写入…" } else { "Writing…" }
                    } else if zh {
                        "备份并写入配置"
                    } else {
                        "Backup & Write Config"
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
                            if zh {
                                "发送测试请求"
                            } else {
                                "Send test request"
                            },
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
                            if zh {
                                "恢复之前配置"
                            } else {
                                "Restore previous"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.restore_prev(cx))),
                    );
            }
            out = out.child(actions);

            if let Some(vr) = &sheet.verify_result {
                let (tone, txt) = match vr.outcome {
                    VerifyOutcome::Served => (
                        Tone::Success,
                        if zh {
                            format!(
                                "测试请求通过 · 目标 {} · 耗时 {} ms",
                                vr.target.as_deref().unwrap_or(&vr.alias),
                                vr.ttft_ms.unwrap_or(vr.latency_ms)
                            )
                        } else {
                            format!(
                                "Test passed · {} · {} ms",
                                vr.target.as_deref().unwrap_or(&vr.alias),
                                vr.ttft_ms.unwrap_or(vr.latency_ms)
                            )
                        },
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
                        if zh {
                            "创建客户端令牌并填入代码片段"
                        } else {
                            "Mint token & fill snippets"
                        },
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
                                                    if zh {
                                                        "已复制到剪贴板".into()
                                                    } else {
                                                        "Copied".into()
                                                    },
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
