//! Core environment CRUD, secret clipboard copy, resolved/compare/scan/hosts analysis, and import/export actions.

use std::path::PathBuf;

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::factory_service::environment_new;
use pingdeck_desktop::core::app::{
    EnvironmentVariableInput, ImportConflict, SaveEnvironmentInput, environment_compare_impl,
    environment_declared_hosts_impl, environment_delete_impl, environment_duplicate_impl,
    environment_export_impl, environment_import_text_impl, environment_list_impl,
    environment_resolved_list_impl, environment_restore_impl, environment_save_impl,
    environment_secret_copy_impl, environment_unresolved_scan_impl, settings_get_impl,
};
use pingdeck_desktop::core::error::AppError;
use pingdeck_desktop::services::clipboard;
use pingdeck_desktop::services::{
    FileFilter, FilePickerSpec, HostDialog as _, RfdDialog, SaveFileSpec,
};

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::Tone;

use super::state::{EditableVarRow, EnvironmentsView};

impl EnvironmentsView {
    pub fn reload_all(&mut self, ws_id: String, cx: &mut Context<Self>) {
        self.lang = settings_get_impl(runtime(cx).state()).general.language;
        self.reload_environments(ws_id.clone(), cx);
        self.reload_globals(ws_id.clone(), cx);
        self.reload_cookies(ws_id.clone(), cx);
        self.reload_certificates(ws_id, cx);
    }

    pub fn reload_environments(&mut self, ws_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| environment_list_impl(state, ws_id),
            |this, res, cx| {
                match res {
                    Ok(envs) => {
                        this.environments = envs;
                        let sel_valid = this
                            .selected_env_id
                            .as_deref()
                            .is_some_and(|id| this.environments.iter().any(|e| e.id == id));
                        if !sel_valid {
                            if let Some(first) = this.environments.first().cloned() {
                                this.load_env_into_draft(&first);
                            } else {
                                this.selected_env_id = None;
                                this.env_draft_vars.clear();
                                this.env_dirty = false;
                            }
                        } else if !this.env_dirty
                            && let Some(id) = this.selected_env_id.as_deref()
                            && let Some(cur) =
                                this.environments.iter().find(|e| e.id == id).cloned()
                        {
                            this.load_env_into_draft(&cur);
                        }
                        this.refresh_env_analysis(cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn select_env(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(env) = self.environments.iter().find(|e| e.id == id).cloned() {
            self.load_env_into_draft(&env);
            if self.compare_target_id.as_deref() == Some(id.as_str()) {
                self.compare_target_id = None;
                self.compare_rows.clear();
            }
            self.refresh_env_analysis(cx);
            cx.notify();
        }
    }

    pub fn refresh_env_analysis(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(env_id)) = (self.workspace_id.clone(), self.selected_env_id.clone())
        else {
            return;
        };
        let target = self
            .compare_target_id
            .clone()
            .filter(|t| t != &env_id && self.environments.iter().any(|e| &e.id == t))
            .or_else(|| {
                self.environments
                    .iter()
                    .find(|e| e.id != env_id)
                    .map(|e| e.id.clone())
            });
        self.compare_target_id = target.clone();
        let include_globals = self.compare_include_globals;
        let eid_match = env_id.clone();

        let task = spawn_blocking_into(
            cx,
            move |state| {
                let resolved = environment_resolved_list_impl(
                    state,
                    ws_id.clone(),
                    Some(env_id.clone()),
                    None,
                )
                .unwrap_or_default();
                let unresolved =
                    environment_unresolved_scan_impl(state, ws_id.clone(), Some(env_id.clone()))
                        .unwrap_or_default();
                let hosts = environment_declared_hosts_impl(state, ws_id.clone(), env_id.clone())
                    .unwrap_or_default();
                let compare = if let Some(rhs) = target {
                    environment_compare_impl(state, ws_id, env_id, rhs, Some(include_globals))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                Ok::<_, AppError>((resolved, unresolved, hosts, compare))
            },
            move |this, res, cx| {
                if this.selected_env_id.as_deref() != Some(eid_match.as_str()) {
                    return;
                }
                if let Ok((resolved, unresolved, hosts, compare)) = res {
                    this.resolved_vars = resolved;
                    this.unresolved_vars = unresolved;
                    this.declared_hosts = hosts;
                    this.compare_rows = compare;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn create_env(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_reload = ws_id.clone();
        let n = self.environments.len() + 1;
        let name = format!("Environment {n}");
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let mut env = environment_new(&ws_id)?;
                env.name = name;
                environment_save_impl(
                    state,
                    SaveEnvironmentInput {
                        id: env.id.as_str().to_owned(),
                        workspace_id: ws_id,
                        name: env.name,
                        variables: Vec::new(),
                        confirm_before_send: Some(false),
                        confirm_safe_methods: Some(false),
                        declared_hosts: Some(Vec::new()),
                        base_revision: None,
                    },
                )
            },
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        this.load_env_into_draft(&saved);
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已创建新环境", "Created environment").to_owned(),
                        ));
                        this.reload_environments(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn save_env(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(env_id)) = (self.workspace_id.clone(), self.selected_env_id.clone())
        else {
            return;
        };
        let name = self.env_name_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.status_msg = Some((
                Tone::Warning,
                self.t("环境名称不能为空", "Environment name cannot be blank")
                    .to_owned(),
            ));
            cx.notify();
            return;
        }
        self.env_draft_name = name.clone();
        let vars: Vec<EnvironmentVariableInput> =
            self.env_draft_vars.iter().map(row_to_input).collect();
        let input = SaveEnvironmentInput {
            id: env_id,
            workspace_id: ws_id.clone(),
            name,
            variables: vars,
            confirm_before_send: Some(self.env_draft_confirm),
            confirm_safe_methods: Some(self.env_draft_confirm_safe),
            declared_hosts: Some(self.env_draft_hosts.clone()),
            base_revision: self.env_draft_revision.clone(),
        };
        let task = spawn_blocking_into(
            cx,
            move |state| environment_save_impl(state, input),
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        this.load_env_into_draft(&saved);
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已保存环境配置", "Saved environment").to_owned(),
                        ));
                        this.reload_environments(ws_id, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn duplicate_selected_env(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(env_id)) = (self.workspace_id.clone(), self.selected_env_id.clone())
        else {
            return;
        };
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| environment_duplicate_impl(state, ws_id, env_id),
            move |this, res, cx| {
                match res {
                    Ok(copy) => {
                        this.load_env_into_draft(&copy);
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已复制环境（含机密变量）", "Duplicated environment")
                                .to_owned(),
                        ));
                        this.reload_environments(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_env(&mut self, id: String, name: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| environment_delete_impl(state, ws_id, id),
            move |this, res, cx| {
                this.active_modal = None;
                match res {
                    Ok(token) => {
                        this.selected_env_id = None;
                        this.env_dirty = false;
                        this.env_undo = Some((token, name));
                        this.reload_environments(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn undo_delete_env(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some((token, _))) = (self.workspace_id.clone(), self.env_undo.take())
        else {
            return;
        };
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| environment_restore_impl(state, ws_id, token),
            move |this, res, cx| {
                match res {
                    Ok(restored) => {
                        this.load_env_into_draft(&restored);
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已恢复已删除的环境", "Restored deleted environment")
                                .to_owned(),
                        ));
                        this.reload_environments(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn copy_secret_variable(
        &mut self,
        env_id: Option<String>,
        var_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                environment_secret_copy_impl(state, &ws_id, env_id.as_deref(), &var_id, |secret| {
                    clipboard::set_secret_text(secret)
                })
            },
            |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.status_msg = Some((
                            Tone::Success,
                            this.t(
                                "已直接将机密值复制到剪贴板",
                                "Copied secret directly to clipboard",
                            )
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

    pub fn export_env(
        &mut self,
        id: String,
        name: String,
        include_secrets: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let suggested = format!("{name}.postman_environment.json");
        let Some(path): Option<PathBuf> = RfdDialog.save_file(&SaveFileSpec {
            title: None,
            file_name: Some(suggested),
            filters: vec![FileFilter {
                name: "JSON".to_owned(),
                extensions: vec!["json".to_owned()],
            }],
            starting_dir: None,
        }) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let json = environment_export_impl(state, ws_id, id, include_secrets)?;
                pingdeck_desktop::core::platform::write_private_file(&path, json)
                    .map_err(|e| pingdeck_desktop::core::error::AppError::io(e.to_string()))?;
                Ok::<_, AppError>(())
            },
            |this, res, cx| {
                this.active_modal = None;
                match res {
                    Ok(()) => {
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已导出环境文件", "Exported environment file")
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

    pub fn import_env_from_editor(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let payload = self.import_editor.read(cx).value().to_string();
        if payload.trim().is_empty() {
            return;
        }
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                environment_import_text_impl(state, ws_id, payload, ImportConflict::KeepBoth)
            },
            move |this, res, cx| {
                match res {
                    Ok(env) => {
                        this.active_modal = None;
                        this.load_env_into_draft(&env);
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已导入环境", "Imported environment").to_owned(),
                        ));
                        this.reload_environments(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn load_import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = RfdDialog.pick_file(&FilePickerSpec {
            title: None,
            filters: vec![FileFilter {
                name: "JSON / .env".to_owned(),
                extensions: vec!["json".to_owned(), "env".to_owned(), "txt".to_owned()],
            }],
            starting_dir: None,
        }) else {
            return;
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            self.import_editor
                .update(cx, |s, cx| s.set_value(text, window, cx));
            cx.notify();
        }
    }
}

pub fn row_to_input(row: &EditableVarRow) -> EnvironmentVariableInput {
    EnvironmentVariableInput {
        id: row.id.clone(),
        key: row.key.clone(),
        value: if row.is_secret {
            row.value.clone().filter(|v| !v.is_empty())
        } else {
            Some(row.value.clone().unwrap_or_default())
        },
        is_secret: row.is_secret,
        enabled: row.enabled,
        note: row.note.clone(),
    }
}
