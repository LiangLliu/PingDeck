//! Settings › Local-First Folder Sync actions: pick/reveal/unlink directory, secret-export
//! risk inspection, secret scrubbing, export now, and previewed import.

use gpui_kit::{Context, Window};
use spanbay_desktop::core::app::{
    SyncFolderKind, inspect_sync_folder_impl, remove_secrets_from_folder_impl, reveal_path_impl,
    sync_export_impl, sync_import_impl, sync_preview_impl,
};
use spanbay_desktop::core::model::settings::SettingsSection as CoreSection;
use spanbay_desktop::core::sync::SyncImportOptions;
use spanbay_desktop::services::{HostDialog as _, RfdDialog};

use crate::bridge::spawn_blocking_into;
use crate::ui::Tone;

use super::SettingsView;
use super::state_inputs::SettingsModal;

impl SettingsView {
    pub fn reveal_sync_dir(&mut self, cx: &mut Context<Self>) {
        let path = self.settings.sync.directory.trim().to_owned();
        if path.is_empty() {
            return;
        }
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

    pub fn pick_sync_directory(&mut self, cx: &mut Context<Self>) {
        let title = crate::ui::tr(cx)
            .settings()
            .actions_sync_choose_local_sync_folder();
        let Some(path) = RfdDialog.pick_folder(Some(title)) else {
            return;
        };
        let dir = path.to_string_lossy().to_string();
        self.patch_section(
            CoreSection::Sync,
            serde_json::json!({ "directory": dir }),
            cx,
        );
        self.refresh_data_and_sync(cx);
    }

    pub fn clear_sync_directory(&mut self, cx: &mut Context<Self>) {
        self.patch_section(
            CoreSection::Sync,
            serde_json::json!({ "directory": "", "auto_export": false }),
            cx,
        );
        self.sync_folder_info = None;
        self.refresh_data_and_sync(cx);
    }

    pub fn toggle_sync_include_secrets(
        &mut self,
        enable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !enable {
            self.patch_section(
                CoreSection::Sync,
                serde_json::json!({ "include_secrets": false }),
                cx,
            );
            return;
        }
        let dir = self.settings.sync.directory.trim().to_owned();
        self.inputs
            .sync_secrets_confirm
            .update(cx, |i, cx| i.set_value("", window, cx));
        if dir.is_empty() {
            self.modal = Some(SettingsModal::ConfirmSyncSecrets {
                kind: SyncFolderKind::Plain,
                cloud_name: None,
                directory: String::new(),
            });
            cx.notify();
            return;
        }
        let dir_clone = dir.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| inspect_sync_folder_impl(state, &dir_clone),
            move |this, res, cx| {
                let (kind, cloud_name) = match res {
                    Ok(info) => {
                        let k = info.kind;
                        let cn = info.cloud_name.clone();
                        this.sync_folder_info = Some(info);
                        (k, cn)
                    }
                    Err(_) => (SyncFolderKind::Plain, None),
                };
                this.modal = Some(SettingsModal::ConfirmSyncSecrets {
                    kind,
                    cloud_name,
                    directory: dir,
                });
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn confirm_sync_include_secrets(&mut self, cx: &mut Context<Self>) {
        self.modal = None;
        self.patch_section(
            CoreSection::Sync,
            serde_json::json!({ "include_secrets": true }),
            cx,
        );
    }

    pub fn remove_sync_secrets(&mut self, cx: &mut Context<Self>) {
        if self.sync_busy {
            return;
        }
        self.sync_busy = true;
        cx.notify();
        let task =
            spawn_blocking_into(cx, remove_secrets_from_folder_impl, move |this, res, cx| {
                this.sync_busy = false;
                match res {
                    Ok(n) => {
                        this.set_flash(
                            Tone::Success,
                            crate::ui::tr(cx)
                                .settings()
                                .actions_sync_replaced_secrets_with_placeholders_in(n),
                            cx,
                        );
                        this.refresh_data_and_sync(cx);
                    }
                    Err(err) => this.set_flash(Tone::Danger, err.message().to_owned(), cx),
                }
            });
        self._tasks.push(task);
    }

    pub fn export_sync_now(&mut self, cx: &mut Context<Self>) {
        if self.sync_busy {
            return;
        }
        self.sync_busy = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            |state| sync_export_impl(state, None),
            move |this, res, cx| {
                this.sync_busy = false;
                match res {
                    Ok(r) => {
                        let skip_note = if r.skipped_newer.is_empty() {
                            String::new()
                        } else {
                            crate::ui::tr(cx)
                                .settings()
                                .actions_sync_skipped_newer_file_s(r.skipped_newer.len())
                        };
                        this.set_flash(
                            Tone::Success,
                            crate::ui::tr(cx)
                                .settings()
                                .actions_sync_exported_written_removed(
                                    r.files_written,
                                    r.files_removed,
                                    skip_note,
                                ),
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

    pub fn preview_sync_import(&mut self, cx: &mut Context<Self>) {
        if self.sync_busy {
            return;
        }
        self.sync_busy = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            |state| sync_preview_impl(state, None),
            |this, res, cx| {
                this.sync_busy = false;
                match res {
                    Ok(preview) => {
                        this.modal = Some(SettingsModal::SyncImportPreview {
                            preview,
                            apply_removals: false,
                        });
                    }
                    Err(err) => this.set_flash(Tone::Danger, err.message().to_owned(), cx),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn confirm_sync_import(&mut self, cx: &mut Context<Self>) {
        let Some(SettingsModal::SyncImportPreview {
            preview,
            apply_removals,
        }) = self.modal.take()
        else {
            return;
        };
        let opts = SyncImportOptions {
            apply_removals,
            keep_local_keys: true,
            overrides: Vec::new(),
            keep_key_providers: Vec::new(),
            preview_token: Some(preview.token),
        };
        self.sync_busy = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |state| sync_import_impl(state, None, &opts),
            move |this, res, cx| {
                this.sync_busy = false;
                match res {
                    Ok(r) => {
                        this.set_flash(
                            Tone::Success,
                            crate::ui::tr(cx)
                                .settings()
                                .actions_sync_imported_added_updated_removed_kept(
                                    r.added,
                                    r.updated,
                                    r.removed,
                                    r.kept_local,
                                ),
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
