//! Collection/Folder/Request CRUD, duplicate, delete + undo, rename, move,
//! URL <-> Query/Path sync, and cURL/code snippet generation.

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::request_model_service::{
    UrlSyncEdit, UrlSyncInput, sync_request_url, url_with_query_params,
};
use pingdeck_desktop::core::app::snippet_service::{
    SnippetLanguage, SnippetMode, SnippetOptions, request_code_snippet_impl,
};
use pingdeck_desktop::core::app::{
    CopyItemInput, CopyItemKind, MoveRequestInput, SaveCollectionInput, SaveFolderInput,
    collection_delete_impl, collection_save_impl, current_time_ms, folder_delete_impl,
    folder_save_impl, request_delete_impl, request_move_impl, request_save_impl,
    restore_deleted_impl, tree_copy_impl,
};
use pingdeck_desktop::core::model::{AuthConfig, Request};
use pingdeck_desktop::services::clipboard;

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::Tone;

use super::RequestsView;
use super::model::{ActiveModal, OpenTab, RequestTabState, TreeItemKind};

impl RequestsView {
    pub fn create_collection(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let trimmed = name.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        let ws_reload = ws_id.clone();
        let input = SaveCollectionInput {
            id: format!("col-{}", current_time_ms()),
            workspace_id: ws_id,
            name: trimmed,
            description: String::new(),
            auth: AuthConfig::None,
            variables: Vec::new(),
            pre_request_script: String::new(),
            test_script: String::new(),
            position: self.collections.len() as i64,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| collection_save_impl(state, input),
            move |this, res, cx| {
                match res {
                    Ok(col) => {
                        this.active_modal = None;
                        this.status_msg = Some((Tone::Success, format!("已创建集合 {}", col.name)));
                        if !this.collections.iter().any(|c| c.id == col.id) {
                            this.collections.push(col);
                        }
                        this.reload_workspace_no_window(ws_reload, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn create_folder(
        &mut self,
        collection_id: String,
        parent_folder_id: Option<String>,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_reload) = self.workspace_id.clone() else {
            return;
        };
        let trimmed = name.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        let input = SaveFolderInput {
            id: format!("fld-{}", current_time_ms()),
            collection_id,
            parent_folder_id,
            name: trimmed,
            description: String::new(),
            auth: AuthConfig::Inherit,
            pre_request_script: String::new(),
            test_script: String::new(),
            position: self.folders.len() as i64,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| folder_save_impl(state, input),
            move |this, res, cx| {
                match res {
                    Ok(f) => {
                        this.active_modal = None;
                        this.status_msg = Some((Tone::Success, format!("已创建文件夹 {}", f.name)));
                        if !this.folders.iter().any(|x| x.id == f.id) {
                            this.folders.push(f);
                        }
                        this.reload_workspace_no_window(ws_reload, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn duplicate_tree_item(&mut self, kind: TreeItemKind, id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_reload = ws_id.clone();
        let copy_kind = match kind {
            TreeItemKind::Collection => CopyItemKind::Collection,
            TreeItemKind::Folder => CopyItemKind::Folder,
            TreeItemKind::Request => CopyItemKind::Request,
        };
        let target_col = match kind {
            TreeItemKind::Request => self
                .requests
                .iter()
                .find(|r| r.id.as_str() == id)
                .and_then(|r| r.collection_id.as_ref().map(|c| c.as_str().to_owned())),
            TreeItemKind::Folder => self
                .folders
                .iter()
                .find(|f| f.id.as_str() == id)
                .map(|f| f.collection_id.as_str().to_owned()),
            TreeItemKind::Collection => None,
        };
        let input = CopyItemInput {
            workspace_id: ws_id.clone(),
            target_workspace_id: ws_id,
            kind: copy_kind,
            id,
            target_collection_id: target_col,
            target_folder_id: None,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| tree_copy_impl(state, input),
            move |this, res, cx| {
                match res {
                    Ok(r) => {
                        this.status_msg = Some((Tone::Success, format!("已复制 {}", r.id)));
                        this.reload_workspace_no_window(ws_reload, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_tree_item(
        &mut self,
        kind: TreeItemKind,
        id: String,
        label: String,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_reload = ws_id.clone();
        let del_id = id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| match kind {
                TreeItemKind::Collection => collection_delete_impl(state, ws_id, id),
                TreeItemKind::Folder => folder_delete_impl(state, ws_id, id),
                TreeItemKind::Request => request_delete_impl(state, ws_id, id),
            },
            move |this, res, cx| {
                match res {
                    Ok(token) => {
                        if kind == TreeItemKind::Request {
                            for tab in &mut this.tabs {
                                if let OpenTab::Request(r) = tab
                                    && r.saved_request_id.as_deref() == Some(del_id.as_str())
                                {
                                    r.saved_request_id = None;
                                    r.saved_snapshot = None;
                                    r.dirty = true;
                                }
                            }
                        }
                        this.last_undo = Some((token, label.clone()));
                        this.status_msg = Some((Tone::Warning, format!("已删除 {label}")));
                        this.reload_workspace_no_window(ws_reload, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn undo_last_delete(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some((token, label))) =
            (self.workspace_id.clone(), self.last_undo.take())
        else {
            return;
        };
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| restore_deleted_impl(state, ws_id, token),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.status_msg = Some((Tone::Success, format!("已撤销删除 {label}")));
                        this.reload_workspace_no_window(ws_reload, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn rename_tree_item(
        &mut self,
        kind: TreeItemKind,
        id: String,
        new_name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let trimmed = new_name.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        let ws_reload = ws_id.clone();
        match kind {
            TreeItemKind::Collection => {
                let Some(col) = self.collections.iter().find(|c| c.id == id).cloned() else {
                    return;
                };
                let vars = col
                    .variables
                    .iter()
                    .map(|v| pingdeck_desktop::core::app::EnvironmentVariableInput {
                        id: Some(v.id.clone()),
                        key: v.key.clone(),
                        value: v.value.clone(),
                        is_secret: v.is_secret,
                        enabled: v.enabled,
                        note: v.note.clone(),
                    })
                    .collect();
                let input = SaveCollectionInput {
                    id: col.id,
                    workspace_id: ws_id,
                    name: trimmed.clone(),
                    description: col.description,
                    auth: col.auth,
                    variables: vars,
                    pre_request_script: col.pre_request_script,
                    test_script: col.test_script,
                    position: col.position,
                };
                let task = spawn_blocking_into(
                    cx,
                    move |state| collection_save_impl(state, input),
                    move |this, res, cx| {
                        if res.is_ok() {
                            this.active_modal = None;
                            this.status_msg =
                                Some((Tone::Success, format!("已重命名为 {trimmed}")));
                            this.reload_workspace_no_window(ws_reload, cx);
                        }
                        cx.notify();
                    },
                );
                self._tasks.push(task);
            }
            TreeItemKind::Folder => {
                let Some(f) = self.folders.iter().find(|f| f.id.as_str() == id).cloned() else {
                    return;
                };
                let input = SaveFolderInput {
                    id: f.id.as_str().to_owned(),
                    collection_id: f.collection_id.as_str().to_owned(),
                    parent_folder_id: f.parent_folder_id.map(|p| p.as_str().to_owned()),
                    name: trimmed.clone(),
                    description: f.description,
                    auth: f.auth,
                    pre_request_script: f.pre_request_script,
                    test_script: f.test_script,
                    position: f.position,
                };
                let task = spawn_blocking_into(
                    cx,
                    move |state| folder_save_impl(state, input),
                    move |this, res, cx| {
                        if res.is_ok() {
                            this.active_modal = None;
                            this.status_msg =
                                Some((Tone::Success, format!("已重命名为 {trimmed}")));
                            this.reload_workspace_no_window(ws_reload, cx);
                        }
                        cx.notify();
                    },
                );
                self._tasks.push(task);
            }
            TreeItemKind::Request => {
                let Some(mut req) = self.requests.iter().find(|r| r.id.as_str() == id).cloned()
                else {
                    return;
                };
                req.name = trimmed.clone();
                let st = RequestTabState::from_request("tmp".into(), req, true);
                let input = st.to_save_input();
                let task = spawn_blocking_into(
                    cx,
                    move |state| request_save_impl(state, input),
                    move |this, res, cx| {
                        if res.is_ok() {
                            this.active_modal = None;
                            this.status_msg =
                                Some((Tone::Success, format!("已重命名为 {trimmed}")));
                            this.reload_workspace_no_window(ws_reload, cx);
                        }
                        cx.notify();
                    },
                );
                self._tasks.push(task);
            }
        }
    }

    pub fn move_request_to(
        &mut self,
        request_id: String,
        target_collection_id: Option<String>,
        target_folder_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_reload = ws_id.clone();
        let input = MoveRequestInput {
            workspace_id: ws_id,
            request_id,
            target_collection_id,
            target_folder_id,
            position: None,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| request_move_impl(state, input),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.active_modal = None;
                        this.status_msg = Some((Tone::Success, "已移动请求".into()));
                        this.reload_workspace_no_window(ws_reload, cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    // ── URL ⇄ Query Params & Path Variables Sync ─────────────────────────────

    pub fn on_url_input_changed(&mut self, cx: &mut Context<Self>) {
        let url = self.url_input.read(cx).value().to_string();
        if let Some(tab) = self.active_req_mut() {
            let out = sync_request_url(UrlSyncInput {
                url,
                query_params: tab.draft.query_params.clone(),
                path_variables: tab.draft.path_variables.clone(),
                edited: UrlSyncEdit::Url,
            });
            tab.draft.url = out.url;
            tab.draft.query_params = out.query_params;
            tab.draft.path_variables = out.path_variables;
            tab.recompute_dirty();
            cx.notify();
        }
    }

    pub fn sync_url_from_params(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_req_mut() else {
            return;
        };
        let next_url = url_with_query_params(&tab.draft.url, &tab.draft.query_params);
        tab.draft.url = next_url.clone();
        tab.recompute_dirty();
        self.hydrating = true;
        self.url_input
            .update(cx, |i, cx| i.set_value(next_url, window, cx));
        self.hydrating = false;
        cx.notify();
    }

    // ── Code Snippets ────────────────────────────────────────────────────────

    pub fn copy_request_as_curl(&mut self, req: &Request, cx: &mut Context<Self>) {
        let opts = SnippetOptions {
            mode: SnippetMode::Values,
            environment_id: self.active_env_id.clone(),
        };
        let code =
            request_code_snippet_impl(runtime(cx).state(), req, SnippetLanguage::Curl, &opts)
                .unwrap_or_else(|e| format!("# {}", e.message()));
        let _ = clipboard::set_text(&code);
        self.status_msg = Some((Tone::Success, "已复制 cURL（秘密变量已脱敏）".into()));
        cx.notify();
    }

    pub fn open_code_snippets_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active_tab(cx);
        self.active_modal = Some(ActiveModal::CodeSnippets { values_mode: true });
        self.refresh_snippet(self.snippet_lang, true, window, cx);
    }

    pub fn refresh_snippet(
        &mut self,
        lang: SnippetLanguage,
        values_mode: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snippet_lang = lang;
        let Some(tab) = self.active_req() else {
            return;
        };
        let opts = SnippetOptions {
            mode: if values_mode {
                SnippetMode::Values
            } else {
                SnippetMode::Template
            },
            environment_id: self.active_env_id.clone(),
        };
        let code = request_code_snippet_impl(runtime(cx).state(), &tab.draft, lang, &opts)
            .unwrap_or_else(|e| format!("// {}", e.message()));
        let hl = match lang {
            SnippetLanguage::Curl => "bash",
            SnippetLanguage::Fetch | SnippetLanguage::Axios => "javascript",
            SnippetLanguage::Python => "python",
            SnippetLanguage::Go => "go",
            SnippetLanguage::Java => "java",
            SnippetLanguage::Csharp => "csharp",
            SnippetLanguage::Ruby => "ruby",
            SnippetLanguage::Php => "php",
        };
        self.snippet_editor.update(cx, |e, cx| {
            e.set_highlighter(hl, cx);
            e.set_value(code, window, cx);
        });
        cx.notify();
    }
}
