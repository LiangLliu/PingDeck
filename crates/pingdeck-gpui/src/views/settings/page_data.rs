//! Settings › Data & Sync page: live storage usage KPIs, Reveal/Backup/Compact/Clear maintenance,
//! history & chat retention with preview/apply, and local-first folder sync (Git/Cloud risk guard,
//! secret scrub, export, preview & import).

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::SyncFolderKind;
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;

use crate::ui::{
    Disableable as _, Tone, UiIcon, alert_banner, badge, btn_danger, btn_ghost, btn_primary,
    btn_secondary, card, choice_chip, h_row, icon_xs, kpi_card, number_field, palette,
    section_header, setting_row, spinner_xs, to_hsla, toggle_switch, v_col,
};

use super::SettingsView;
use super::state_inputs::format_bytes;

pub fn render_data(view: &SettingsView, zh: bool, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    v_col()
        .w_full()
        .gap(px(22.0))
        .child(render_storage_section(view, zh, cx))
        .child(render_retention_section(view, zh, cx))
        .child(render_sync_section(view, zh, cx))
}

fn render_storage_section(
    view: &SettingsView,
    zh: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let usage = view.storage_usage.unwrap_or_default();
    let has_usage = view.storage_usage.is_some();

    let db_val = if has_usage {
        format_bytes(usage.database_bytes)
    } else {
        "—".to_owned()
    };
    let db_sub = if has_usage && usage.database_free_bytes > 0 {
        Some(SharedString::from(if zh {
            format!("可压缩回收 {}", format_bytes(usage.database_free_bytes))
        } else {
            format!("{} reclaimable", format_bytes(usage.database_free_bytes))
        }))
    } else {
        Some(SharedString::from("SQLite WAL"))
    };

    let chats_val = if has_usage {
        format_bytes(usage.chats_bytes)
    } else {
        "—".to_owned()
    };
    let chats_sub = has_usage.then(|| {
        SharedString::from(if zh {
            format!("{} 个对话", usage.chats_count)
        } else {
            format!("{} chats", usage.chats_count)
        })
    });

    let att_val = if has_usage {
        format_bytes(usage.attachments_bytes)
    } else {
        "—".to_owned()
    };
    let att_sub = has_usage.then(|| {
        SharedString::from(if zh {
            format!("{} 个附件", usage.attachments_count)
        } else {
            format!("{} files", usage.attachments_count)
        })
    });

    let cap_val = if has_usage {
        format_bytes(usage.captured_bodies_bytes + usage.logs_bytes)
    } else {
        "—".to_owned()
    };
    let cap_sub = has_usage.then(|| {
        SharedString::from(if zh {
            format!(
                "捕获 {} 条 · 日志 {}",
                usage.captured_bodies_count,
                format_bytes(usage.logs_bytes)
            )
        } else {
            format!(
                "{} captures · {} logs",
                usage.captured_bodies_count,
                format_bytes(usage.logs_bytes)
            )
        })
    });

    let mut sec = v_col()
        .w_full()
        .gap(px(12.0))
        .child(section_header(
            if zh {
                "本地存储与维护"
            } else {
                "Local Storage & Maintenance"
            },
            Some(
                h_row()
                    .gap(px(6.0))
                    .children(view.storage_loading.then(spinner_xs))
                    .child(
                        btn_ghost("data-refresh", if zh { "刷新" } else { "Refresh" })
                            .icon(icon_xs(UiIcon::Refresh))
                            .disabled(view.storage_loading)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_data_and_sync(cx);
                            })),
                    )
                    .into_any_element(),
            ),
            cx,
        ))
        .child(
            h_row()
                .w_full()
                .gap(px(10.0))
                .child(kpi_card(
                    if zh { "主数据库" } else { "Database" },
                    db_val,
                    db_sub,
                    cx,
                ))
                .child(kpi_card(
                    if zh {
                        "对话与输出"
                    } else {
                        "Chats & Output"
                    },
                    chats_val,
                    chats_sub,
                    cx,
                ))
                .child(kpi_card(
                    if zh { "对话附件" } else { "Attachments" },
                    att_val,
                    att_sub,
                    cx,
                ))
                .child(kpi_card(
                    if zh {
                        "捕获正文与日志"
                    } else {
                        "Captures & Logs"
                    },
                    cap_val,
                    cap_sub,
                    cx,
                )),
        );

    let data_dir_str = view.data_dir_path.clone().unwrap_or_else(|| {
        if zh {
            "内存模式（无磁盘目录）".to_owned()
        } else {
            "In-memory mode (no disk directory)".to_owned()
        }
    });

    sec = sec
        .child(
            card(cx).p(px(12.0)).child(
                v_col()
                    .gap(px(10.0))
                    .child(
                        h_row()
                            .w_full()
                            .justify_between()
                            .gap(px(10.0))
                            .child(
                                v_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .text_color(to_hsla(p.ink))
                                            .child(if zh { "数据目录" } else { "Data directory" }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(to_hsla(p.muted))
                                            .truncate()
                                            .child(SharedString::from(data_dir_str)),
                                    ),
                            )
                            .child(
                                btn_secondary(
                                    "data-reveal-dir",
                                    if zh { "打开目录" } else { "Reveal folder" },
                                )
                                .icon(icon_xs(UiIcon::FolderOpen))
                                .disabled(view.data_dir_path.is_none())
                                .on_click(cx.listener(|this, _, _, cx| this.reveal_data_dir(cx))),
                            ),
                    )
                    .child(
                        h_row()
                            .flex_wrap()
                            .gap(px(8.0))
                            .child(
                                btn_secondary(
                                    "data-backup-btn",
                                    if zh { "备份数据库…" } else { "Back up database…" },
                                )
                                .icon(icon_xs(UiIcon::Download))
                                .disabled(view.data_busy || view.data_dir_path.is_none())
                                .on_click(cx.listener(|this, _, _, cx| this.backup_database(cx))),
                            )
                            .child(
                                btn_secondary(
                                    "data-compact-btn",
                                    if zh { "压缩数据库" } else { "Compact database" },
                                )
                                .icon(icon_xs(UiIcon::Archive))
                                .disabled(view.data_busy || view.data_dir_path.is_none())
                                .on_click(cx.listener(|this, _, _, cx| this.compact_database(cx))),
                            )
                            .child(
                                btn_secondary(
                                    "data-clear-att-30",
                                    if zh {
                                        "清理 30 天前附件…"
                                    } else {
                                        "Clear attachments (>30d)…"
                                    },
                                )
                                .disabled(view.data_busy || view.data_dir_path.is_none())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.preview_clear_attachments(Some(30), cx);
                                })),
                            )
                            .child(
                                btn_secondary(
                                    "data-clear-att-all",
                                    if zh { "清理全部附件…" } else { "Clear all attachments…" },
                                )
                                .disabled(view.data_busy || view.data_dir_path.is_none())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.preview_clear_attachments(None, cx);
                                })),
                            )
                            .child(
                                btn_secondary(
                                    "data-clear-cap",
                                    if zh { "清空捕获正文…" } else { "Clear captured bodies…" },
                                )
                                .disabled(view.data_busy || usage.captured_bodies_count == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.request_clear_captured_bodies(cx);
                                })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(if zh {
                                "备份会同时复制数据库与同目录的 0600 明文 .secrets 保险库副本（目标路径必须位于 PingDeck 数据目录之外）。"
                            } else {
                                "Backups copy both the database and its sidecar owner-only (0600) plaintext .secrets vault (destination must be outside PingDeck's data folder)."
                            }),
                    ),
            ),
        );
    sec
}

fn render_retention_section(
    view: &SettingsView,
    zh: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let data = &view.settings.data;
    let mut sec = v_col()
        .w_full()
        .child(section_header(
            if zh { "历史与对话保留策略" } else { "Retention Policies" },
            None,
            cx,
        ))
        .child(setting_row(
            if zh { "每个工作区请求历史上限" } else { "Request history limit per workspace" },
            None,
            Some(SharedString::from(if zh {
                "超出上限时自动淘汰最早的请求历史记录（10–100,000 条，默认 2,000）。"
            } else {
                "Oldest request history entries are pruned past this count (10–100,000, default 2,000)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.history_retention,
                    Some(if zh { "条" } else { "rows" }),
                    96.0,
                    cx,
                ))
                .child(
                    btn_secondary("data-save-hist", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.history_retention.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                10,
                                100_000,
                                CoreSection::Data,
                                |n| serde_json::json!({ "history_retention": n }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "对话自动过期天数" } else { "Chat auto-retention" },
            None,
            Some(SharedString::from(if zh {
                "启动时及每日自动清理超过该天数未使用的对话；已置顶或运行中的对话始终保留。"
            } else {
                "Removes inactive chats older than the chosen age at launch and daily; pinned and active chats are always kept."
            })),
            h_row()
                .gap(px(5.0))
                .children([0u16, 30, 90, 180, 365].into_iter().map(|days| {
                    let label = if days == 0 {
                        if zh { "关闭".to_owned() } else { "Off".to_owned() }
                    } else if zh {
                        format!("{days} 天")
                    } else {
                        format!("{days}d")
                    };
                    choice_chip(
                        SharedString::from(format!("chat-ret-{days}")),
                        label,
                        data.chat_retention_days == days,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_chat_retention_days(days, cx);
                    }))
                })),
            cx,
        ));

    if let Some(count) = view.chat_retention_preview
        && data.chat_retention_days > 0
    {
        sec = sec.child(
            div().pt(px(8.0)).child(alert_banner(
                if count > 0 { Tone::Warning } else { Tone::Neutral },
                if count > 0 {
                    if zh {
                        format!(
                            "按当前 {} 天策略，有 {count} 个闲置对话符合清理条件。",
                            data.chat_retention_days
                        )
                    } else {
                        format!(
                            "{count} inactive chat(s) currently qualify for deletion under the {}-day policy.",
                            data.chat_retention_days
                        )
                    }
                } else if zh {
                    "当前没有符合过期清理条件的闲置对话。".to_owned()
                } else {
                    "No inactive chats currently qualify for expiration.".to_owned()
                },
                (count > 0).then(|| {
                    btn_secondary(
                        "chat-ret-apply-now",
                        if zh { "立即清理" } else { "Clean up now" },
                    )
                    .disabled(view.data_busy)
                    .on_click(cx.listener(|this, _, _, cx| this.apply_chat_retention_now(cx)))
                    .into_any_element()
                }),
                cx,
            )),
        );
    }

    sec
}

fn render_sync_section(
    view: &SettingsView,
    zh: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let sync = &view.settings.sync;
    let has_dir = !sync.directory.trim().is_empty();

    let kind_badge = view.sync_folder_info.as_ref().map(|info| match info.kind {
        SyncFolderKind::Plain => badge(
            if zh {
                "普通本地目录"
            } else {
                "Local folder"
            },
            Tone::Neutral,
            cx,
        ),
        SyncFolderKind::Git => badge(
            if zh {
                "Git 仓库内"
            } else {
                "Inside Git repo"
            },
            Tone::Warning,
            cx,
        ),
        SyncFolderKind::Cloud => badge(
            info.cloud_name.clone().unwrap_or_else(|| {
                if zh {
                    "云同步盘".into()
                } else {
                    "Cloud drive".into()
                }
            }),
            Tone::Warning,
            cx,
        ),
    });

    let mut sec = v_col()
        .w_full()
        .gap(px(8.0))
        .child(section_header(
            if zh { "本地文件夹同步" } else { "Local-First Folder Sync" },
            Some(
                h_row()
                    .gap(px(6.0))
                    .children(kind_badge)
                    .child(
                        btn_primary("sync-export-now", if zh { "立即导出" } else { "Export now" })
                            .icon(icon_xs(UiIcon::Upload))
                            .disabled(!has_dir || view.sync_busy)
                            .on_click(cx.listener(|this, _, _, cx| this.export_sync_now(cx))),
                    )
                    .child(
                        btn_secondary(
                            "sync-preview-imp",
                            if zh { "预览并导入…" } else { "Preview & import…" },
                        )
                        .icon(icon_xs(UiIcon::Download))
                        .disabled(!has_dir || view.sync_busy)
                        .on_click(cx.listener(|this, _, _, cx| this.preview_sync_import(cx))),
                    )
                    .into_any_element(),
            ),
            cx,
        ))
        .child(setting_row(
            if zh { "同步目录" } else { "Sync folder" },
            None,
            Some(SharedString::from(if has_dir {
                sync.directory.clone()
            } else if zh {
                "将工作区、集合、环境、流程、Mock、服务商配置与设置导出为可读文件，不经过任何云服务。"
                    .to_owned()
            } else {
                "Exports workspaces, collections, environments, flows, mocks, providers, and settings as readable local files."
                    .to_owned()
            })),
            h_row()
                .gap(px(6.0))
                .child(
                    btn_secondary("sync-pick-dir", if zh { "选择文件夹…" } else { "Choose…" })
                        .icon(icon_xs(UiIcon::FolderOpen))
                        .on_click(cx.listener(|this, _, _, cx| this.pick_sync_directory(cx))),
                )
                .children(has_dir.then(|| {
                    btn_ghost("sync-reveal-dir", if zh { "打开" } else { "Reveal" })
                        .on_click(cx.listener(|this, _, _, cx| this.reveal_sync_dir(cx)))
                }))
                .children(has_dir.then(|| {
                    btn_ghost("sync-clear-dir", if zh { "取消绑定" } else { "Unlink" })
                        .on_click(cx.listener(|this, _, _, cx| this.clear_sync_directory(cx)))
                })),
            cx,
        ))
        .child(setting_row(
            if zh { "变更后自动导出" } else { "Auto-export on changes" },
            None,
            Some(SharedString::from(if zh {
                "每次修改配置后后台增量写入；若检测到其他设备更新过目录内容会自动暂停并提示先导入。"
            } else {
                "Writes incremental exports automatically after edits; pauses safely if incoming edits from another device are detected."
            })),
            toggle_switch("sync-auto-export", sync.auto_export)
                .disabled(!has_dir)
                .on_change(cx.listener(|this, checked: &bool, _, cx| {
                    this.patch_section(
                        CoreSection::Sync,
                        serde_json::json!({ "auto_export": *checked }),
                        cx,
                    );
                })),
            cx,
        ))
        .child(setting_row(
            if zh { "导出中包含明文秘密" } else { "Include plaintext secrets in exports" },
            Some(SharedString::from(if zh { "默认关闭" } else { "Off by default" })),
            Some(SharedString::from(if zh {
                "关闭时环境变量秘密与服务商密钥仅导出占位符；开启后将以明文写入同步目录。"
            } else {
                "When off, environment secrets and provider API keys export as placeholders; turning on writes plaintext values."
            })),
            toggle_switch("sync-inc-secrets", sync.include_secrets).on_change(cx.listener(
                |this, checked: &bool, window, cx| {
                    this.toggle_sync_include_secrets(*checked, window, cx);
                },
            )),
            cx,
        ));

    if let Some(info) = &view.sync_folder_info
        && info.holds_secrets
    {
        sec = sec.child(alert_banner(
            Tone::Warning,
            if zh {
                format!(
                    "当前同步目录中有 {} 个导出文件包含明文秘密值。",
                    info.files_with_secrets
                )
            } else {
                format!(
                    "{} exported file(s) in this sync folder currently contain plaintext secrets.",
                    info.files_with_secrets
                )
            },
            Some(
                btn_danger(
                    "sync-scrub-secrets",
                    if zh {
                        "替换为占位符"
                    } else {
                        "Remove secrets"
                    },
                )
                .disabled(view.sync_busy)
                .on_click(cx.listener(|this, _, _, cx| this.remove_sync_secrets(cx)))
                .into_any_element(),
            ),
            cx,
        ));
    }

    if let Some(st) = &view.sync_status {
        if let Some(err) = &st.last_error {
            sec = sec.child(alert_banner(
                Tone::Danger,
                if zh {
                    format!("上次自动导出暂停/失败：{err}")
                } else {
                    format!("Last export paused/failed: {err}")
                },
                None,
                cx,
            ));
        } else if st.last_export_at_ms.is_some() {
            sec = sec.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                "同步状态正常：已记录最近一次导出基线。"
            } else {
                "Sync healthy: last export baseline recorded."
            }));
        }
    }

    sec
}
