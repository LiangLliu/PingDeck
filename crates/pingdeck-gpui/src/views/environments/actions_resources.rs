//! Globals, Cookie Jar, and TLS Certificate actions for `EnvironmentsView`.

use std::path::PathBuf;

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::factory_service::new_entity_id;
use pingdeck_desktop::core::app::{
    GlobalsSaveInput, TlsCertificateDto, certificate_delete_impl, certificate_list_impl,
    certificate_save_impl, cookie_jar_delete_expired_impl, cookie_jar_delete_many_impl,
    cookie_jar_export_save_suggested_filename, cookie_jar_export_save_to_path_impl,
    cookie_jar_import_impl, cookie_jar_import_open_file_from_path_impl, cookie_jar_list_impl,
    cookie_jar_restore_many_impl, cookie_jar_update_flags_impl, cookie_jar_update_value_impl,
    globals_get_impl, globals_save_impl,
};
use pingdeck_desktop::core::model::WorkspaceId;
use pingdeck_desktop::core::model::models::{TlsCertificateId, TlsCertificateKind};
use pingdeck_desktop::services::{
    FileFilter, FilePickerSpec, HostDialog as _, RfdDialog, SaveFileSpec,
};

use crate::bridge::spawn_blocking_into;
use crate::ui::Tone;

use super::actions::row_to_input;
use super::state::{EditableVarRow, EnvironmentsView};

impl EnvironmentsView {
    pub fn reload_globals(&mut self, ws_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| globals_get_impl(state, ws_id),
            |this, res, cx| {
                if let Ok(dto) = res {
                    if !this.globals_dirty {
                        this.globals_draft =
                            dto.variables.iter().map(EditableVarRow::from).collect();
                    }
                    this.globals_dto = Some(dto);
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn save_globals(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let base_revision = self.globals_dto.as_ref().map(|g| g.revision.clone());
        let input = GlobalsSaveInput {
            workspace_id: ws_id,
            variables: self.globals_draft.iter().map(row_to_input).collect(),
            base_revision,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| globals_save_impl(state, input),
            |this, res, cx| {
                match res {
                    Ok(dto) => {
                        this.globals_draft =
                            dto.variables.iter().map(EditableVarRow::from).collect();
                        this.globals_dto = Some(dto);
                        this.globals_dirty = false;
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已保存全局变量", "Saved globals").to_owned(),
                        ));
                        this.refresh_env_analysis(cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn reload_cookies(&mut self, ws_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| cookie_jar_list_impl(state, ws_id),
            |this, res, cx| {
                if let Ok(list) = res {
                    this.cookies = list;
                    if this
                        .selected_cookie_id
                        .as_deref()
                        .is_none_or(|id| !this.cookies.iter().any(|c| c.id.as_str() == id))
                    {
                        this.selected_cookie_id =
                            this.cookies.first().map(|c| c.id.as_str().to_owned());
                        this.pending_sync = true;
                    }
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn save_cookie_value(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let val = self.cookie_edit_val.read(cx).value().to_string();
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| cookie_jar_update_value_impl(state, ws_id, id, val),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已更新 Cookie 值", "Updated cookie value")
                                .to_owned(),
                        ));
                        this.reload_cookies(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn update_cookie_flags(
        &mut self,
        id: String,
        secure: bool,
        http_only: bool,
        same_site: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                cookie_jar_update_flags_impl(state, ws_id, id, secure, http_only, same_site)
            },
            move |this, res, cx| {
                match res {
                    Ok(_) => this.reload_cookies(ws_reload, cx),
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_cookie(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let backup: Vec<_> = self
            .cookies
            .iter()
            .filter(|c| c.id.as_str() == id.as_str())
            .cloned()
            .collect();
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| cookie_jar_delete_many_impl(state, ws_id, vec![id]),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.deleted_cookies_undo = Some(backup);
                        this.reload_cookies(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn undo_delete_cookies(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(entries)) =
            (self.workspace_id.clone(), self.deleted_cookies_undo.take())
        else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| cookie_jar_restore_many_impl(state, entries),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已恢复删除的 Cookie", "Restored deleted cookie(s)")
                                .to_owned(),
                        ));
                        this.reload_cookies(ws_id, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_expired_cookies(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| cookie_jar_delete_expired_impl(state, ws_id),
            move |this, res, cx| {
                match res {
                    Ok(ids) => {
                        this.status_msg = Some((
                            Tone::Success,
                            if this.is_zh() {
                                format!("已清理 {} 条过期 Cookie", ids.len())
                            } else {
                                format!("Removed {} expired cookie(s)", ids.len())
                            },
                        ));
                        this.reload_cookies(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn add_cookie_from_modal(
        &mut self,
        secure: bool,
        http_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let domain = self.ck_domain_input.read(cx).value().trim().to_owned();
        let name = self.ck_name_input.read(cx).value().trim().to_owned();
        let value = self.ck_value_input.read(cx).value().to_string();
        let path_raw = self.ck_path_input.read(cx).value().trim().to_owned();
        let path = if path_raw.is_empty() {
            "/".to_owned()
        } else {
            path_raw
        };
        if domain.is_empty() || name.is_empty() {
            return;
        }
        let prefix = if http_only { "#HttpOnly_" } else { "" };
        let sec = if secure { "TRUE" } else { "FALSE" };
        let line = format!("{prefix}{domain}\tFALSE\t{path}\t{sec}\t0\t{name}\t{value}\n");
        let ws_reload = ws_id.clone();
        self.ck_name_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.ck_value_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        let task = spawn_blocking_into(
            cx,
            move |state| cookie_jar_import_impl(state, ws_id, line),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.active_modal = None;
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已添加 Cookie", "Added cookie").to_owned(),
                        ));
                        this.reload_cookies(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn import_cookies_file(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let path: Option<PathBuf> = RfdDialog.pick_file(&FilePickerSpec {
            title: None,
            filters: vec![FileFilter {
                name: "JSON / cookies.txt".to_owned(),
                extensions: vec!["json".to_owned(), "txt".to_owned()],
            }],
            starting_dir: None,
        });
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| cookie_jar_import_open_file_from_path_impl(state, ws_id, path),
            move |this, res, cx| {
                match res {
                    Ok(r) if r.opened => {
                        this.status_msg = Some((
                            Tone::Success,
                            if this.is_zh() {
                                format!("已导入 {} 条 Cookie", r.imported_ids.len())
                            } else {
                                format!("Imported {} cookie(s)", r.imported_ids.len())
                            },
                        ));
                        this.reload_cookies(ws_reload, cx);
                    }
                    Ok(_) => {}
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn export_cookies_file(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let suggested = cookie_jar_export_save_suggested_filename(&ws_id)
            .unwrap_or_else(|_| "pingdeck-cookie-jar.json".to_owned());
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
            move |state| cookie_jar_export_save_to_path_impl(state, ws_id, path),
            |this, res, cx| {
                match res {
                    Ok(r) if r.saved => {
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已导出 Cookie 罐", "Exported cookie jar").to_owned(),
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

    pub fn reload_certificates(&mut self, ws_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| certificate_list_impl(state, ws_id),
            |this, res, cx| {
                if let Ok(certs) = res {
                    this.certificates = certs;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn add_certificate(
        &mut self,
        kind: TlsCertificateKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let host_pattern = self.cert_host_input.read(cx).value().trim().to_owned();
        let cert_path = self.cert_path_input.read(cx).value().trim().to_owned();
        let key_raw = self.cert_key_input.read(cx).value().trim().to_owned();
        let pass_raw = self.cert_pass_input.read(cx).value().to_string();
        if cert_path.is_empty() {
            return;
        }
        let Ok(id) = TlsCertificateId::new(new_entity_id("cert")) else {
            return;
        };
        let Ok(workspace_id) = WorkspaceId::new(&ws_id) else {
            return;
        };
        let dto = TlsCertificateDto {
            id,
            workspace_id,
            kind,
            host_pattern,
            cert_path,
            key_path: (!key_raw.is_empty()).then_some(key_raw),
            has_passphrase: false,
            passphrase: (!pass_raw.is_empty()).then_some(pass_raw),
            enabled: true,
        };
        self.cert_pass_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        let task = spawn_blocking_into(
            cx,
            move |state| certificate_save_impl(state, dto),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.active_modal = None;
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已保存 TLS 证书", "Saved TLS certificate")
                                .to_owned(),
                        ));
                        this.reload_certificates(ws_id, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn toggle_certificate(&mut self, mut cert: TlsCertificateDto, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        cert.enabled = !cert.enabled;
        cert.passphrase = None;
        let task = spawn_blocking_into(
            cx,
            move |state| certificate_save_impl(state, cert),
            move |this, res, cx| {
                match res {
                    Ok(_) => this.reload_certificates(ws_id, cx),
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_certificate(&mut self, cert_id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| certificate_delete_impl(state, ws_id, cert_id),
            move |this, res, cx| {
                this.active_modal = None;
                match res {
                    Ok(()) => {
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已删除 TLS 证书", "Deleted TLS certificate")
                                .to_owned(),
                        ));
                        this.reload_certificates(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}
