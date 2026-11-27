//! Settings › Data & Sync page: live storage usage KPIs, Reveal/Backup/Compact/Clear maintenance,
//! history & chat retention with preview/apply, and local-first folder sync (Git/Cloud risk guard,
//! secret scrub, export, preview & import).

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::app::SyncFolderKind;
use spanbay_desktop::core::model::settings::SettingsSection as CoreSection;

use crate::ui::{
    Disableable as _, Tone, UiIcon, alert_banner, badge, btn_danger, btn_ghost, btn_primary,
    btn_secondary, choice_chip, h_row, icon_xs, kpi_card, number_field, palette, setting_row,
    settings_section, settings_sub_well, spinner_xs, to_hsla, toggle_switch, v_col,
};

use super::SettingsView;
use super::state_inputs::format_bytes;

pub fn render_data(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    v_col()
        .w_full()
        .gap(px(22.0))
        .child(render_storage_section(view, cx))
        .child(render_retention_section(view, cx))
        .child(render_sync_section(view, cx))
}

fn render_storage_section(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let p = palette(cx);
    let usage = view.storage_usage.unwrap_or_default();
    let has_usage = view.storage_usage.is_some();

    let fmt_size = |bytes: u64| {
        if has_usage {
            format_bytes(bytes)
        } else {
            "—".to_owned()
        }
    };
    let db_sub = Some(SharedString::from(
        if has_usage && usage.database_free_bytes > 0 {
            let free = format_bytes(usage.database_free_bytes);
            crate::ui::tr(cx).settings().page_data_reclaimable(free)
        } else {
            "SQLite WAL".to_owned()
        },
    ));
    let chats_sub = has_usage.then(|| {
        SharedString::from(
            crate::ui::tr(cx)
                .settings()
                .page_data_chats(usage.chats_count),
        )
    });
    let att_sub = has_usage.then(|| {
        SharedString::from(
            crate::ui::tr(cx)
                .settings()
                .page_data_files(usage.attachments_count),
        )
    });
    let cap_sub = has_usage.then(|| {
        let logs = format_bytes(usage.logs_bytes);
        SharedString::from(
            crate::ui::tr(cx)
                .settings()
                .page_data_captures_logs(usage.captured_bodies_count, logs),
        )
    });

    let kpi_row = h_row()
        .w_full()
        .gap(px(10.0))
        .child(kpi_card(
            crate::ui::tr(cx).settings().page_data_database(),
            fmt_size(usage.database_bytes),
            db_sub,
            cx,
        ))
        .child(kpi_card(
            crate::ui::tr(cx).settings().page_data_chats_output(),
            fmt_size(usage.chats_bytes),
            chats_sub,
            cx,
        ))
        .child(kpi_card(
            crate::ui::tr(cx).settings().page_data_attachments(),
            fmt_size(usage.attachments_bytes),
            att_sub,
            cx,
        ))
        .child(kpi_card(
            crate::ui::tr(cx).settings().page_data_captures_logs_2(),
            fmt_size(usage.captured_bodies_bytes + usage.logs_bytes),
            cap_sub,
            cx,
        ));

    let data_dir_str = view.data_dir_path.clone().unwrap_or_else(|| {
        crate::ui::tr(cx)
            .settings()
            .page_data_in_memory_mode_no_disk()
            .to_owned()
    });

    let maint_card = v_col()
        .w_full()
        .p(px(16.0))
        .gap(px(12.0))
        .rounded(px(10.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line_strong.composite_over(p.pane)))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(10.0))
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.5))
                        .child(
                            div()
                                .text_size(px(13.5))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .child(crate::ui::tr(cx).settings().page_data_data_directory()),
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
                        crate::ui::tr(cx).settings().page_data_reveal_folder(),
                    )
                    .icon(icon_xs(UiIcon::FolderOpen))
                    .disabled(view.data_dir_path.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.reveal_data_dir(cx))),
                ),
        )
        .child(
            settings_sub_well(cx)
                .child(
                    h_row()
                        .flex_wrap()
                        .gap(px(8.0))
                        .child(
                            btn_secondary(
                                "data-backup-btn",
                                crate::ui::tr(cx).settings().page_data_back_up_database(),
                            )
                            .icon(icon_xs(UiIcon::Download))
                            .disabled(view.data_busy || view.data_dir_path.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.backup_database(cx))),
                        )
                        .child(
                            btn_secondary(
                                "data-compact-btn",
                                crate::ui::tr(cx).settings().page_data_compact_database(),
                            )
                            .icon(icon_xs(UiIcon::Archive))
                            .disabled(view.data_busy || view.data_dir_path.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.compact_database(cx))),
                        )
                        .child(
                            btn_secondary(
                                "data-clear-att-30",
                                crate::ui::tr(cx)
                                    .settings()
                                    .page_data_clear_attachments_30d(),
                            )
                            .disabled(view.data_busy || view.data_dir_path.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.preview_clear_attachments(Some(30), cx);
                            })),
                        )
                        .child(
                            btn_secondary(
                                "data-clear-att-all",
                                crate::ui::tr(cx)
                                    .settings()
                                    .page_data_clear_all_attachments(),
                            )
                            .disabled(view.data_busy || view.data_dir_path.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.preview_clear_attachments(None, cx);
                            })),
                        )
                        .child(
                            btn_secondary(
                                "data-clear-cap",
                                crate::ui::tr(cx)
                                    .settings()
                                    .page_data_clear_captured_bodies(),
                            )
                            .disabled(view.data_busy || usage.captured_bodies_count == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_clear_captured_bodies(cx);
                            })),
                        ),
                )
                .child(
                    div().text_xs().text_color(to_hsla(p.muted)).child(
                        crate::ui::tr(cx)
                            .settings()
                            .page_data_backups_copy_both_the_database(),
                    ),
                ),
        );

    v_col()
        .w_full()
        .gap(px(10.0))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .min_h(px(24.0))
                .px(px(4.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.muted))
                        .child(
                            crate::ui::tr(cx)
                                .settings()
                                .page_data_local_storage_maintenance(),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .children(view.storage_loading.then(spinner_xs))
                        .child(
                            btn_ghost("data-refresh", crate::ui::tr(cx).common().refresh())
                                .icon(icon_xs(UiIcon::Refresh))
                                .disabled(view.storage_loading)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.refresh_data_and_sync(cx);
                                })),
                        ),
                ),
        )
        .child(kpi_row)
        .child(maint_card)
}

fn render_retention_section(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let data = &view.settings.data;
    let rows = v_col()
        .w_full()
        .child(setting_row(
            crate::ui::tr(cx)
                .settings()
                .page_data_request_history_limit_per_workspace(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_data_oldest_request_history_entries_are(),
            )),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.history_retention,
                    Some(crate::ui::tr(cx).settings().page_advanced_rows()),
                    96.0,
                    cx,
                ))
                .child(
                    btn_secondary("data-save-hist", crate::ui::tr(cx).common().kv_bulk_apply())
                        .on_click(cx.listener(|this, _, _, cx| {
                            let v = this.inputs.history_retention.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                10,
                                100_000,
                                CoreSection::Data,
                                |n| serde_json::json!({ "history_retention": n }),
                                cx,
                            );
                        })),
                ),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx).settings().page_data_chat_auto_retention(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_data_removes_inactive_chats_older_than(),
            )),
            h_row()
                .gap(px(5.0))
                .children([0u16, 30, 90, 180, 365].into_iter().map(|days| {
                    let label = if days == 0 {
                        crate::ui::tr(cx).settings().notify_off().to_owned()
                    } else {
                        crate::ui::tr(cx).settings().page_data_d(days)
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

    let mut sec = v_col().w_full().gap(px(10.0)).child(settings_section(
        crate::ui::tr(cx).settings().page_data_retention_policies(),
        None,
        rows,
        cx,
    ));

    if let Some(count) = view.chat_retention_preview
        && data.chat_retention_days > 0
    {
        sec = sec.child(alert_banner(
            if count > 0 {
                Tone::Warning
            } else {
                Tone::Neutral
            },
            if count > 0 {
                crate::ui::tr(cx)
                    .settings()
                    .page_data_inactive_chat_s_currently_qualify(count, data.chat_retention_days)
            } else {
                crate::ui::tr(cx)
                    .settings()
                    .page_data_no_inactive_chats_currently_qualify()
                    .to_owned()
            },
            (count > 0).then(|| {
                btn_secondary(
                    "chat-ret-apply-now",
                    crate::ui::tr(cx).settings().page_data_clean_up_now(),
                )
                .disabled(view.data_busy)
                .on_click(cx.listener(|this, _, _, cx| this.apply_chat_retention_now(cx)))
                .into_any_element()
            }),
            cx,
        ));
    }

    sec
}

fn render_sync_section(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let p = palette(cx);
    let sync = &view.settings.sync;
    let has_dir = !sync.directory.trim().is_empty();

    let kind_badge = view.sync_folder_info.as_ref().map(|info| match info.kind {
        SyncFolderKind::Plain => badge(
            crate::ui::tr(cx).settings().page_data_local_folder(),
            Tone::Neutral,
            cx,
        ),
        SyncFolderKind::Git => badge(
            crate::ui::tr(cx).settings().page_data_inside_git_repo(),
            Tone::Warning,
            cx,
        ),
        SyncFolderKind::Cloud => badge(
            info.cloud_name
                .clone()
                .unwrap_or_else(|| crate::ui::tr(cx).settings().page_data_cloud_drive().into()),
            Tone::Warning,
            cx,
        ),
    });

    let sync_rows = v_col()
        .w_full()
        .child(setting_row(
            crate::ui::tr(cx).settings().page_data_sync_folder(),
            None,
            Some(SharedString::from(if has_dir {
                sync.directory.clone()
            } else {
                crate::ui::tr(cx)
                    .settings()
                    .page_data_exports_workspaces_collections_environment()
                    .to_owned()
            })),
            h_row()
                .gap(px(6.0))
                .child(
                    btn_secondary(
                        "sync-pick-dir",
                        crate::ui::tr(cx).settings().page_data_choose(),
                    )
                    .icon(icon_xs(UiIcon::FolderOpen))
                    .on_click(cx.listener(|this, _, _, cx| this.pick_sync_directory(cx))),
                )
                .children(has_dir.then(|| {
                    btn_ghost(
                        "sync-reveal-dir",
                        crate::ui::tr(cx).settings().page_data_reveal(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.reveal_sync_dir(cx)))
                }))
                .children(has_dir.then(|| {
                    btn_ghost(
                        "sync-clear-dir",
                        crate::ui::tr(cx).settings().page_data_unlink(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.clear_sync_directory(cx)))
                })),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx)
                .settings()
                .page_data_auto_export_on_changes(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_data_writes_incremental_exports_automatically_a(),
            )),
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
            crate::ui::tr(cx)
                .settings()
                .page_data_include_plaintext_secrets_in_exports(),
            Some(SharedString::from(
                crate::ui::tr(cx).settings().page_data_off_by_default(),
            )),
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_data_when_off_environment_secrets_and(),
            )),
            toggle_switch("sync-inc-secrets", sync.include_secrets).on_change(cx.listener(
                |this, checked: &bool, window, cx| {
                    this.toggle_sync_include_secrets(*checked, window, cx);
                },
            )),
            cx,
        ));

    let mut sec = v_col().w_full().gap(px(10.0)).child(settings_section(
        crate::ui::tr(cx)
            .settings()
            .page_data_local_first_folder_sync(),
        Some(
            h_row()
                .gap(px(6.0))
                .children(kind_badge)
                .child(
                    btn_primary(
                        "sync-export-now",
                        crate::ui::tr(cx).settings().page_data_export_now(),
                    )
                    .icon(icon_xs(UiIcon::Upload))
                    .disabled(!has_dir || view.sync_busy)
                    .on_click(cx.listener(|this, _, _, cx| this.export_sync_now(cx))),
                )
                .child(
                    btn_secondary(
                        "sync-preview-imp",
                        crate::ui::tr(cx).settings().page_data_preview_import(),
                    )
                    .icon(icon_xs(UiIcon::Download))
                    .disabled(!has_dir || view.sync_busy)
                    .on_click(cx.listener(|this, _, _, cx| this.preview_sync_import(cx))),
                )
                .into_any_element(),
        ),
        sync_rows,
        cx,
    ));

    if let Some(info) = &view.sync_folder_info
        && info.holds_secrets
    {
        sec = sec.child(alert_banner(
            Tone::Warning,
            crate::ui::tr(cx)
                .settings()
                .page_data_exported_file_s_in_this(info.files_with_secrets),
            Some(
                btn_danger(
                    "sync-scrub-secrets",
                    crate::ui::tr(cx).settings().page_data_remove_secrets(),
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
                crate::ui::tr(cx)
                    .settings()
                    .page_data_last_export_paused_failed(err),
                None,
                cx,
            ));
        } else if st.last_export_at_ms.is_some() {
            sec = sec.child(
                div()
                    .px(px(4.0))
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(
                        crate::ui::tr(cx)
                            .settings()
                            .page_data_sync_healthy_last_export_baseline(),
                    ),
            );
        }
    }

    sec
}
