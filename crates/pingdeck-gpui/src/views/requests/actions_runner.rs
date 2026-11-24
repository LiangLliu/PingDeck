//! Collection Runner execution and Collection/Folder Settings load/save.

use std::collections::BTreeSet;

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::{
    EnvironmentVariableInput, RunCollectionInput, SaveCollectionInput, SaveFolderInput,
    collection_save_checked_impl, current_time_ms, execution_cancel_impl, folder_save_checked_impl,
    run_collection_impl,
};
use pingdeck_desktop::core::error::AppError;
use pingdeck_desktop::core::model::events::RunnerEvent;
use pingdeck_desktop::core::model::script_result::ConsoleLevel;
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};
use crate::ui::Tone;

use super::RequestsView;
use super::model::{
    ActiveModal, ContainerSection, ContainerSettingsTabState, OpenTab, RunnerTabState, ScriptSubTab,
};

impl RequestsView {
    // ── Collection / Folder Runner ───────────────────────────────────────────

    pub fn open_collection_runner_tab(
        &mut self,
        collection_id: String,
        folder_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let Some(col) = self.collections.iter().find(|c| c.id == collection_id) else {
            return;
        };
        let folder_name = folder_id.as_deref().and_then(|fid| {
            self.folders
                .iter()
                .find(|f| f.id.as_str() == fid)
                .map(|f| f.name.clone())
        });
        let scope_ids: BTreeSet<String> = self
            .requests
            .iter()
            .filter(|r| {
                r.collection_id.as_ref().map(|c| c.as_str()) == Some(collection_id.as_str())
                    && match &folder_id {
                        Some(fid) => r.folder_id.as_ref().map(|f| f.as_str()) == Some(fid.as_str()),
                        None => true,
                    }
            })
            .map(|r| r.id.as_str().to_owned())
            .collect();

        let tid = format!("runner-{}-{}", collection_id, current_time_ms());
        let state = RunnerTabState {
            tab_id: tid.clone(),
            collection_id,
            collection_name: col.name.clone(),
            folder_id,
            folder_name,
            environment_id: self.active_env_id.clone(),
            iterations: 1,
            delay_ms: 0,
            stop_on_error: false,
            selected_request_ids: scope_ids,
            running: false,
            cancel_token: None,
            live_items: Vec::new(),
            result: None,
            error: None,
        };
        self.tabs.push(OpenTab::Runner(Box::new(state)));
        self.active_tab_id = Some(tid);
        self.hydrate_active_tab(window, cx);
        cx.notify();
    }

    pub fn start_collection_run(
        &mut self,
        tab_id: String,
        confirmed: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let Some(OpenTab::Runner(run)) = self.tabs.iter_mut().find(|t| t.tab_id() == tab_id) else {
            return;
        };
        if run.running {
            return;
        }
        let cancel_id = format!("run-{}-{}", tab_id, current_time_ms());
        run.running = true;
        run.cancel_token = Some(cancel_id.clone());
        run.live_items.clear();
        run.result = None;
        run.error = None;

        let req_ids = if run.selected_request_ids.is_empty() {
            None
        } else {
            Some(run.selected_request_ids.iter().cloned().collect())
        };
        let input = RunCollectionInput {
            workspace_id: ws_id,
            collection_id: run.collection_id.clone(),
            environment_id: run.environment_id.clone(),
            iterations: Some(run.iterations),
            delay_ms: Some(run.delay_ms),
            stop_on_error: Some(run.stop_on_error),
            data: None,
            confirmed,
            folder_id: run.folder_id.clone(),
            request_ids: req_ids,
        };

        let (sink, rx) = channel_sink::<RunnerEvent>();
        let tid_s = tab_id.clone();
        let sub = subscribe_stream_into(cx, rx, 16, move |this, events, cx| {
            if let Some(OpenTab::Runner(r)) = this.tabs.iter_mut().find(|t| t.tab_id() == tid_s) {
                for RunnerEvent::ItemCompleted { item } in events {
                    r.live_items.push(item);
                }
            }
            cx.notify();
        });
        self._tasks.push(sub);

        let tid_r = tab_id.clone();
        let task = spawn_async_into(
            cx,
            move |state| async move {
                run_collection_impl(&state, input, Some(cancel_id), sink.as_ref()).await
            },
            move |this, res, cx| {
                if let Some(OpenTab::Runner(r)) = this.tabs.iter_mut().find(|t| t.tab_id() == tid_r)
                {
                    r.running = false;
                    r.cancel_token = None;
                }
                match res {
                    Ok(done) => {
                        this.push_console(
                            "RUNNER",
                            if done.failed == 0 {
                                ConsoleLevel::Info
                            } else {
                                ConsoleLevel::Warn
                            },
                            format!(
                                "集合运行完成：{} 通过 / {} 失败（共 {}）",
                                done.passed, done.failed, done.total
                            ),
                            None,
                        );
                        if let Some(OpenTab::Runner(r)) =
                            this.tabs.iter_mut().find(|t| t.tab_id() == tid_r)
                        {
                            r.result = Some(done);
                        }
                    }
                    Err(AppError::ConfirmationRequired { message, .. }) => {
                        this.active_modal = Some(ActiveModal::ConfirmGuard {
                            tab_id: tid_r,
                            message,
                            is_ai: false,
                            is_runner: true,
                            save_to_file: None,
                        });
                    }
                    Err(err) => {
                        if let Some(OpenTab::Runner(r)) =
                            this.tabs.iter_mut().find(|t| t.tab_id() == tid_r)
                        {
                            r.error = Some(err.message().to_owned());
                        }
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
        cx.notify();
    }

    pub fn stop_collection_run(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        if let Some(OpenTab::Runner(r)) = self.tabs.iter().find(|t| t.tab_id() == tab_id)
            && let Some(tok) = r.cancel_token.clone()
        {
            execution_cancel_impl(runtime(cx).state(), tok);
            self.status_msg = Some((Tone::Warning, "正在停止集合运行…".into()));
            cx.notify();
        }
    }

    // ── Collection / Folder Settings Tabs ────────────────────────────────────

    pub fn open_collection_settings_tab(
        &mut self,
        collection_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let tid = format!("col-settings-{collection_id}");
        if self.tabs.iter().any(|t| t.tab_id() == tid) {
            self.active_tab_id = Some(tid);
            self.hydrate_active_tab(window, cx);
            cx.notify();
            return;
        }
        let Some(col) = self
            .collections
            .iter()
            .find(|c| c.id == collection_id)
            .cloned()
        else {
            return;
        };
        let pending_var_values = col.variables.iter().map(|v| v.value.clone()).collect();
        let st = ContainerSettingsTabState {
            tab_id: tid.clone(),
            is_collection: true,
            id: col.id.clone(),
            collection_id: col.id,
            parent_folder_id: None,
            name: col.name,
            description: col.description,
            auth: col.auth,
            variables: col.variables,
            pending_var_values,
            pre_request_script: col.pre_request_script,
            test_script: col.test_script,
            position: col.position,
            revision: col.revision,
            section: ContainerSection::General,
            script_sub: ScriptSubTab::Pre,
            dirty: false,
            saving: false,
        };
        self.tabs.push(OpenTab::Container(Box::new(st)));
        self.active_tab_id = Some(tid);
        self.hydrate_active_tab(window, cx);
        cx.notify();
    }

    pub fn open_folder_settings_tab(
        &mut self,
        folder_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let tid = format!("fld-settings-{folder_id}");
        if self.tabs.iter().any(|t| t.tab_id() == tid) {
            self.active_tab_id = Some(tid);
            self.hydrate_active_tab(window, cx);
            cx.notify();
            return;
        }
        let Some(f) = self
            .folders
            .iter()
            .find(|f| f.id.as_str() == folder_id)
            .cloned()
        else {
            return;
        };
        let st = ContainerSettingsTabState {
            tab_id: tid.clone(),
            is_collection: false,
            id: f.id.as_str().to_owned(),
            collection_id: f.collection_id.as_str().to_owned(),
            parent_folder_id: f.parent_folder_id.map(|p| p.as_str().to_owned()),
            name: f.name,
            description: f.description,
            auth: f.auth,
            variables: Vec::new(),
            pending_var_values: Vec::new(),
            pre_request_script: f.pre_request_script,
            test_script: f.test_script,
            position: f.position,
            revision: f.revision,
            section: ContainerSection::General,
            script_sub: ScriptSubTab::Pre,
            dirty: false,
            saving: false,
        };
        self.tabs.push(OpenTab::Container(Box::new(st)));
        self.active_tab_id = Some(tid);
        self.hydrate_active_tab(window, cx);
        cx.notify();
    }

    pub fn save_container_settings_tab(&mut self, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let Some(OpenTab::Container(c)) = self.active_tab_mut() else {
            return;
        };
        let tid = c.tab_id.clone();
        c.saving = true;
        let is_col = c.is_collection;
        let snapshot = c.clone();
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                if is_col {
                    let vars: Vec<EnvironmentVariableInput> = snapshot
                        .variables
                        .iter()
                        .enumerate()
                        .map(|(i, v)| EnvironmentVariableInput {
                            id: Some(v.id.clone()),
                            key: v.key.clone(),
                            value: snapshot
                                .pending_var_values
                                .get(i)
                                .cloned()
                                .unwrap_or_else(|| v.value.clone()),
                            is_secret: v.is_secret,
                            enabled: v.enabled,
                            note: v.note.clone(),
                        })
                        .collect();
                    let input = SaveCollectionInput {
                        id: snapshot.id,
                        workspace_id: ws_id,
                        name: snapshot.name,
                        description: snapshot.description,
                        auth: snapshot.auth,
                        variables: vars,
                        pre_request_script: snapshot.pre_request_script,
                        test_script: snapshot.test_script,
                        position: snapshot.position,
                    };
                    collection_save_checked_impl(state, input, snapshot.revision.as_deref())
                        .map(|d| d.revision)
                } else {
                    let input = SaveFolderInput {
                        id: snapshot.id,
                        collection_id: snapshot.collection_id,
                        parent_folder_id: snapshot.parent_folder_id,
                        name: snapshot.name,
                        description: snapshot.description,
                        auth: snapshot.auth,
                        pre_request_script: snapshot.pre_request_script,
                        test_script: snapshot.test_script,
                        position: snapshot.position,
                    };
                    folder_save_checked_impl(state, input, snapshot.revision.as_deref())
                        .map(|f| f.revision)
                }
            },
            move |this, res, cx| {
                if let Some(OpenTab::Container(c)) =
                    this.tabs.iter_mut().find(|t| t.tab_id() == tid)
                {
                    c.saving = false;
                    if let Ok(rev) = &res {
                        c.revision = rev.clone();
                        c.dirty = false;
                    }
                }
                match res {
                    Ok(_) => {
                        this.status_msg = Some((Tone::Success, "已保存设置".into()));
                        this.reload_workspace_no_window(ws_reload, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
        cx.notify();
    }

    // ── Import & Export ──────────────────────────────────────────────────────
}
