//! Workspace reload, tab open/switch/close, dirty checks, and request save/save-as.

use gpui_kit::{Context, Window};
use spanbay_desktop::core::app::request_model_service::{
    EffectiveAuthInput, request_effective_auth,
};
use spanbay_desktop::core::app::tab_session_service::tab_session_load;
use spanbay_desktop::core::app::{
    ai_config_get_impl, collection_list_impl, current_time_ms, environment_list_impl,
    environment_resolved_list_impl, folder_list_impl, request_list_impl, request_save_checked_impl,
    request_save_impl, settings_get_impl,
};
use spanbay_desktop::core::error::AppError;
use spanbay_desktop::core::model::{HttpMethod, ProtocolKind, Request, RequestBody};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::RequestsView;
use super::model::{ActiveModal, NewItemKind, OpenTab, RequestTabState, default_ai_request};

impl RequestsView {
    /// Reloads collections, nested folders, requests, environments, and optionally restores saved tabs.
    pub fn reload_workspace(
        &mut self,
        workspace_id: String,
        restore_tabs: bool,
        cx: &mut Context<Self>,
    ) {
        let active_env = self.active_env_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let settings = settings_get_impl(state);
                let cols = collection_list_impl(state, workspace_id.clone()).unwrap_or_default();
                let mut folders = Vec::new();
                for c in &cols {
                    if let Ok(mut fs) = folder_list_impl(state, workspace_id.clone(), c.id.clone())
                    {
                        folders.append(&mut fs);
                    }
                }
                let reqs = request_list_impl(state, workspace_id.clone()).unwrap_or_default();
                let envs = environment_list_impl(state, workspace_id.clone()).unwrap_or_default();
                let restored = if restore_tabs {
                    tab_session_load(state, &workspace_id).ok().flatten()
                } else {
                    None
                };
                let eff_env = active_env
                    .filter(|id| envs.iter().any(|e| &e.id == id))
                    .or_else(|| envs.first().map(|e| e.id.clone()));
                let resolved =
                    environment_resolved_list_impl(state, workspace_id, eff_env.clone(), None)
                        .unwrap_or_default();
                (
                    settings, cols, folders, reqs, envs, eff_env, resolved, restored,
                )
            },
            move |this, (settings, cols, folders, reqs, envs, eff_env, resolved, restored), cx| {
                this.settings = settings;
                this.collections = cols;
                this.folders = folders;
                this.requests = reqs;
                this.environments = envs;
                this.active_env_id = eff_env;
                this.resolved_vars = resolved;

                if restore_tabs && this.tabs.is_empty() {
                    if let Some(session) = restored
                        && !session.tabs.is_empty()
                    {
                        for rt in session.tabs {
                            let saved = rt.request_id.is_some();
                            let mut st =
                                RequestTabState::from_request(rt.tab_id.clone(), rt.draft, saved);
                            st.dirty = rt.dirty;
                            this.tabs.push(OpenTab::Request(Box::new(st)));
                        }
                        this.active_tab_id = session
                            .active_tab_id
                            .or_else(|| this.tabs.first().map(|t| t.tab_id().to_owned()));
                    } else if let Some(first) = this.requests.first().cloned() {
                        let tid = format!("tab-{}", first.id.as_str());
                        this.tabs
                            .push(OpenTab::Request(Box::new(RequestTabState::from_request(
                                tid.clone(),
                                first,
                                true,
                            ))));
                        this.active_tab_id = Some(tid);
                    } else if let Some(ws) = this.workspace_id.clone()
                        && let Ok(blank) = Request::new(
                            format!("req-{}", current_time_ms()),
                            &ws,
                            "Untitled Request",
                            "https://httpbin.org/get",
                        )
                    {
                        let tid = format!("tab-draft-{}", current_time_ms());
                        this.tabs
                            .push(OpenTab::Request(Box::new(RequestTabState::from_request(
                                tid.clone(),
                                blank,
                                false,
                            ))));
                        this.active_tab_id = Some(tid);
                    }
                    this.pending_hydrate = true;
                    this.refresh_effective_auth(cx);
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn reload_workspace_no_window(&mut self, workspace_id: String, cx: &mut Context<Self>) {
        let active_env = self.active_env_id.clone();
        let active_col = self.active_req().and_then(|t| {
            t.draft
                .collection_id
                .as_ref()
                .map(|c| c.as_str().to_owned())
        });
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let settings = settings_get_impl(state);
                let cols = collection_list_impl(state, workspace_id.clone()).unwrap_or_default();
                let mut folders = Vec::new();
                for c in &cols {
                    if let Ok(mut fs) = folder_list_impl(state, workspace_id.clone(), c.id.clone())
                    {
                        folders.append(&mut fs);
                    }
                }
                let reqs = request_list_impl(state, workspace_id.clone()).unwrap_or_default();
                let envs = environment_list_impl(state, workspace_id.clone()).unwrap_or_default();
                let eff_env = active_env.filter(|id| envs.iter().any(|e| &e.id == id));
                let resolved = environment_resolved_list_impl(
                    state,
                    workspace_id,
                    eff_env.clone(),
                    active_col,
                )
                .unwrap_or_default();
                (settings, cols, folders, reqs, envs, eff_env, resolved)
            },
            |this, (settings, cols, folders, reqs, envs, eff_env, resolved), cx| {
                this.settings = settings;
                this.collections = cols;
                this.folders = folders;
                this.requests = reqs;
                this.environments = envs;
                if this.active_env_id.is_some() {
                    this.active_env_id = eff_env;
                }
                this.resolved_vars = resolved;
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn reload_ai_catalog(&mut self, cx: &mut Context<Self>) {
        let task = spawn_async_into(
            cx,
            |state| async move { ai_config_get_impl(&state).await.ok() },
            |this, cfg, cx| {
                if let Some(c) = cfg {
                    this.providers = c.providers;
                    this.models = c.models;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn reload_resolved_vars(&mut self, cx: &mut Context<Self>) {
        let Some(ws) = self.workspace_id.clone() else {
            return;
        };
        let env_id = self.active_env_id.clone();
        let col_id = self.active_req().and_then(|t| {
            t.draft
                .collection_id
                .as_ref()
                .map(|c| c.as_str().to_owned())
        });
        let task = spawn_blocking_into(
            cx,
            move |state| {
                environment_resolved_list_impl(state, ws, env_id, col_id).unwrap_or_default()
            },
            |this, vars, cx| {
                this.resolved_vars = vars;
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn refresh_effective_auth(&mut self, cx: &mut Context<Self>) {
        let Some(ws) = self.workspace_id.clone() else {
            return;
        };
        let Some(tab) = self.active_req() else {
            return;
        };
        let tid = tab.tab_id.clone();
        let input = EffectiveAuthInput {
            workspace_id: ws,
            request_id: None,
            collection_id: tab
                .draft
                .collection_id
                .as_ref()
                .map(|c| c.as_str().to_owned()),
            folder_id: tab.draft.folder_id.as_ref().map(|f| f.as_str().to_owned()),
            auth: Some(tab.draft.auth.clone()),
        };
        let task = spawn_blocking_into(
            cx,
            move |state| request_effective_auth(state, input).ok(),
            move |this, eff, cx| {
                if let Some(t) = this.tab_req_mut(&tid) {
                    t.effective_auth = eff;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    // ── Multi-tab lifecycle ──────────────────────────────────────────────────

    pub fn open_request_tab(&mut self, req: &Request, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        let req_id = req.id.as_str();
        if let Some(existing_id) = self.tabs.iter().find_map(|t| match t {
            OpenTab::Request(r) if r.saved_request_id.as_deref() == Some(req_id) => {
                Some(r.tab_id.clone())
            }
            _ => None,
        }) {
            self.active_tab_id = Some(existing_id);
        } else {
            let tid = format!("tab-{req_id}-{}", current_time_ms());
            self.tabs
                .push(OpenTab::Request(Box::new(RequestTabState::from_request(
                    tid.clone(),
                    req.clone(),
                    true,
                ))));
            self.active_tab_id = Some(tid);
        }
        self.hydrate_active_tab(window, cx);
        self.refresh_effective_auth(cx);
        self.reload_resolved_vars(cx);
        self.persist_tab_session(cx);
        cx.notify();
    }

    pub fn open_new_request_tab(
        &mut self,
        kind: NewItemKind,
        collection_id: Option<String>,
        folder_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace_id.clone() else {
            return;
        };
        self.snapshot_active_tab(cx);
        let first_model = self.first_enabled_model_tuple();
        let id = format!("req-{}", current_time_ms());
        let Some(mut req) = (match kind {
            NewItemKind::AiApi => default_ai_request(
                &ws,
                "New AI Request",
                first_model
                    .as_ref()
                    .map(|(a, b, c, d)| (a.as_str(), b.as_str(), c.as_str(), d.as_str())),
            ),
            NewItemKind::HttpPost => {
                Request::new(&id, &ws, "New POST Request", "https://httpbin.org/post")
                    .ok()
                    .map(|mut r| {
                        r.method = HttpMethod::Post;
                        r.body = RequestBody::Json {
                            content: "{\n  \"key\": \"value\"\n}".into(),
                        };
                        r
                    })
            }
            NewItemKind::Graphql => Request::new(
                &id,
                &ws,
                "New GraphQL Query",
                "https://api.example.com/graphql",
            )
            .ok()
            .map(|mut r| {
                r.method = HttpMethod::Post;
                r.protocol = ProtocolKind::Graphql;
                r.body = RequestBody::Graphql {
                    query: "query {\n  viewer {\n    id\n  }\n}".into(),
                    variables_json: Some("{}".into()),
                    operation_name: None,
                };
                r
            }),
            _ => Request::new(&id, &ws, "New Request", "https://httpbin.org/get").ok(),
        }) else {
            return;
        };
        if let Some(cid) = collection_id {
            req.collection_id = spanbay_desktop::core::model::CollectionId::new(cid).ok();
        }
        if let Some(fid) = folder_id {
            req.folder_id = spanbay_desktop::core::model::FolderId::new(fid).ok();
        }
        let tid = format!("tab-new-{}", current_time_ms());
        self.tabs
            .push(OpenTab::Request(Box::new(RequestTabState::from_request(
                tid.clone(),
                req,
                false,
            ))));
        self.active_tab_id = Some(tid);
        self.hydrate_active_tab(window, cx);
        self.refresh_effective_auth(cx);
        self.persist_tab_session(cx);
        cx.notify();
    }

    #[must_use]
    pub fn first_enabled_model_tuple(&self) -> Option<(String, String, String, String)> {
        for pv in &self.providers {
            if !pv.provider.enabled {
                continue;
            }
            if let Some(m) = self
                .models
                .iter()
                .find(|m| m.provider_id == pv.provider.id && m.enabled)
            {
                return Some((
                    pv.provider.id.clone(),
                    pv.provider.name.clone(),
                    pv.provider.base_url.clone(),
                    m.model_id.clone(),
                ));
            }
        }
        None
    }

    pub fn switch_to_tab(&mut self, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab_id.as_deref() == Some(tab_id) {
            return;
        }
        self.snapshot_active_tab(cx);
        self.active_tab_id = Some(tab_id.to_owned());
        self.hydrate_active_tab(window, cx);
        self.refresh_effective_auth(cx);
        self.reload_resolved_vars(cx);
        self.persist_tab_session(cx);
        cx.notify();
    }

    pub fn request_close_tab(&mut self, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        let lang = crate::ui::active_language(cx);
        let dirty_name = self
            .tabs
            .iter()
            .find(|t| t.tab_id() == tab_id)
            .and_then(|t| t.is_dirty().then(|| t.title(lang)));
        if let Some(name) = dirty_name {
            self.active_modal = Some(ActiveModal::ConfirmCloseDirty {
                tab_id: tab_id.to_owned(),
                name,
            });
            cx.notify();
            return;
        }
        self.force_close_tab(tab_id, window, cx);
    }

    pub fn force_close_tab(&mut self, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let was_active = self.active_tab_id.as_deref() == Some(tab_id);
        self.tabs.retain(|t| t.tab_id() != tab_id);
        if was_active {
            self.active_tab_id = self.tabs.last().map(|t| t.tab_id().to_owned());
            self.hydrate_active_tab(window, cx);
            self.refresh_effective_auth(cx);
        }
        self.persist_tab_session(cx);
        cx.notify();
    }

    pub fn save_active_tab(
        &mut self,
        force_overwrite: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let Some(tab) = self.active_req() else {
            return;
        };
        let tid = tab.tab_id.clone();
        let ws_id = tab.draft.workspace_id.as_str().to_owned();
        let input = tab.to_save_input();
        let base_rev = if force_overwrite {
            None
        } else {
            tab.revision.clone()
        };
        let req_name = input.name.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| request_save_checked_impl(state, input, base_rev.as_deref()),
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        if let Some(t) = this.tab_req_mut(&tid) {
                            t.saved_request_id = Some(saved.id.as_str().to_owned());
                            t.revision = saved.revision.clone();
                            t.draft = saved.clone();
                            t.saved_snapshot = Some(saved.clone());
                            t.dirty = false;
                        }
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).requests().act_saved_item(&saved.name),
                        ));
                        this.reload_workspace_no_window(ws_id, cx);
                        this.persist_tab_session(cx);
                    }
                    Err(AppError::Conflict { .. }) => {
                        this.active_modal = Some(ActiveModal::StaleWriteConfirm {
                            tab_id: tid,
                            name: req_name,
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
    }

    pub fn save_active_as(
        &mut self,
        name: String,
        collection_id: Option<String>,
        folder_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_active_tab(cx);
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        if !name.trim().is_empty() {
            tab.draft.name = name.trim().to_owned();
        }
        tab.draft.collection_id = collection_id
            .as_deref()
            .and_then(|s| spanbay_desktop::core::model::CollectionId::new(s).ok());
        tab.draft.folder_id = folder_id
            .as_deref()
            .and_then(|s| spanbay_desktop::core::model::FolderId::new(s).ok());
        let tid = tab.tab_id.clone();
        let ws_id = tab.draft.workspace_id.as_str().to_owned();
        let input = tab.to_save_input();
        self.name_input
            .update(cx, |i, cx| i.set_value(input.name.clone(), window, cx));
        let task = spawn_blocking_into(
            cx,
            move |state| request_save_impl(state, input),
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        if let Some(t) = this.tab_req_mut(&tid) {
                            t.saved_request_id = Some(saved.id.as_str().to_owned());
                            t.revision = saved.revision.clone();
                            t.draft = saved.clone();
                            t.saved_snapshot = Some(saved.clone());
                            t.dirty = false;
                        }
                        this.active_modal = None;
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).requests().act_saved_item(&saved.name),
                        ));
                        this.reload_workspace_no_window(ws_id, cx);
                        this.refresh_effective_auth(cx);
                        this.persist_tab_session(cx);
                    }
                    Err(err) => {
                        this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    // ── Tree CRUD + Undo ─────────────────────────────────────────────────────
}
