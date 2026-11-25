//! Modal dialogs owned by [`SettingsView`]: Reset-All confirmation, Folder Sync secret export
//! confirmation (with Git/Cloud typed-phrase safeguard), Sync Import preview, and storage cleanups.

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};
use pingdeck_desktop::core::app::SyncFolderKind;
use pingdeck_desktop::core::sync::SyncKind;

use crate::ui::{
    Disableable as _, ScrollableElement as _, Tone, alert_banner, badge, btn_danger, btn_primary,
    btn_secondary, data_row, h_row, modal_dialog, palette, text_input, to_hsla, toggle_switch,
    v_col,
};

use super::SettingsView;
use super::state_inputs::{SettingsModal, format_bytes};

pub fn render_settings_modal(
    view: &SettingsView,
    zh: bool,
    window: &Window,
    cx: &mut Context<SettingsView>,
) -> Option<AnyElement> {
    let modal = view.modal.as_ref()?;
    let p = palette(cx);

    let entity = cx.entity().clone();
    let on_close = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        entity.update(cx, |this, cx| {
            this.modal = None;
            cx.notify();
        });
    };

    match modal {
        SettingsModal::ConfirmResetAll => {
            let body = v_col()
                .gap(px(10.0))
                .child(alert_banner(
                    Tone::Warning,
                    if zh {
                        "所有设置分区将恢复为出厂默认值。操作完成后 60 秒内可一键撤销。"
                    } else {
                        "Every settings section will return to factory defaults. You can undo within 60 seconds."
                    },
                    None,
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(if zh {
                            "保留内容：全部工作区、请求集合、对话历史、密钥与 API 令牌、历史/对话/用量保留期限，以及同步文件夹路径绑定均不会被删除。"
                        } else {
                            "Preserved: all workspaces, collections, chats, secrets & keys, retention limits, and your sync folder binding remain untouched."
                        }),
                )
                .into_any_element();

            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("modal-reset-cancel", if zh { "取消" } else { "Cancel" })
                        .on_click(on_close.clone()),
                )
                .child(
                    btn_danger(
                        "modal-reset-confirm",
                        if zh {
                            "确认全部重置"
                        } else {
                            "Reset all settings"
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.modal = None;
                        this.reset_all(cx);
                    })),
                )
                .into_any_element();

            Some(modal_dialog(
                "settings-reset-all-modal",
                if zh {
                    "重置全部设置？"
                } else {
                    "Reset all settings?"
                },
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            ))
        }

        SettingsModal::ConfirmSyncSecrets {
            kind,
            cloud_name,
            directory,
        } => {
            let risky = !matches!(kind, SyncFolderKind::Plain);
            let typed = view
                .inputs
                .sync_secrets_confirm
                .read(cx)
                .value()
                .trim()
                .to_owned();
            let phrase_ok =
                !risky || typed.eq_ignore_ascii_case("INCLUDE SECRETS") || typed == "导出秘密";

            let place_desc = match kind {
                SyncFolderKind::Git => {
                    if zh {
                        "该目录位于 Git 仓库内。若推送到远端仓库，环境变量秘密与服务商密钥将以明文暴露！"
                    } else {
                        "This folder is inside a Git repository. Pushing commits will expose environment secrets and provider API keys in plaintext!"
                    }
                }
                SyncFolderKind::Cloud => {
                    if zh {
                        "该目录位于云同步盘内，自动同步将把明文密钥上传到第三方云服务。"
                    } else {
                        "This folder is inside a cloud-synced drive; cloud sync will upload plaintext secrets."
                    }
                }
                SyncFolderKind::Plain => {
                    if zh {
                        "开启后，导出文件将直接写入明文环境变量秘密与服务商 API 密钥。请妥善保管该目录访问权限。"
                    } else {
                        "Exports will write plaintext environment secrets and provider API keys into files in this folder."
                    }
                }
            };

            let mut body_col = v_col()
                .gap(px(10.0))
                .child(alert_banner(
                    if risky { Tone::Danger } else { Tone::Warning },
                    place_desc,
                    None,
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(match cloud_name {
                            Some(name) if zh => format!("目录：{directory}（{name}）"),
                            Some(name) => format!("Folder: {directory} ({name})"),
                            None if zh => format!("目录：{directory}"),
                            None => format!("Folder: {directory}"),
                        })),
                );

            if risky {
                body_col = body_col
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .child(if zh {
                                "为防止意外泄露，自动导出会阻止向 Git/云盘写入秘密；仅支持手动导出。请输入 INCLUDE SECRETS 或 导出秘密 确认开启："
                            } else {
                                "Auto-export refuses writing secrets to Git/cloud folders; only manual export is allowed. Type INCLUDE SECRETS to confirm:"
                            }),
                    )
                    .child(text_input(&view.inputs.sync_secrets_confirm));
            }

            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("modal-sec-cancel", if zh { "取消" } else { "Cancel" })
                        .on_click(on_close.clone()),
                )
                .child(
                    btn_danger(
                        "modal-sec-confirm",
                        if zh {
                            "确认包含秘密"
                        } else {
                            "Include secrets"
                        },
                    )
                    .disabled(!phrase_ok)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_sync_include_secrets(cx);
                    })),
                )
                .into_any_element();

            Some(modal_dialog(
                "settings-sync-secrets-modal",
                if zh {
                    "导出文件包含明文秘密？"
                } else {
                    "Include plaintext secrets in sync exports?"
                },
                500.0,
                body_col.into_any_element(),
                footer,
                window,
                cx,
                on_close,
            ))
        }

        SettingsModal::SyncImportPreview {
            preview,
            apply_removals,
        } => {
            let mut body_col =
                v_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(if zh {
                                format!("来源目录：{}", preview.directory)
                            } else {
                                format!("Source folder: {}", preview.directory)
                            })),
                    );

            if preview.counts.is_empty() {
                body_col = body_col.child(alert_banner(
                    Tone::Success,
                    if zh {
                        "本地数据与同步目录完全一致，无需导入变更。"
                    } else {
                        "Local data already matches the sync folder. No changes to import."
                    },
                    None,
                    cx,
                ));
            } else {
                let mut rows = v_col().gap(px(4.0)).max_h(px(220.0)).overflow_y_scrollbar();
                for count in &preview.counts {
                    rows = rows.child(
                        data_row(cx)
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .child(sync_kind_label(count.kind, zh)),
                            )
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .children((count.added > 0).then(|| {
                                        badge(format!("+{}", count.added), Tone::Success, cx)
                                    }))
                                    .children((count.changed > 0).then(|| {
                                        badge(format!("~{}", count.changed), Tone::Accent, cx)
                                    }))
                                    .children((count.removed > 0).then(|| {
                                        badge(format!("-{}", count.removed), Tone::Warning, cx)
                                    }))
                                    .children((count.conflicts > 0).then(|| {
                                        badge(
                                            if zh {
                                                format!("{} 冲突保留本地", count.conflicts)
                                            } else {
                                                format!("{} kept local", count.conflicts)
                                            },
                                            Tone::Danger,
                                            cx,
                                        )
                                    })),
                            ),
                    );
                }
                body_col = body_col.child(rows);
            }

            if !preview.warnings.is_empty() {
                body_col = body_col.child(alert_banner(
                    Tone::Warning,
                    preview.warnings.join(" · "),
                    None,
                    cx,
                ));
            }

            let has_removals = preview.counts.iter().any(|c| c.removed > 0);
            if has_removals {
                let checked = *apply_removals;
                body_col =
                    body_col.child(
                        h_row()
                            .justify_between()
                            .gap(px(12.0))
                            .pt(px(4.0))
                            .child(div().flex_1().text_xs().text_color(to_hsla(p.ink_2)).child(
                                if zh {
                                    "同步删除本地多余条目（默认关闭，仅新增与更新）"
                                } else {
                                    "Apply removals locally (off by default; only adds & updates)"
                                },
                            ))
                            .child(toggle_switch("sync-import-removals", checked).on_change(
                                cx.listener(|this, val: &bool, _, cx| {
                                    if let Some(SettingsModal::SyncImportPreview {
                                        apply_removals,
                                        ..
                                    }) = &mut this.modal
                                    {
                                        *apply_removals = *val;
                                        cx.notify();
                                    }
                                }),
                            )),
                    );
            }

            let can_import = !preview.counts.is_empty();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("modal-imp-cancel", if zh { "关闭" } else { "Close" })
                        .on_click(on_close.clone()),
                )
                .child(
                    btn_primary(
                        "modal-imp-confirm",
                        if zh { "执行导入" } else { "Import now" },
                    )
                    .disabled(!can_import || view.sync_busy)
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_sync_import(cx))),
                )
                .into_any_element();

            Some(modal_dialog(
                "settings-sync-import-modal",
                if zh {
                    "预览文件夹导入"
                } else {
                    "Preview sync import"
                },
                540.0,
                body_col.into_any_element(),
                footer,
                window,
                cx,
                on_close,
            ))
        }

        SettingsModal::ConfirmClearAttachments {
            older_than_days,
            report,
        } => {
            let scope = match older_than_days {
                Some(d) if zh => format!("超过 {d} 天的附件"),
                Some(d) => format!("attachments older than {d} days"),
                None if zh => "全部附件文件".to_owned(),
                None => "all stored attachment blobs".to_owned(),
            };
            let summary = if zh {
                format!(
                    "将清理 {} 个附件文件（释放 {}），涉及 {} 个对话。对话文字和历史记录会被保留，仅移除了原文件二进制。",
                    report.removed,
                    format_bytes(report.freed_bytes),
                    report.chats
                )
            } else {
                format!(
                    "This will remove {} attachment blob(s) ({}) referenced by {} chat(s). Messages stay intact; only stored binaries are removed.",
                    report.removed,
                    format_bytes(report.freed_bytes),
                    report.chats
                )
            };

            let body = v_col()
                .gap(px(10.0))
                .child(alert_banner(
                    if report.removed > 0 {
                        Tone::Warning
                    } else {
                        Tone::Neutral
                    },
                    if report.removed > 0 {
                        summary
                    } else if zh {
                        format!("没有可清理的{scope}。")
                    } else {
                        format!("No clearable {scope} found.")
                    },
                    None,
                    cx,
                ))
                .into_any_element();

            let days = *older_than_days;
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("modal-att-cancel", if zh { "取消" } else { "Cancel" })
                        .on_click(on_close.clone()),
                )
                .child(
                    btn_danger(
                        "modal-att-confirm",
                        if zh { "确认清理" } else { "Clear blobs" },
                    )
                    .disabled(report.removed == 0 || view.data_busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_clear_attachments(days, cx);
                    })),
                )
                .into_any_element();

            Some(modal_dialog(
                "settings-clear-att-modal",
                if zh {
                    "清理对话附件"
                } else {
                    "Clear chat attachments"
                },
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            ))
        }

        SettingsModal::ConfirmClearCapturedBodies { count, bytes } => {
            let body = v_col()
                .gap(px(10.0))
                .child(alert_banner(
                    Tone::Warning,
                    if zh {
                        format!(
                            "将清空 {count} 条网关捕获的请求/响应正文（约 {}）。日志元数据行和用量统计会保留；随后可点击「压缩数据库」回收磁盘空间。",
                            format_bytes(*bytes)
                        )
                    } else {
                        format!(
                            "Clears {count} captured gateway body record(s) ({}). Log metadata and usage rollups stay intact; run Compact database afterwards to reclaim disk space.",
                            format_bytes(*bytes)
                        )
                    },
                    None,
                    cx,
                ))
                .into_any_element();

            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("modal-cap-cancel", if zh { "取消" } else { "Cancel" })
                        .on_click(on_close.clone()),
                )
                .child(
                    btn_danger(
                        "modal-cap-confirm",
                        if zh { "确认清空" } else { "Clear bodies" },
                    )
                    .disabled(view.data_busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_clear_captured_bodies(cx);
                    })),
                )
                .into_any_element();

            Some(modal_dialog(
                "settings-clear-cap-modal",
                if zh {
                    "清空捕获的网关正文？"
                } else {
                    "Clear captured gateway bodies?"
                },
                460.0,
                body,
                footer,
                window,
                cx,
                on_close,
            ))
        }
    }
}

fn sync_kind_label(kind: SyncKind, zh: bool) -> &'static str {
    match (kind, zh) {
        (SyncKind::Workspace, true) => "工作区",
        (SyncKind::Workspace, false) => "Workspaces",
        (SyncKind::Collection, true) => "请求集合",
        (SyncKind::Collection, false) => "Collections",
        (SyncKind::LooseRequests, true) => "独立请求",
        (SyncKind::LooseRequests, false) => "Loose requests",
        (SyncKind::Environment, true) => "环境与变量",
        (SyncKind::Environment, false) => "Environments",
        (SyncKind::Globals, true) => "全局变量",
        (SyncKind::Globals, false) => "Global variables",
        (SyncKind::Mock, true) => "Mock 服务",
        (SyncKind::Mock, false) => "Mock servers",
        (SyncKind::Flow, true) => "流程",
        (SyncKind::Flow, false) => "Flows",
        (SyncKind::LlmProvider, true) => "AI 服务商",
        (SyncKind::LlmProvider, false) => "AI providers",
        (SyncKind::LlmAlias, true) => "模型别名",
        (SyncKind::LlmAlias, false) => "Model aliases",
        (SyncKind::Settings, true) => "设置",
        (SyncKind::Settings, false) => "Settings",
        (SyncKind::Skills, true) => "技能状态",
        (SyncKind::Skills, false) => "Skills",
        (SyncKind::McpServer, true) => "MCP 服务器",
        (SyncKind::McpServer, false) => "MCP servers",
    }
}
