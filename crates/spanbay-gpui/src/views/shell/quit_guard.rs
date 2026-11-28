//! Cross-platform window close & application quit guard for [`AppShell`].
//!
//! All close/quit entry points across Windows, macOS, and Linux — window close button
//! (`on_window_should_close` and Linux CSD `TitleBar::on_close_window`), `⌘Q` / `Ctrl+Q` /
//! application menu (`QuitApp`), and menu-bar tray Quit (`HostCommand::Quit`) — funnel through
//! [`AppShell::handle_window_close_request`]:
//! - Collects live [`RunningWork`] from core plus unsaved edits across mounted modules.
//! - If idle and clean, flushes active drafts + local UI state and permits closing/quitting.
//! - Otherwise keeps the window open, brings it to front, and displays a localized confirmation
//!   modal (Danger tone when unsaved edits or active runs would be lost; standard confirmation
//!   when only resident local services such as Local Gateway, Mock, or MCP servers are running).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::{
    AnyElement, Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, Window,
    div, px,
};
use spanbay_desktop::core::app::{RunningWork, app_running_work_impl};

use crate::bridge::runtime;
use crate::ui::{
    ScrollableElement as _, StatusShape, WindowExt as _, btn_danger, btn_primary, btn_secondary,
    h_row, modal_dialog, palette, section_heading, status_indicator, to_hsla, tr, v_col,
};
use crate::views::requests::OpenTab;

use super::AppShell;
use super::module_host::ModuleHost;

/// One unsaved edit in a mounted view that does not have automatic draft persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsavedItem {
    Environment(String),
    Globals,
    ContainerSettings(String),
    Flow(String),
    MockServer(String),
    AgentInstructions,
}

/// Snapshot of everything currently blocking a silent close/quit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuitConfirmState {
    pub unsaved_items: Vec<UnsavedItem>,
    pub running_work: RunningWork,
    pub gateway_port: Option<u16>,
}

impl QuitConfirmState {
    /// Returns `true` if any unsaved document or running task/service needs user confirmation.
    #[must_use]
    pub fn has_blockers(&self) -> bool {
        !self.unsaved_items.is_empty() || self.running_work.any()
    }

    /// Returns `true` if active work (agent/subagent/process/flow/collection/benchmark runs)
    /// would be interrupted on exit.
    #[must_use]
    pub fn has_active_tasks(&self) -> bool {
        self.running_work.agent_runs > 0
            || self.running_work.subagents > 0
            || self.running_work.background_processes > 0
            || self.running_work.flow_runs > 0
            || self.running_work.collection_runs > 0
            || self.running_work.bench_runs > 0
    }

    /// Returns `true` when quitting discards unsaved edits or aborts in-flight work (Danger tone).
    #[must_use]
    pub fn is_destructive(&self) -> bool {
        !self.unsaved_items.is_empty() || self.has_active_tasks()
    }
}

impl ModuleHost {
    /// Inspects all lazily-mounted modules for unsaved edits that are not auto-persisted as drafts.
    pub fn collect_unsaved_items<V: 'static>(&self, cx: &Context<V>) -> Vec<UnsavedItem> {
        let mut items = Vec::new();

        if let Some(env_ent) = &self.environments {
            let env = env_ent.read(cx);
            if env.env_dirty {
                let name = if env.env_draft_name.trim().is_empty() {
                    env.selected_env_id.clone().unwrap_or_default()
                } else {
                    env.env_draft_name.clone()
                };
                items.push(UnsavedItem::Environment(name));
            }
            if env.globals_dirty {
                items.push(UnsavedItem::Globals);
            }
        }

        if let Some(req_ent) = &self.requests {
            let req = req_ent.read(cx);
            for tab in &req.tabs {
                if let OpenTab::Container(c) = tab
                    && c.dirty
                {
                    items.push(UnsavedItem::ContainerSettings(c.name.clone()));
                }
            }
        }

        if let Some(flow_ent) = &self.flows {
            let flow = flow_ent.read(cx);
            if flow.dirty {
                let name = flow
                    .draft
                    .as_ref()
                    .map(|f| f.name.clone())
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| flow.selected_flow_id.clone())
                    .unwrap_or_default();
                items.push(UnsavedItem::Flow(name));
            }
        }

        if let Some(mock_ent) = &self.mock {
            let mock = mock_ent.read(cx);
            if mock.dirty {
                let name = mock
                    .draft
                    .as_ref()
                    .map(|m| m.name.clone())
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| mock.selected_id.clone())
                    .unwrap_or_default();
                items.push(UnsavedItem::MockServer(name));
            }
        }

        if let Some(settings_ent) = &self.settings
            && let Some(agent_ent) = &settings_ent.read(cx).agent_view
            && agent_ent.read(cx).instructions_dirty
        {
            items.push(UnsavedItem::AgentInstructions);
        }

        items
    }

    /// Synchronously flushes auto-persisted drafts (Chat composer draft, Requests tab session)
    /// before the application terminates.
    pub fn flush_drafts_for_quit<V: 'static>(&mut self, cx: &mut Context<V>) {
        if let Some(chat_ent) = &self.chat {
            chat_ent.update(cx, |chat, cx| chat.flush_active_draft(cx));
        }
        if let Some(req_ent) = &self.requests {
            req_ent.update(cx, |req, cx| req.flush_tab_session_sync(cx));
        }
    }
}

impl AppShell {
    /// Shared flag checked by `window.on_window_should_close` once a close/quit has been approved.
    #[must_use]
    pub fn allow_close_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.allow_close)
    }

    /// Returns whether the close/quit confirmation modal is currently visible.
    #[must_use]
    pub fn is_quit_confirm_open(&self) -> bool {
        self.quit_confirm.is_some()
    }

    /// Collects a fresh snapshot of all running work and unsaved edits across the workspace.
    pub fn collect_quit_state(&mut self, cx: &mut Context<Self>) -> QuitConfirmState {
        let rt = runtime(cx);
        let st = rt.state();
        let running_work = app_running_work_impl(st);
        let gateway_port = running_work
            .gateway_running
            .then(|| st.llm().gateway.status(st.llm()).port);
        self.state.running_work = running_work;
        self.state.gateway_port = gateway_port;
        let unsaved_items = self.modules.collect_unsaved_items(cx);
        QuitConfirmState {
            unsaved_items,
            running_work,
            gateway_port,
        }
    }

    /// Flushes persistent drafts and local UI state prior to closing the window / exiting.
    pub fn flush_before_quit(&mut self, cx: &mut Context<Self>) {
        self.modules.flush_drafts_for_quit(cx);
        self.persist_local(cx);
    }

    /// Unified close/quit gate called from `on_window_should_close`, Linux CSD `TitleBar` close,
    /// `QuitApp`, and `HostCommand::Quit`.
    ///
    /// Returns `true` when the window is clear to close immediately (drafts flushed and
    /// `allow_close` armed); returns `false` and opens the confirmation modal when active work or
    /// unsaved changes require explicit user confirmation.
    pub fn handle_window_close_request(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.allow_close.load(Ordering::SeqCst) {
            return true;
        }
        let state = self.collect_quit_state(cx);
        if !state.has_blockers() {
            self.quit_confirm = None;
            self.flush_before_quit(cx);
            self.allow_close.store(true, Ordering::SeqCst);
            return true;
        }

        // Dismiss transient chrome so the quit confirmation dialog sits cleanly above the window.
        self.state.palette_open = false;
        self.new_workspace_input = None;
        self.new_workspace_sub = None;
        if window.has_active_sheet(cx) {
            window.close_sheet(cx);
        }
        self.quit_confirm = Some(state);
        cx.activate(true);
        window.activate_window();
        cx.notify();
        false
    }

    /// Handles `QuitApp` (`⌘Q` / `Ctrl+Q` / application menu Quit) with the unified close check.
    pub fn request_quit_with_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.handle_window_close_request(window, cx) {
            cx.quit();
        }
    }

    /// Handles `HostCommand::Quit` (e.g. macOS menu-bar tray "Quit SpanBay") by activating the
    /// main window and routing through [`Self::request_quit_with_window`] on the next turn.
    pub(crate) fn request_quit_from_host(&mut self, cx: &mut Context<Self>) {
        let win_handle = self.window_handle;
        let weak_shell = cx.entity().downgrade();
        let allow_close = Arc::clone(&self.allow_close);
        cx.defer(move |cx| {
            cx.activate(true);
            let handled = win_handle
                .update(cx, |_, window, cx| {
                    window.activate_window();
                    if let Some(shell) = weak_shell.upgrade() {
                        shell.update(cx, |this, cx| this.request_quit_with_window(window, cx));
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
            if !handled {
                allow_close.store(true, Ordering::SeqCst);
                cx.quit();
            }
        });
    }

    /// Brings the main window to the foreground (including restoring from minimized state).
    pub(crate) fn show_and_focus_from_host(&mut self, cx: &mut Context<Self>) {
        let win_handle = self.window_handle;
        cx.activate(true);
        cx.defer(move |cx| {
            let _ = win_handle.update(cx, |_, window, _| {
                window.activate_window();
            });
        });
    }

    /// Keeps an open quit-confirmation modal synchronized when background work changes state.
    pub(crate) fn sync_open_quit_confirm(&mut self, cx: &mut Context<Self>) {
        if self.quit_confirm.is_none() {
            return;
        }
        let refreshed = self.collect_quit_state(cx);
        if refreshed.has_blockers() {
            self.quit_confirm = Some(refreshed);
        } else {
            self.quit_confirm = None;
        }
    }

    /// Cancels the close/quit confirmation modal without stopping any task or modifying state.
    pub fn dismiss_quit_confirm(&mut self, cx: &mut Context<Self>) -> bool {
        if self.quit_confirm.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    /// Confirms quitting from the confirmation modal: flushes drafts, arms `allow_close`, and
    /// terminates the GPUI application (triggering `on_app_quit` -> `DesktopRuntime::shutdown_blocking`).
    pub fn confirm_quit(&mut self, cx: &mut Context<Self>) {
        self.quit_confirm = None;
        self.flush_before_quit(cx);
        self.allow_close.store(true, Ordering::SeqCst);
        cx.notify();
        cx.quit();
    }
}

/// Renders the full-window close/quit confirmation modal when active.
pub fn render_quit_confirm_modal(
    shell: &AppShell,
    window: &Window,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    let state = shell.quit_confirm.as_ref()?;
    let p = palette(cx);
    let t = tr(cx);
    let sh = t.shell();
    let c = t.common();

    let destructive = state.is_destructive();
    let title = if destructive {
        sh.quit_title_danger()
    } else {
        sh.quit_title_services()
    };
    let description = if destructive {
        sh.quit_desc_danger()
    } else {
        sh.quit_desc_services()
    };

    let mut body = v_col().w_full().gap(px(12.0)).overflow_y_scrollbar().child(
        div()
            .text_xs()
            .text_color(to_hsla(p.ink_2))
            .child(description),
    );

    if !state.unsaved_items.is_empty() {
        let mut unsaved_list = v_col()
            .w_full()
            .gap(px(6.0))
            .p(px(10.0))
            .rounded(px(8.0))
            .bg(to_hsla(p.sunken))
            .border_1()
            .border_color(to_hsla(p.line_strong.composite_over(p.sunken)))
            .child(section_heading(sh.quit_section_unsaved(), cx));

        for item in &state.unsaved_items {
            let label = match item {
                UnsavedItem::Environment(name) => sh.quit_unsaved_env(name),
                UnsavedItem::Globals => sh.quit_unsaved_globals().to_owned(),
                UnsavedItem::ContainerSettings(name) => sh.quit_unsaved_container(name),
                UnsavedItem::Flow(name) => sh.quit_unsaved_flow(name),
                UnsavedItem::MockServer(name) => sh.quit_unsaved_mock(name),
                UnsavedItem::AgentInstructions => sh.quit_unsaved_instructions().to_owned(),
            };
            unsaved_list = unsaved_list.child(
                h_row()
                    .w_full()
                    .gap(px(8.0))
                    .child(status_indicator(StatusShape::Warn, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(label)),
                    ),
            );
        }
        body = body.child(unsaved_list);
    }

    if state.running_work.any() {
        let rw = &state.running_work;
        let mut rows: Vec<(StatusShape, String)> = Vec::new();
        if rw.agent_runs > 0 {
            rows.push((StatusShape::Bad, sh.quit_run_agent(rw.agent_runs)));
        }
        if rw.subagents > 0 {
            rows.push((StatusShape::Bad, sh.quit_run_subagents(rw.subagents)));
        }
        if rw.background_processes > 0 {
            rows.push((
                StatusShape::Bad,
                sh.quit_run_bg_procs(rw.background_processes),
            ));
        }
        if rw.flow_runs > 0 {
            rows.push((StatusShape::Warn, sh.quit_run_flows(rw.flow_runs)));
        }
        if rw.collection_runs > 0 {
            rows.push((
                StatusShape::Warn,
                sh.quit_run_collections(rw.collection_runs),
            ));
        }
        if rw.bench_runs > 0 {
            rows.push((StatusShape::Warn, sh.quit_run_bench(rw.bench_runs)));
        }
        if rw.gateway_running {
            let label = match state.gateway_port {
                Some(port) => sh.quit_svc_gateway(port),
                None => sh.quit_svc_gateway_no_port().to_owned(),
            };
            rows.push((StatusShape::Live, label));
        }
        if rw.mock_servers > 0 {
            rows.push((StatusShape::Live, sh.quit_svc_mocks(rw.mock_servers)));
        }
        if rw.mcp_servers > 0 {
            rows.push((StatusShape::Live, sh.quit_svc_mcp(rw.mcp_servers)));
        }

        let mut running_list = v_col()
            .w_full()
            .gap(px(6.0))
            .p(px(10.0))
            .rounded(px(8.0))
            .bg(to_hsla(p.sunken))
            .border_1()
            .border_color(to_hsla(p.line_strong.composite_over(p.sunken)))
            .child(section_heading(sh.quit_section_running(), cx));

        for (shape, label) in rows {
            running_list = running_list.child(
                h_row()
                    .w_full()
                    .gap(px(8.0))
                    .child(status_indicator(shape, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(label)),
                    ),
            );
        }
        body = body.child(running_list);
    }

    let confirm_btn = if destructive {
        btn_danger("quit-confirm-ok", sh.quit_btn_stop_quit())
    } else {
        btn_primary("quit-confirm-ok", sh.quit_btn_quit_services())
    }
    .on_click(cx.listener(|this, _, _, cx| this.confirm_quit(cx)));

    let footer = h_row()
        .gap(px(8.0))
        .child(
            btn_secondary("quit-confirm-cancel", c.cancel()).on_click(cx.listener(
                |this, _, _, cx| {
                    this.dismiss_quit_confirm(cx);
                },
            )),
        )
        .child(confirm_btn)
        .into_any_element();

    let ent = cx.entity().clone();
    Some(modal_dialog(
        "quit-confirm-dialog",
        title,
        460.0,
        body.into_any_element(),
        footer,
        window,
        cx,
        move |_, _, cx| {
            ent.update(cx, |this, cx| {
                this.dismiss_quit_confirm(cx);
            });
        },
    ))
}
