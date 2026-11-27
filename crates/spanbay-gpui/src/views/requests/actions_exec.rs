//! HTTP/GraphQL execution, send-and-save-to-file, cancel, response formatting,
//! JSONPath filtering, save response body, and extract-to-environment.

use std::path::PathBuf;

use gpui_kit::{Context, Window};
use spanbay_desktop::core::app::environment_service::{
    DefineVariableInput, VariableScope, environment_variable_define,
};
use spanbay_desktop::core::app::{
    BodySource, ExecuteDraftRequestInput, ResponseBodyEncoding, ai_request_cancel_impl,
    current_time_ms, execution_cancel_impl, request_execute_draft_impl,
    request_execute_draft_to_file_impl, response_filter_jsonpath_impl, response_format_body_impl,
    response_save_handle_to_path_impl, response_save_to_path_impl, response_suggested_file_name,
};
use spanbay_desktop::core::error::AppError;
use spanbay_desktop::core::executor::ExecutionResult;
use spanbay_desktop::core::model::script_result::ConsoleLevel;
use spanbay_desktop::core::response_viewer::{
    JsonPathFilterOutcomeDto, ResponseBodyFormat, detect_response_format,
};
use spanbay_desktop::services::{HostDialog as _, RfdDialog, SaveFileSpec};

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::RequestsView;
use super::model::{ActiveModal, OpenTab, RequestFailureInfo, method_str};

impl RequestsView {
    pub fn send_active_request(
        &mut self,
        confirmed: Option<bool>,
        save_to_file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        if tab.sending {
            return;
        }
        let tid = tab.tab_id.clone();
        let cancel_id = format!("req-exec-{}-{}", tid, current_time_ms());
        tab.sending = true;
        tab.cancel_token = Some(cancel_id.clone());
        tab.failure = None;
        let save_input = tab.to_save_input();
        let method_lbl = method_str(save_input.method);
        let url_lbl = save_input.url.clone();
        let env_id = self.active_env_id.clone();
        let input = ExecuteDraftRequestInput {
            request: save_input,
            environment_id: env_id,
            confirmed,
            save_to_history: Some(true),
        };
        let win = window.window_handle();
        let save_path_for_modal = save_to_file.clone();

        let task = spawn_async_into(
            cx,
            move |state| async move {
                match save_to_file {
                    Some(path) => {
                        request_execute_draft_to_file_impl(&state, input, Some(cancel_id), path)
                            .await
                    }
                    None => request_execute_draft_impl(&state, input, Some(cancel_id)).await,
                }
            },
            move |this, res, cx| {
                if let Some(t) = this.tab_req_mut(&tid) {
                    t.sending = false;
                    t.cancel_token = None;
                }
                match res {
                    Ok(exec) => {
                        let status_line = format!(
                            "{method_lbl} {} → {} {} ({} ms)",
                            exec.url, exec.status_code, exec.status_text, exec.elapsed_ms
                        );
                        let lvl = if exec.status_code < 400 {
                            ConsoleLevel::Info
                        } else {
                            ConsoleLevel::Warn
                        };
                        this.push_console("HTTP", lvl, status_line, None);
                        for c in &exec.console {
                            this.push_console(
                                "SCRIPT",
                                c.level,
                                c.message.clone(),
                                Some(c.source.clone()),
                            );
                        }
                        let resp_ct = exec
                            .headers
                            .iter()
                            .find(|h| h.key.eq_ignore_ascii_case("content-type"))
                            .map(|h| h.value.as_str());
                        let default_fmt = match exec.suggested_view {
                            spanbay_desktop::core::executor::SuggestedView::Image => {
                                ResponseBodyFormat::Hex
                            }
                            _ => detect_response_format(resp_ct, &exec.body_text),
                        };
                        let body_src = body_source_of(&exec);
                        let pretty =
                            response_format_body_impl(runtime(cx).state(), body_src, default_fmt)
                                .ok();
                        let display_text = pretty
                            .as_ref()
                            .map(|f| f.text.clone())
                            .unwrap_or_else(|| exec.body_text.clone());

                        if let Some(t) = this.tab_req_mut(&tid) {
                            t.http_format = default_fmt;
                            t.http_pretty = true;
                            t.http_formatted_text = pretty.as_ref().map(|f| f.text.clone());
                            t.http_format_mode = pretty.map(|f| f.mode);
                            t.http_jsonpath_query.clear();
                            t.http_jsonpath_result = None;
                            t.last_http_at_ms = Some(current_time_ms());
                            t.last_http = Some(exec);
                        }
                        if this.active_tab_id.as_deref() == Some(tid.as_str()) {
                            let _ = win.update(cx, |_, window, cx| {
                                this.response_editor.update(cx, |e, cx| {
                                    e.set_highlighter(
                                        crate::ui::highlighter_for_format(default_fmt),
                                        cx,
                                    );
                                    e.set_value(display_text, window, cx);
                                });
                            });
                        }
                    }
                    Err(AppError::ConfirmationRequired { message, .. }) => {
                        this.active_modal = Some(ActiveModal::ConfirmGuard {
                            tab_id: tid,
                            message,
                            is_ai: false,
                            is_runner: false,
                            save_to_file: save_path_for_modal,
                        });
                    }
                    Err(err) => {
                        let fail = classify_error(&err, crate::ui::active_language(cx));
                        this.push_console(
                            "ERROR",
                            ConsoleLevel::Error,
                            format!("{method_lbl} {url_lbl} — {}", fail.message),
                            Some(fail.kind_label.clone()),
                        );
                        if let Some(t) = this.tab_req_mut(&tid) {
                            t.failure = Some(fail);
                        }
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
        cx.notify();
    }

    pub fn send_and_save_to_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let suggested = self
            .active_req()
            .map(|t| response_suggested_file_name(&t.draft.name))
            .unwrap_or_else(|| "response.bin".into());
        let spec = SaveFileSpec {
            title: Some(crate::ui::tr(cx).requests().act_send_save_title().into()),
            file_name: Some(suggested),
            filters: Vec::new(),
            starting_dir: None,
        };
        if let Some(path) = RfdDialog.save_file(&spec) {
            self.send_active_request(None, Some(path), window, cx);
        }
    }

    pub fn stop_active_request(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        match tab {
            OpenTab::Request(t) => {
                if let Some(tok) = t.cancel_token.clone() {
                    let rt = runtime(cx);
                    let st = rt.state();
                    if t.is_ai() {
                        ai_request_cancel_impl(st, tok);
                    } else {
                        execution_cancel_impl(st, tok);
                    }
                    self.status_msg = Some((
                        Tone::Warning,
                        crate::ui::tr(cx).requests().act_stopping_request().into(),
                    ));
                    cx.notify();
                }
            }
            OpenTab::Runner(r) => {
                let tid = r.tab_id.clone();
                self.stop_collection_run(&tid, cx);
            }
            _ => {}
        }
    }

    pub fn set_http_response_format(
        &mut self,
        format: ResponseBodyFormat,
        pretty: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        tab.http_format = format;
        tab.http_pretty = pretty;
        tab.http_jsonpath_result = None;
        let Some(resp) = tab.last_http.clone() else {
            cx.notify();
            return;
        };
        let text = if !pretty {
            tab.http_formatted_text = None;
            tab.http_format_mode = Some("Raw".into());
            resp.body_text
        } else {
            match response_format_body_impl(runtime(cx).state(), body_source_of(&resp), format) {
                Ok(fmt) => {
                    tab.http_formatted_text = Some(fmt.text.clone());
                    tab.http_format_mode = Some(fmt.mode);
                    fmt.text
                }
                Err(_) => resp.body_text,
            }
        };
        self.response_editor.update(cx, |e, cx| {
            e.set_highlighter(crate::ui::highlighter_for_format(format), cx);
            e.set_value(text, window, cx);
        });
        cx.notify();
    }

    pub fn toggle_http_response_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        tab.http_soft_wrap = !tab.http_soft_wrap;
        let wrap = tab.http_soft_wrap;
        self.response_editor
            .update(cx, |e, cx| e.set_soft_wrap(wrap, window, cx));
        cx.notify();
    }

    pub fn apply_jsonpath_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let expr = self.resp_filter_input.read(cx).value().trim().to_owned();
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        tab.http_jsonpath_query = expr.clone();
        let Some(resp) = tab.last_http.clone() else {
            return;
        };
        if expr.is_empty() {
            tab.http_jsonpath_result = None;
            let text = tab
                .http_formatted_text
                .clone()
                .unwrap_or_else(|| resp.body_text.clone());
            self.response_editor
                .update(cx, |e, cx| e.set_value(text, window, cx));
            cx.notify();
            return;
        }
        if expr.starts_with('$') {
            match response_filter_jsonpath_impl(runtime(cx).state(), body_source_of(&resp), &expr) {
                Ok(JsonPathFilterOutcomeDto::Filtered { text, mode }) => {
                    tab.http_jsonpath_result = Some((true, text.clone(), mode));
                    self.response_editor.update(cx, |e, cx| {
                        e.set_highlighter("json", cx);
                        e.set_value(text, window, cx);
                    });
                }
                Ok(JsonPathFilterOutcomeDto::Error { message }) => {
                    tab.http_jsonpath_result = Some((false, String::new(), message));
                }
                Err(err) => {
                    tab.http_jsonpath_result =
                        Some((false, String::new(), err.message().to_owned()));
                }
            }
        } else {
            // Plain text line filter
            let source = tab
                .http_formatted_text
                .clone()
                .unwrap_or_else(|| resp.body_text.clone());
            let q = expr.to_lowercase();
            let matched: Vec<&str> = source
                .lines()
                .filter(|ln| ln.to_lowercase().contains(&q))
                .collect();
            let mode = crate::ui::tr(cx)
                .requests()
                .act_lines_matched(matched.len());
            let out = matched.join("\n");
            tab.http_jsonpath_result = Some((true, out.clone(), mode));
            self.response_editor
                .update(cx, |e, cx| e.set_value(out, window, cx));
        }
        cx.notify();
    }

    pub fn save_response_body_to_file(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_req() else {
            return;
        };
        let Some(resp) = &tab.last_http else {
            return;
        };
        let suggested = response_suggested_file_name(&tab.draft.name);
        let spec = SaveFileSpec {
            title: Some(crate::ui::tr(cx).requests().act_save_resp_title().into()),
            file_name: Some(suggested),
            filters: Vec::new(),
            starting_dir: None,
        };
        let Some(path) = RfdDialog.save_file(&spec) else {
            return;
        };
        let rt = runtime(cx);
        let st = rt.state();
        let res = if let Some(handle) = &resp.body_handle {
            response_save_handle_to_path_impl(st, handle, Some(path))
        } else if let Some(b64) = &resp.body_base64 {
            let cap = st.settings().data.response_body_cap();
            response_save_to_path_impl(
                st,
                cap,
                b64.clone(),
                ResponseBodyEncoding::Base64,
                Some(path),
            )
        } else {
            let cap = st.settings().data.response_body_cap();
            response_save_to_path_impl(
                st,
                cap,
                resp.body_text.clone(),
                ResponseBodyEncoding::Text,
                Some(path),
            )
        };
        match res {
            Ok(saved) if saved.saved => {
                this_status(
                    self,
                    Tone::Success,
                    crate::ui::tr(cx)
                        .requests()
                        .act_saved_to(saved.path.unwrap_or_default()),
                );
            }
            Ok(_) => {}
            Err(err) => this_status(self, Tone::Danger, err.message().to_owned()),
        }
        cx.notify();
    }

    pub fn open_extract_variable_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_req() else {
            return;
        };
        let val = tab
            .http_jsonpath_result
            .as_ref()
            .filter(|(ok, txt, _)| *ok && !txt.trim().is_empty())
            .map(|(_, txt, _)| txt.trim().trim_matches('"').to_owned())
            .or_else(|| {
                tab.last_http
                    .as_ref()
                    .map(|r| r.body_text.chars().take(200).collect())
            })
            .unwrap_or_default();
        self.modal_name_input
            .update(cx, |i, cx| i.set_value("extracted_var", window, cx));
        self.active_modal = Some(ActiveModal::ExtractVariable {
            value: val,
            is_secret: false,
            use_globals: self.active_env_id.is_none(),
        });
        cx.notify();
    }

    pub fn commit_extract_variable(
        &mut self,
        key: String,
        value: String,
        is_secret: bool,
        use_globals: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let (scope, owner_id) = if use_globals {
            (VariableScope::Global, None)
        } else {
            (VariableScope::Environment, self.active_env_id.clone())
        };
        let key_label = key.trim().to_owned();
        let input = DefineVariableInput {
            workspace_id: ws_id.clone(),
            scope,
            owner_id,
            key: key_label.clone(),
            value,
            is_secret,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| environment_variable_define(state, input, "response-extract"),
            move |this, res, cx| {
                match res {
                    Ok(done) => {
                        this.active_modal = None;
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).requests().act_extracted_var(
                                format!("{{{{{key_label}}}}}"),
                                format!("{:?}", done.scope),
                            ),
                        ));
                        this.reload_workspace_no_window(ws_id, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    // ── Binary / Multipart file pickers & validation ─────────────────────────
}

pub(super) fn this_status(view: &mut RequestsView, tone: Tone, msg: String) {
    view.status_msg = Some((tone, msg));
}

pub(super) fn body_source_of(resp: &ExecutionResult) -> BodySource {
    if let Some(handle) = &resp.body_handle {
        BodySource::Handle(handle.clone())
    } else if let Some(b64) = &resp.body_base64 {
        BodySource::Base64(b64.clone())
    } else {
        BodySource::Text(resp.body_text.clone())
    }
}

pub(super) fn classify_error(
    err: &AppError,
    lang: spanbay_desktop::core::model::settings::Language,
) -> RequestFailureInfo {
    let r = crate::ui::Tr(lang).requests();
    match err {
        AppError::Cancelled { bytes_received, .. } => RequestFailureInfo {
            kind_label: r.act_cancelled_kind().into(),
            message: match bytes_received {
                Some(b) if *b > 0 => r.act_stopped_with_bytes(b),
                _ => r.act_stopped_no_bytes().into(),
            },
            attempt_count: None,
            connect_phase: None,
            unresolved_keys: Vec::new(),
            history_unsaved: false,
            is_cancelled: true,
        },
        AppError::Validation {
            message,
            detail,
            keys,
        } => RequestFailureInfo {
            kind_label: r.act_validation_kind().into(),
            message: message.clone(),
            attempt_count: None,
            connect_phase: None,
            unresolved_keys: keys.as_deref().cloned().unwrap_or_default(),
            history_unsaved: detail.history_unsaved.unwrap_or(false),
            is_cancelled: false,
        },
        AppError::Network {
            message,
            connect_phase,
            attempt_count,
            detail,
            ..
        } => RequestFailureInfo {
            kind_label: r.act_network_kind().into(),
            message: message.clone(),
            attempt_count: *attempt_count,
            connect_phase: *connect_phase,
            unresolved_keys: Vec::new(),
            history_unsaved: detail.history_unsaved.unwrap_or(false),
            is_cancelled: false,
        },
        other => RequestFailureInfo {
            kind_label: r.act_exec_fail_kind().into(),
            message: other.message().to_owned(),
            attempt_count: None,
            connect_phase: None,
            unresolved_keys: Vec::new(),
            history_unsaved: false,
            is_cancelled: false,
        },
    }
}
