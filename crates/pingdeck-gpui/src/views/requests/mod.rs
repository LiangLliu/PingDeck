//! API Request Workbench module (`NavModule::Requests`).
//!
//! Native GPUI layout aligned with the PingDeck prototype:
//! - Left resizable Collections & Requests sidebar (`h_resizable`)
//! - Document tab bar (open tabs with method badges & close buttons)
//! - Combined URL strip (native Popover method picker, URL bar, Environment picker, Send/Save)
//! - Vertical resizable split (`v_resizable`) between Request Editor (Params / Headers / Auth /
//!   Body / Scripts / Snippets) and Response Inspector (Body / Headers / Cookies / Summary).

pub mod model;
pub mod panes;
pub mod sidebar;
pub mod toolbar;

pub use model::{ALL_METHODS, AuthMode, OpenReqTab, ReqTab, RespTab};

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Task,
    Window, div, px,
};
use pingdeck_desktop::core::app::snippet_service::{
    SnippetLanguage, SnippetOptions, request_code_snippet_impl,
};
use pingdeck_desktop::core::app::{
    CollectionDto, EnvironmentDto, ExecuteDraftRequestInput, SaveRequestInput,
    collection_list_impl, current_time_ms, environment_list_impl, request_delete_impl,
    request_execute_draft_impl, request_list_impl, request_save_impl, settings_get_impl,
};
use pingdeck_desktop::core::executor::ExecutionResult;
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::core::model::{
    ApiKeyLocation, AuthConfig, HttpMethod, KeyValue, NetworkOptions, ProtocolKind, Request,
    RequestBody,
};

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into};
use crate::ui::{
    EditorState, InputState, Tone, UiIcon, empty_state, h_resizable, palette, resizable_panel,
    to_hsla, v_col, v_resizable,
};

pub struct RequestsView {
    pub workspace_id: Option<String>,
    pub collections: Vec<CollectionDto>,
    pub requests: Vec<Request>,
    pub environments: Vec<EnvironmentDto>,
    pub active_env_id: Option<String>,
    pub open_tabs: Vec<OpenReqTab>,
    pub selected_id: Option<String>,
    pub method: HttpMethod,
    pub req_tab: ReqTab,
    pub resp_tab: RespTab,
    pub snippet_lang: SnippetLanguage,
    pub query_params: Vec<KeyValue>,
    pub headers: Vec<KeyValue>,
    pub auth_mode: AuthMode,
    pub search_input: Entity<InputState>,
    pub name_input: Entity<InputState>,
    pub url_input: Entity<InputState>,
    pub new_param_key: Entity<InputState>,
    pub new_param_val: Entity<InputState>,
    pub new_hdr_key: Entity<InputState>,
    pub new_hdr_val: Entity<InputState>,
    pub auth_primary: Entity<InputState>,
    pub auth_secondary: Entity<InputState>,
    pub body_editor: Entity<EditorState>,
    pub script_editor: Entity<EditorState>,
    pub snippet_editor: Entity<EditorState>,
    pub response_editor: Entity<EditorState>,
    pub last_response: Option<ExecutionResult>,
    pub sending: bool,
    pub status_msg: Option<(Tone, String)>,
    _tasks: Vec<Task<()>>,
}

impl RequestsView {
    pub fn new(workspace_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索请求 / Filter requests…"));
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Request name"));
        let url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://api.example.com/v1/resource")
                .default_value("https://httpbin.org/get")
        });
        let new_param_key = cx.new(|cx| InputState::new(window, cx).placeholder("key"));
        let new_param_val = cx.new(|cx| InputState::new(window, cx).placeholder("value"));
        let new_hdr_key = cx.new(|cx| InputState::new(window, cx).placeholder("Header-Name"));
        let new_hdr_val = cx.new(|cx| InputState::new(window, cx).placeholder("value"));
        let auth_primary =
            cx.new(|cx| InputState::new(window, cx).placeholder("Token / Username / Header name"));
        let auth_secondary = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Password / API key value")
                .masked(true)
        });
        let body_editor = cx.new(|cx| {
            let mut ed =
                EditorState::new(window, cx).default_value("{\n  \"hello\": \"PingDeck\"\n}");
            ed.set_highlighter("json", cx);
            ed
        });
        let script_editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx)
                .default_value("// Pre-request / test assertions (JavaScript)\n");
            ed.set_highlighter("javascript", cx);
            ed
        });
        let snippet_editor = cx.new(|cx| {
            let mut ed =
                EditorState::new(window, cx).default_value("# Select a snippet language above");
            ed.set_highlighter("bash", cx);
            ed
        });
        let response_editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx)
                .default_value("// Send a request to inspect the live response here");
            ed.set_highlighter("json", cx);
            ed
        });

        let mut view = Self {
            workspace_id: workspace_id.clone(),
            collections: Vec::new(),
            requests: Vec::new(),
            environments: Vec::new(),
            active_env_id: None,
            open_tabs: Vec::new(),
            selected_id: None,
            method: HttpMethod::Get,
            req_tab: ReqTab::Body,
            resp_tab: RespTab::Body,
            snippet_lang: SnippetLanguage::Curl,
            query_params: Vec::new(),
            headers: Vec::new(),
            auth_mode: AuthMode::None,
            search_input,
            name_input,
            url_input,
            new_param_key,
            new_param_val,
            new_hdr_key,
            new_hdr_val,
            auth_primary,
            auth_secondary,
            body_editor,
            script_editor,
            snippet_editor,
            response_editor,
            last_response: None,
            sending: false,
            status_msg: None,
            _tasks: Vec::new(),
        };
        if let Some(ws) = workspace_id {
            view.reload(ws, cx);
        }
        view
    }

    pub fn set_workspace(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) {
        if self.workspace_id == workspace_id {
            return;
        }
        self.workspace_id = workspace_id.clone();
        self.collections.clear();
        self.requests.clear();
        self.environments.clear();
        self.open_tabs.clear();
        self.active_env_id = None;
        self.selected_id = None;
        if let Some(ws) = workspace_id {
            self.reload(ws, cx);
        }
        cx.notify();
    }

    pub fn reload(&mut self, workspace_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let cols = collection_list_impl(state, workspace_id.clone()).unwrap_or_default();
                let reqs = request_list_impl(state, workspace_id.clone()).unwrap_or_default();
                let envs = environment_list_impl(state, workspace_id).unwrap_or_default();
                (cols, reqs, envs)
            },
            |this, (cols, reqs, envs), cx| {
                this.collections = cols;
                this.requests = reqs;
                this.environments = envs;
                if let Some(env_id) = &this.active_env_id
                    && !this.environments.iter().any(|e| &e.id == env_id)
                {
                    this.active_env_id = None;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn select_request(&mut self, req: &Request, window: &mut Window, cx: &mut Context<Self>) {
        let req_id = req.id.as_str().to_owned();
        self.selected_id = Some(req_id.clone());
        self.method = req.method;
        self.query_params = req.query_params.clone();
        self.headers = req.headers.clone();

        if let Some(tab) = self.open_tabs.iter_mut().find(|t| t.id == req_id) {
            tab.name = req.name.clone();
            tab.method = req.method;
        } else {
            self.open_tabs.push(OpenReqTab {
                id: req_id,
                name: req.name.clone(),
                method: req.method,
            });
            if self.open_tabs.len() > 10 {
                self.open_tabs.remove(0);
            }
        }

        match &req.auth {
            AuthConfig::Bearer { token_ref } => {
                self.auth_mode = AuthMode::Bearer;
                self.auth_primary
                    .update(cx, |i, cx| i.set_value(token_ref.clone(), window, cx));
            }
            AuthConfig::Basic {
                username,
                password_ref,
            } => {
                self.auth_mode = AuthMode::Basic;
                self.auth_primary
                    .update(cx, |i, cx| i.set_value(username.clone(), window, cx));
                self.auth_secondary
                    .update(cx, |i, cx| i.set_value(password_ref.clone(), window, cx));
            }
            AuthConfig::ApiKey { key, value_ref, .. } => {
                self.auth_mode = AuthMode::ApiKey;
                self.auth_primary
                    .update(cx, |i, cx| i.set_value(key.clone(), window, cx));
                self.auth_secondary
                    .update(cx, |i, cx| i.set_value(value_ref.clone(), window, cx));
            }
            _ => {
                self.auth_mode = AuthMode::None;
            }
        }

        self.name_input
            .update(cx, |i, cx| i.set_value(req.name.clone(), window, cx));
        self.url_input
            .update(cx, |i, cx| i.set_value(req.url.clone(), window, cx));
        let body_text = match &req.body {
            RequestBody::Json { content } | RequestBody::RawText { content, .. } => content.clone(),
            _ => String::new(),
        };
        self.body_editor
            .update(cx, |ed, cx| ed.set_value(body_text, window, cx));
        self.script_editor.update(cx, |ed, cx| {
            ed.set_value(req.test_script.clone(), window, cx)
        });
        if self.req_tab == ReqTab::Snippets {
            self.refresh_snippet(self.snippet_lang, window, cx);
        }
        cx.notify();
    }

    pub fn new_blank_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_id = None;
        self.method = HttpMethod::Get;
        self.query_params.clear();
        self.headers.clear();
        self.auth_mode = AuthMode::None;
        self.name_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.url_input
            .update(cx, |i, cx| i.set_value("https://", window, cx));
        self.body_editor
            .update(cx, |ed, cx| ed.set_value("", window, cx));
        cx.notify();
    }

    pub fn close_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        self.open_tabs.retain(|t| t.id != id);
        if self.selected_id.as_deref() == Some(id) {
            self.selected_id = self.open_tabs.last().map(|t| t.id.clone());
        }
        cx.notify();
    }

    pub fn add_param(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.new_param_key.read(cx).value().trim().to_owned();
        let value = self.new_param_val.read(cx).value().to_string();
        if key.is_empty() {
            return;
        }
        self.query_params.push(KeyValue {
            key,
            value,
            enabled: true,
            description: None,
        });
        self.new_param_key
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.new_param_val
            .update(cx, |i, cx| i.set_value("", window, cx));
        cx.notify();
    }

    pub fn add_header(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.new_hdr_key.read(cx).value().trim().to_owned();
        let value = self.new_hdr_val.read(cx).value().to_string();
        if key.is_empty() {
            return;
        }
        self.headers.push(KeyValue {
            key,
            value,
            enabled: true,
            description: None,
        });
        self.new_hdr_key
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.new_hdr_val
            .update(cx, |i, cx| i.set_value("", window, cx));
        cx.notify();
    }

    fn build_auth(&self, cx: &Context<Self>) -> AuthConfig {
        match self.auth_mode {
            AuthMode::None => AuthConfig::None,
            AuthMode::Bearer => AuthConfig::Bearer {
                token_ref: self.auth_primary.read(cx).value().trim().to_owned(),
            },
            AuthMode::Basic => AuthConfig::Basic {
                username: self.auth_primary.read(cx).value().trim().to_owned(),
                password_ref: self.auth_secondary.read(cx).value().to_string(),
            },
            AuthMode::ApiKey => AuthConfig::ApiKey {
                key: self.auth_primary.read(cx).value().trim().to_owned(),
                value_ref: self.auth_secondary.read(cx).value().to_string(),
                location: ApiKeyLocation::Header,
            },
        }
    }

    fn build_draft_input(&self, cx: &Context<Self>) -> Option<SaveRequestInput> {
        let ws_id = self.workspace_id.clone()?;
        let id = self
            .selected_id
            .clone()
            .unwrap_or_else(|| format!("req-{}", current_time_ms()));
        let name_raw = self.name_input.read(cx).value().trim().to_owned();
        let url = self.url_input.read(cx).value().trim().to_owned();
        let name = if name_raw.is_empty() {
            if url.is_empty() {
                "Untitled Request".to_owned()
            } else {
                url.clone()
            }
        } else {
            name_raw
        };
        let raw_body = self.body_editor.read(cx).value().to_string();
        let body = if matches!(
            self.method,
            HttpMethod::Get | HttpMethod::Delete | HttpMethod::Head
        ) || raw_body.trim().is_empty()
        {
            RequestBody::None
        } else {
            RequestBody::Json { content: raw_body }
        };
        let existing_col = self
            .selected_id
            .as_deref()
            .and_then(|sid| self.requests.iter().find(|r| r.id.as_str() == sid))
            .and_then(|r| r.collection_id.as_ref().map(|c| c.as_str().to_owned()));

        Some(SaveRequestInput {
            id,
            workspace_id: ws_id,
            collection_id: existing_col,
            folder_id: None,
            name,
            protocol: ProtocolKind::HttpRest,
            method: self.method,
            url,
            query_params: self.query_params.clone(),
            path_variables: Vec::new(),
            headers: self.headers.clone(),
            auth: self.build_auth(cx),
            body,
            network_options: NetworkOptions::default(),
            assertions: Vec::new(),
            description: String::new(),
            pre_request_script: String::new(),
            test_script: self.script_editor.read(cx).value().to_string(),
            position: 0,
            mode: Default::default(),
            ai: None,
            examples: None,
        })
    }

    pub fn refresh_snippet(
        &mut self,
        lang: SnippetLanguage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snippet_lang = lang;
        let Some(draft) = self.build_draft_input(cx) else {
            return;
        };
        let Ok(mut req) = Request::new(&draft.id, &draft.workspace_id, &draft.name, &draft.url)
        else {
            return;
        };
        req.method = draft.method;
        req.query_params = draft.query_params;
        req.headers = draft.headers;
        req.auth = draft.auth;
        req.body = draft.body;
        let rt = runtime(cx);
        let code = request_code_snippet_impl(rt.state(), &req, lang, &SnippetOptions::default())
            .unwrap_or_else(|e| format!("// {}", e.message()));
        let hl = match lang {
            SnippetLanguage::Curl => "bash",
            SnippetLanguage::Fetch | SnippetLanguage::Axios => "javascript",
            SnippetLanguage::Python => "python",
            SnippetLanguage::Go => "go",
            _ => "bash",
        };
        self.snippet_editor.update(cx, |ed, cx| {
            ed.set_highlighter(hl, cx);
            ed.set_value(code, window, cx);
        });
        cx.notify();
    }

    pub fn save_current(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.build_draft_input(cx) else {
            return;
        };
        let ws_id = input.workspace_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| request_save_impl(state, input),
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        this.selected_id = Some(saved.id.as_str().to_owned());
                        this.status_msg = Some((Tone::Success, format!("Saved {}", saved.name)));
                        this.reload(ws_id, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn send_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(req) = self.build_draft_input(cx) else {
            return;
        };
        self.sending = true;
        self.status_msg = None;
        let input = ExecuteDraftRequestInput {
            request: req,
            environment_id: self.active_env_id.clone(),
            confirmed: Some(true),
            save_to_history: Some(true),
        };
        let resp_editor = self.response_editor.clone();
        let window_handle = window.window_handle();
        let task = spawn_async_into(
            cx,
            move |state| async move { request_execute_draft_impl(&state, input, None).await },
            move |this, res, cx| {
                this.sending = false;
                match res {
                    Ok(exec) => {
                        let body = exec.body_text.clone();
                        this.last_response = Some(exec);
                        let _ = window_handle.update(cx, |_, window, cx| {
                            resp_editor.update(cx, |ed, cx| ed.set_value(body, window, cx));
                        });
                    }
                    Err(err) => {
                        this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
        cx.notify();
    }

    pub fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some(req_id)) = (self.workspace_id.clone(), self.selected_id.clone())
        else {
            return;
        };
        let ws_reload = ws_id.clone();
        let del_id = req_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| request_delete_impl(state, ws_id, req_id),
            move |this, res, cx| {
                if res.is_ok() {
                    this.open_tabs.retain(|t| t.id != del_id);
                    this.selected_id = None;
                    this.reload(ws_reload, cx);
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}

impl Render for RequestsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let zh = settings_get_impl(runtime(cx).state()).general.language == Language::ZhCn;

        let Some(_ws) = self.workspace_id.as_ref() else {
            return empty_state(
                UiIcon::Send,
                if zh {
                    "未选择工作区"
                } else {
                    "No workspace selected"
                },
                if zh {
                    "先在左侧选择或创建一个工作区以编辑和发送 HTTP/AI 请求。"
                } else {
                    "Select or create a workspace in the sidebar to craft and send requests."
                },
                cx,
            )
            .into_any_element();
        };

        let doc_tabbar = toolbar::render_doc_tabbar(self, zh, cx);
        let url_strip = toolbar::render_url_strip(self, zh, cx);

        let right_work = v_col()
            .size_full()
            .min_w_0()
            .bg(to_hsla(p.surface))
            .child(doc_tabbar)
            .child(url_strip)
            .child(
                div().flex_1().min_h_0().child(
                    v_resizable("req-vert-split")
                        .child(
                            resizable_panel()
                                .size(px(300.0))
                                .size_range(px(170.0)..px(560.0))
                                .child(panes::render_request_editor(self, zh, cx)),
                        )
                        .child(
                            resizable_panel()
                                .size_range(px(160.0)..px(580.0))
                                .child(panes::render_response_inspector(self, zh, cx)),
                        ),
                ),
            );

        h_resizable("req-main-split")
            .child(
                resizable_panel()
                    .size(px(262.0))
                    .size_range(px(200.0)..px(380.0))
                    .child(sidebar::render_sidebar(self, zh, cx)),
            )
            .child(resizable_panel().child(right_work))
            .into_any_element()
    }
}
