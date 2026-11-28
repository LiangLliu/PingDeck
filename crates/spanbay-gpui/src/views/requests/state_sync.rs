//! Tab editor snapshotting, workspace tab-session persistence, and unresolved-variable scanning.

use gpui_kit::Context;
use spanbay_desktop::core::app::tab_session_service::{
    EditorTab, SessionTab, TAB_SESSION_VERSION, TabSession, tab_session_save,
};
use spanbay_desktop::core::app::unresolved_scan::request_tokens;
use spanbay_desktop::core::model::{
    ApiKeyLocation, AuthConfig, KeyValue, OAuth2ClientAuth, OAuth2GrantType, RequestBody,
};

use crate::bridge::spawn_blocking_into;

use super::RequestsView;
use super::model::{AiEdTab, AuthKind, HttpEdTab, OpenTab, RequestTabState, build_ai_body_json};

impl RequestsView {
    /// Snapshots all shared GPUI editor widgets back into the currently active tab.
    pub fn snapshot_active_tab(&mut self, cx: &Context<Self>) {
        let name = self.name_input.read(cx).value().to_string();
        let url = self.url_input.read(cx).value().to_string();
        let body_txt = self.body_editor.read(cx).value().to_string();
        let gql_vars = self.gql_vars_editor.read(cx).value().to_string();
        let bin_path = self.binary_path_input.read(cx).value().to_string();
        let pre_script = self.pre_script_editor.read(cx).value().to_string();
        let post_script = self.post_script_editor.read(cx).value().to_string();
        let docs = self.docs_input.read(cx).value().to_string();
        let f1 = self.auth_f1.read(cx).value().to_string();
        let f2 = self.auth_f2.read(cx).value().to_string();
        let f3 = self.auth_f3.read(cx).value().to_string();
        let f4 = self.auth_f4.read(cx).value().to_string();
        let f5 = self.auth_f5.read(cx).value().to_string();
        let f6 = self.auth_f6.read(cx).value().to_string();
        let net_to = self.net_timeout_input.read(cx).value().trim().to_owned();
        let net_tot = self
            .net_total_timeout_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        let net_ret = self.net_retry_input.read(cx).value().trim().to_owned();
        let net_red = self.net_max_redir_input.read(cx).value().trim().to_owned();
        let net_prx = self.net_proxy_input.read(cx).value().trim().to_owned();
        let ai_url = self.ai_custom_url_input.read(cx).value().to_string();
        let ai_model = self.ai_custom_model_input.read(cx).value().to_string();
        let ai_temp = self.ai_temp_input.read(cx).value().to_string();
        let ai_max_tok = self.ai_max_tok_input.read(cx).value().to_string();
        let ai_top_p = self.ai_top_p_input.read(cx).value().to_string();
        let ai_tools = self.ai_tools_editor.read(cx).value().to_string();
        let ai_raw = self.ai_raw_editor.read(cx).value().to_string();
        let r_iter = self.runner_iter_input.read(cx).value().trim().to_owned();
        let r_delay = self.runner_delay_input.read(cx).value().trim().to_owned();
        let c_name = self.container_name_input.read(cx).value().to_string();
        let c_desc = self.container_desc_input.read(cx).value().to_string();
        let c_pre = self.container_pre_editor.read(cx).value().to_string();
        let c_post = self.container_post_editor.read(cx).value().to_string();

        match self.active_tab_mut() {
            Some(OpenTab::Request(tab)) => {
                tab.draft.name = name;
                tab.draft.pre_request_script = pre_script;
                tab.draft.test_script = post_script;
                tab.draft.description = docs;

                if tab.is_ai() {
                    tab.ai_params.temperature = ai_temp;
                    tab.ai_params.max_tokens = ai_max_tok;
                    tab.ai_params.top_p = ai_top_p;
                    tab.ai_tools_json = ai_tools;
                    if let Some(ai) = &mut tab.draft.ai {
                        if let spanbay_desktop::core::model::ai_request::AiTarget::Custom {
                            model,
                        } = &mut ai.target
                        {
                            *model = ai_model;
                            tab.draft.url = ai_url;
                        }
                        if tab.ai_ed_tab == AiEdTab::RawJson {
                            tab.draft.body = RequestBody::Json { content: ai_raw };
                        } else {
                            let content = build_ai_body_json(
                                ai.protocol,
                                ai.target.model(),
                                &tab.ai_messages,
                                &tab.ai_params,
                                &tab.ai_tools_json,
                            );
                            tab.draft.body = RequestBody::Json { content };
                        }
                    }
                } else {
                    tab.draft.url = url;
                    match &mut tab.draft.body {
                        RequestBody::Json { content } => *content = body_txt,
                        RequestBody::RawText { content, .. } => *content = body_txt,
                        RequestBody::Graphql {
                            query,
                            variables_json,
                            ..
                        } => {
                            *query = body_txt;
                            let trimmed = gql_vars.trim();
                            *variables_json = (!trimmed.is_empty()).then(|| trimmed.to_owned());
                        }
                        RequestBody::Binary { file_path, .. } => *file_path = bin_path,
                        _ => {}
                    }
                }

                // Snapshot Auth fields
                match &mut tab.draft.auth {
                    AuthConfig::Bearer { token_ref } => *token_ref = f1.trim().to_owned(),
                    AuthConfig::Basic {
                        username,
                        password_ref,
                    } => {
                        *username = f1.trim().to_owned();
                        *password_ref = f2;
                    }
                    AuthConfig::ApiKey { key, value_ref, .. } => {
                        *key = f1.trim().to_owned();
                        *value_ref = f2;
                    }
                    AuthConfig::OAuth2 {
                        access_token_url,
                        client_id,
                        client_secret_ref,
                        username,
                        password_ref,
                        scope,
                        authorization_url,
                        grant_type,
                        ..
                    } => {
                        *access_token_url = f2.trim().to_owned();
                        *client_id = f3.trim().to_owned();
                        *client_secret_ref = f4;
                        *scope = f5.trim().to_owned();
                        *authorization_url = f6.trim().to_owned();
                        if *grant_type == OAuth2GrantType::Password {
                            if let Some((u, p)) = f1.split_once(':') {
                                *username = u.trim().to_owned();
                                *password_ref = p.to_owned();
                            } else {
                                *username = f1.trim().to_owned();
                            }
                        }
                    }
                    AuthConfig::Digest {
                        username,
                        password_ref,
                    } => {
                        *username = f1.trim().to_owned();
                        *password_ref = (!f2.is_empty()).then_some(f2);
                    }
                    AuthConfig::AwsSignatureV4 {
                        access_key,
                        secret_key_ref,
                        region,
                        service,
                        session_token_ref,
                    } => {
                        *access_key = f1.trim().to_owned();
                        *secret_key_ref = (!f2.is_empty()).then_some(f2);
                        *region = f3.trim().to_owned();
                        *service = f4.trim().to_owned();
                        *session_token_ref = (!f5.trim().is_empty()).then(|| f5.trim().to_owned());
                    }
                    _ => {}
                }

                // Snapshot Network fields
                if let Ok(ms) = net_to.parse::<u64>() {
                    tab.draft.network_options.timeout_ms = ms.clamp(100, 600_000);
                }
                tab.draft.network_options.total_timeout_ms =
                    net_tot.parse::<u64>().ok().filter(|v| *v > 0);
                if let Ok(r) = net_ret.parse::<u8>() {
                    tab.draft.network_options.retry_count = r.min(10);
                }
                if let Ok(m) = net_red.parse::<u32>() {
                    tab.draft.network_options.max_redirects = m.min(50);
                }
                tab.draft.network_options.proxy_url = (!net_prx.is_empty()).then_some(net_prx);
                tab.recompute_dirty();
            }
            Some(OpenTab::Runner(run)) => {
                if let Ok(it) = r_iter.parse::<u32>() {
                    run.iterations = it.clamp(1, 500);
                }
                if let Ok(dl) = r_delay.parse::<u64>() {
                    run.delay_ms = dl.min(60_000);
                }
            }
            Some(OpenTab::Container(c)) => {
                if c.name != c_name
                    || c.description != c_desc
                    || c.pre_request_script != c_pre
                    || c.test_script != c_post
                {
                    c.dirty = true;
                }
                c.name = c_name;
                c.description = c_desc;
                c.pre_request_script = c_pre;
                c.test_script = c_post;
            }
            None => {}
        }
    }

    fn build_tab_session(&self) -> TabSession {
        TabSession {
            version: TAB_SESSION_VERSION,
            active_tab_id: self.active_tab_id.clone(),
            tabs: self
                .tabs
                .iter()
                .filter_map(|t| match t {
                    OpenTab::Request(r) => Some(SessionTab {
                        tab_id: r.tab_id.clone(),
                        request_id: r.saved_request_id.clone(),
                        history_entry_id: None,
                        dirty: r.dirty,
                        draft: Some(r.draft.clone()),
                        editor_tab: Some(to_session_editor_tab(r)),
                        preview: false,
                        proxy_password_lost: false,
                    }),
                    _ => None,
                })
                .collect(),
        }
    }

    /// Persists open request tabs per workspace via `tab_session_save`.
    pub fn persist_tab_session(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let session = self.build_tab_session();
        let task = spawn_blocking_into(
            cx,
            move |state| tab_session_save(state, &ws_id, &session),
            |_, _, _| {},
        );
        self._tasks.push(task);
    }

    /// Snapshots active editor widgets and flushes the workspace tab session synchronously before exit.
    pub fn flush_tab_session_sync(&mut self, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let session = self.build_tab_session();
        let _ = tab_session_save(crate::bridge::runtime(cx).state(), &ws_id, &session);
    }

    /// Returns unresolved `{{var}}` names referenced by the active request.
    #[must_use]
    pub fn active_unresolved_var_names(&self, _cx: &Context<Self>) -> Vec<String> {
        let Some(tab) = self.active_req() else {
            return Vec::new();
        };
        let tokens = request_tokens(&tab.draft);
        tokens
            .into_iter()
            .filter(|name| {
                !name.starts_with('$')
                    && !self.resolved_vars.iter().any(|rv| {
                        rv.name == *name
                            && !matches!(
                                rv.source,
                                spanbay_desktop::core::model::VariableResolutionSource::Unresolved
                            )
                    })
            })
            .collect()
    }

    // ── Keyboard shortcut handlers (called from AppShell when Requests is active) ──
}

fn to_session_editor_tab(r: &RequestTabState) -> EditorTab {
    if r.is_ai() {
        match r.ai_ed_tab {
            AiEdTab::Messages => EditorTab::Messages,
            AiEdTab::Params => EditorTab::Parameters,
            AiEdTab::Tools => EditorTab::Tools,
            AiEdTab::RawJson => EditorTab::RawJson,
            AiEdTab::Assertions => EditorTab::Assertions,
            AiEdTab::Scripts => EditorTab::Scripts,
        }
    } else {
        match r.http_ed_tab {
            HttpEdTab::Params => EditorTab::Params,
            HttpEdTab::Headers => EditorTab::Headers,
            HttpEdTab::Auth => EditorTab::Auth,
            HttpEdTab::Body => EditorTab::Body,
            HttpEdTab::Assertions => EditorTab::Assertions,
            HttpEdTab::Scripts => EditorTab::Scripts,
            HttpEdTab::Network => EditorTab::Network,
            HttpEdTab::Docs => EditorTab::Docs,
        }
    }
}

/// Ensures default AuthConfig struct for switching between the 8 auth schemes.
pub(super) fn default_auth_for_kind(kind: AuthKind) -> AuthConfig {
    match kind {
        AuthKind::Inherit => AuthConfig::Inherit,
        AuthKind::None => AuthConfig::None,
        AuthKind::Bearer => AuthConfig::Bearer {
            token_ref: "{{token}}".into(),
        },
        AuthKind::Basic => AuthConfig::Basic {
            username: String::new(),
            password_ref: String::new(),
        },
        AuthKind::ApiKey => AuthConfig::ApiKey {
            key: "X-API-Key".into(),
            value_ref: "{{api_key}}".into(),
            location: ApiKeyLocation::Header,
        },
        AuthKind::OAuth2 => AuthConfig::OAuth2 {
            grant_type: OAuth2GrantType::ClientCredentials,
            access_token_url: String::new(),
            client_id: String::new(),
            client_secret_ref: String::new(),
            username: String::new(),
            password_ref: String::new(),
            scope: String::new(),
            authorization_url: String::new(),
            redirect_port: None,
            client_auth: OAuth2ClientAuth::Body,
            extra_params: Vec::<KeyValue>::new(),
        },
        AuthKind::Digest => AuthConfig::Digest {
            username: String::new(),
            password_ref: None,
        },
        AuthKind::AwsSigV4 => AuthConfig::AwsSignatureV4 {
            access_key: String::new(),
            secret_key_ref: None,
            region: "us-east-1".into(),
            service: "execute-api".into(),
            session_token_ref: None,
        },
    }
}
