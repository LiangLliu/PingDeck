//! Import/Export file picker, preview, commit, and sanitized/plaintext export actions for `UtilityPanel`.

use std::path::PathBuf;

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::{
    SaveRequestInput, collection_import_text_impl, environment_export_impl,
    environment_import_text_impl, import_preview_impl, postman_export_save_to_path_impl,
    postman_export_suggested_filename, postman_import_text_impl, request_curl_import_impl,
    request_save_impl,
};
use pingdeck_desktop::core::error::AppError;
use pingdeck_desktop::services::{
    FileFilter, FilePickerSpec, HostDialog as _, RfdDialog, SaveFileSpec,
};

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::Tone;

use super::UtilityPanel;

impl UtilityPanel {
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
            let p_low = path.to_string_lossy().to_ascii_lowercase();
            let t_trim = text.trim_start();
            self.io_format = if p_low.ends_with(".har") || t_trim.contains("\"log\"") {
                "har"
            } else if t_trim.starts_with("curl ") {
                "curl"
            } else if t_trim.contains("\"openapi\"")
                || t_trim.contains("\"swagger\"")
                || p_low.ends_with(".yaml")
                || p_low.ends_with(".yml")
            {
                "openapi"
            } else if t_trim.contains("\"_postman_variable_scope\"") {
                "environment"
            } else if t_trim.contains("\"_type\": \"export\"") {
                "insomnia"
            } else {
                "postman"
            }
            .to_owned();
            self.curl_input
                .update(cx, |s, cx| s.set_value(text, window, cx));
            self.run_io_preview(cx);
        }
    }

    pub fn run_io_preview(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let payload = self.curl_input.read(cx).value().to_string();
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
        let payload = self.curl_input.read(cx).value().to_string();
        if payload.trim().is_empty() {
            return;
        }
        let fmt = self.io_format.clone();
        let conflict = self.io_conflict;
        let task = spawn_blocking_into(
            cx,
            move |state| -> Result<String, AppError> {
                match fmt.as_str() {
                    "curl" => {
                        let req = request_curl_import_impl(ws_id.clone(), payload)?;
                        let input = SaveRequestInput {
                            id: req.id.as_str().to_owned(),
                            workspace_id: ws_id,
                            collection_id: req.collection_id.map(|c| c.as_str().to_owned()),
                            folder_id: req.folder_id.map(|f| f.as_str().to_owned()),
                            name: req.name,
                            protocol: req.protocol,
                            method: req.method,
                            url: req.url,
                            query_params: req.query_params,
                            path_variables: req.path_variables,
                            headers: req.headers,
                            auth: req.auth,
                            body: req.body,
                            network_options: req.network_options,
                            assertions: req.assertions,
                            description: req.description,
                            pre_request_script: req.pre_request_script,
                            test_script: req.test_script,
                            position: req.position,
                            mode: req.mode,
                            ai: req.ai,
                            examples: Some(req.examples),
                        };
                        let saved = request_save_impl(state, input)?;
                        Ok(format!("Imported cURL request: {}", saved.name))
                    }
                    "postman" => {
                        let res = postman_import_text_impl(state, ws_id, payload, conflict)?;
                        Ok(format!(
                            "Imported Postman collection ({} requests)",
                            res.request_count
                        ))
                    }
                    "environment" => {
                        let env = environment_import_text_impl(state, ws_id, payload, conflict)?;
                        Ok(format!("Imported environment: {}", env.name))
                    }
                    _ => {
                        let res =
                            collection_import_text_impl(state, ws_id, fmt, payload, conflict)?;
                        Ok(format!(
                            "Imported collection ({} requests)",
                            res.request_count
                        ))
                    }
                }
            },
            |this, res, cx| {
                match res {
                    Ok(msg) => {
                        this.io_preview = None;
                        this.status_note = Some((Tone::Success, msg));
                        this.reload_active(cx);
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
        let path: Option<PathBuf> = RfdDialog.save_file(&SaveFileSpec {
            title: None,
            file_name: Some(suggested),
            filters: vec![FileFilter {
                name: "JSON".to_owned(),
                extensions: vec!["json".into()],
            }],
            starting_dir: None,
        });
        let task = spawn_blocking_into(
            cx,
            move |state| {
                postman_export_save_to_path_impl(state, ws_id, col_id, None, include_secrets, path)
            },
            |this, res, cx| {
                match res {
                    Ok(r) if r.saved => {
                        this.status_note = Some((Tone::Success, "Collection exported".into()));
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
                        this.status_note = Some((Tone::Success, "Environment exported".into()));
                    }
                    Err(e) => this.status_note = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}
