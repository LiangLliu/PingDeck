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
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        self.lang = settings_get_impl(runtime(cx).state()).general.language;
        let query = self.search_input.read(cx).value().trim().to_owned();
        self.filter.query = query;
        let filter = self.filter.clone();
        let cursor = if reset {
            None
        } else {
            self.next_cursor.clone()
        };
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
                let envs = if reset {
                    Some(environment_list_impl(state, ws_id).unwrap_or_default())
                } else {
                    None
                };
                Ok::<_, AppError>((page, envs))
            },
            move |this, res, cx| {
                this.loading = false;
                match res {
                    Ok::<_, AppError>((page, envs)) => {
                        if let Some(e) = envs {
                            this.environments = e;
                        }
                        if reset {
                            this.entries = page.entries;
                            this.total = page.total;
                            this.workspace_total = page.workspace_total;
                            this.mock_dropped = page.mock_dropped;
                            let sel_ok = this
                                .selected_id
                                .as_deref()
                                .is_some_and(|id| this.entries.iter().any(|e| e.id.as_str() == id));
                            if !sel_ok {
                                let first = this.entries.first().map(|e| e.id.as_str().to_owned());
                                this.selected_id = first.clone();
                                this.selected_entry = None;
                                this.run_children.clear();
                                if let Some(id) = first {
                                    this.fetch_entry_detail(id, cx);
                                }
                            }
                        } else {
                            for row in page.entries {
                                if !this.entries.iter().any(|e| e.id == row.id) {
                                    this.entries.push(row);
                                }
                            }
                        }
                        this.next_cursor = page.next_cursor;
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
                let children = if let Some(run_id) = entry.run_id.clone() {
                    history_run_children_impl(state, ws_id, run_id, HistoryFilter::default(), None)
                        .map(|p| p.entries)
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                Ok::<_, AppError>((entry, children))
            },
            move |this, res, cx| {
                if this.selected_id.as_deref() != Some(id_match.as_str()) {
                    return;
                }
                if let Ok((entry, children)) = res {
                    this.selected_entry = Some(entry);
                    this.run_children = children;
                    cx.notify();
                }
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
                            if this.is_zh() {
                                format!("已删除 {count} 条历史记录")
                            } else {
                                format!("Deleted {count} history entry")
                            },
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
                            if this.is_zh() {
                                format!("已恢复 {} 条历史记录", ids.len())
                            } else {
                                format!("Restored {} history entry(s)", ids.len())
                            },
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
                            if this.is_zh() {
                                format!("已清空 {removed} 条匹配的历史记录")
                            } else {
                                format!("Cleared {removed} matching history entries")
                            },
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
                            if this.is_zh() {
                                format!("已导出 {} 条记录至 HAR 文件（凭据已脱敏）", r.entries)
                            } else {
                                format!(
                                    "Exported {} entry(s) to HAR (credentials redacted)",
                                    r.entries
                                )
                            },
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
        let md = format!(
            "请帮我分析这条 API 执行历史：\n\n- **请求**: `{method} {}`\n- **状态**: `{status}` ({} ms, {} B)\n- **来源**: `{:?}`\n{}\n\n```text\n{preview}\n```",
            entry.request_url,
            entry.elapsed_ms,
            entry.body_size_bytes,
            entry.source,
            entry
                .error_message
                .as_deref()
                .map(|m| format!("- **错误**: {m}"))
                .unwrap_or_default(),
        );
        self.pending_handoff.set(Some(HistoryHandoff::AskAgent {
            title: format!("{method} {}", entry.request_name),
            context_markdown: md,
        }));
        cx.notify();
    }
}
