//! Lazy module host mounting and keeping all 10 SpanBay feature views alive across navigation.
//!
//! Switching modules never drops mounted module entities or cancels active streams, matching the
//! run/view separation contract.

use std::cell::Cell;

use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, IntoElement as _, Subscription, Window,
};
use spanbay_desktop::core::llm::capture::ApiClientDraft;
use spanbay_desktop::state::{NavModule, SettingsSection};
use spanbay_desktop::theme::ThemePreferences;

use crate::views::{
    chat::ChatView, environments::EnvironmentsView, flows::FlowsView, gateway::GatewayView,
    history::HistoryView, mock::MockView, providers::ProvidersView, requests::RequestsView,
    runs::RunsView, settings::SettingsView, usage::UsageView,
};

/// Owns lazily-mounted GPUI entities for all 11 navigation destinations.
#[derive(Default)]
pub struct ModuleHost {
    pub(super) requests: Option<Entity<RequestsView>>,
    pub(super) mock: Option<Entity<MockView>>,
    pub(super) flows: Option<Entity<FlowsView>>,
    pub(super) history: Option<Entity<HistoryView>>,
    pub(super) environments: Option<Entity<EnvironmentsView>>,
    pub(super) chat: Option<Entity<ChatView>>,
    pub(super) runs: Option<Entity<RunsView>>,
    pub(super) providers: Option<Entity<ProvidersView>>,
    pub(super) gateway: Option<Entity<GatewayView>>,
    pub(super) usage: Option<Entity<UsageView>>,
    pub(super) settings: Option<Entity<SettingsView>>,
    active_workspace_id: Option<String>,
    active_environment_id: Option<String>,
    _subscriptions: Vec<Subscription>,
}

fn cell_has<T>(cell: &Cell<Option<T>>) -> bool {
    let val = cell.take();
    let has = val.is_some();
    cell.set(val);
    has
}

fn ensure_watched<V: 'static, M: 'static>(
    slot: &mut Option<Entity<M>>,
    subs: &mut Vec<Subscription>,
    build: impl FnOnce(&mut Context<M>) -> M,
    has_signal: fn(&M) -> bool,
    cx: &mut Context<V>,
) -> Entity<M> {
    if let Some(existing) = slot {
        return existing.clone();
    }
    let ent = cx.new(build);
    subs.push(cx.observe(&ent, move |_, view, cx| {
        if has_signal(view.read(cx)) {
            cx.notify();
        }
    }));
    *slot = Some(ent.clone());
    ent
}

impl ModuleHost {
    pub fn with_active_binding(
        active_workspace_id: Option<String>,
        active_environment_id: Option<String>,
    ) -> Self {
        Self {
            active_workspace_id,
            active_environment_id,
            ..Self::default()
        }
    }

    /// Synchronizes workspace-scoped modules (and Chat's default active SpanBay workspace)
    /// whenever the active workspace changes.
    pub fn set_workspace<V: 'static>(&mut self, ws_id: Option<String>, cx: &mut Context<V>) {
        self.active_workspace_id = ws_id.clone();
        self.active_environment_id = None;
        if let Some(v) = &self.requests {
            v.update(cx, |m, cx| m.set_workspace(ws_id.clone(), cx));
        }
        if let Some(v) = &self.mock {
            v.update(cx, |m, cx| m.set_workspace(ws_id.clone(), cx));
        }
        if let Some(v) = &self.flows {
            v.update(cx, |m, cx| m.set_workspace(ws_id.clone(), cx));
        }
        if let Some(v) = &self.history {
            v.update(cx, |m, cx| m.set_workspace(ws_id.clone(), cx));
        }
        if let Some(v) = &self.environments {
            v.update(cx, |m, cx| m.set_workspace(ws_id.clone(), cx));
        }
        if let Some(v) = &self.chat {
            v.update(cx, |m, cx| m.set_active_workspace(ws_id, cx));
        }
    }

    /// Propagates the workspace's active environment across mounted API modules and Chat.
    pub fn set_active_environment<V: 'static>(
        &mut self,
        env_id: Option<String>,
        cx: &mut Context<V>,
    ) {
        self.active_environment_id = env_id.clone();
        if let Some(v) = &self.requests {
            v.update(cx, |r, cx| r.set_active_environment(env_id.clone(), cx));
        }
        if let Some(v) = &self.flows {
            v.update(cx, |f, cx| f.set_active_environment(env_id.clone(), cx));
        }
        if let Some(v) = &self.environments {
            v.update(cx, |e, cx| e.set_active_environment(env_id.clone(), cx));
        }
        if let Some(v) = &self.chat {
            v.update(cx, |c, cx| c.set_active_environment(env_id, cx));
        }
    }

    /// Selects a specific section in the Settings view if mounted.
    pub fn set_settings_section<V: 'static>(
        &mut self,
        section: SettingsSection,
        cx: &mut Context<V>,
    ) {
        if let Some(v) = &self.settings {
            v.update(cx, |s, cx| s.set_section(section, cx));
        }
    }

    /// Opens a specific chat session when invoked from notifications, popover, or command palette.
    pub fn open_chat_session<V: 'static>(&mut self, session_id: String, cx: &mut Context<V>) {
        if let Some(v) = &self.chat {
            v.update(cx, |c, cx| c.open_session(session_id, cx));
        }
    }

    pub(super) fn ensure_requests<V: 'static>(
        &mut self,
        workspace_id: Option<String>,
        active_env_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Entity<RequestsView> {
        ensure_watched(
            &mut self.requests,
            &mut self._subscriptions,
            |cx| {
                let mut v = RequestsView::new(workspace_id, window, cx);
                if active_env_id.is_some() {
                    v.set_active_environment(active_env_id, cx);
                }
                v
            },
            |v| cell_has(&v.pending_handoff),
            cx,
        )
    }

    pub(super) fn ensure_chat<V: 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Entity<ChatView> {
        let ws_id = self.active_workspace_id.clone();
        let env_id = self.active_environment_id.clone();
        ensure_watched(
            &mut self.chat,
            &mut self._subscriptions,
            |cx| {
                let mut v = ChatView::new(window, cx);
                v.set_active_binding(ws_id, env_id, cx);
                v
            },
            |v| cell_has(&v.pending_handoff),
            cx,
        )
    }

    pub(super) fn ensure_runs<V: 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Entity<RunsView> {
        ensure_watched(
            &mut self.runs,
            &mut self._subscriptions,
            |cx| RunsView::new(window, cx),
            |v| cell_has(&v.pending_handoff),
            cx,
        )
    }

    /// Returns `(session_id, active_or_latest_run_id)` for the currently open chat when available.
    pub fn active_chat_workflow_target<V: 'static>(
        &self,
        cx: &Context<V>,
    ) -> Option<(String, Option<String>)> {
        let chat = self.chat.as_ref()?.read(cx);
        if chat.thread.session_id.is_empty() {
            return None;
        }
        let run_id = chat
            .thread
            .active_run
            .as_ref()
            .map(|r| r.run_id.clone())
            .or_else(|| chat.latest_session_run_id.clone());
        Some((chat.thread.session_id.clone(), run_id))
    }

    /// Refreshes the mounted global `RunsView` if present.
    pub fn refresh_runs_if_mounted<V: 'static>(&self, cx: &mut Context<V>) {
        if let Some(v) = &self.runs {
            v.update(cx, |r, cx| r.reload_all(cx));
        }
    }

    pub(super) fn ensure_environments<V: 'static>(
        &mut self,
        workspace_id: Option<String>,
        active_env_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Entity<EnvironmentsView> {
        ensure_watched(
            &mut self.environments,
            &mut self._subscriptions,
            |cx| {
                let mut v = EnvironmentsView::new(workspace_id, window, cx);
                if active_env_id.is_some() {
                    v.set_active_environment(active_env_id, cx);
                }
                v
            },
            |v| cell_has(&v.pending_active_env),
            cx,
        )
    }

    pub(super) fn ensure_providers<V: 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Entity<ProvidersView> {
        ensure_watched(
            &mut self.providers,
            &mut self._subscriptions,
            |cx| ProvidersView::new(window, cx),
            |v| cell_has(&v.pending_handoff),
            cx,
        )
    }

    pub(super) fn ensure_gateway<V: 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Entity<GatewayView> {
        ensure_watched(
            &mut self.gateway,
            &mut self._subscriptions,
            |cx| GatewayView::new(window, cx),
            |v| cell_has(&v.pending_handoff),
            cx,
        )
    }

    pub(super) fn ensure_usage<V: 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Entity<UsageView> {
        ensure_watched(
            &mut self.usage,
            &mut self._subscriptions,
            |cx| UsageView::new(window, cx),
            |v| cell_has(&v.pending_handoff),
            cx,
        )
    }

    /// Mounts `ChatView` if needed and opens a specific chat session.
    pub fn open_chat_session_mounted<V: 'static>(
        &mut self,
        session_id: String,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let chat = self.ensure_chat(window, cx);
        chat.update(cx, |c, cx| c.open_session(session_id, cx));
    }

    /// Refreshes lightweight catalogs when navigating back into a mounted module.
    pub fn on_enter_module<V: 'static>(&mut self, module: NavModule, cx: &mut Context<V>) {
        match module {
            NavModule::Chat => {
                if let Some(v) = &self.chat {
                    v.update(cx, |c, cx| c.refresh_catalog(cx));
                }
            }
            NavModule::Runs => {
                if let Some(v) = &self.runs {
                    v.update(cx, |r, cx| r.reload_all(cx));
                }
            }
            NavModule::Requests => {
                if let Some(v) = &self.requests {
                    v.update(cx, |r, cx| r.refresh_catalog(cx));
                }
            }
            NavModule::Mock => {
                if let Some(v) = &self.mock {
                    v.update(cx, |m, cx| m.refresh_catalog(cx));
                }
            }
            NavModule::Flows => {
                if let Some(v) = &self.flows {
                    v.update(cx, |f, cx| f.refresh_catalog(cx));
                }
            }
            NavModule::History => {
                if let Some(v) = &self.history {
                    v.update(cx, |h, cx| h.refresh_catalog(cx));
                }
            }
            NavModule::Environments => {
                if let Some(v) = &self.environments {
                    v.update(cx, |e, cx| e.refresh_catalog(cx));
                }
            }
            NavModule::Providers => {
                if let Some(v) = &self.providers {
                    v.update(cx, |p, cx| p.reload_all(cx));
                }
            }
            NavModule::Gateway => {
                if let Some(v) = &self.gateway {
                    v.update(cx, |g, cx| g.reload_all(cx));
                }
            }
            NavModule::Usage => {
                if let Some(v) = &self.usage {
                    v.update(cx, |u, cx| u.reload_all(cx));
                }
            }
            NavModule::Settings => {}
        }
    }

    /// Opens an external request draft (from History replay or Mock route test) inside Requests.
    pub fn open_request_in_requests<V: 'static>(
        &mut self,
        workspace_id: Option<String>,
        req: spanbay_desktop::core::model::Request,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = self.ensure_requests(workspace_id, None, window, cx);
        view.update(cx, |r, cx| r.open_external_draft_request(req, window, cx));
    }

    /// Opens a saved request by ID inside Requests (from Flows inspector or Command Palette).
    pub fn open_saved_request_by_id<V: 'static>(
        &mut self,
        workspace_id: Option<String>,
        request_id: String,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = self.ensure_requests(workspace_id, None, window, cx);
        view.update(cx, |r, cx| {
            r.open_saved_request_by_id(request_id, window, cx);
        });
    }

    /// Opens a Usage Call Log `ApiClientDraft` in a fresh Requests workbench tab.
    pub fn open_ai_client_draft<V: 'static>(
        &mut self,
        workspace_id: Option<String>,
        draft: ApiClientDraft,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = self.ensure_requests(workspace_id, None, window, cx);
        view.update(cx, |r, cx| r.open_ai_client_draft(draft, window, cx));
    }

    /// Selects a specific Mock server in `MockView`.
    pub fn open_mock_server<V: 'static>(
        &mut self,
        workspace_id: Option<String>,
        server_id: String,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = ensure_watched(
            &mut self.mock,
            &mut self._subscriptions,
            |cx| MockView::new(workspace_id, window, cx),
            |v| cell_has(&v.pending_handoff),
            cx,
        );
        view.update(cx, |m, cx| m.select_server(server_id, cx));
    }

    /// Selects a specific Flow in `FlowsView`.
    pub fn open_flow<V: 'static>(
        &mut self,
        workspace_id: Option<String>,
        flow_id: String,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = ensure_watched(
            &mut self.flows,
            &mut self._subscriptions,
            |cx| FlowsView::new(workspace_id, window, cx),
            |v| cell_has(&v.pending_handoff),
            cx,
        );
        view.update(cx, |f, cx| f.select_flow(flow_id, cx));
    }

    /// Dismisses an open modal inside the active feature module when pressing `Esc`.
    pub fn dismiss_active_modal<V: 'static>(
        &mut self,
        module: NavModule,
        cx: &mut Context<V>,
    ) -> bool {
        match module {
            NavModule::Requests => self
                .requests
                .as_ref()
                .is_some_and(|v| v.update(cx, |r, cx| r.dismiss_modal_if_open(cx))),
            NavModule::Mock => self
                .mock
                .as_ref()
                .is_some_and(|v| v.update(cx, |m, cx| m.dismiss_modal_if_open(cx))),
            NavModule::Flows => self
                .flows
                .as_ref()
                .is_some_and(|v| v.update(cx, |f, cx| f.dismiss_modal_if_open(cx))),
            NavModule::History => self
                .history
                .as_ref()
                .is_some_and(|v| v.update(cx, |h, cx| h.dismiss_modal_if_open(cx))),
            NavModule::Environments => self
                .environments
                .as_ref()
                .is_some_and(|v| v.update(cx, |e, cx| e.dismiss_modal_if_open(cx))),
            NavModule::Chat => self
                .chat
                .as_ref()
                .is_some_and(|v| v.update(cx, |c, cx| c.dismiss_modal_if_open(cx))),
            NavModule::Runs => self
                .runs
                .as_ref()
                .is_some_and(|v| v.update(cx, |r, cx| r.dismiss_modal_if_open(cx))),
            NavModule::Providers => self
                .providers
                .as_ref()
                .is_some_and(|v| v.update(cx, |p, cx| p.dismiss_modal_if_open(cx))),
            NavModule::Gateway => self
                .gateway
                .as_ref()
                .is_some_and(|v| v.update(cx, |g, cx| g.dismiss_modal_if_open(cx))),
            NavModule::Usage => self
                .usage
                .as_ref()
                .is_some_and(|v| v.update(cx, |u, cx| u.dismiss_modal_if_open(cx))),
            NavModule::Settings => self
                .settings
                .as_ref()
                .is_some_and(|v| v.update(cx, |s, cx| s.dismiss_modal_if_open(cx))),
        }
    }

    /// Hands off sanitized context from Requests/History/Usage into a Chat draft (no auto-send).
    pub fn handoff_to_chat<V: 'static>(
        &mut self,
        title: String,
        context_markdown: String,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let chat = self.ensure_chat(window, cx);
        chat.update(cx, |c, cx| {
            c.accept_handoff_draft(title, context_markdown, window, cx);
        });
    }

    /// Syncs external theme-preference updates (e.g. Titlebar Quick Theme Popover) into the
    /// mounted Settings view so both surfaces stay identical.
    pub fn sync_settings_theme_prefs<V: 'static>(
        &self,
        prefs: ThemePreferences,
        cx: &mut Context<V>,
    ) {
        if let Some(v) = &self.settings {
            v.update(cx, |s, cx| s.sync_theme_prefs(prefs, cx));
        }
    }

    /// Polls settings view for theme-preference, back-button, or cross-module jump signals.
    pub fn poll_settings_signals<V: 'static>(
        &self,
        cx: &Context<V>,
    ) -> (Option<ThemePreferences>, bool, Option<NavModule>) {
        let Some(v) = &self.settings else {
            return (None, false, None);
        };
        let s = v.read(cx);
        (
            s.take_pending_theme_prefs(),
            s.take_pending_back(),
            s.take_pending_navigate(),
        )
    }

    /// Lazily mounts (if first visit) and returns the active module element.
    #[allow(clippy::too_many_arguments)]
    pub fn render_active<V: 'static>(
        &mut self,
        module: NavModule,
        workspace_id: Option<String>,
        active_env_id: Option<String>,
        settings_section: SettingsSection,
        theme_prefs: ThemePreferences,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> AnyElement {
        self.active_workspace_id = workspace_id.clone();
        self.active_environment_id = active_env_id.clone();
        match module {
            NavModule::Requests => self
                .ensure_requests(workspace_id, active_env_id, window, cx)
                .into_any_element(),
            NavModule::Mock => ensure_watched(
                &mut self.mock,
                &mut self._subscriptions,
                |cx| MockView::new(workspace_id, window, cx),
                |v| cell_has(&v.pending_handoff),
                cx,
            )
            .into_any_element(),
            NavModule::Flows => ensure_watched(
                &mut self.flows,
                &mut self._subscriptions,
                |cx| {
                    let mut v = FlowsView::new(workspace_id, window, cx);
                    if active_env_id.is_some() {
                        v.set_active_environment(active_env_id, cx);
                    }
                    v
                },
                |v| cell_has(&v.pending_handoff),
                cx,
            )
            .into_any_element(),
            NavModule::History => ensure_watched(
                &mut self.history,
                &mut self._subscriptions,
                |cx| HistoryView::new(workspace_id, window, cx),
                |v| cell_has(&v.pending_handoff),
                cx,
            )
            .into_any_element(),
            NavModule::Environments => self
                .ensure_environments(workspace_id, active_env_id, window, cx)
                .into_any_element(),
            NavModule::Chat => self.ensure_chat(window, cx).into_any_element(),
            NavModule::Runs => self.ensure_runs(window, cx).into_any_element(),
            NavModule::Providers => self.ensure_providers(window, cx).into_any_element(),
            NavModule::Gateway => self.ensure_gateway(window, cx).into_any_element(),
            NavModule::Usage => self.ensure_usage(window, cx).into_any_element(),
            NavModule::Settings => ensure_watched(
                &mut self.settings,
                &mut self._subscriptions,
                |cx| SettingsView::new(settings_section, theme_prefs, window, cx),
                SettingsView::has_pending_shell_signal,
                cx,
            )
            .into_any_element(),
        }
    }
}
