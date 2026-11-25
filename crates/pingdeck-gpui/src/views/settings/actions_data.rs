//! Settings › Data storage usage, reveal, backup, compaction, attachment/capture cleanup,
//! and chat retention actions.

use gpui_kit::Context;
use pingdeck_desktop::core::app::{
    ClearAttachmentsOptions, chat_retention_apply_impl, chat_retention_preview_impl,
    clear_attachments_impl, clear_captured_bodies_impl, current_time_ms, data_backup_impl,
    data_compact_impl, inspect_sync_folder_impl, llm_gateway_status_impl, reveal_path_impl,
    storage_usage_impl, sync_status_impl,
};
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;
use pingdeck_desktop::services::{FileFilter, HostDialog as _, RfdDialog, SaveFileSpec};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::SettingsView;
use super::state_inputs::{SettingsModal, format_bytes};

impl SettingsView {
    /// Refreshes storage usage, sync status/risk, chat retention preview, and live gateway status.
    pub fn refresh_data_and_sync(&mut self, cx: &mut Context<Self>) {
        self.storage_loading = true;
        cx.notify();
        let retention_days = u32::from(self.settings.data.chat_retention_days);
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let usage = storage_usage_impl(state).ok();
                let data_dir = state
                    .data_dirs()
                    .map(|d| d.root.to_string_lossy().into_owned());
                let sync = sync_status_impl(state);
                let folder_info = if sync.directory.trim().is_empty() {
                    None
                } else {
                    inspect_sync_folder_impl(state, &sync.directory).ok()
                };
                let preview = if retention_days > 0 {
                    chat_retention_preview_impl(state, retention_days)
                        .ok()
                        .map(|p| p.chats)
                } else {
                    None
                };
                let gw = llm_gateway_status_impl(state);
                (usage, data_dir, sync, folder_info, preview, gw)
            },
            |this, (usage, data_dir, sync, folder_info, preview, gw), cx| {
                this.storage_loading = false;
                this.storage_usage = usage;
                this.data_dir_path = data_dir;
                this.sync_status = Some(sync);
                this.sync_folder_info = folder_info;
                this.chat_retention_preview = preview;
                this.gateway_status = Some(gw);
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn reveal_data_dir(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.data_dir_path.clone() else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| reveal_path_impl(state, &path),
            |this, res, cx| {
                if let Err(err) = res {
                    this.set_flash(Tone::Danger, err.message().to_owned(), cx);
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn backup_database(&mut self, cx: &mut Context<Self>) {
        if self.data_busy {
            return;
        }
        let zh = self.is_zh();
        let spec = SaveFileSpec {
            title: Some(if zh {
                "备份 PingDeck 数据库".to_owned()
            } else {
                "Back up PingDeck database".to_owned()
            }),
            file_name: Some("pingdeck-backup.db".to_owned()),
            filters: vec![FileFilter {
                name: "SQLite Database".to_owned(),
                extensions: vec!["db".into(), "sqlite".into()],
            }],
            starting_dir: None,
        };
        let Some(dest) = RfdDialog.save_file(&spec) else {
            return;
        };
        self.data_busy = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |state| data_backup_impl(state, &dest),
            move |this, res, cx| {
                this.data_busy = false;
                match res {
                    Ok(dto) => {
                        let vault_note = if dto.secrets_path.is_some() {
                            if zh {
                                "（含同目录 0600 明文 .secrets 保险库副本）"
                            } else {
                                " (including sidecar 0600 plaintext .secrets vault)"
                            }
                        } else {
                            ""
                        };
                        this.set_flash(
                            Tone::Success,
                            if zh {
                                format!(
                                    "已备份 {} 至 {}{}",
                                    format_bytes(dto.bytes),
                                    dto.path,
                                    vault_note
                                )
                            } else {
                                format!(
                                    "Backed up {} to {}{}",
                                    format_bytes(dto.bytes),
                                    dto.path,
                                    vault_note
                                )
                            },
                            cx,
                        );
                    }
                    Err(err) => this.set_flash(Tone::Danger, err.message().to_owned(), cx),
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn compact_database(&mut self, cx: &mut Context<Self>) {
        if self.data_busy {
            return;
        }
        let zh = self.is_zh();
        self.data_busy = true;
        cx.notify();
        let task = spawn_blocking_into(cx, data_compact_impl, move |this, res, cx| {
            this.data_busy = false;
            match res {
                Ok(dto) => {
                    let saved = dto.before_bytes.saturating_sub(dto.after_bytes);
                    this.set_flash(
                        Tone::Success,
                        if zh {
                            format!(
                                "数据库压缩完成：{} → {}（回收 {}）",
                                format_bytes(dto.before_bytes),
                                format_bytes(dto.after_bytes),
                                format_bytes(saved)
                            )
                        } else {
                            format!(
                                "Database compacted: {} → {} (reclaimed {})",
                                format_bytes(dto.before_bytes),
                                format_bytes(dto.after_bytes),
                                format_bytes(saved)
                            )
                        },
                        cx,
                    );
                    this.refresh_data_and_sync(cx);
                }
                Err(err) => this.set_flash(Tone::Danger, err.message().to_owned(), cx),
            }
        });
        self._tasks.push(task);
    }

    pub fn preview_clear_attachments(
        &mut self,
        older_than_days: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        if self.data_busy {
            return;
        }
        self.data_busy = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                clear_attachments_impl(
                    state,
                    ClearAttachmentsOptions {
                        older_than_days,
                        dry_run: true,
                    },
                )
            },
            move |this, res, cx| {
                this.data_busy = false;
                match res {
                    Ok(report) => {
                        this.modal = Some(SettingsModal::ConfirmClearAttachments {
                            older_than_days,
                            report,
                        });
                    }
                    Err(err) => this.set_flash(Tone::Danger, err.message().to_owned(), cx),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn confirm_clear_attachments(
        &mut self,
        older_than_days: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        let zh = self.is_zh();
        self.modal = None;
        self.data_busy = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                clear_attachments_impl(
                    state,
                    ClearAttachmentsOptions {
                        older_than_days,
                        dry_run: false,
                    },
                )
            },
            move |this, res, cx| {
                this.data_busy = false;
                match res {
                    Ok(report) => {
                        this.set_flash(
                            Tone::Success,
                            if zh {
                                format!(
                                    "已清理 {} 个附件文件，释放 {}",
                                    report.removed,
                                    format_bytes(report.freed_bytes)
                                )
                            } else {
                                format!(
                                    "Removed {} attachment blob(s), freed {}",
                                    report.removed,
                                    format_bytes(report.freed_bytes)
                                )
                            },
                            cx,
                        );
                        this.refresh_data_and_sync(cx);
                    }
                    Err(err) => this.set_flash(Tone::Danger, err.message().to_owned(), cx),
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn request_clear_captured_bodies(&mut self, cx: &mut Context<Self>) {
        let (count, bytes) = self
            .storage_usage
            .map(|u| (u.captured_bodies_count, u.captured_bodies_bytes))
            .unwrap_or((0, 0));
        self.modal = Some(SettingsModal::ConfirmClearCapturedBodies { count, bytes });
        cx.notify();
    }

    pub fn confirm_clear_captured_bodies(&mut self, cx: &mut Context<Self>) {
        let zh = self.is_zh();
        self.modal = None;
        self.data_busy = true;
        cx.notify();
        let task = spawn_async_into(
            cx,
            |state| async move { clear_captured_bodies_impl(&state).await },
            move |this, res, cx| {
                this.data_busy = false;
                match res {
                    Ok(report) => {
                        this.set_flash(
                            Tone::Success,
                            if zh {
                                format!(
                                    "已清空 {} 条捕获正文（约 {}）",
                                    report.removed,
                                    format_bytes(report.freed_bytes)
                                )
                            } else {
                                format!(
                                    "Cleared {} captured bodies ({})",
                                    report.removed,
                                    format_bytes(report.freed_bytes)
                                )
                            },
                            cx,
                        );
                        this.refresh_data_and_sync(cx);
                    }
                    Err(err) => this.set_flash(Tone::Danger, err.message().to_owned(), cx),
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn select_chat_retention_days(&mut self, days: u16, cx: &mut Context<Self>) {
        self.patch_section(
            CoreSection::Data,
            serde_json::json!({ "chat_retention_days": days }),
            cx,
        );
        if days == 0 {
            self.chat_retention_preview = None;
            cx.notify();
            return;
        }
        let d = u32::from(days);
        let task = spawn_blocking_into(
            cx,
            move |state| chat_retention_preview_impl(state, d),
            |this, res, cx| {
                if let Ok(dto) = res {
                    this.chat_retention_preview = Some(dto.chats);
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn apply_chat_retention_now(&mut self, cx: &mut Context<Self>) {
        let days = u32::from(self.settings.data.chat_retention_days);
        if days == 0 || self.data_busy {
            return;
        }
        let zh = self.is_zh();
        self.data_busy = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |state| chat_retention_apply_impl(state, days, current_time_ms()),
            move |this, res, cx| {
                this.data_busy = false;
                match res {
                    Ok(deleted) => {
                        this.set_flash(
                            Tone::Success,
                            if zh {
                                format!("已清理 {deleted} 个过期对话（置顶与运行中的对话已保留）")
                            } else {
                                format!(
                                    "Deleted {deleted} expired chat(s) (pinned & active chats kept)"
                                )
                            },
                            cx,
                        );
                        this.refresh_data_and_sync(cx);
                    }
                    Err(err) => this.set_flash(Tone::Danger, err.message().to_owned(), cx),
                }
            },
        );
        self._tasks.push(task);
    }
}
