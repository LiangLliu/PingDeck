//! Model management actions for `ProvidersView`:
//! refresh, drift apply/ack, single/bulk enable toggle with Undo, manual add, and delete.

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::{
    llm_model_delete_impl, llm_model_save_impl, llm_models_drift_ack_impl,
    llm_models_drift_apply_impl, llm_models_refresh_impl, llm_models_set_enabled_impl,
    llm_models_undo_impl,
};
use pingdeck_desktop::core::llm::store::{ModelSource, ProviderModel};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::ProvidersView;

impl ProvidersView {
    pub fn refresh_provider_models(
        &mut self,
        pid: String,
        key_id: Option<String>,
        zh: bool,
        cx: &mut Context<Self>,
    ) {
        self.refreshing_models = true;
        cx.notify();
        let task = spawn_async_into(
            cx,
            move |state| async move {
                llm_models_refresh_impl(&state, &pid, key_id.as_deref(), None).await
            },
            move |this, res, cx| {
                this.refreshing_models = false;
                match res {
                    Ok(drift) => {
                        this.status_msg = Some((
                            Tone::Success,
                            if zh {
                                format!(
                                    "模型列表已刷新：新增 {}，移除 {}",
                                    drift.added.len(),
                                    drift.removed.len()
                                )
                            } else {
                                format!(
                                    "Models refreshed: {} added, {} removed",
                                    drift.added.len(),
                                    drift.removed.len()
                                )
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn apply_model_drift(&mut self, pid: String, zh: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_models_drift_apply_impl(state, &pid, None),
            move |this, res, cx| {
                match res {
                    Ok(rep) => {
                        this.status_msg = Some((
                            Tone::Success,
                            if zh {
                                format!("已添加 {} 个新增模型", rep.added.len())
                            } else {
                                format!("Added {} new model(s)", rep.added.len())
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn ack_model_drift(&mut self, pid: String, zh: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_models_drift_ack_impl(state, &pid),
            move |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.status_msg = Some((
                            Tone::Neutral,
                            if zh {
                                "漂移提醒已标记已读".into()
                            } else {
                                "Drift dismissed".into()
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn toggle_single_model(
        &mut self,
        pid: String,
        mid: String,
        enabled: bool,
        zh: bool,
        cx: &mut Context<Self>,
    ) {
        let changes = vec![(mid.clone(), enabled)];
        let task = spawn_blocking_into(
            cx,
            move |state| llm_models_set_enabled_impl(state, &pid, &changes),
            move |this, res, cx| {
                match res {
                    Ok(rep) => {
                        this.last_bulk_undo = Some((
                            rep.id,
                            if zh {
                                format!("{} {mid}", if enabled { "已启用" } else { "已停用" })
                            } else {
                                format!("{} {mid}", if enabled { "Enabled" } else { "Disabled" })
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn bulk_toggle_selected_models(
        &mut self,
        pid: String,
        enabled: bool,
        zh: bool,
        cx: &mut Context<Self>,
    ) {
        let changes: Vec<(String, bool)> = self
            .selected_models
            .iter()
            .cloned()
            .map(|m| (m, enabled))
            .collect();
        let n = changes.len();
        self.selected_models.clear();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_models_set_enabled_impl(state, &pid, &changes),
            move |this, res, cx| {
                match res {
                    Ok(rep) => {
                        this.last_bulk_undo = Some((
                            rep.id,
                            if zh {
                                format!("{} {n} 个模型", if enabled { "已启用" } else { "已停用" })
                            } else {
                                format!(
                                    "{} {n} model(s)",
                                    if enabled { "Enabled" } else { "Disabled" }
                                )
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn undo_bulk_models(&mut self, op_id: String, zh: bool, cx: &mut Context<Self>) {
        self.last_bulk_undo = None;
        let task = spawn_blocking_into(
            cx,
            move |state| llm_models_undo_impl(state, &op_id),
            move |this, res, cx| {
                match res {
                    Ok(u) => {
                        this.status_msg = Some((
                            Tone::Success,
                            if zh {
                                format!("已撤销（恢复 {} 个模型）", u.restored.len())
                            } else {
                                format!("Undone ({} restored)", u.restored.len())
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn add_manual_model(
        &mut self,
        pid: String,
        zh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mid = self.new_model_id_input.read(cx).value().trim().to_owned();
        if mid.is_empty() {
            return;
        }
        let ctx_raw = self.new_model_ctx_input.read(cx).value().trim().to_owned();
        let context = ctx_raw.parse::<u64>().ok().filter(|v| *v > 0);
        self.new_model_id_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.new_model_ctx_input
            .update(cx, |i, cx| i.set_value("", window, cx));

        let model = ProviderModel {
            provider_id: pid,
            model_id: mid.clone(),
            source: ModelSource::Manual,
            context,
            enabled: true,
            ..ProviderModel::default()
        };
        let task = spawn_blocking_into(
            cx,
            move |state| llm_model_save_impl(state, model),
            move |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.adding_model_open = false;
                        this.status_msg = Some((
                            Tone::Success,
                            if zh {
                                format!("已添加模型 {mid}")
                            } else {
                                format!("Added model {mid}")
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_single_model(
        &mut self,
        pid: String,
        mid: String,
        zh: bool,
        cx: &mut Context<Self>,
    ) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_model_delete_impl(state, &pid, &mid),
            move |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.status_msg = Some((
                            Tone::Neutral,
                            if zh {
                                "模型已删除".into()
                            } else {
                                "Model deleted".into()
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}
