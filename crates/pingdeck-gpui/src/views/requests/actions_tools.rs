//! Binary/multipart file pickers & validation, OAuth 2.0 token card actions,
//! saved response examples, and Ask Agent handoff builders.

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::{
    OAuthTokenInput, oauth_token_clear_impl, oauth_token_fetch_impl, oauth_token_refresh_impl,
    oauth_token_status_impl, oauth_token_store_as_impl,
    request_body_validate_file_path_with_context_impl,
};
use pingdeck_desktop::core::model::models::RequestExample;
use pingdeck_desktop::core::model::{FormPart, KeyValue, RequestBody};
use pingdeck_desktop::services::{FilePickerSpec, HostDialog as _, RfdDialog};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::RequestsView;
use super::model::{RequestsHandoff, method_str};

impl RequestsView {
    pub fn pick_binary_body_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = FilePickerSpec {
            title: Some(crate::ui::tr(cx).requests().act_pick_binary_title().into()),
            ..Default::default()
        };
        let Some(path) = RfdDialog.pick_file(&spec) else {
            return;
        };
        let path_str = path.to_string_lossy().to_string();
        self.binary_path_input
            .update(cx, |i, cx| i.set_value(path_str.clone(), window, cx));
        if let Some(tab) = self.active_req_mut() {
            tab.draft.body = RequestBody::Binary {
                file_path: path_str,
                content_type: None,
            };
            tab.recompute_dirty();
        }
        self.validate_binary_body_file(cx);
    }

    pub fn validate_binary_body_file(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let Some(tab) = self.active_req() else {
            return;
        };
        let tid = tab.tab_id.clone();
        let path = self.binary_path_input.read(cx).value().to_string();
        let env_id = self.active_env_id.clone();
        let req_id = tab
            .saved_request_id
            .clone()
            .unwrap_or_else(|| tab.draft.id.as_str().to_owned());
        let task = spawn_blocking_into(
            cx,
            move |state| {
                request_body_validate_file_path_with_context_impl(
                    state, ws_id, req_id, env_id, path,
                )
            },
            move |this, res, cx| {
                if let Some(t) = this.tab_req_mut(&tid) {
                    if let Ok(v) = res {
                        t.binary_validation = Some(v);
                    }
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn pick_multipart_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = FilePickerSpec {
            title: Some(
                crate::ui::tr(cx)
                    .requests()
                    .act_pick_multipart_title()
                    .into(),
            ),
            ..Default::default()
        };
        if let Some(path) = RfdDialog.pick_file(&spec) {
            let s = path.to_string_lossy().to_string();
            self.kv_file_input
                .update(cx, |i, cx| i.set_value(s, window, cx));
        }
    }

    pub fn add_multipart_part(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.kv_key_input.read(cx).value().trim().to_owned();
        let val = self.kv_val_input.read(cx).value().to_string();
        let fp = self.kv_file_input.read(cx).value().trim().to_owned();
        if key.is_empty() {
            return;
        }
        if let Some(tab) = self.active_req_mut()
            && let RequestBody::Multipart { parts } = &mut tab.draft.body
        {
            parts.push(FormPart {
                key,
                value: val,
                file_path: (!fp.is_empty()).then_some(fp),
                enabled: true,
            });
            tab.recompute_dirty();
        }
        self.kv_key_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.kv_val_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.kv_file_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        cx.notify();
    }

    // ── OAuth 2.0 Token Card ─────────────────────────────────────────────────

    fn build_oauth_input(&self, _cx: &Context<Self>) -> Option<(String, OAuthTokenInput)> {
        let ws = self.workspace_id.clone()?;
        let tab = self.active_req()?;
        Some((
            tab.tab_id.clone(),
            OAuthTokenInput {
                workspace_id: ws,
                environment_id: self.active_env_id.clone(),
                collection_id: tab
                    .draft
                    .collection_id
                    .as_ref()
                    .map(|c| c.as_str().to_owned()),
                request_id: tab.saved_request_id.clone(),
                auth: tab.draft.auth.clone(),
            },
        ))
    }

    pub fn oauth_refresh_status(&mut self, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        let Some((tid, input)) = self.build_oauth_input(cx) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| oauth_token_status_impl(state, &input).ok().flatten(),
            move |this, info, cx| {
                if let Some(t) = this.tab_req_mut(&tid) {
                    t.oauth_token_info = info;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn oauth_fetch_token(&mut self, is_refresh: bool, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        let Some((tid, input)) = self.build_oauth_input(cx) else {
            return;
        };
        if let Some(t) = self.tab_req_mut(&tid) {
            t.oauth_busy = true;
        }
        cx.notify();
        let task = spawn_async_into(
            cx,
            move |state| async move {
                if is_refresh {
                    oauth_token_refresh_impl(&state, input, None).await
                } else {
                    oauth_token_fetch_impl(&state, input, None).await
                }
            },
            move |this, res, cx| {
                if let Some(t) = this.tab_req_mut(&tid) {
                    t.oauth_busy = false;
                    match res {
                        Ok(info) => {
                            t.oauth_token_info = Some(info);
                            this.status_msg = Some((
                                Tone::Success,
                                crate::ui::tr(cx).requests().act_oauth_fetched().into(),
                            ));
                        }
                        Err(err) => {
                            this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                        }
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn oauth_clear_token(&mut self, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        let Some((tid, input)) = self.build_oauth_input(cx) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| oauth_token_clear_impl(state, &input),
            move |this, res, cx| {
                if res.is_ok()
                    && let Some(t) = this.tab_req_mut(&tid)
                {
                    t.oauth_token_info = None;
                    this.status_msg = Some((
                        Tone::Success,
                        crate::ui::tr(cx).requests().act_oauth_cleared().into(),
                    ));
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn oauth_store_token_as_var(&mut self, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        let var_name = self.oauth_var_input.read(cx).value().trim().to_owned();
        if var_name.is_empty() {
            return;
        }
        let Some((_, input)) = self.build_oauth_input(cx) else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| oauth_token_store_as_impl(state, &input, &var_name),
            move |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).requests().act_oauth_saved_var().into(),
                        ));
                        this.reload_resolved_vars(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    // ── Response Examples & Ask Agent Handoff ────────────────────────────────

    pub fn save_current_response_as_example(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        let Some(resp) = tab.last_http.clone() else {
            return;
        };
        let (example, _) = RequestExample {
            name: format!("{} {}", resp.status_code, resp.status_text),
            status: resp.status_code,
            headers: resp
                .headers
                .iter()
                .map(|h| KeyValue {
                    key: h.key.clone(),
                    value: h.value.clone(),
                    enabled: true,
                    description: None,
                })
                .collect(),
            body: resp.body_text,
        }
        .capped();
        if tab.draft.examples.len() >= 20 {
            tab.draft.examples.remove(0);
        }
        tab.draft.examples.push(example);
        tab.recompute_dirty();
        self.status_msg = Some((
            Tone::Success,
            crate::ui::tr(cx).requests().act_added_example().into(),
        ));
        cx.notify();
    }

    pub fn handoff_ask_agent_http(&self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_req() else {
            return;
        };
        let r = crate::ui::tr(cx).requests();
        let mut ctx = r.act_ask_agent_http_intro(method_str(tab.draft.method), &tab.draft.url);
        if let Some(resp) = &tab.last_http {
            let preview: String = resp.body_text.chars().take(1800).collect();
            ctx.push_str(&r.act_ask_agent_http_resp(
                resp.status_code,
                &resp.status_text,
                resp.elapsed_ms,
                resp.body_size_bytes,
                &resp.url,
                &preview,
            ));
        } else if let Some(fail) = &tab.failure {
            ctx.push_str(&r.act_ask_agent_fail(&fail.kind_label, &fail.message));
        }
        self.emit_handoff(
            RequestsHandoff::AskAgent {
                title: tab.draft.name.clone(),
                context_markdown: ctx,
            },
            cx,
        );
    }

    pub fn handoff_ask_agent_ai(&self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_req() else {
            return;
        };
        let r = crate::ui::tr(cx).requests();
        let mut ctx = r.act_ask_agent_ai_intro(&tab.draft.name);
        if let Some(ai) = &tab.last_ai {
            let reply: String = ai.reply.chars().take(1600).collect();
            ctx.push_str(&r.act_ask_agent_ai_detail(
                &ai.model,
                ai.status.label(),
                format!("{:?}", ai.http_status),
                ai.total_ms,
                &reply,
            ));
        }
        self.emit_handoff(
            RequestsHandoff::AskAgent {
                title: tab.draft.name.clone(),
                context_markdown: ctx,
            },
            cx,
        );
    }
}
