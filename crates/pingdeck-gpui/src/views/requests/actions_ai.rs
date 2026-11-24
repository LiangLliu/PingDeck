//! HTTP <-> AI mode switching, message/parameter builder sync, and live streaming
//! AI execution (`ai_request_run_impl`).

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::request_model_service::request_switch_mode;
use pingdeck_desktop::core::app::{AiRequestRunInput, ai_request_run_impl, current_time_ms};
use pingdeck_desktop::core::error::AppError;
use pingdeck_desktop::core::llm::ai_run::AiRunEvent;
use pingdeck_desktop::core::llm::wire::Protocol;
use pingdeck_desktop::core::model::RequestBody;
use pingdeck_desktop::core::model::ai_request::{
    AiRequestConfig, AiTarget, RequestMode, non_routable_label,
};
use pingdeck_desktop::core::model::script_result::ConsoleLevel;
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{spawn_async_into, subscribe_stream_into};

use super::RequestsView;
use super::model::{
    ActiveModal, AiMessageDraft, RequestFailureInfo, build_ai_body_json,
    parse_ai_builder_from_request,
};

impl RequestsView {
    /// Safely switches the active request between HTTP and AI API mode via `request_switch_mode`.
    pub fn switch_active_request_mode(
        &mut self,
        to: RequestMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let first_model = self.first_enabled_model_tuple();
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        if tab.draft.mode == to {
            return;
        }
        let resolved_url = match (&tab.draft.ai, to) {
            (Some(ai), RequestMode::Http) => match &ai.target {
                AiTarget::Provider { base_url_hint, .. } if !base_url_hint.is_empty() => {
                    Some(format!(
                        "{}/v1/chat/completions",
                        base_url_hint.trim_end_matches('/')
                    ))
                }
                _ => None,
            },
            _ => None,
        };
        tab.draft = request_switch_mode(tab.draft.clone(), to, resolved_url.as_deref());
        if to == RequestMode::AiApi {
            tab.draft.method = pingdeck_desktop::core::model::HttpMethod::Post;
            if let Some(ai) = &mut tab.draft.ai
                && ai.target.model().trim().is_empty()
                && let Some((pid, pname, base_url, mid)) = first_model
            {
                ai.target = AiTarget::Provider {
                    provider_id: pid,
                    provider_name: pname.clone(),
                    base_url_hint: base_url,
                    model: mid,
                };
                tab.draft.url = non_routable_label(&pname, "/v1/chat/completions");
            }
            let (msgs, params, tools) = parse_ai_builder_from_request(&tab.draft);
            tab.ai_messages = msgs;
            tab.ai_params = params;
            tab.ai_tools_json = tools;
            if let Some(ai) = &tab.draft.ai {
                let json = build_ai_body_json(
                    ai.protocol,
                    ai.target.model(),
                    &tab.ai_messages,
                    &tab.ai_params,
                    &tab.ai_tools_json,
                );
                tab.draft.body = RequestBody::Json { content: json };
            }
        }
        tab.recompute_dirty();
        self.hydrate_active_tab(window, cx);
        cx.notify();
    }

    pub fn set_ai_protocol(
        &mut self,
        proto: Protocol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        let ai = tab.draft.ai.get_or_insert_with(AiRequestConfig::default);
        ai.protocol = proto;
        let json = build_ai_body_json(
            ai.protocol,
            ai.target.model(),
            &tab.ai_messages,
            &tab.ai_params,
            &tab.ai_tools_json,
        );
        tab.draft.body = RequestBody::Json {
            content: json.clone(),
        };
        tab.recompute_dirty();
        self.ai_raw_editor
            .update(cx, |e, cx| e.set_value(json, window, cx));
        cx.notify();
    }

    pub fn set_ai_target_provider(
        &mut self,
        provider_id: String,
        provider_name: String,
        base_url: String,
        model: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        let ai = tab.draft.ai.get_or_insert_with(AiRequestConfig::default);
        ai.target = AiTarget::Provider {
            provider_id,
            provider_name: provider_name.clone(),
            base_url_hint: base_url,
            model: model.clone(),
        };
        tab.draft.url = non_routable_label(&provider_name, "/v1/chat/completions");
        let json = build_ai_body_json(
            ai.protocol,
            &model,
            &tab.ai_messages,
            &tab.ai_params,
            &tab.ai_tools_json,
        );
        tab.draft.body = RequestBody::Json {
            content: json.clone(),
        };
        tab.recompute_dirty();
        self.ai_raw_editor
            .update(cx, |e, cx| e.set_value(json, window, cx));
        cx.notify();
    }

    pub fn set_ai_target_custom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        let ai = tab.draft.ai.get_or_insert_with(AiRequestConfig::default);
        let prev_model = ai.target.model().to_owned();
        let prev_base = match &ai.target {
            AiTarget::Provider { base_url_hint, .. } if !base_url_hint.is_empty() => {
                format!(
                    "{}/v1/chat/completions",
                    base_url_hint.trim_end_matches('/')
                )
            }
            _ => "https://api.openai.com/v1/chat/completions".into(),
        };
        ai.target = AiTarget::Custom {
            model: if prev_model.is_empty() {
                "gpt-4o-mini".into()
            } else {
                prev_model
            },
        };
        if tab.draft.url.starts_with("pingdeck-provider://") {
            tab.draft.url = prev_base;
        }
        tab.recompute_dirty();
        self.hydrate_active_tab(window, cx);
        cx.notify();
    }

    pub fn ai_add_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.ai_msg_new_input.read(cx).value().to_string();
        if content.trim().is_empty() {
            return;
        }
        self.snapshot_active_tab(cx);
        let raw_json = {
            let Some(tab) = self.active_req_mut() else {
                return;
            };
            let next_role = match tab.ai_messages.last().map(|m| m.role.as_str()) {
                Some("user") => "assistant",
                _ => "user",
            };
            tab.ai_messages.push(AiMessageDraft {
                role: next_role.into(),
                content,
            });
            let json = tab.draft.ai.as_ref().map(|ai| {
                build_ai_body_json(
                    ai.protocol,
                    ai.target.model(),
                    &tab.ai_messages,
                    &tab.ai_params,
                    &tab.ai_tools_json,
                )
            });
            if let Some(ref j) = json {
                tab.draft.body = RequestBody::Json { content: j.clone() };
            }
            tab.recompute_dirty();
            json
        };
        if let Some(j) = raw_json {
            self.ai_raw_editor
                .update(cx, |e, cx| e.set_value(j, window, cx));
        }
        self.ai_msg_new_input
            .update(cx, |t, cx| t.set_value("", window, cx));
        cx.notify();
    }

    pub fn ai_cycle_message_role(
        &mut self,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        if let Some(m) = tab.ai_messages.get_mut(idx) {
            m.role = match m.role.as_str() {
                "system" => "user".into(),
                "user" => "assistant".into(),
                _ => "system".into(),
            };
        }
        self.ai_sync_builder_to_raw(window, cx);
    }

    pub fn ai_remove_message(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        if let Some(tab) = self.active_req_mut()
            && idx < tab.ai_messages.len()
        {
            tab.ai_messages.remove(idx);
        }
        self.ai_sync_builder_to_raw(window, cx);
    }

    pub fn ai_sync_builder_to_raw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        if let Some(ai) = &tab.draft.ai {
            let json = build_ai_body_json(
                ai.protocol,
                ai.target.model(),
                &tab.ai_messages,
                &tab.ai_params,
                &tab.ai_tools_json,
            );
            tab.draft.body = RequestBody::Json {
                content: json.clone(),
            };
            tab.recompute_dirty();
            self.ai_raw_editor
                .update(cx, |e, cx| e.set_value(json, window, cx));
        }
        cx.notify();
    }

    pub fn ai_sync_raw_to_builder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.ai_raw_editor.read(cx).value().to_string();
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        tab.draft.body = RequestBody::Json { content: raw };
        let (msgs, params, tools) = parse_ai_builder_from_request(&tab.draft);
        tab.ai_messages = msgs;
        tab.ai_params = params;
        tab.ai_tools_json = tools;
        tab.recompute_dirty();
        self.hydrate_active_tab(window, cx);
        cx.notify();
    }

    pub fn ai_insert_sample_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sample = r#"[
  {
    "type": "function",
    "function": {
      "name": "get_weather",
      "description": "Get current weather by city name",
      "parameters": {
        "type": "object",
        "properties": {
          "city": { "type": "string", "description": "City name" }
        },
        "required": ["city"]
      }
    }
  }
]"#;
        self.ai_tools_editor
            .update(cx, |e, cx| e.set_value(sample, window, cx));
        if let Some(tab) = self.active_req_mut() {
            tab.ai_tools_json = sample.to_owned();
        }
        self.ai_sync_builder_to_raw(window, cx);
    }

    /// Streams an AI API request live via `ai_request_run_impl` + `channel_sink::<AiRunEvent>`.
    pub fn send_active_ai_request(
        &mut self,
        confirmed: Option<bool>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        if tab.sending {
            return;
        }
        let tid = tab.tab_id.clone();
        let cancel_id = format!("ai-exec-{}-{}", tid, current_time_ms());
        tab.sending = true;
        tab.cancel_token = Some(cancel_id.clone());
        tab.ai_live_frames.clear();
        tab.ai_selected_frame = None;
        tab.failure = None;
        let keep_exchange = tab.ai_params.keep_exchange;
        let save_input = tab.to_save_input();
        let input = AiRequestRunInput {
            workspace_id: ws_id,
            request_id: None,
            request: Some(save_input),
            environment_id: self.active_env_id.clone(),
            confirmed,
            keep_exchange,
        };

        let (sink, rx) = channel_sink::<AiRunEvent>();
        let tid_stream = tid.clone();
        let stream_task = subscribe_stream_into(cx, rx, 32, move |this, events, cx| {
            for ev in events {
                match ev {
                    AiRunEvent::Started { .. } => {}
                    AiRunEvent::Frames { items } => {
                        if let Some(t) = this.tab_req_mut(&tid_stream) {
                            t.ai_live_frames.extend(items);
                        }
                    }
                    AiRunEvent::Done { result } => {
                        if let Some(t) = this.tab_req_mut(&tid_stream) {
                            t.last_ai = Some(*result);
                        }
                    }
                }
            }
            cx.notify();
        });
        self._tasks.push(stream_task);

        let run_task = spawn_async_into(
            cx,
            move |state| async move {
                ai_request_run_impl(&state, input, Some(cancel_id), sink.as_ref()).await
            },
            move |this, res, cx| {
                if let Some(t) = this.tab_req_mut(&tid) {
                    t.sending = false;
                    t.cancel_token = None;
                }
                match res {
                    Ok(dto) => {
                        let summary = format!(
                            "AI {} → {} ({} ms, {} 帧)",
                            dto.model,
                            dto.status.label(),
                            dto.total_ms,
                            dto.event_count
                        );
                        this.push_console("AI", ConsoleLevel::Info, summary, dto.message.clone());
                        for c in &dto.console {
                            this.push_console(
                                "SCRIPT",
                                c.level,
                                c.message.clone(),
                                Some(c.source.clone()),
                            );
                        }
                        if let Some(t) = this.tab_req_mut(&tid) {
                            t.last_ai = Some(dto);
                        }
                    }
                    Err(AppError::ConfirmationRequired { message, .. }) => {
                        this.active_modal = Some(ActiveModal::ConfirmGuard {
                            tab_id: tid,
                            message,
                            is_ai: true,
                            is_runner: false,
                            save_to_file: None,
                        });
                    }
                    Err(err) => {
                        this.push_console(
                            "AI",
                            ConsoleLevel::Error,
                            err.message().to_owned(),
                            None,
                        );
                        if let Some(t) = this.tab_req_mut(&tid) {
                            t.failure = Some(RequestFailureInfo {
                                kind_label: "AI 执行失败 / AI Error".into(),
                                message: err.message().to_owned(),
                                attempt_count: None,
                                connect_phase: None,
                                unresolved_keys: Vec::new(),
                                history_unsaved: false,
                                is_cancelled: matches!(err, AppError::Cancelled { .. }),
                            });
                        }
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(run_task);
        cx.notify();
    }

    // ── Multi-Model Compare ("多模型对比…") ───────────────────────────────────
}
