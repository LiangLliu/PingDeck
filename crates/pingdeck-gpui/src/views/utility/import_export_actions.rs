//! File picker, preview, commit, and sanitized/plaintext export actions for `ImportExportModal`.

use std::path::PathBuf;

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::{
    UndoToken, collection_import_text_impl, environment_export_impl, environment_import_text_impl,
    import_preview_impl, postman_export_save_to_path_impl, postman_export_suggested_filename,
    postman_import_text_impl, request_curl_import_impl,
};
use pingdeck_desktop::core::error::AppError;
use pingdeck_desktop::services::{
    FileFilter, FilePickerSpec, HostDialog as _, RfdDialog, SaveFileSpec,
};

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::{Tone, tr};

use super::import_export::{ImportExportModal, ImportExportOutcome};

impl ImportExportModal {
    pub fn pick_import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = RfdDialog.pick_file(&FilePickerSpec {
            title: None,
            filters: vec![FileFilter {
                name: "JSON / YAML / HAR / Text".to_owned(),
                extensions: vec![
                    "json".into(),
                    "yaml".into(),
                    "yml".into(),
                    "har".into(),
                    "txt".into(),
                    "env".into(),
                ],
            }],
            starting_dir: None,
        }) else {
            return;
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            self.io_format = guess_import_format(&path.to_string_lossy(), &text);
            self.payload_input
                .update(cx, |s, cx| s.set_value(text, window, cx));
            self.run_io_preview(cx);
        }
    }

    pub fn run_io_preview(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let payload = self.payload_input.read(cx).value().to_string();
        if payload.trim().is_empty() {
            return;
        }
        match import_preview_impl(runtime(cx).state(), ws_id, self.io_format.clone(), payload) {
            Ok(prev) => {
                self.io_preview = Some(prev);
                self.status_note = None;
            }
            Err(e) => self.status_note = Some((Tone::Danger, e.message().to_owned())),
        }
        cx.notify();
    }

    pub fn run_io_import(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let payload = self.payload_input.read(cx).value().to_string();
        if payload.trim().is_empty() {
            return;
        }
        let fmt = self.io_format.clone();
        let conflict = self.io_conflict;
        let u = tr(cx).utility();

        // cURL opens directly as an unsaved request draft in Requests (docs/product.md §1).
        if fmt == "curl" {
            match request_curl_import_impl(ws_id, payload) {
                Ok(req) => {
                    self.io_preview = None;
                    self.pending_outcome
                        .set(Some(ImportExportOutcome::OpenCurlDraft {
                            request: Box::new(req),
                            message: u.io_opened_curl_draft().to_owned(),
                        }));
                    self.dismissed.set(true);
                }
                Err(err) => {
                    self.status_note = Some((Tone::Danger, err.message().to_owned()));
                }
            }
            cx.notify();
            return;
        }

        let task = spawn_blocking_into(
            cx,
            move |state| -> Result<(String, Option<UndoToken>), AppError> {
                match fmt.as_str() {
                    "postman" => {
                        let res = postman_import_text_impl(state, ws_id, payload, conflict)?;
                        Ok((u.io_imported_postman(res.request_count), res.undo_token))
                    }
                    "environment" => {
                        let env = environment_import_text_impl(state, ws_id, payload, conflict)?;
                        Ok((u.io_imported_env(env.name), None))
                    }
                    other => {
                        let res = collection_import_text_impl(
                            state,
                            ws_id,
                            other.to_owned(),
                            payload,
                            conflict,
                        )?;
                        Ok((u.io_imported_col(res.request_count), res.undo_token))
                    }
                }
            },
            |this, res, cx| {
                match res {
                    Ok((message, undo_token)) => {
                        this.io_preview = None;
                        this.pending_outcome
                            .set(Some(ImportExportOutcome::WorkspaceImported {
                                message,
                                undo_token,
                            }));
                        this.dismissed.set(true);
                    }
                    Err(e) => this.status_note = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn export_collection(&mut self, include_secrets: bool, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(col_id)) =
            (self.workspace_id.clone(), self.io_export_col_id.clone())
        else {
            return;
        };
        let suggested = postman_export_suggested_filename(runtime(cx).state(), &ws_id, &col_id)
            .unwrap_or_else(|_| "collection.postman_collection.json".to_owned());
        let Some(path): Option<PathBuf> = RfdDialog.save_file(&SaveFileSpec {
            title: None,
            file_name: Some(suggested),
            filters: vec![FileFilter {
                name: "JSON".to_owned(),
                extensions: vec!["json".into()],
            }],
            starting_dir: None,
        }) else {
            return;
        };
        let env_id = self.active_env_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                postman_export_save_to_path_impl(
                    state,
                    ws_id,
                    col_id,
                    env_id,
                    include_secrets,
                    Some(path),
                )
            },
            |this, res, cx| {
                match res {
                    Ok(r) if r.saved => {
                        this.pending_outcome
                            .set(Some(ImportExportOutcome::Exported {
                                message: tr(cx).utility().io_exported_col().to_owned(),
                            }));
                        this.dismissed.set(true);
                    }
                    Ok(_) => {}
                    Err(e) => this.status_note = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn export_environment(&mut self, include_secrets: bool, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(env_id)) =
            (self.workspace_id.clone(), self.io_export_env_id.clone())
        else {
            return;
        };
        let name = self
            .environments
            .iter()
            .find(|e| e.id == env_id)
            .map(|e| e.name.clone())
            .unwrap_or_else(|| "environment".to_owned());
        let Some(path) = RfdDialog.save_file(&SaveFileSpec {
            title: None,
            file_name: Some(format!("{name}.postman_environment.json")),
            filters: vec![FileFilter {
                name: "JSON".to_owned(),
                extensions: vec!["json".into()],
            }],
            starting_dir: None,
        }) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let json = environment_export_impl(state, ws_id, env_id, include_secrets)?;
                pingdeck_desktop::core::platform::write_private_file(&path, json)
                    .map_err(|e| AppError::io(e.to_string()))
            },
            |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.pending_outcome
                            .set(Some(ImportExportOutcome::Exported {
                                message: tr(cx).utility().io_exported_env().to_owned(),
                            }));
                        this.dismissed.set(true);
                    }
                    Err(e) => this.status_note = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}

fn guess_import_format(path: &str, text: &str) -> String {
    let p_low = path.to_ascii_lowercase();
    let trimmed = text.trim_start();
    if trimmed.starts_with("curl ") || trimmed.starts_with("curl\t") {
        return "curl".to_owned();
    }
    if p_low.ends_with(".har") || (trimmed.contains("\"log\"") && trimmed.contains("\"entries\"")) {
        return "har".to_owned();
    }
    if trimmed.contains("\"openapi\"")
        || trimmed.contains("\"swagger\"")
        || trimmed.starts_with("openapi:")
        || p_low.ends_with(".yaml")
        || p_low.ends_with(".yml")
    {
        return "openapi".to_owned();
    }
    if trimmed.contains("_postman_variable_scope") {
        return "environment".to_owned();
    }
    if trimmed.contains("\"_type\": \"export\"") || trimmed.contains("\"_type\":\"export\"") {
        return "insomnia".to_owned();
    }
    if trimmed.contains("\"v\":")
        && trimmed.contains("\"requests\"")
        && trimmed.contains("\"folders\"")
    {
        return "hoppscotch".to_owned();
    }
    "postman".to_owned()
}
