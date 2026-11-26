//! Cross-module handoff orchestration, Command Palette dynamic indexing, and timed toasts for `AppShell`.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::{App, Context, WeakEntity, Window};
use pingdeck_desktop::core::app::{
    agent_session_list_impl, app_running_work_impl, flow_list_impl, llm_provider_list_impl,
    llm_provider_status_list_impl, mock_server_list_impl, mock_server_status_list_impl,
    request_list_impl,
};
use pingdeck_desktop::core::llm::status::ProviderStatus;
use pingdeck_desktop::state::{
    EnvironmentResourceTarget, NavModule, PaletteAction, PaletteDynamicItems, ToastKind,
};

use crate::bridge::{runtime, spawn_async_into};
use crate::views::{
    chat::ChatHandoff, environments::state::EnvResourceTab, flows::FlowsHandoff,
    gateway::GatewayHandoff, history::HistoryHandoff, mock::MockHandoff,
    providers::ProvidersHandoff, requests::RequestsHandoff, usage::UsageHandoff, utility::IoTab,
};

use super::AppShell;
use super::module_host::ModuleHost;

impl ModuleHost {
    pub fn poll_requests_handoff<V: 'static>(&self, cx: &Context<V>) -> Option<RequestsHandoff> {
        self.requests
            .as_ref()
            .and_then(|v| v.read(cx).take_pending_handoff())
    }

    pub fn poll_mock_handoff<V: 'static>(&self, cx: &Context<V>) -> Option<MockHandoff> {
        self.mock
            .as_ref()
            .and_then(|v| v.read(cx).take_pending_handoff())
    }

    pub fn poll_flows_handoff<V: 'static>(&self, cx: &Context<V>) -> Option<FlowsHandoff> {
        self.flows
            .as_ref()
            .and_then(|v| v.read(cx).take_pending_handoff())
    }

    pub fn poll_history_handoff<V: 'static>(&self, cx: &Context<V>) -> Option<HistoryHandoff> {
        self.history
            .as_ref()
            .and_then(|v| v.read(cx).take_pending_handoff())
    }

    pub fn poll_usage_handoff<V: 'static>(&self, cx: &Context<V>) -> Option<UsageHandoff> {
        self.usage
            .as_ref()
            .and_then(|v| v.read(cx).take_pending_handoff())
    }

    pub fn poll_providers_handoff<V: 'static>(&self, cx: &Context<V>) -> Option<ProvidersHandoff> {
        self.providers
            .as_ref()
            .and_then(|v| v.read(cx).take_pending_handoff())
    }

    pub fn poll_gateway_handoff<V: 'static>(&self, cx: &Context<V>) -> Option<GatewayHandoff> {
        self.gateway
            .as_ref()
            .and_then(|v| v.read(cx).take_pending_handoff())
    }

    pub fn poll_chat_handoff<V: 'static>(&self, cx: &Context<V>) -> Option<ChatHandoff> {
        self.chat
            .as_ref()
            .and_then(|v| v.read(cx).take_pending_handoff())
    }

    pub fn poll_env_active_change<V: 'static>(&self, cx: &Context<V>) -> Option<Option<String>> {
        self.environments
            .as_ref()
            .and_then(|v| v.read(cx).take_pending_active_env())
    }

    /// Reloads mounted workspace-scoped views after an Import/Export modal commits changes.
    pub fn refresh_workspace_after_import<V: 'static>(&mut self, cx: &mut Context<V>) {
        if let Some(v) = &self.requests {
            v.update(cx, |r, cx| r.refresh_catalog(cx));
        }
        if let Some(v) = &self.environments {
            v.update(cx, |e, cx| e.refresh_catalog(cx));
        }
        if let Some(v) = &self.mock {
            v.update(cx, |m, cx| m.refresh_catalog(cx));
        }
    }

    /// Deep-links into Environments › Globals / Cookies / TLS Certificates.
    pub fn open_environment_resource<V: 'static>(
        &mut self,
        workspace_id: Option<String>,
        active_env_id: Option<String>,
        target: EnvironmentResourceTarget,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = self.ensure_environments(workspace_id, active_env_id, window, cx);
        let tab = match target {
            EnvironmentResourceTarget::Globals => EnvResourceTab::Globals,
            EnvironmentResourceTarget::Cookies => EnvResourceTab::Cookies,
            EnvironmentResourceTarget::Certificates => EnvResourceTab::Certificates,
        };
        view.update(cx, |e, cx| e.open_resource_tab(tab, cx));
    }

    /// Deep-links into Requests › Collection Runner for the first workspace collection.
    pub fn open_first_collection_runner<V: 'static>(
        &mut self,
        workspace_id: Option<String>,
        active_env_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = self.ensure_requests(workspace_id, active_env_id, window, cx);
        view.update(cx, |r, cx| r.open_first_collection_runner(window, cx));
    }

    pub fn open_provider_by_id<V: 'static>(
        &mut self,
        provider_id: String,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = self.ensure_providers(window, cx);
        view.update(cx, |p, cx| p.select_provider(provider_id, window, cx));
    }

    pub fn open_sniff_wizard<V: 'static>(&mut self, window: &mut Window, cx: &mut Context<V>) {
        let view = self.ensure_providers(window, cx);
        view.update(cx, |p, cx| {
            p.open_sniff_sheet(None, None, None, window, cx);
        });
    }

    pub fn open_connect_client_wizard<V: 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = self.ensure_gateway(window, cx);
        view.update(cx, |g, cx| g.open_connect_sheet(None, window, cx));
    }

    pub fn open_usage_provider<V: 'static>(
        &mut self,
        provider_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = self.ensure_usage(window, cx);
        view.update(cx, |u, cx| u.select_provider_scope(provider_id, false, cx));
    }

    pub fn open_usage_call_id<V: 'static>(
        &mut self,
        call_id: i64,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = self.ensure_usage(window, cx);
        view.update(cx, |u, cx| u.open_call_by_id(call_id, cx));
    }

    pub fn open_usage_call_request_id<V: 'static>(
        &mut self,
        request_id: String,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = self.ensure_usage(window, cx);
        view.update(cx, |u, cx| u.open_call_by_request_id(request_id, cx));
    }

    pub fn open_usage_run_id<V: 'static>(
        &mut self,
        run_id: String,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let view = self.ensure_usage(window, cx);
        view.update(cx, |u, cx| u.open_calls_for_run(run_id, window, cx));
    }
}

impl AppShell {
    /// Refreshes running work counts, gateway port, running mock servers, workspace item counts,
    /// and asynchronous provider health attention badges.
    pub(crate) fn refresh_running_work(&mut self, cx: &mut Context<Self>) {
        let rt = runtime(cx);
        let st = rt.state();
        self.state.running_work = app_running_work_impl(st);
        self.state.gateway_port = self
            .state
            .running_work
            .gateway_running
            .then(|| st.llm().gateway.status(st.llm()).port);

        let prov_task = spawn_async_into(
            cx,
            |state| async move { llm_provider_status_list_impl(&state).await.ok() },
            |this, statuses, cx| {
                if let Some(list) = statuses {
                    let next = list
                        .iter()
                        .filter(|s| {
                            matches!(
                                s.status,
                                ProviderStatus::NeedsAttention | ProviderStatus::Unreachable
                            )
                        })
                        .count();
                    if this.provider_attention_count != next {
                        this.provider_attention_count = next;
                        cx.notify();
                    }
                }
            },
        );
        self._tasks.push(prov_task);

        let mut mocks_live = Vec::new();
        let mut counts = HashMap::new();
        for ws in &self.state.workspaces {
            let wid = ws.id.as_str().to_owned();
            let req_n = request_list_impl(st, wid.clone()).map_or(0, |v| v.len());
            let servers = mock_server_list_impl(st, wid.clone()).unwrap_or_default();
            let mock_n = servers.len();
            counts.insert(wid.clone(), (req_n, mock_n));
            if self.state.active_workspace_id.as_deref() == Some(wid.as_str())
                && let Ok(sts) = mock_server_status_list_impl(st, wid)
            {
                for s in sts.into_iter().filter(|x| x.running) {
                    if let Some(srv) = servers.iter().find(|m| m.id == s.server_id) {
                        mocks_live.push((srv.id.clone(), srv.name.clone(), srv.port));
                    }
                }
            }
        }
        self.running_mock_summary = mocks_live;
        self.workspace_counts = counts;
    }

    pub fn execute_palette_action(
        &mut self,
        item_id: String,
        action: PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entry = self
            .local_ui
            .palette_frecency
            .entry(item_id)
            .or_insert((0, 0));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = pingdeck_desktop::core::app::current_time_ms();
        self.state.palette_open = false;

        let ws_id = self.state.active_workspace_id.clone();
        let active_env_id = self.state.active_environment_id.clone();
        match action {
            PaletteAction::Navigate(m) => self.navigate(m, cx),
            PaletteAction::OpenSettings(s) => self.open_settings_section(s, cx),
            PaletteAction::OpenUtility(u) => self.open_utility(u, window, cx),
            PaletteAction::OpenEnvironmentResource(target) => {
                self.navigate(NavModule::Environments, cx);
                self.modules
                    .open_environment_resource(ws_id, active_env_id, target, window, cx);
            }
            PaletteAction::OpenCollectionRunner => {
                self.navigate(NavModule::Requests, cx);
                self.modules
                    .open_first_collection_runner(ws_id, active_env_id, window, cx);
            }
            PaletteAction::SwitchWorkspace(id) => self.switch_workspace(id, cx),
            PaletteAction::OpenChatSession(sid) => {
                self.navigate(NavModule::Chat, cx);
                self.modules.open_chat_session_mounted(sid, window, cx);
            }
            PaletteAction::OpenRequestById(rid) => {
                self.navigate(NavModule::Requests, cx);
                self.modules
                    .open_saved_request_by_id(ws_id, rid, window, cx);
            }
            PaletteAction::OpenMockServer(mid) => {
                self.navigate(NavModule::Mock, cx);
                self.modules.open_mock_server(ws_id, mid, window, cx);
            }
            PaletteAction::OpenFlow(fid) => {
                self.navigate(NavModule::Flows, cx);
                self.modules.open_flow(ws_id, fid, window, cx);
            }
            PaletteAction::OpenProvider(pid) => {
                self.navigate(NavModule::Providers, cx);
                self.modules.open_provider_by_id(pid, window, cx);
            }
            PaletteAction::OpenGateway => self.navigate(NavModule::Gateway, cx),
            PaletteAction::OpenSniffWizard => {
                self.navigate(NavModule::Providers, cx);
                self.modules.open_sniff_wizard(window, cx);
            }
            PaletteAction::OpenConnectClientWizard => {
                self.navigate(NavModule::Gateway, cx);
                self.modules.open_connect_client_wizard(window, cx);
            }
            PaletteAction::ToggleSidebar => self.toggle_sidebar(cx),
            PaletteAction::CycleThemeMode => self.toggle_theme_mode(cx),
            PaletteAction::ToggleLanguage => self.toggle_language(cx),
        }
    }

    /// Pushes a toast notification with automatic dismissal (`3.5s` info/success, `7s` warning;
    /// errors remain visible until dismissed by the user).
    pub fn push_toast_auto(
        &mut self,
        kind: ToastKind,
        title: impl Into<String>,
        detail: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let id = self.state.push_toast(kind, title, detail);
        let delay = match kind {
            ToastKind::Info | ToastKind::Success => Some(Duration::from_millis(3_500)),
            ToastKind::Warning => Some(Duration::from_millis(7_000)),
            ToastKind::Error => None,
        };
        if let Some(wait) = delay {
            let task = cx.spawn(async move |this: WeakEntity<Self>, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |shell, cx| {
                    shell.state.dismiss_toast(id);
                    cx.notify();
                });
            });
            self._tasks.push(task);
        }
        cx.notify();
    }

    /// Snapshots dynamic workspace & shared-AI entities for the `⌘K` Command Palette.
    pub fn collect_palette_dynamic_items(&self, cx: &App) -> PaletteDynamicItems {
        let rt = runtime(cx);
        let state = rt.state();
        let mut out = PaletteDynamicItems::default();

        if let Some(ws_id) = self.state.active_workspace_id.as_deref() {
            if let Ok(reqs) = request_list_impl(state, ws_id.to_owned()) {
                out.requests = reqs
                    .into_iter()
                    .take(40)
                    .map(|r| {
                        (
                            r.id.as_str().to_owned(),
                            format!("{:?}", r.method).to_uppercase(),
                            if r.name.trim().is_empty() {
                                r.url
                            } else {
                                r.name
                            },
                        )
                    })
                    .collect();
            }
            if let Ok(servers) = mock_server_list_impl(state, ws_id.to_owned()) {
                let statuses =
                    mock_server_status_list_impl(state, ws_id.to_owned()).unwrap_or_default();
                out.mocks = servers
                    .into_iter()
                    .take(20)
                    .map(|s| {
                        let running = statuses.iter().any(|st| st.server_id == s.id && st.running);
                        (s.id, s.name, running)
                    })
                    .collect();
            }
            if let Ok(flows) = flow_list_impl(state, ws_id.to_owned()) {
                out.flows = flows
                    .into_iter()
                    .take(20)
                    .map(|f| (f.flow.id, f.flow.name))
                    .collect();
            }
        }

        if let Ok(page) = agent_session_list_impl(state, None, Some(20), None) {
            out.chats = page
                .sessions
                .into_iter()
                .map(|c| {
                    (
                        c.id,
                        if c.title.trim().is_empty() {
                            "Untitled Chat".to_owned()
                        } else {
                            c.title
                        },
                    )
                })
                .collect();
        }
        if let Ok(providers) = llm_provider_list_impl(state) {
            out.providers = providers
                .into_iter()
                .take(20)
                .map(|p| (p.id, p.name))
                .collect();
        }
        out
    }

    /// Polls all mounted modules for cross-module navigation, handoffs, and shared environment changes.
    pub fn poll_all_handoffs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 1. Settings signals
        let (pending_pal, pending_back, pending_nav) = self.modules.poll_settings_signals(cx);
        if let Some(pid) = pending_pal {
            self.set_palette(pid, cx);
        }
        if let Some(target_mod) = pending_nav {
            self.navigate(target_mod, cx);
        } else if pending_back {
            self.state.leave_settings();
            self.persist_local(cx);
        }

        // 2. Active environment sync from EnvironmentsView
        if let Some(next_env) = self.modules.poll_env_active_change(cx) {
            self.state.active_environment_id = next_env.clone();
            self.modules.set_active_environment(next_env, cx);
            self.persist_local(cx);
        }

        // 3. Requests handoffs
        if let Some(handoff) = self.modules.poll_requests_handoff(cx) {
            match handoff {
                RequestsHandoff::OpenSettings(sec) => self.open_settings_section(sec, cx),
                RequestsHandoff::OpenEnvironments => self.navigate(NavModule::Environments, cx),
                RequestsHandoff::OpenHistory => self.navigate(NavModule::History, cx),
                RequestsHandoff::OpenUsageCall(req_id) => {
                    self.navigate(NavModule::Usage, cx);
                    self.modules.open_usage_call_request_id(req_id, window, cx);
                }
                RequestsHandoff::OpenImportExport {
                    export_collection_id,
                } => {
                    let tab = if export_collection_id.is_some() {
                        IoTab::SanitizedExport
                    } else {
                        IoTab::Import
                    };
                    self.open_import_export(tab, export_collection_id, window, cx);
                }
                RequestsHandoff::AskAgent {
                    title,
                    context_markdown,
                } => {
                    self.navigate(NavModule::Chat, cx);
                    self.modules
                        .handoff_to_chat(title, context_markdown, window, cx);
                }
            }
        }

        // 4. Mock handoffs
        if let Some(MockHandoff::OpenTestRequest { name, method, url }) =
            self.modules.poll_mock_handoff(cx)
            && let Some(ws_id) = self.state.active_workspace_id.clone()
            && let Ok(mut req) = pingdeck_desktop::core::model::Request::new(
                pingdeck_desktop::core::app::factory_service::new_entity_id("req"),
                &ws_id,
                name,
                url,
            )
        {
            req.method = match method.as_str() {
                "POST" => pingdeck_desktop::core::model::HttpMethod::Post,
                "PUT" => pingdeck_desktop::core::model::HttpMethod::Put,
                "PATCH" => pingdeck_desktop::core::model::HttpMethod::Patch,
                "DELETE" => pingdeck_desktop::core::model::HttpMethod::Delete,
                "HEAD" => pingdeck_desktop::core::model::HttpMethod::Head,
                "OPTIONS" => pingdeck_desktop::core::model::HttpMethod::Options,
                _ => pingdeck_desktop::core::model::HttpMethod::Get,
            };
            self.navigate(NavModule::Requests, cx);
            self.modules
                .open_request_in_requests(Some(ws_id), req, window, cx);
        }

        // 5. Flows handoffs
        if let Some(FlowsHandoff::OpenRequest(req_id)) = self.modules.poll_flows_handoff(cx) {
            let ws_id = self.state.active_workspace_id.clone();
            self.navigate(NavModule::Requests, cx);
            self.modules
                .open_saved_request_by_id(ws_id, req_id, window, cx);
        }

        // 6. History handoffs
        if let Some(handoff) = self.modules.poll_history_handoff(cx) {
            match handoff {
                HistoryHandoff::OpenInRequests(req) => {
                    let ws_id = self.state.active_workspace_id.clone();
                    self.navigate(NavModule::Requests, cx);
                    self.modules
                        .open_request_in_requests(ws_id, *req, window, cx);
                }
                HistoryHandoff::AskAgent {
                    title,
                    context_markdown,
                } => {
                    self.navigate(NavModule::Chat, cx);
                    self.modules
                        .handoff_to_chat(title, context_markdown, window, cx);
                }
            }
        }

        // 7. Usage handoffs
        if let Some(handoff) = self.modules.poll_usage_handoff(cx) {
            match handoff {
                UsageHandoff::OpenSettings(sec) => self.open_settings_section(sec, cx),
                UsageHandoff::OpenAiRequest(draft) => {
                    let ws_id = self.state.active_workspace_id.clone();
                    self.navigate(NavModule::Requests, cx);
                    self.modules.open_ai_client_draft(ws_id, *draft, window, cx);
                }
                UsageHandoff::AskAgent {
                    title,
                    context_markdown,
                } => {
                    self.navigate(NavModule::Chat, cx);
                    self.modules
                        .handoff_to_chat(title, context_markdown, window, cx);
                }
            }
        }

        // 8. Providers handoffs
        if let Some(handoff) = self.modules.poll_providers_handoff(cx) {
            match handoff {
                ProvidersHandoff::OpenUsageProvider(pid) => {
                    self.navigate(NavModule::Usage, cx);
                    self.modules.open_usage_provider(pid, window, cx);
                }
                ProvidersHandoff::OpenUsageCall(call_id) => {
                    self.navigate(NavModule::Usage, cx);
                    self.modules.open_usage_call_id(call_id, window, cx);
                }
            }
        }

        // 9. Local Gateway handoffs
        if let Some(handoff) = self.modules.poll_gateway_handoff(cx) {
            match handoff {
                GatewayHandoff::OpenProviders => self.navigate(NavModule::Providers, cx),
                GatewayHandoff::OpenUsageProvider(pid) => {
                    self.navigate(NavModule::Usage, cx);
                    self.modules.open_usage_provider(pid, window, cx);
                }
                GatewayHandoff::OpenSettings(sec) => self.open_settings_section(sec, cx),
            }
        }

        // 10. Chat handoffs
        if let Some(handoff) = self.modules.poll_chat_handoff(cx) {
            match handoff {
                ChatHandoff::OpenSettings(sec) => self.open_settings_section(sec, cx),
                ChatHandoff::OpenProviders => self.navigate(NavModule::Providers, cx),
                ChatHandoff::OpenUsageRun(run_id) => {
                    self.navigate(NavModule::Usage, cx);
                    self.modules.open_usage_run_id(run_id, window, cx);
                }
            }
        }
    }
}
