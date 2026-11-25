//! Recording, live-data, import/export, and route/response/bucket/callback draft mutations for `MockView`.

use std::path::PathBuf;

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::factory_service::{
    mock_bucket_new, mock_callback_new, mock_response_duplicate, mock_response_new,
    mock_route_duplicate, mock_route_test_request, mock_rule_new, mock_server_add_route,
};
use pingdeck_desktop::core::app::{
    mock_openapi_import_impl, mock_recording_routes_impl, mock_route_from_log_impl,
    mock_server_clear_logs_impl, mock_server_data_get_impl, mock_server_data_reset_impl,
    mock_server_export_save_to_path_impl, mock_server_export_suggested_filename,
    mock_server_import_json_impl, mock_server_import_mockoon_impl, mock_server_save_impl,
};
use pingdeck_desktop::core::error::AppError;
use pingdeck_desktop::core::model::items::MockTransactionLog;
use pingdeck_desktop::services::{
    FileFilter, FilePickerSpec, HostDialog as _, RfdDialog, SaveFileSpec,
};

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::Tone;

use super::state::{MockHandoff, MockImportFormat, MockModal, MockSubTab, MockView};

impl MockView {
    pub fn save_recorded_routes(&mut self, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let (Some(ws_id), Some(srv)) = (self.workspace_id.clone(), self.draft.clone()) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let res = mock_recording_routes_impl(state, ws_id, srv.clone())?;
                if res.routes.is_empty() {
                    return Ok::<_, AppError>((srv, 0, res.skipped));
                }
                let added = res.routes.len();
                let mut updated = srv;
                updated.routes.extend(res.routes);
                let saved = mock_server_save_impl(state, updated)?;
                Ok::<_, AppError>((saved, added, res.skipped))
            },
            |this, res, cx| {
                match res {
                    Ok((saved, added, skipped)) => {
                        this.draft = Some(saved.clone());
                        if let Some(slot) = this.servers.iter_mut().find(|s| s.id == saved.id) {
                            *slot = saved;
                        }
                        this.dirty = false;
                        this.pending_sync = true;
                        this.status_msg = Some((
                            Tone::Success,
                            if this.is_zh() {
                                format!("已从录制生成 {added} 条路由（跳过已覆盖 {skipped} 条）")
                            } else {
                                format!(
                                    "Generated {added} route(s) from recording ({skipped} already covered)"
                                )
                            },
                        ));
                        if added > 0 {
                            this.active_tab = MockSubTab::Routes;
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn create_route_from_log(&mut self, log: MockTransactionLog, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(mut srv) = self.draft.clone() else {
            return;
        };
        let route = mock_route_from_log_impl(runtime(cx).state(), &srv, &log);
        let route_id = route.id.clone();
        srv.routes.push(route);
        self.draft = Some(srv);
        self.selected_route_id = Some(route_id);
        self.selected_response_id = None;
        self.dirty = true;
        self.pending_sync = true;
        self.active_tab = MockSubTab::Routes;
        self.status_msg = Some((
            Tone::Success,
            self.t(
                "已由日志创建路由，请点击保存生效",
                "Created route from log — save to apply",
            )
            .to_owned(),
        ));
        cx.notify();
    }

    pub fn clear_logs(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(sid)) = (self.workspace_id.clone(), self.selected_id.clone()) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| mock_server_clear_logs_impl(state, ws_id, sid),
            |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.logs.clear();
                        this.selected_log_id = None;
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn refresh_live_data(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(sid)) = (self.workspace_id.clone(), self.selected_id.clone()) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| mock_server_data_get_impl(state, ws_id, sid),
            |this, res, cx| {
                if let Ok(data) = res {
                    this.live_data = Some(data);
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn reset_live_data(&mut self, bucket_id: Option<String>, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(sid)) = (self.workspace_id.clone(), self.selected_id.clone()) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| mock_server_data_reset_impl(state, ws_id, sid, bucket_id),
            |this, res, cx| {
                match res {
                    Ok(data) => {
                        this.live_data = Some(data);
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已重置运行时数据桶", "Reset live data bucket(s)")
                                .to_owned(),
                        ));
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn export_selected(&mut self, include_credentials: bool, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(sid)) = (self.workspace_id.clone(), self.selected_id.clone()) else {
            return;
        };
        let suggested = mock_server_export_suggested_filename();
        let path: Option<PathBuf> = RfdDialog.save_file(&SaveFileSpec {
            title: None,
            file_name: Some(suggested),
            filters: vec![FileFilter {
                name: "JSON".to_owned(),
                extensions: vec!["json".to_owned()],
            }],
            starting_dir: None,
        });
        let task = spawn_blocking_into(
            cx,
            move |state| {
                mock_server_export_save_to_path_impl(
                    state,
                    ws_id,
                    Some(vec![sid]),
                    include_credentials,
                    path,
                )
            },
            |this, res, cx| {
                this.active_modal = None;
                match res {
                    Ok(r) if r.saved => {
                        let note = if r.warnings.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", r.warnings.join("; "))
                        };
                        this.status_msg = Some((
                            Tone::Success,
                            format!(
                                "{}{note}",
                                this.t("已导出 Mock 服务配置", "Exported mock server")
                            ),
                        ));
                    }
                    Ok(_) => {}
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn run_import(&mut self, format: MockImportFormat, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let payload = self.import_editor.read(cx).value().to_string();
        if payload.trim().is_empty() {
            self.status_msg = Some((
                Tone::Warning,
                self.t(
                    "请先粘贴或选择要导入的文件内容",
                    "Paste or load a file first",
                )
                .to_owned(),
            ));
            cx.notify();
            return;
        }
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| match format {
                MockImportFormat::PingDeckJson => {
                    let res = mock_server_import_json_impl(state, ws_id, payload)?;
                    Ok::<_, AppError>((
                        res.servers.first().map(|s| s.id.clone()),
                        res.servers.len(),
                        res.warnings,
                    ))
                }
                MockImportFormat::Mockoon => {
                    let res = mock_server_import_mockoon_impl(state, ws_id, payload)?;
                    Ok::<_, AppError>((Some(res.server.id), 1, res.warnings))
                }
                MockImportFormat::OpenApi => {
                    let res = mock_openapi_import_impl(state, ws_id, payload)?;
                    Ok::<_, AppError>((Some(res.server.id), 1, res.warnings))
                }
            },
            move |this, res, cx| {
                match res {
                    Ok((first_id, count, warnings)) => {
                        this.active_modal = None;
                        if let Some(id) = first_id {
                            this.selected_id = Some(id);
                            this.dirty = false;
                        }
                        let warn_suffix = if warnings.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", warnings.join("; "))
                        };
                        this.status_msg = Some((
                            Tone::Success,
                            if this.is_zh() {
                                format!("已导入 {count} 个 Mock 服务{warn_suffix}")
                            } else {
                                format!("Imported {count} mock server(s){warn_suffix}")
                            },
                        ));
                        this.reload(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn load_import_from_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = RfdDialog.pick_file(&FilePickerSpec {
            title: None,
            filters: vec![FileFilter {
                name: "JSON / YAML".to_owned(),
                extensions: vec!["json".to_owned(), "yaml".to_owned(), "yml".to_owned()],
            }],
            starting_dir: None,
        }) else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.import_editor
                    .update(cx, |s, cx| s.set_value(text, window, cx));
                cx.notify();
            }
            Err(e) => {
                self.status_msg = Some((Tone::Danger, e.to_string()));
                cx.notify();
            }
        }
    }

    pub fn add_route(&mut self, kind: &'static str, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(srv) = self.draft.clone() else {
            return;
        };
        match mock_server_add_route(srv, kind) {
            Ok(updated) => {
                let new_id = updated.routes.last().map(|r| r.id.clone());
                self.draft = Some(updated);
                self.selected_route_id = new_id;
                self.selected_response_id = None;
                self.dirty = true;
                self.pending_sync = true;
            }
            Err(e) => self.status_msg = Some((Tone::Danger, e.message().to_owned())),
        }
        cx.notify();
    }

    pub fn duplicate_route(&mut self, route_id: String, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(srv) = self.draft.as_mut() else {
            return;
        };
        match mock_route_duplicate(srv.routes.clone(), &route_id) {
            Ok(routes) => {
                let new_id = routes.last().map(|r| r.id.clone());
                srv.routes = routes;
                self.selected_route_id = new_id;
                self.selected_response_id = None;
                self.dirty = true;
                self.pending_sync = true;
            }
            Err(e) => self.status_msg = Some((Tone::Danger, e.message().to_owned())),
        }
        cx.notify();
    }

    pub fn delete_route(&mut self, route_id: String, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(srv) = self.draft.as_mut() else {
            return;
        };
        srv.routes.retain(|r| r.id != route_id);
        for (i, r) in srv.routes.iter_mut().enumerate() {
            r.position = i as i64;
        }
        self.dirty = true;
        self.pending_sync = true;
        cx.notify();
    }

    pub fn test_route_in_requests(&mut self, route_id: &str, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(srv) = &self.draft else {
            return;
        };
        let Some(route) = srv.routes.iter().find(|r| r.id == route_id) else {
            return;
        };
        let req = mock_route_test_request(srv, route);
        self.pending_handoff.set(Some(MockHandoff::OpenTestRequest {
            name: format!("Mock {} {}", req.method, route.path),
            method: req.method,
            url: req.url,
        }));
        cx.notify();
    }

    pub fn add_response(&mut self, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(route) = self.selected_route_mut() else {
            return;
        };
        let resp = mock_response_new(route.responses.len());
        let id = resp.id.clone();
        route.responses.push(resp);
        self.selected_response_id = Some(id);
        self.dirty = true;
        self.pending_sync = true;
        cx.notify();
    }

    pub fn duplicate_selected_response(&mut self, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(rid) = self.selected_response_id.clone() else {
            return;
        };
        let Some(route) = self.selected_route().cloned() else {
            return;
        };
        match mock_response_duplicate(route, &rid) {
            Ok(updated) => {
                let new_id = updated.responses.last().map(|r| r.id.clone());
                if let Some(slot) = self.selected_route_mut() {
                    *slot = updated;
                }
                self.selected_response_id = new_id;
                self.dirty = true;
                self.pending_sync = true;
            }
            Err(e) => self.status_msg = Some((Tone::Danger, e.message().to_owned())),
        }
        cx.notify();
    }

    pub fn add_rule_to_response(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let modifier = self.rule_mod_input.read(cx).value().to_string();
        let value = self.rule_val_input.read(cx).value().to_string();
        let target = self.new_rule_target;
        let op = self.new_rule_op;
        let invert = self.new_rule_invert;
        let Some(resp) = self.selected_response_mut() else {
            return;
        };
        let mut rule = mock_rule_new(resp.rules.len());
        rule.target = target;
        rule.modifier = modifier;
        rule.operator = op;
        rule.value = value;
        rule.invert = invert;
        resp.rules.push(rule);
        self.rule_mod_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.rule_val_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.dirty = true;
        cx.notify();
    }

    pub fn add_bucket(&mut self, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(srv) = self.draft.as_mut() else {
            return;
        };
        let name = format!("bucket_{}", srv.data_buckets.len() + 1);
        let b = mock_bucket_new(&name);
        let id = b.id.clone();
        srv.data_buckets.push(b);
        self.selected_bucket_id = Some(id);
        self.dirty = true;
        self.pending_sync = true;
        cx.notify();
    }

    pub fn add_callback(&mut self, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(srv) = self.draft.as_mut() else {
            return;
        };
        let cb = mock_callback_new(srv.callbacks.len());
        let id = cb.id.clone();
        srv.callbacks.push(cb);
        self.selected_callback_id = Some(id);
        self.dirty = true;
        self.pending_sync = true;
        cx.notify();
    }

    pub fn open_import_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.import_editor
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.active_modal = Some(MockModal::Import {
            format: MockImportFormat::PingDeckJson,
        });
        cx.notify();
    }
}
