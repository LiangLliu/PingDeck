//! One-shot inline Try-It route tester and Callback Test Now executor for `MockView`.

use gpui_kit::Context;
use pingdeck_desktop::core::app::factory_service::{mock_route_test_request, new_entity_id};
use pingdeck_desktop::core::app::{
    ExecuteDraftRequestInput, SaveRequestInput, request_execute_draft_impl,
};
use pingdeck_desktop::core::model::ai_request::RequestMode;
use pingdeck_desktop::core::model::{
    AuthConfig, HttpMethod, KeyValue, NetworkOptions, ProtocolKind, RequestBody,
};

use crate::bridge::spawn_async_into;
use crate::ui::Tone;

use super::state::{MockHandoff, MockView};

impl MockView {
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

    pub fn try_route_inline(&mut self, route_id: &str, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let (Some(ws_id), Some(srv)) = (self.workspace_id.clone(), self.draft.clone()) else {
            return;
        };
        if !self.is_running(&srv.id) {
            self.status_msg = Some((
                Tone::Warning,
                crate::ui::Tr(self.lang)
                    .mock()
                    .actions_try_start_the_mock_server_before()
                    .to_owned(),
            ));
            cx.notify();
            return;
        }
        let Some(route) = srv.routes.iter().find(|r| r.id == route_id) else {
            return;
        };
        let req = mock_route_test_request(&srv, route);
        let method = parse_method(&req.method);
        self.try_it_busy = true;
        self.try_it_result = None;
        cx.notify();
        let input = ExecuteDraftRequestInput {
            request: quick_draft_input(
                ws_id,
                format!("Try {}", route.path),
                method,
                req.url,
                Vec::new(),
                RequestBody::None,
            ),
            environment_id: srv.environment_id.clone(),
            confirmed: Some(true),
            save_to_history: Some(false),
        };
        let task = spawn_async_into(
            cx,
            move |state| async move { request_execute_draft_impl(&state, input, None).await },
            move |this, res, cx| {
                this.try_it_busy = false;
                match res {
                    Ok(exec) => {
                        let body = exec.body_text.chars().take(1200).collect::<String>();
                        this.try_it_result =
                            Some((exec.status_code, exec.elapsed_ms as u64, exec.url, body));
                    }
                    Err(e) => {
                        this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn test_callback_now(&mut self, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let (Some(ws_id), Some(cb)) =
            (self.workspace_id.clone(), self.selected_callback().cloned())
        else {
            return;
        };
        if cb.url.trim().is_empty() {
            self.status_msg = Some((
                Tone::Warning,
                crate::ui::Tr(self.lang)
                    .mock()
                    .actions_try_enter_a_callback_url_first()
                    .to_owned(),
            ));
            cx.notify();
            return;
        }
        let body = if cb.body.is_empty() {
            RequestBody::None
        } else {
            RequestBody::Json {
                content: cb.body.clone(),
            }
        };
        let input = ExecuteDraftRequestInput {
            request: quick_draft_input(
                ws_id,
                format!("Callback {}", cb.name),
                cb.method,
                cb.url.clone(),
                cb.headers.clone(),
                body,
            ),
            environment_id: self.draft.as_ref().and_then(|s| s.environment_id.clone()),
            confirmed: Some(true),
            save_to_history: Some(false),
        };
        let task = spawn_async_into(
            cx,
            move |state| async move { request_execute_draft_impl(&state, input, None).await },
            |this, res, cx| {
                match res {
                    Ok(exec) => {
                        let tone = if exec.status_code < 400 {
                            Tone::Success
                        } else {
                            Tone::Warning
                        };
                        this.status_msg = Some((
                            tone,
                            crate::ui::Tr(this.lang)
                                .mock()
                                .actions_try_callback_returned_ms(
                                    exec.status_code,
                                    exec.status_text,
                                    exec.elapsed_ms,
                                ),
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

fn parse_method(s: &str) -> HttpMethod {
    match s.to_ascii_uppercase().as_str() {
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        _ => HttpMethod::Get,
    }
}

fn quick_draft_input(
    workspace_id: String,
    name: String,
    method: HttpMethod,
    url: String,
    headers: Vec<KeyValue>,
    body: RequestBody,
) -> SaveRequestInput {
    SaveRequestInput {
        id: new_entity_id("req"),
        workspace_id,
        collection_id: None,
        folder_id: None,
        name,
        protocol: ProtocolKind::HttpRest,
        method,
        url,
        query_params: Vec::new(),
        path_variables: Vec::new(),
        headers,
        auth: AuthConfig::None,
        body,
        network_options: NetworkOptions::default(),
        assertions: Vec::new(),
        description: String::new(),
        pre_request_script: String::new(),
        test_script: String::new(),
        position: 0,
        mode: RequestMode::Http,
        ai: None,
        examples: None,
    }
}
