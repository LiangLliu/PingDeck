//! Complete API & AI Request Workbench module (`NavModule::Requests`).
//!
//! Pure-Rust GPUI workbench implementing the full PingDeck Requests loop:
//! - Resizable Collection / Nested Folder / Request tree with create, rename, move, duplicate,
//!   delete + undo, agent-touched filter, and Import / Export
//! - Multi-tab isolated document workbench (HTTP/GraphQL requests, AI API requests, Collection
//!   Runner tabs, and Collection/Folder Settings tabs) with per-tab drafts, dirty-close guard,
//!   and workspace tab-session persistence
//! - Bidirectional URL ⇄ Query Params & Path Variables sync, full 8-scheme Auth + OAuth 2.0
//!   token card, 7-mode Body editor, Assertions + JSON Schema builder, Pre/Post Scripts with Boa
//!   compat lint, 7 per-field inheritable Network options, Markdown Docs + Response Examples
//! - Live HTTP & streaming AI execution with cancel, Production/Agent guards, timing waterfall,
//!   call-chain trace, JSONPath filter + extract-to-env, multi-model AI compare, `⌘J` Console
//!   drawer, and cross-module handoffs.

pub mod actions_ai;
pub mod actions_compare;
pub mod actions_crud;
pub mod actions_exec;
pub mod actions_io;
pub mod actions_runner;
pub mod actions_tools;
pub mod actions_tree;
pub mod console;
pub mod container_settings;
pub mod editor_ai;
pub mod editor_ai_bar;
pub mod editor_assertions;
pub mod editor_auth;
pub mod editor_auth_net;
pub mod editor_body;
pub mod editor_http;
pub mod editor_network;
pub mod editor_scripts_docs;
pub mod model;
pub mod model_ai;
pub mod model_tabs;
pub mod overlays;
pub mod overlays_create;
pub mod overlays_dialogs;
pub mod resp_ai;
pub mod resp_ai_tabs;
pub mod resp_http;
pub mod resp_http_tabs;
pub mod runner_and_settings;
pub mod sidebar;
pub mod sidebar_nodes;
pub mod state_hydrate;
pub mod state_sync;
pub mod toolbar;
pub mod toolbar_url;
pub mod widgets_init;

use std::cell::Cell;
use std::collections::HashSet;

use gpui_kit::{
    Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Subscription, Task,
    Window, div, px,
};
use pingdeck_desktop::core::app::snippet_service::SnippetLanguage;
use pingdeck_desktop::core::app::{
    CollectionDto, EnvironmentDto, ProviderView, ResolvedVariable, UndoToken, current_time_ms,
    settings_get_impl,
};
use pingdeck_desktop::core::llm::store::ProviderModel;
use pingdeck_desktop::core::model::script_result::ConsoleLevel;
use pingdeck_desktop::core::model::settings::{Language, Settings};
use pingdeck_desktop::core::model::{Folder, Request};

use crate::bridge::runtime;
use crate::ui::{
    EditorState, InputState, TextareaState, Tone, UiIcon, btn_primary, btn_secondary,
    dismissible_banner, empty_state, empty_state_with_actions, h_resizable, h_row, icon_xs,
    palette, resizable_panel, to_hsla, v_col, v_resizable,
};

pub use model::{ALL_METHODS, ActiveModal, OpenTab, RequestsHandoff};
use model::{ConsoleRecord, ImportExportTab, NewItemKind, RequestTabState};
use state_sync::default_auth_for_kind;

pub struct RequestsView {
    pub workspace_id: Option<String>,
    pub settings: Settings,
    pub collections: Vec<CollectionDto>,
    pub folders: Vec<Folder>,
    pub requests: Vec<Request>,
    pub environments: Vec<EnvironmentDto>,
    pub active_env_id: Option<String>,
    pub resolved_vars: Vec<ResolvedVariable>,
    pub providers: Vec<ProviderView>,
    pub models: Vec<ProviderModel>,

    // Sidebar state
    pub collapsed_nodes: HashSet<String>,
    pub agent_touched_only: bool,
    pub last_undo: Option<(UndoToken, String)>,

    // Multi-tab state
    pub tabs: Vec<OpenTab>,
    pub active_tab_id: Option<String>,

    // Shared active-tab editor widgets (hydrated on tab switch, snapshotted before leave)
    pub search_input: Entity<InputState>,
    pub name_input: Entity<InputState>,
    pub url_input: Entity<InputState>,
    pub kv_key_input: Entity<InputState>,
    pub kv_val_input: Entity<InputState>,
    pub kv_desc_input: Entity<InputState>,
    pub kv_file_input: Entity<InputState>,
    pub auth_f1: Entity<InputState>,
    pub auth_f2: Entity<InputState>,
    pub auth_f3: Entity<InputState>,
    pub auth_f4: Entity<InputState>,
    pub auth_f5: Entity<InputState>,
    pub auth_f6: Entity<InputState>,
    pub body_editor: Entity<EditorState>,
    pub gql_vars_editor: Entity<EditorState>,
    pub binary_path_input: Entity<InputState>,
    pub pre_script_editor: Entity<EditorState>,
    pub post_script_editor: Entity<EditorState>,
    pub docs_input: Entity<TextareaState>,
    pub net_timeout_input: Entity<InputState>,
    pub net_total_timeout_input: Entity<InputState>,
    pub net_retry_input: Entity<InputState>,
    pub net_max_redir_input: Entity<InputState>,
    pub net_proxy_input: Entity<InputState>,
    pub assert_target_input: Entity<InputState>,
    pub assert_val_input: Entity<InputState>,
    pub assert_target_kind: usize,
    pub assert_op_idx: usize,
    pub oauth_var_input: Entity<InputState>,

    // AI request widgets
    pub ai_custom_url_input: Entity<InputState>,
    pub ai_custom_model_input: Entity<InputState>,
    pub ai_msg_new_input: Entity<TextareaState>,
    pub ai_temp_input: Entity<InputState>,
    pub ai_max_tok_input: Entity<InputState>,
    pub ai_top_p_input: Entity<InputState>,
    pub ai_tools_editor: Entity<EditorState>,
    pub ai_raw_editor: Entity<EditorState>,

    // Response widgets
    pub response_editor: Entity<EditorState>,
    pub resp_filter_input: Entity<InputState>,

    // Runner & Container Settings widgets
    pub runner_iter_input: Entity<InputState>,
    pub runner_delay_input: Entity<InputState>,
    pub container_name_input: Entity<InputState>,
    pub container_desc_input: Entity<TextareaState>,
    pub container_var_key: Entity<InputState>,
    pub container_var_val: Entity<InputState>,
    pub container_var_secret: bool,
    pub container_pre_editor: Entity<EditorState>,
    pub container_post_editor: Entity<EditorState>,

    // Modal & snippet widgets
    pub active_modal: Option<ActiveModal>,
    pub snippet_lang: SnippetLanguage,
    pub snippet_editor: Entity<EditorState>,
    pub modal_name_input: Entity<InputState>,
    pub modal_text_input: Entity<TextareaState>,

    // Console & feedback
    pub console_open: bool,
    pub console_entries: Vec<ConsoleRecord>,
    pub status_msg: Option<(Tone, String)>,
    pub(super) pending_handoff: Cell<Option<RequestsHandoff>>,
    pub(super) pending_hydrate: bool,
    pub(super) hydrating: bool,
    _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl RequestsView {
    pub fn set_workspace(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) {
        if self.workspace_id == workspace_id {
            return;
        }
        self.persist_tab_session(cx);
        self.workspace_id = workspace_id.clone();
        self.collections.clear();
        self.folders.clear();
        self.requests.clear();
        self.environments.clear();
        self.resolved_vars.clear();
        self.tabs.clear();
        self.active_tab_id = None;
        self.active_env_id = None;
        self.last_undo = None;
        self.active_modal = None;
        if let Some(ws) = workspace_id {
            self.reload_workspace(ws, true, cx);
        }
        cx.notify();
    }

    /// Refreshes environments, settings, and AI provider/model catalogs when entering Requests.
    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.settings = settings_get_impl(runtime(cx).state());
        self.reload_ai_catalog(cx);
        if let Some(ws) = self.workspace_id.clone() {
            self.reload_workspace_no_window(ws, cx);
        }
    }

    pub fn take_pending_handoff(&self) -> Option<RequestsHandoff> {
        self.pending_handoff.take()
    }

    pub fn emit_handoff(&self, handoff: RequestsHandoff, cx: &mut Context<Self>) {
        self.pending_handoff.set(Some(handoff));
        cx.notify();
    }

    pub fn push_console(
        &mut self,
        tag: &'static str,
        level: ConsoleLevel,
        summary: impl Into<String>,
        detail: Option<String>,
    ) {
        self.console_entries.push(ConsoleRecord {
            ts_ms: current_time_ms(),
            tag,
            level,
            summary: summary.into(),
            detail,
        });
        if self.console_entries.len() > 500 {
            let excess = self.console_entries.len() - 500;
            self.console_entries.drain(0..excess);
        }
    }

    #[must_use]
    pub fn active_tab(&self) -> Option<&OpenTab> {
        let id = self.active_tab_id.as_deref()?;
        self.tabs.iter().find(|t| t.tab_id() == id)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut OpenTab> {
        let id = self.active_tab_id.clone()?;
        self.tabs.iter_mut().find(|t| t.tab_id() == id.as_str())
    }

    #[must_use]
    pub fn active_req(&self) -> Option<&RequestTabState> {
        match self.active_tab()? {
            OpenTab::Request(t) => Some(t),
            _ => None,
        }
    }

    pub fn active_req_mut(&mut self) -> Option<&mut RequestTabState> {
        match self.active_tab_mut()? {
            OpenTab::Request(t) => Some(t),
            _ => None,
        }
    }

    pub fn tab_req_mut(&mut self, tab_id: &str) -> Option<&mut RequestTabState> {
        self.tabs.iter_mut().find_map(|t| match t {
            OpenTab::Request(r) if r.tab_id == tab_id => Some(r.as_mut()),
            _ => None,
        })
    }

    pub fn mark_active_dirty(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_req_mut() {
            tab.dirty = true;
            cx.notify();
        }
    }

    pub fn handle_save_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        match self.active_tab() {
            Some(OpenTab::Request(_)) => self.save_active_tab(false, window, cx),
            Some(OpenTab::Container(_)) => self.save_container_settings_tab(cx),
            _ => {}
        }
    }

    pub fn handle_send_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        match self.active_tab() {
            Some(OpenTab::Request(t)) if t.is_ai() => {
                self.send_active_ai_request(None, window, cx);
            }
            Some(OpenTab::Request(_)) => {
                self.send_active_request(None, None, window, cx);
            }
            Some(OpenTab::Runner(r)) if !r.running => {
                let tid = r.tab_id.clone();
                self.start_collection_run(tid, None, cx);
            }
            _ => {}
        }
    }

    pub fn handle_toggle_console(&mut self, cx: &mut Context<Self>) {
        self.console_open = !self.console_open;
        cx.notify();
    }

    pub fn handle_new_tab_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_new_request_tab(model::NewItemKind::HttpGet, None, None, window, cx);
    }

    pub fn handle_close_tab_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tid) = self.active_tab_id.clone() {
            self.request_close_tab(&tid, window, cx);
        }
    }

    pub fn dismiss_modal_if_open(&mut self, cx: &mut Context<Self>) -> bool {
        if self.active_modal.take().is_some() {
            cx.notify();
            return true;
        }
        false
    }
}

impl Render for RequestsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_hydrate {
            self.pending_hydrate = false;
            self.hydrate_active_tab(window, cx);
        }
        let p = palette(cx);
        let zh = self.settings.general.language == Language::ZhCn;

        if self.workspace_id.is_none() {
            return empty_state(
                UiIcon::Send,
                if zh {
                    "未选择工作区"
                } else {
                    "No workspace selected"
                },
                if zh {
                    "先在左侧选择或创建一个工作区以编辑和发送 HTTP / GraphQL / AI 请求。"
                } else {
                    "Select or create a workspace in the sidebar to craft and send requests."
                },
                cx,
            )
            .into_any_element();
        }

        let doc_tabbar = toolbar::render_doc_tabbar(self, zh, cx);

        let main_body: gpui_kit::AnyElement = match self.active_tab() {
            Some(OpenTab::Request(tab)) if tab.is_ai() => v_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(toolbar::render_identity_strip(self, zh, cx))
                .child(editor_ai::render_ai_url_strip(self, zh, cx))
                .child(
                    div().flex_1().min_h_0().w_full().child(
                        v_resizable("req-ai-vert-split")
                            .child(
                                resizable_panel()
                                    .size(px(300.0))
                                    .size_range(px(160.0)..px(560.0))
                                    .child(editor_ai::render_ai_editor(self, zh, cx)),
                            )
                            .child(
                                resizable_panel()
                                    .size_range(px(180.0)..px(640.0))
                                    .child(resp_ai::render_ai_response(self, zh, cx)),
                            ),
                    ),
                )
                .into_any_element(),
            Some(OpenTab::Request(_)) => v_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(toolbar::render_identity_strip(self, zh, cx))
                .child(toolbar::render_http_url_strip(self, zh, cx))
                .child(
                    div().flex_1().min_h_0().w_full().child(
                        v_resizable("req-http-vert-split")
                            .child(
                                resizable_panel()
                                    .size(px(290.0))
                                    .size_range(px(160.0)..px(560.0))
                                    .child(editor_http::render_http_editor(self, zh, cx)),
                            )
                            .child(
                                resizable_panel()
                                    .size_range(px(180.0)..px(640.0))
                                    .child(resp_http::render_http_response(self, zh, cx)),
                            ),
                    ),
                )
                .into_any_element(),
            Some(OpenTab::Runner(_)) => {
                runner_and_settings::render_runner_tab(self, zh, cx).into_any_element()
            }
            Some(OpenTab::Container(_)) => {
                runner_and_settings::render_container_settings_tab(self, zh, cx).into_any_element()
            }
            None => empty_state_with_actions(
                UiIcon::Send,
                if zh {
                    "打开或新建一个请求"
                } else {
                    "Open or create a request"
                },
                if zh {
                    "从左侧集合树选择请求，或点击下方按钮立即新建 HTTP / GraphQL / AI 调试请求，也可直接导入 cURL 或 Postman。"
                } else {
                    "Pick a request from the sidebar, or create an HTTP, GraphQL, or AI request below — or import cURL / Postman."
                },
                Some(
                    h_row()
                        .gap(px(8.0))
                        .flex_wrap()
                        .justify_center()
                        .child(
                            btn_primary(
                                "req-empty-new-http",
                                if zh { "新建 HTTP 请求" } else { "New HTTP" },
                            )
                            .icon(icon_xs(UiIcon::Plus))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_new_request_tab(NewItemKind::HttpGet, None, None, window, cx);
                            })),
                        )
                        .child(
                            btn_secondary(
                                "req-empty-new-ai",
                                if zh { "新建 AI 调试" } else { "New AI API" },
                            )
                            .icon(icon_xs(UiIcon::Sparkles))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_new_request_tab(NewItemKind::AiApi, None, None, window, cx);
                            })),
                        )
                        .child(
                            btn_secondary(
                                "req-empty-import-btn",
                                if zh { "导入 cURL / Postman…" } else { "Import…" },
                            )
                            .icon(icon_xs(UiIcon::Upload))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_import_export_modal(
                                    ImportExportTab::Import,
                                    None,
                                    window,
                                    cx,
                                );
                            })),
                        )
                        .into_any_element(),
                ),
                cx,
            )
            .into_any_element(),
        };

        let mut right_work = v_col()
            .size_full()
            .min_w_0()
            .bg(to_hsla(p.surface))
            .child(doc_tabbar);

        if let Some((tone, msg)) = &self.status_msg {
            right_work =
                right_work.child(div().px(px(12.0)).pt(px(6.0)).child(dismissible_banner(
                    "req-status-dismiss",
                    *tone,
                    msg.clone(),
                    cx.listener(|this, _, _, cx| {
                        this.status_msg = None;
                        cx.notify();
                    }),
                    cx,
                )));
        }

        right_work = right_work.child(main_body);

        if let Some(drawer) = console::render_console_drawer(self, zh, cx) {
            right_work = right_work.child(drawer);
        }
        right_work = right_work.child(console::render_footer_bar(self, zh, cx));

        let mut root = div().relative().size_full().child(
            h_resizable("req-main-split")
                .child(
                    resizable_panel()
                        .size(px(268.0))
                        .size_range(px(200.0)..px(400.0))
                        .child(sidebar::render_sidebar(self, zh, cx)),
                )
                .child(resizable_panel().child(right_work)),
        );

        if let Some(modal) = overlays::render_active_modal(self, zh, window, cx) {
            root = root.child(modal);
        }

        root.into_any_element()
    }
}
