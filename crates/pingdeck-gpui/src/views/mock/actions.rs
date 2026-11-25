//! Server lifecycle, status polling, live log stream subscription, and recording toggle for `MockView`.

use gpui_kit::Context;
use pingdeck_desktop::core::app::factory_service::{mock_server_add_route, mock_server_new};
use pingdeck_desktop::core::app::{
    environment_list_impl, mock_port_suggest_impl, mock_recording_set_impl,
    mock_server_data_get_impl, mock_server_delete_impl, mock_server_duplicate_impl,
    mock_server_list_impl, mock_server_logs_impl, mock_server_logs_subscribe_impl,
    mock_server_logs_unsubscribe_impl, mock_server_save_impl, mock_server_start_impl,
    mock_server_state_reset_impl, mock_server_status_list_impl, mock_server_stop_impl,
    settings_get_impl,
};
use pingdeck_desktop::core::error::AppError;
use pingdeck_desktop::core::model::events::MockLogEvent;
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_blocking_into, subscribe_stream_into};
use crate::ui::Tone;

use super::state::MockView;

impl MockView {
    pub fn reload(&mut self, ws_id: String, cx: &mut Context<Self>) {
        self.lang = settings_get_impl(runtime(cx).state()).general.language;
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let servers = mock_server_list_impl(state, ws_id.clone())?;
                let statuses = mock_server_status_list_impl(state, ws_id.clone())?;
                let envs = environment_list_impl(state, ws_id).unwrap_or_default();
                Ok::<_, AppError>((servers, statuses, envs))
            },
            |this, res, cx| {
                match res {
                    Ok((servers, statuses, envs)) => {
                        this.servers = servers;
                        this.statuses = statuses
                            .into_iter()
                            .map(|s| (s.server_id.clone(), s))
                            .collect();
                        this.environments = envs;
                        let sel_valid = this
                            .selected_id
                            .as_deref()
                            .is_some_and(|id| this.servers.iter().any(|s| s.id == id));
                        if !sel_valid {
                            this.selected_id = this.servers.first().map(|s| s.id.clone());
                            this.draft = this.servers.first().cloned();
                            this.dirty = false;
                            this.pending_sync = true;
                        } else if !this.dirty
                            && let Some(id) = this.selected_id.as_deref()
                            && let Some(srv) = this.servers.iter().find(|s| s.id == id)
                        {
                            this.draft = Some(srv.clone());
                            this.pending_sync = true;
                        }
                        if let Some(id) = this.selected_id.clone() {
                            this.attach_live_server(&id, cx);
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn select_server(&mut self, id: String, cx: &mut Context<Self>) {
        if self.selected_id.as_deref() == Some(id.as_str()) {
            return;
        }
        self.selected_id = Some(id.clone());
        self.draft = self.servers.iter().find(|s| s.id == id).cloned();
        self.dirty = false;
        self.selected_route_id = None;
        self.selected_response_id = None;
        self.selected_bucket_id = None;
        self.selected_callback_id = None;
        self.selected_log_id = None;
        self.logs.clear();
        self.live_data = None;
        self.pending_sync = true;
        self.attach_live_server(&id, cx);
        cx.notify();
    }

    pub fn unsubscribe_logs(&mut self, cx: &Context<Self>) {
        if let Some(sub_id) = self.log_sub_id.take() {
            let _ = mock_server_logs_unsubscribe_impl(runtime(cx).state(), sub_id);
        }
        self.log_stream_task = None;
    }

    pub fn attach_live_server(&mut self, server_id: &str, cx: &mut Context<Self>) {
        self.unsubscribe_logs(cx);
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let sid = server_id.to_owned();
        let ws_for_logs = ws_id.clone();
        let sid_for_logs = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let logs = mock_server_logs_impl(state, ws_for_logs.clone(), sid_for_logs.clone())
                    .unwrap_or_default();
                let live = mock_server_data_get_impl(state, ws_for_logs, sid_for_logs).ok();
                Ok::<_, AppError>((logs, live))
            },
            move |this, res, cx| {
                if this.selected_id.as_deref() != Some(sid.as_str()) {
                    return;
                }
                if let Ok((logs, live)) = res {
                    this.logs = logs;
                    this.live_data = live;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);

        if self.is_running(server_id) {
            let (sink, rx) = channel_sink::<MockLogEvent>();
            if let Ok(sub_id) = mock_server_logs_subscribe_impl(
                runtime(cx).state(),
                ws_id,
                server_id.to_owned(),
                sink,
            ) {
                self.log_sub_id = Some(sub_id);
                let sid_match = server_id.to_owned();
                self.log_stream_task = Some(subscribe_stream_into(
                    cx,
                    rx,
                    32,
                    move |this, events, cx| {
                        if this.selected_id.as_deref() != Some(sid_match.as_str()) {
                            return;
                        }
                        for ev in events {
                            match ev {
                                MockLogEvent::Batch { entries } => {
                                    for entry in entries {
                                        if !this.logs.iter().any(|l| l.id == entry.id) {
                                            this.logs.insert(0, entry);
                                        }
                                    }
                                    this.logs.truncate(500);
                                }
                                MockLogEvent::Callback { log_id, result } => {
                                    if let Some(row) = this.logs.iter_mut().find(|l| l.id == log_id)
                                    {
                                        row.callbacks.push(result);
                                    }
                                }
                            }
                        }
                        if let Some(ws) = this.workspace_id.clone() {
                            this.refresh_statuses(ws, cx);
                        }
                        cx.notify();
                    },
                ));
            }
        }
    }

    pub fn refresh_statuses(&mut self, ws_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| mock_server_status_list_impl(state, ws_id),
            |this, res, cx| {
                if let Ok(statuses) = res {
                    this.statuses = statuses
                        .into_iter()
                        .map(|s| (s.server_id.clone(), s))
                        .collect();
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn create_server(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let mut srv = mock_server_new(state, &ws_id)?;
                srv = mock_server_add_route(srv, "GET")?;
                mock_server_save_impl(state, srv)
            },
            |this, res, cx| {
                match res {
                    Ok(srv) => {
                        let id = srv.id.clone();
                        this.servers.push(srv.clone());
                        this.selected_id = Some(id.clone());
                        this.draft = Some(srv);
                        this.dirty = false;
                        this.pending_sync = true;
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已新建 Mock 服务", "Created mock server").to_owned(),
                        ));
                        if let Some(ws) = this.workspace_id.clone() {
                            this.refresh_statuses(ws, cx);
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn save_draft(&mut self, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(draft) = self.draft.clone() else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| mock_server_save_impl(state, draft),
            |this, res, cx| {
                match res {
                    Ok(saved) => {
                        if let Some(slot) = this.servers.iter_mut().find(|s| s.id == saved.id) {
                            *slot = saved.clone();
                        } else {
                            this.servers.push(saved.clone());
                        }
                        this.draft = Some(saved.clone());
                        this.dirty = false;
                        this.pending_sync = true;
                        let msg = if this.is_running(&saved.id) {
                            this.t(
                                "已保存并实时热更新运行中的服务",
                                "Saved and hot-reloaded running server",
                            )
                        } else {
                            this.t("已保存 Mock 服务配置", "Saved mock server")
                        };
                        this.status_msg = Some((Tone::Success, msg.to_owned()));
                        if let Some(ws) = this.workspace_id.clone() {
                            this.refresh_statuses(ws, cx);
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn toggle_start_stop(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(sid)) = (self.workspace_id.clone(), self.selected_id.clone()) else {
            return;
        };
        let running = self.is_running(&sid);
        if !running && self.dirty {
            self.snapshot_inputs(cx);
        }
        let draft_to_save = (!running && self.dirty)
            .then(|| self.draft.clone())
            .flatten();
        let sid_ret = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                if let Some(d) = draft_to_save {
                    let _ = mock_server_save_impl(state, d)?;
                }
                if running {
                    mock_server_stop_impl(state, ws_id, sid)
                } else {
                    mock_server_start_impl(state, ws_id, sid)
                }
            },
            move |this, res, cx| {
                match res {
                    Ok(status) => {
                        let now_running = status.running;
                        this.statuses.insert(status.server_id.clone(), status);
                        if now_running {
                            this.dirty = false;
                            this.attach_live_server(&sid_ret, cx);
                            this.status_msg = Some((
                                Tone::Success,
                                this.t("Mock 服务已启动", "Mock server started").to_owned(),
                            ));
                        } else {
                            this.unsubscribe_logs(cx);
                            this.live_data = None;
                            this.status_msg = Some((
                                Tone::Neutral,
                                this.t("Mock 服务已停止", "Mock server stopped").to_owned(),
                            ));
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn duplicate_selected(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(sid)) = (self.workspace_id.clone(), self.selected_id.clone()) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| mock_server_duplicate_impl(state, ws_id, sid),
            |this, res, cx| {
                match res {
                    Ok(copy) => {
                        let id = copy.id.clone();
                        this.servers.push(copy.clone());
                        this.selected_id = Some(id);
                        this.draft = Some(copy);
                        this.dirty = false;
                        this.pending_sync = true;
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已复制 Mock 服务", "Duplicated mock server")
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

    pub fn delete_server(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| mock_server_delete_impl(state, ws_id, id),
            move |this, res, cx| {
                this.active_modal = None;
                match res {
                    Ok(()) => {
                        this.selected_id = None;
                        this.draft = None;
                        this.dirty = false;
                        this.status_msg = Some((
                            Tone::Success,
                            this.t("已删除 Mock 服务", "Deleted mock server").to_owned(),
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

    pub fn reset_runtime_state(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(sid)) = (self.workspace_id.clone(), self.selected_id.clone()) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| mock_server_state_reset_impl(state, ws_id, sid),
            |this, res, cx| {
                match res {
                    Ok(st) => {
                        this.statuses.insert(st.server_id.clone(), st);
                        this.status_msg = Some((
                            Tone::Success,
                            this.t(
                                "已重置计数器与顺序游标",
                                "Reset counters and sequence cursors",
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

    pub fn suggest_free_port(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let from_port = self
            .draft
            .as_ref()
            .map_or(3001, |s| s.port.saturating_add(1));
        let task = spawn_blocking_into(
            cx,
            move |state| mock_port_suggest_impl(state, ws_id, from_port),
            |this, res, cx| {
                match res {
                    Ok(port) => {
                        if let Some(srv) = this.draft.as_mut() {
                            srv.port = port;
                            this.dirty = true;
                            this.pending_sync = true;
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn toggle_recording(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(sid)) = (self.workspace_id.clone(), self.selected_id.clone()) else {
            return;
        };
        let next = !self.status_of(&sid).is_some_and(|s| s.recording);
        let sid_ret = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| mock_recording_set_impl(state, ws_id, sid, next),
            move |this, res, cx| {
                match res {
                    Ok(rec) => {
                        if let Some(st) = this.statuses.get_mut(&sid_ret) {
                            st.recording = rec.enabled;
                            st.recorded_count = rec.count;
                        }
                        this.status_msg = Some((
                            Tone::Accent,
                            if rec.enabled {
                                this.t("已开启代理流量录制", "Proxy recording started")
                            } else {
                                this.t("已停止代理流量录制", "Proxy recording stopped")
                            }
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
}
