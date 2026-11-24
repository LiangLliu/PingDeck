//! Multi-model AI comparison: modal setup, preflight cost estimate, and streaming compare run.

use gpui_kit::Context;
use pingdeck_desktop::core::app::{
    AiCompareEvent, AiCompareRunInput, ai_compare_estimate_impl, ai_compare_run_impl,
    current_time_ms,
};
use pingdeck_desktop::core::model::ai_request::CompareModel;
use pingdeck_desktop::core::model::script_result::ConsoleLevel;
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_async_into, subscribe_stream_into};
use crate::ui::Tone;

use super::RequestsView;
use super::model::{ActiveModal, AiEdTab, AiRespTab};

impl RequestsView {
    pub fn open_compare_models_modal(&mut self, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        let Some(tab) = self.active_req() else {
            return;
        };
        let tid = tab.tab_id.clone();
        let mut selected: Vec<(String, String)> = tab
            .draft
            .ai
            .as_ref()
            .map(|a| {
                a.compare_models
                    .iter()
                    .map(|m| (m.provider_id.clone(), m.model.clone()))
                    .collect()
            })
            .unwrap_or_default();
        if selected.is_empty()
            && let Some((pid, _, _, mid)) = self.first_enabled_model_tuple()
        {
            selected.push((pid, mid));
        }
        self.active_modal = Some(ActiveModal::CompareModels {
            tab_id: tid,
            selected,
            concurrency: 3,
            estimate: None,
            error: None,
        });
        self.refresh_compare_estimate(cx);
        cx.notify();
    }

    pub fn toggle_compare_model(&mut self, pid: String, mid: String, cx: &mut Context<Self>) {
        if let Some(ActiveModal::CompareModels { selected, .. }) = &mut self.active_modal {
            if let Some(pos) = selected.iter().position(|(p, m)| *p == pid && *m == mid) {
                selected.remove(pos);
            } else if selected.len() < 12 {
                selected.push((pid, mid));
            }
        }
        self.refresh_compare_estimate(cx);
        cx.notify();
    }

    pub fn refresh_compare_estimate(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let Some(ActiveModal::CompareModels {
            tab_id,
            selected,
            concurrency,
            ..
        }) = self.active_modal.clone()
        else {
            return;
        };
        if selected.is_empty() {
            if let Some(ActiveModal::CompareModels { estimate, .. }) = &mut self.active_modal {
                *estimate = None;
            }
            return;
        }
        let Some(tab) = self.tabs.iter().find_map(|t| match t {
            super::model::OpenTab::Request(r) if r.tab_id == tab_id => Some(r),
            _ => None,
        }) else {
            return;
        };
        let input = AiCompareRunInput {
            workspace_id: ws_id,
            request_id: None,
            request: Some(tab.to_save_input()),
            environment_id: self.active_env_id.clone(),
            confirmed: Some(true),
            keep_exchange: tab.ai_params.keep_exchange,
            models: selected
                .into_iter()
                .map(|(provider_id, model)| CompareModel { provider_id, model })
                .collect(),
            concurrency,
        };
        match ai_compare_estimate_impl(runtime(cx).state(), &input) {
            Ok(est) => {
                if let Some(ActiveModal::CompareModels {
                    estimate, error, ..
                }) = &mut self.active_modal
                {
                    *estimate = Some(est);
                    *error = None;
                }
            }
            Err(err) => {
                if let Some(ActiveModal::CompareModels {
                    estimate, error, ..
                }) = &mut self.active_modal
                {
                    *estimate = None;
                    *error = Some(err.message().to_owned());
                }
            }
        }
    }

    pub fn start_ai_compare(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let Some(ActiveModal::CompareModels {
            tab_id,
            selected,
            concurrency,
            ..
        }) = self.active_modal.take()
        else {
            return;
        };
        if selected.is_empty() {
            return;
        }
        let compare_models: Vec<CompareModel> = selected
            .into_iter()
            .map(|(provider_id, model)| CompareModel { provider_id, model })
            .collect();
        let env_id = self.active_env_id.clone();
        let Some(tab) = self.tab_req_mut(&tab_id) else {
            return;
        };
        if let Some(ai) = &mut tab.draft.ai {
            ai.compare_models = compare_models.clone();
        }
        tab.ai_compare_running = true;
        tab.ai_compare_live_rows.clear();
        tab.ai_compare_result = None;
        tab.ai_resp_tab = AiRespTab::Compare;
        let cancel_id = format!("ai-cmp-{}-{}", tab_id, current_time_ms());
        tab.cancel_token = Some(cancel_id.clone());
        let input = AiCompareRunInput {
            workspace_id: ws_id,
            request_id: None,
            request: Some(tab.to_save_input()),
            environment_id: env_id,
            confirmed: Some(true),
            keep_exchange: tab.ai_params.keep_exchange,
            models: compare_models,
            concurrency,
        };

        let (sink, rx) = channel_sink::<AiCompareEvent>();
        let tid_s = tab_id.clone();
        let sub = subscribe_stream_into(cx, rx, 16, move |this, events, cx| {
            for ev in events {
                match ev {
                    AiCompareEvent::RowDone { row, .. } => {
                        if let Some(t) = this.tab_req_mut(&tid_s) {
                            t.ai_compare_live_rows.push(*row);
                        }
                    }
                    AiCompareEvent::Done { result } => {
                        if let Some(t) = this.tab_req_mut(&tid_s) {
                            t.ai_compare_result = Some(*result);
                        }
                    }
                    _ => {}
                }
            }
            cx.notify();
        });
        self._tasks.push(sub);

        let tid_r = tab_id.clone();
        let run_task = spawn_async_into(
            cx,
            move |state| async move {
                ai_compare_run_impl(&state, input, Some(cancel_id), sink.as_ref()).await
            },
            move |this, res, cx| {
                if let Some(t) = this.tab_req_mut(&tid_r) {
                    t.ai_compare_running = false;
                    t.cancel_token = None;
                    match res {
                        Ok(done) => {
                            let n = done.rows.len();
                            t.ai_compare_result = Some(done);
                            this.push_console(
                                "AI",
                                ConsoleLevel::Info,
                                format!("多模型对比完成（{n} 个模型）"),
                                None,
                            );
                        }
                        Err(err) => {
                            this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                        }
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(run_task);
        let _ = AiEdTab::Messages;
        cx.notify();
    }
}
