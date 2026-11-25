//! Import preview + conflict merge (Postman/OpenAPI/HAR/Insomnia/Bruno/Hoppscotch/cURL/Env)
//! and Postman v2.1 collection export.

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::{
    ImportConflict, collection_import_text_impl, current_time_ms, environment_import_text_impl,
    import_preview_impl, postman_export_save_to_path_impl, postman_export_suggested_filename,
    postman_import_text_impl, request_curl_import_impl,
};
use pingdeck_desktop::services::{
    FileFilter, FilePickerSpec, HostDialog as _, RfdDialog, SaveFileSpec,
};

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::Tone;

use super::RequestsView;
use super::model::{ActiveModal, ImportExportTab, OpenTab, RequestTabState};

impl RequestsView {
    pub fn open_import_export_modal(
        &mut self,
        tab: ImportExportTab,
        export_col_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal_text_input
            .update(cx, |t, cx| t.set_value("", window, cx));
        let col = export_col_id.or_else(|| self.collections.first().map(|c| c.id.clone()));
        self.active_modal = Some(ActiveModal::ImportExport {
            tab,
            format: "curl".into(),
            preview: None,
            conflict: ImportConflict::Update,
            export_collection_id: col,
            export_include_secrets: false,
            export_ack_secrets: false,
        });
        cx.notify();
    }

    pub fn import_pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = FilePickerSpec {
            title: Some(crate::ui::tr(cx).requests().act_import_title().into()),
            filters: vec![FileFilter {
                name: "API Collections & Specs".into(),
                extensions: vec![
                    "json".into(),
                    "yaml".into(),
                    "yml".into(),
                    "har".into(),
                    "txt".into(),
                ],
            }],
            starting_dir: None,
        };
        let Some(path) = RfdDialog.pick_file(&spec) else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let guessed = guess_import_format(&path.to_string_lossy(), &content);
                self.modal_text_input
                    .update(cx, |t, cx| t.set_value(content, window, cx));
                if let Some(ActiveModal::ImportExport { format, .. }) = &mut self.active_modal {
                    *format = guessed;
                }
                self.import_run_preview(cx);
            }
            Err(err) => {
                self.status_msg = Some((
                    Tone::Danger,
                    crate::ui::tr(cx).requests().act_read_file_failed(err),
                ));
                cx.notify();
            }
        }
    }

    pub fn import_run_preview(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let payload = self.modal_text_input.read(cx).value().to_string();
        if payload.trim().is_empty() {
            return;
        }
        let fmt = match &self.active_modal {
            Some(ActiveModal::ImportExport { format, .. }) => format.clone(),
            _ => return,
        };
        match import_preview_impl(runtime(cx).state(), ws_id, fmt, payload) {
            Ok(prev) => {
                if let Some(ActiveModal::ImportExport { preview, .. }) = &mut self.active_modal {
                    *preview = Some(prev);
                }
            }
            Err(err) => {
                self.status_msg = Some((Tone::Danger, err.message().to_owned()));
            }
        }
        cx.notify();
    }

    pub fn import_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let payload = self.modal_text_input.read(cx).value().to_string();
        if payload.trim().is_empty() {
            return;
        }
        let (fmt, conflict) = match &self.active_modal {
            Some(ActiveModal::ImportExport {
                format, conflict, ..
            }) => (format.clone(), *conflict),
            _ => return,
        };

        // cURL opens directly as an unsaved request draft (per docs/product.md §1).
        if fmt == "curl" {
            match request_curl_import_impl(ws_id, payload) {
                Ok(req) => {
                    self.active_modal = None;
                    let tid = format!("tab-curl-{}", current_time_ms());
                    self.tabs
                        .push(OpenTab::Request(Box::new(RequestTabState::from_request(
                            tid.clone(),
                            req,
                            false,
                        ))));
                    self.active_tab_id = Some(tid);
                    self.hydrate_active_tab(window, cx);
                    self.status_msg = Some((
                        Tone::Success,
                        crate::ui::tr(cx).requests().act_opened_curl_draft().into(),
                    ));
                }
                Err(err) => {
                    self.status_msg = Some((Tone::Danger, err.message().to_owned()));
                }
            }
            cx.notify();
            return;
        }

        let ws_reload = ws_id.clone();
        let lang = crate::ui::active_language(cx);
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let r_tr = crate::ui::Tr(lang).requests();
                match fmt.as_str() {
                    "postman" => {
                        postman_import_text_impl(state, ws_id, payload, conflict).map(|r| {
                            (
                                r_tr.act_imported_collection(&r.collection_name, r.request_count),
                                r.undo_token,
                            )
                        })
                    }
                    "environment" => environment_import_text_impl(state, ws_id, payload, conflict)
                        .map(|e| (r_tr.act_imported_env(&e.name), None)),
                    other => collection_import_text_impl(
                        state,
                        ws_id,
                        other.to_owned(),
                        payload,
                        conflict,
                    )
                    .map(|r| {
                        (
                            r_tr.act_imported_collection(&r.collection_name, r.request_count),
                            r.undo_token,
                        )
                    }),
                }
            },
            move |this, res, cx| {
                match res {
                    Ok((msg, undo)) => {
                        this.active_modal = None;
                        if let Some(tok) = undo {
                            this.last_undo = Some((
                                tok,
                                crate::ui::tr(cx).requests().act_import_merge_label().into(),
                            ));
                        }
                        this.status_msg = Some((Tone::Success, msg));
                        this.reload_workspace_no_window(ws_reload, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn export_collection_postman(
        &mut self,
        collection_id: String,
        include_literal_credentials: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let col_name = self
            .collections
            .iter()
            .find(|c| c.id == collection_id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "collection".into());
        let rt = runtime(cx);
        let suggested = postman_export_suggested_filename(rt.state(), &ws_id, &collection_id)
            .unwrap_or_else(|_| format!("{col_name}.postman_collection.json"));
        let spec = SaveFileSpec {
            title: Some(crate::ui::tr(cx).requests().act_export_title().into()),
            file_name: Some(suggested),
            filters: vec![FileFilter {
                name: "JSON".into(),
                extensions: vec!["json".into()],
            }],
            starting_dir: None,
        };
        let Some(path) = RfdDialog.save_file(&spec) else {
            return;
        };
        let env_id = self.active_env_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                postman_export_save_to_path_impl(
                    state,
                    ws_id,
                    collection_id,
                    env_id,
                    include_literal_credentials,
                    Some(path),
                )
            },
            |this, res, cx| {
                match res {
                    Ok(done) if done.saved => {
                        this.active_modal = None;
                        let warn_note = if done.warnings.is_empty() {
                            String::new()
                        } else {
                            crate::ui::tr(cx)
                                .requests()
                                .act_warnings_note(done.warnings.len())
                        };
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .requests()
                                .act_exported_to(done.path.unwrap_or_default(), warn_note),
                        ));
                    }
                    Ok(_) => {}
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}

fn guess_import_format(path: &str, text: &str) -> String {
    let trimmed = text.trim_start();
    if trimmed.starts_with("curl ") || trimmed.starts_with("curl\t") {
        return "curl".into();
    }
    if path.ends_with(".har") || trimmed.contains("\"log\"") && trimmed.contains("\"entries\"") {
        return "har".into();
    }
    if trimmed.contains("\"openapi\"")
        || trimmed.contains("\"swagger\"")
        || trimmed.starts_with("openapi:")
    {
        return "openapi".into();
    }
    if trimmed.contains("_postman_variable_scope") {
        return "environment".into();
    }
    if trimmed.contains("\"_type\": \"export\"") || trimmed.contains("\"_type\":\"export\"") {
        return "insomnia".into();
    }
    if trimmed.contains("\"v\":")
        && trimmed.contains("\"requests\"")
        && trimmed.contains("\"folders\"")
    {
        return "hoppscotch".into();
    }
    "postman".into()
}
