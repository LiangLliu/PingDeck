//! Hydrates shared GPUI editor widgets whenever the active tab changes.

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::model::{AuthConfig, RequestBody};

use super::RequestsView;
use super::model::OpenTab;

impl RequestsView {
    /// Hydrates all shared GPUI editor widgets from the currently active tab.
    pub fn hydrate_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hydrating = true;
        match self.active_tab().cloned() {
            Some(OpenTab::Request(tab)) => {
                self.name_input
                    .update(cx, |i, cx| i.set_value(tab.draft.name.clone(), window, cx));
                self.url_input
                    .update(cx, |i, cx| i.set_value(tab.draft.url.clone(), window, cx));
                self.pre_script_editor.update(cx, |e, cx| {
                    e.set_value(tab.draft.pre_request_script.clone(), window, cx)
                });
                self.post_script_editor.update(cx, |e, cx| {
                    e.set_value(tab.draft.test_script.clone(), window, cx)
                });
                self.docs_input.update(cx, |t, cx| {
                    t.set_value(tab.draft.description.clone(), window, cx)
                });

                // Body
                let (body_txt, hl, gql_vars, bin_path) = match &tab.draft.body {
                    RequestBody::Json { content } => {
                        (content.clone(), "json", String::new(), String::new())
                    }
                    RequestBody::RawText { content, language } => {
                        let h = match language {
                            Some(pingdeck_desktop::core::model::RawLanguage::Javascript) => {
                                "javascript"
                            }
                            Some(pingdeck_desktop::core::model::RawLanguage::Html) => "html",
                            Some(pingdeck_desktop::core::model::RawLanguage::Xml) => "xml",
                            _ => "plain",
                        };
                        (content.clone(), h, String::new(), String::new())
                    }
                    RequestBody::Graphql {
                        query,
                        variables_json,
                        ..
                    } => (
                        query.clone(),
                        "graphql",
                        variables_json.clone().unwrap_or_default(),
                        String::new(),
                    ),
                    RequestBody::Binary { file_path, .. } => {
                        (String::new(), "json", String::new(), file_path.clone())
                    }
                    _ => (String::new(), "json", String::new(), String::new()),
                };
                self.body_editor.update(cx, |e, cx| {
                    e.set_highlighter(hl, cx);
                    e.set_value(body_txt, window, cx);
                });
                self.gql_vars_editor
                    .update(cx, |e, cx| e.set_value(gql_vars, window, cx));
                self.binary_path_input
                    .update(cx, |i, cx| i.set_value(bin_path, window, cx));

                // Auth
                self.hydrate_auth_inputs(&tab.draft.auth, window, cx);

                // Network
                let net = &tab.draft.network_options;
                self.net_timeout_input.update(cx, |i, cx| {
                    i.set_value(net.timeout_ms.to_string(), window, cx)
                });
                self.net_total_timeout_input.update(cx, |i, cx| {
                    i.set_value(
                        net.total_timeout_ms
                            .map(|v| v.to_string())
                            .unwrap_or_default(),
                        window,
                        cx,
                    )
                });
                self.net_retry_input.update(cx, |i, cx| {
                    i.set_value(net.retry_count.to_string(), window, cx)
                });
                self.net_max_redir_input.update(cx, |i, cx| {
                    i.set_value(net.max_redirects.to_string(), window, cx)
                });
                self.net_proxy_input.update(cx, |i, cx| {
                    i.set_value(net.proxy_url.clone().unwrap_or_default(), window, cx)
                });

                // AI fields
                self.ai_custom_url_input
                    .update(cx, |i, cx| i.set_value(tab.draft.url.clone(), window, cx));
                let model_val = tab
                    .draft
                    .ai
                    .as_ref()
                    .map(|a| a.target.model().to_owned())
                    .unwrap_or_default();
                self.ai_custom_model_input
                    .update(cx, |i, cx| i.set_value(model_val, window, cx));
                self.ai_temp_input.update(cx, |i, cx| {
                    i.set_value(tab.ai_params.temperature.clone(), window, cx)
                });
                self.ai_max_tok_input.update(cx, |i, cx| {
                    i.set_value(tab.ai_params.max_tokens.clone(), window, cx)
                });
                self.ai_top_p_input.update(cx, |i, cx| {
                    i.set_value(tab.ai_params.top_p.clone(), window, cx)
                });
                self.ai_tools_editor.update(cx, |e, cx| {
                    e.set_value(tab.ai_tools_json.clone(), window, cx)
                });
                let raw_ai = match &tab.draft.body {
                    RequestBody::Json { content } | RequestBody::RawText { content, .. } => {
                        content.clone()
                    }
                    _ => String::new(),
                };
                self.ai_raw_editor
                    .update(cx, |e, cx| e.set_value(raw_ai, window, cx));

                // Response editor
                let resp_txt = if let Some((_, txt, _)) = &tab.http_jsonpath_result {
                    txt.clone()
                } else if let Some(formatted) = &tab.http_formatted_text {
                    formatted.clone()
                } else if let Some(resp) = &tab.last_http {
                    resp.body_text.clone()
                } else {
                    "// 发送请求后在此查看实时响应 / Send a request to inspect the response".into()
                };
                self.response_editor
                    .update(cx, |e, cx| e.set_value(resp_txt, window, cx));
                self.resp_filter_input.update(cx, |i, cx| {
                    i.set_value(tab.http_jsonpath_query.clone(), window, cx)
                });
            }
            Some(OpenTab::Runner(run)) => {
                self.runner_iter_input.update(cx, |i, cx| {
                    i.set_value(run.iterations.to_string(), window, cx)
                });
                self.runner_delay_input.update(cx, |i, cx| {
                    i.set_value(run.delay_ms.to_string(), window, cx)
                });
            }
            Some(OpenTab::Container(c)) => {
                self.container_name_input
                    .update(cx, |i, cx| i.set_value(c.name.clone(), window, cx));
                self.container_desc_input
                    .update(cx, |t, cx| t.set_value(c.description.clone(), window, cx));
                self.container_pre_editor.update(cx, |e, cx| {
                    e.set_value(c.pre_request_script.clone(), window, cx)
                });
                self.container_post_editor
                    .update(cx, |e, cx| e.set_value(c.test_script.clone(), window, cx));
            }
            None => {}
        }
        self.hydrating = false;
    }

    pub fn hydrate_auth_inputs(
        &mut self,
        auth: &AuthConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (v1, v2, v3, v4, v5, v6) = match auth {
            AuthConfig::Bearer { token_ref } => (
                token_ref.clone(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ),
            AuthConfig::Basic {
                username,
                password_ref,
            } => (
                username.clone(),
                password_ref.clone(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ),
            AuthConfig::ApiKey { key, value_ref, .. } => (
                key.clone(),
                value_ref.clone(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ),
            AuthConfig::OAuth2 {
                access_token_url,
                client_id,
                client_secret_ref,
                username,
                password_ref,
                scope,
                authorization_url,
                ..
            } => (
                if password_ref.is_empty() {
                    username.clone()
                } else {
                    format!("{username}:{password_ref}")
                },
                access_token_url.clone(),
                client_id.clone(),
                client_secret_ref.clone(),
                scope.clone(),
                authorization_url.clone(),
            ),
            AuthConfig::Digest {
                username,
                password_ref,
            } => (
                username.clone(),
                password_ref.clone().unwrap_or_default(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ),
            AuthConfig::AwsSignatureV4 {
                access_key,
                secret_key_ref,
                region,
                service,
                session_token_ref,
            } => (
                access_key.clone(),
                secret_key_ref.clone().unwrap_or_default(),
                region.clone(),
                service.clone(),
                session_token_ref.clone().unwrap_or_default(),
                String::new(),
            ),
            _ => Default::default(),
        };
        self.auth_f1.update(cx, |i, cx| i.set_value(v1, window, cx));
        self.auth_f2.update(cx, |i, cx| i.set_value(v2, window, cx));
        self.auth_f3.update(cx, |i, cx| i.set_value(v3, window, cx));
        self.auth_f4.update(cx, |i, cx| i.set_value(v4, window, cx));
        self.auth_f5.update(cx, |i, cx| i.set_value(v5, window, cx));
        self.auth_f6.update(cx, |i, cx| i.set_value(v6, window, cx));
    }
}
