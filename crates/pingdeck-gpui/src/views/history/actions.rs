//! Server-side filtering, keyset paging, HAR export, delete+undo, clear-filtered, and cross-module handoffs for `HistoryView`.

use std::path::PathBuf;

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::factory_service::new_entity_id;
use pingdeck_desktop::core::app::misc_logic_service::{
    HISTORY_PAGE_SIZE, history_entries_delete_impl, history_entries_undo_impl, history_page_impl,
    history_run_children_impl,
};
use pingdeck_desktop::core::app::{
    HISTORY_EXPORT_FILE_NAME, HistoryExportScope, environment_list_impl,
    history_clear_filtered_impl, history_export_save_to_path_impl, history_get_impl,
    settings_get_impl,
};
use pingdeck_desktop::core::error::AppError;
use pingdeck_desktop::core::model::{ExecutionHistoryEntry, HistoryFilter, HistorySource, Request};
use pingdeck_desktop::services::{FileFilter, HostDialog as _, RfdDialog, SaveFileSpec};

use crate::bridge::{runtime, spawn_blocking_into};
use crate::ui::Tone;

use super::state::{HistoryHandoff, HistoryView};

impl HistoryView {
    pub fn reload_page(&mut self, reset: bool, cx: &mut Context<Self>) {
        if reset {
            self.page_cursors = vec![None];
            self.page_index = 0;
        }
        self.load_current_page(reset, cx);
    }

    pub fn go_first_page(&mut self, cx: &mut Context<Self>) {
        if self.page_index == 0 {
            return;
        }
        self.page_cursors.truncate(1);
        self.page_index = 0;
        self.load_current_page(false, cx);
    }

    pub fn go_prev_page(&mut self, cx: &mut Context<Self>) {
        if self.page_index == 0 {
            return;
        }
        self.page_index -= 1;
        self.load_current_page(false, cx);
    }

    pub fn go_next_page(&mut self, cx: &mut Context<Self>) {
        let Some(next) = self.next_cursor.clone() else {
            return;
        };
        let next_idx = self.page_index + 1;
        self.page_cursors.truncate(next_idx);
        self.page_cursors.push(Some(next));
        self.page_index = next_idx;
        self.load_current_page(false, cx);
    }

    fn load_current_page(&mut self, refresh_envs: bool, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        self.lang = settings_get_impl(runtime(cx).state()).general.language;
        let query = self.search_input.read(cx).value().trim().to_owned();
        self.filter.query = query;
        let filter = self.filter.clone();
        let cursor = self.page_cursors.get(self.page_index).cloned().flatten();
        let req_page = self.page_index;
        self.loading = true;
        cx.notify();

        let task = spawn_blocking_into(
            cx,
            move |state| {
                let page = history_page_impl(
                    state,
                    ws_id.clone(),
                    filter,
                    Some(HISTORY_PAGE_SIZE),
                    cursor,
                )?;
                let envs = if refresh_envs {
                    Some(environment_list_impl(state, ws_id).unwrap_or_default())
                } else {
                    None
                };
                Ok::<_, AppError>((page, envs))
            },
            move |this, res, cx| {
                if this.page_index != req_page {
                    return;
                }
                this.loading = false;
                match res {
                    Ok::<_, AppError>((page, envs)) => {
                        if let Some(e) = envs {
                            this.environments = e;
                        }
                        if page.total.is_some() {
                            this.total = page.total;
                        }
                        if page.workspace_total.is_some() {
                            this.workspace_total = page.workspace_total;
                        }
                        if page.mock_dropped.is_some() {
                            this.mock_dropped = page.mock_dropped;
                        }
                        this.entries = page.entries;
                        this.next_cursor = page.next_cursor;

                        let sel_ok = this
                            .selected_id
                            .as_deref()
                            .is_some_and(|id| this.entries.iter().any(|e| e.id.as_str() == id));
                        if !sel_ok {
                            let first = this.entries.first().map(|e| e.id.as_str().to_owned());
                            this.selected_id = first.clone();
                            this.selected_entry = None;
                            this.run_children.clear();
                            this.run_children_next_cursor = None;
                            this.run_children_total = None;
                            if let Some(id) = first {
                                this.fetch_entry_detail(id, cx);
                            }
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn select_entry(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected_id = Some(id.clone());
        self.selected_entry = self
            .entries
            .iter()
            .find(|e| e.id.as_str() == id.as_str())
            .cloned();
        self.run_children.clear();
        self.run_children_next_cursor = None;
        self.run_children_total = None;
        self.run_children_loading = false;
        self.fetch_entry_detail(id, cx);
        cx.notify();
    }

    fn fetch_entry_detail(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let id_match = id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let entry = history_get_impl(state, ws_id.clone(), id)?;
                let children_page = if let Some(run_id) = entry.run_id.clone() {
                    history_run_children_impl(state, ws_id, run_id, HistoryFilter::default(), None)
                        .ok()
                } else {
                    None
                };
                Ok::<_, AppError>((entry, children_page))
            },
            move |this, res, cx| {
                if this.selected_id.as_deref() != Some(id_match.as_str()) {
                    return;
                }
                if let Ok((entry, children_page)) = res {
                    this.selected_entry = Some(entry);
                    if let Some(pg) = children_page {
                        this.run_children = pg.entries;
                        this.run_children_next_cursor = pg.next_cursor;
                        this.run_children_total = pg.total;
                    } else {
                        this.run_children.clear();
                        this.run_children_next_cursor = None;
                        this.run_children_total = None;
                    }
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn load_more_run_children(&mut self, cx: &mut Context<Self>) {
        if self.run_children_loading {
            return;
        }
        let (Some(ws_id), Some(run_id), Some(cursor)) = (
            self.workspace_id.clone(),
            self.active_entry().and_then(|e| e.run_id.clone()),
            self.run_children_next_cursor.clone(),
        ) else {
            return;
        };
        let expected_sel = self.selected_id.clone();
        self.run_children_loading = true;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                history_run_children_impl(
                    state,
                    ws_id,
                    run_id,
                    HistoryFilter::default(),
                    Some(cursor),
                )
            },
            move |this, res, cx| {
                this.run_children_loading = false;
                if this.selected_id != expected_sel {
                    return;
                }
                if let Ok(pg) = res {
                    for row in pg.entries {
                        if !this.run_children.iter().any(|c| c.id == row.id) {
                            this.run_children.push(row);
                        }
                    }
                    this.run_children_next_cursor = pg.next_cursor;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn toggle_source(&mut self, src: HistorySource, cx: &mut Context<Self>) {
        if let Some(pos) = self.filter.sources.iter().position(|s| *s == src) {
            self.filter.sources.remove(pos);
        } else {
            self.filter.sources.push(src);
        }
        self.reload_page(true, cx);
    }

    pub fn toggle_status_class(&mut self, class: u8, cx: &mut Context<Self>) {
        if let Some(pos) = self.filter.status_classes.iter().position(|c| *c == class) {
            self.filter.status_classes.remove(pos);
        } else {
            self.filter.status_classes.push(class);
        }
        self.reload_page(true, cx);
    }

    pub fn toggle_failed(&mut self, cx: &mut Context<Self>) {
        self.filter.include_failed = !self.filter.include_failed;
        self.reload_page(true, cx);
    }

    pub fn toggle_env_filter(&mut self, env_id: String, cx: &mut Context<Self>) {
        if let Some(pos) = self
            .filter
            .environment_ids
            .iter()
            .position(|e| e == &env_id)
        {
            self.filter.environment_ids.remove(pos);
        } else {
            self.filter.environment_ids.push(env_id);
        }
        self.reload_page(true, cx);
    }

    pub fn reset_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = HistoryFilter::default();
        self.search_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.reload_page(true, cx);
    }

    pub fn delete_entry(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| history_entries_delete_impl(state, ws_id, vec![id]),
            |this, res, cx| {
                match res {
                    Ok(rm) => {
                        let count = rm.removed.len();
                        this.entries
                            .retain(|e| !rm.removed.iter().any(|r| r == e.id.as_str()));
                        if this
                            .selected_id
                            .as_deref()
                            .is_some_and(|sel| rm.removed.iter().any(|r| r == sel))
                        {
                            this.selected_id =
                                this.entries.first().map(|e| e.id.as_str().to_owned());
                            this.selected_entry = None;
                            this.run_children.clear();
                            if let Some(next) = this.selected_id.clone() {
                                this.fetch_entry_detail(next, cx);
                            }
                        }
                        this.last_undo = rm.undo_token.map(|tok| (tok, count));
                        this.status_msg = Some((
                            Tone::Neutral,
                            crate::ui::Tr(this.lang)
                                .history()
                                .actions_deleted_history_entry(count),
                        ));
                        this.reload_page(true, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn undo_delete(&mut self, cx: &mut Context<Self>) {
        let (Some(ws_id), Some((token, _))) = (self.workspace_id.clone(), self.last_undo.take())
        else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| history_entries_undo_impl(state, ws_id, token),
            |this, res, cx| {
                match res {
                    Ok(ids) => {
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::Tr(this.lang)
                                .history()
                                .actions_restored_history_entry_s(ids.len()),
                        ));
                        this.reload_page(true, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn clear_filtered(&mut self, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let filter = self.filter.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| history_clear_filtered_impl(state, ws_id, filter),
            |this, res, cx| {
                this.active_modal = None;
                match res {
                    Ok(removed) => {
                        this.last_undo = None;
                        this.selected_id = None;
                        this.selected_entry = None;
                        this.run_children.clear();
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::Tr(this.lang)
                                .history()
                                .actions_cleared_matching_history_entries(removed),
                        ));
                        this.reload_page(true, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn export_har(&mut self, single_id: Option<String>, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let path: Option<PathBuf> = RfdDialog.save_file(&SaveFileSpec {
            title: None,
            file_name: Some(HISTORY_EXPORT_FILE_NAME.to_owned()),
            filters: vec![FileFilter {
                name: "HAR 1.2".to_owned(),
                extensions: vec!["har".to_owned(), "json".to_owned()],
            }],
            starting_dir: None,
        });
        let scope = match single_id {
            Some(id) => HistoryExportScope::Ids(vec![id]),
            None => HistoryExportScope::Filter(self.filter.clone()),
        };
        let task = spawn_blocking_into(
            cx,
            move |state| history_export_save_to_path_impl(state, ws_id, scope, path),
            |this, res, cx| {
                match res {
                    Ok(r) if r.saved => {
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::Tr(this.lang)
                                .history()
                                .actions_exported_entry_s_to_har(r.entries),
                        ));
                    }
                    Ok(_) => {}
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn open_in_requests(&mut self, entry: &ExecutionHistoryEntry, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let req = if let Some(snap) = &entry.request_snapshot {
            let mut copy = snap.clone();
            if let Ok(new_id) = pingdeck_desktop::core::model::RequestId::new(new_entity_id("req"))
            {
                copy.id = new_id;
            }
            copy.revision = None;
            copy
        } else {
            let url = entry
                .sent_request
                .as_ref()
                .map(|s| s.url.clone())
                .unwrap_or_else(|| entry.request_url.clone());
            let name = if entry.request_name.trim().is_empty() {
                format!("{:?} {}", entry.method, url)
            } else {
                entry.request_name.clone()
            };
            let Ok(mut r) = Request::new(new_entity_id("req"), ws_id, name, url) else {
                return;
            };
            r.method = entry.method;
            if let Some(sent) = &entry.sent_request {
                r.headers = sent.headers.clone();
            }
            r
        };
        self.pending_handoff
            .set(Some(HistoryHandoff::OpenInRequests(Box::new(req))));
        cx.notify();
    }

    pub fn ask_agent(&mut self, entry: &ExecutionHistoryEntry, cx: &mut Context<Self>) {
        let method = format!("{:?}", entry.method).to_uppercase();
        let status = if entry.status_code > 0 {
            format!("{} {}", entry.status_code, entry.status_text)
        } else {
            entry
                .error_kind
                .clone()
                .unwrap_or_else(|| "Transport Error".to_owned())
        };
        let preview: String = entry.response_body_preview.chars().take(1800).collect();
        let h = crate::ui::Tr(self.lang).history();
        let error_line = entry
            .error_message
            .as_deref()
            .map(|m| h.ask_agent_error_line(m))
            .unwrap_or_default();
        let md = h.ask_agent_prompt(
            &method,
            &entry.request_url,
            &status,
            entry.elapsed_ms,
            entry.body_size_bytes,
            format!("{:?}", entry.source),
            error_line,
            &preview,
        );
        self.pending_handoff.set(Some(HistoryHandoff::AskAgent {
            title: format!("{method} {}", entry.request_name),
            context_markdown: md,
        }));
        cx.notify();
    }
}
