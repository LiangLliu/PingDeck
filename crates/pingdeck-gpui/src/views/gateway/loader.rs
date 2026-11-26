//! Async data loading, live `LlmEvent` subscription, and centered Connect Client modal launcher
//! for `GatewayView`.

use std::collections::{HashMap, HashSet};

use gpui_kit::{AppContext as _, Context, Window};
use pingdeck_desktop::core::app::{
    ClientInspectDto, GatewayOverviewDto, GatewayRule, ManagedTool, ProviderModelView,
    ProviderView, llm_client_detect_impl, llm_client_inspect_impl, llm_client_managed_list_impl,
    llm_gateway_client_list_impl, llm_gateway_overview_impl, llm_gateway_rules_impl,
    llm_model_list_view_impl, llm_provider_list_view_impl, llm_subscribe_impl, settings_get_impl,
};
use pingdeck_desktop::core::client_config::{ClientApp, DetectedClient, DetectedSnippetClient};
use pingdeck_desktop::core::llm::events::LlmEvent;
use pingdeck_desktop::core::llm::store::GatewayClient;
use pingdeck_desktop::core::model::events::Sequenced;
use pingdeck_desktop::core::model::settings::AiGatewaySettings;
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_blocking_into, subscribe_stream_into};

use super::GatewayView;
use super::connect_sheet::{ConnectClientSheet, ConnectToolKind};

struct GatewaySnapshot {
    providers: Vec<ProviderView>,
    models: Vec<ProviderModelView>,
    overview: Option<GatewayOverviewDto>,
    gw_settings: AiGatewaySettings,
    detected_clients: Vec<DetectedClient>,
    snippet_clients: Vec<DetectedSnippetClient>,
    inspections: HashMap<ClientApp, ClientInspectDto>,
    managed_tools: Vec<ManagedTool>,
    gateway_clients: Vec<GatewayClient>,
    gateway_rules: Vec<GatewayRule>,
}

impl GatewayView {
    pub fn ensure_llm_subscribed(&mut self, cx: &mut Context<Self>) {
        if self.llm_stream_task.is_some() {
            return;
        }
        let (sink, rx) = channel_sink::<Sequenced<LlmEvent>>();
        if let Ok(sub_id) = llm_subscribe_impl(runtime(cx).state(), sink, None) {
            self.llm_sub_id = Some(sub_id);
            self.llm_stream_task = Some(subscribe_stream_into(cx, rx, 16, |this, batch, cx| {
                let mut need_overview = false;
                let mut incoming_gw_logs = 0u64;
                for ev in batch {
                    match ev.event {
                        LlmEvent::GatewayLogBatch { entries } => {
                            let gw_cnt =
                                entries.iter().filter(|e| e.source == "gateway").count() as u64;
                            if gw_cnt > 0 {
                                incoming_gw_logs = incoming_gw_logs.saturating_add(gw_cnt);
                                need_overview = true;
                            }
                        }
                        LlmEvent::GatewayStatus { .. }
                        | LlmEvent::ProvidersChanged
                        | LlmEvent::ModelsChanged { .. }
                        | LlmEvent::ClientsChanged
                        | LlmEvent::Gap { .. } => {
                            need_overview = true;
                        }
                        _ => {}
                    }
                }
                if need_overview {
                    this.reload_all(cx);
                }
                if incoming_gw_logs > 0 {
                    if this.traffic_page == 0 && !this.traffic_body_searching {
                        this.reload_traffic_page(false, cx);
                    } else {
                        this.traffic_pending_live =
                            this.traffic_pending_live.saturating_add(incoming_gw_logs);
                        cx.notify();
                    }
                }
            }));
        }
    }

    pub fn reload_all(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let det = llm_client_detect_impl().ok();
                let (detected_clients, snippet_clients) = det
                    .map(|d| (d.clients, d.snippet_clients))
                    .unwrap_or_default();
                let mut inspections = HashMap::new();
                for app in ClientApp::ALL {
                    if let Ok(ins) = llm_client_inspect_impl(state, app) {
                        inspections.insert(app, ins);
                    }
                }
                GatewaySnapshot {
                    providers: llm_provider_list_view_impl(state).unwrap_or_default(),
                    models: llm_model_list_view_impl(state, None).unwrap_or_default(),
                    overview: llm_gateway_overview_impl(state).ok(),
                    gw_settings: settings_get_impl(state).ai.gateway,
                    detected_clients,
                    snippet_clients,
                    inspections,
                    managed_tools: llm_client_managed_list_impl(state).unwrap_or_default(),
                    gateway_clients: llm_gateway_client_list_impl(state).unwrap_or_default(),
                    gateway_rules: llm_gateway_rules_impl(state).unwrap_or_default(),
                }
            },
            |this, snap, cx| {
                this.providers = snap.providers;
                this.models = snap.models;
                this.overview = snap.overview;
                let gw_changed = this.gateway_settings.port != snap.gw_settings.port
                    || this.gateway_settings.extra_allowed_hosts
                        != snap.gw_settings.extra_allowed_hosts;
                if gw_changed {
                    this.pending_gw_inputs_sync = Some((
                        snap.gw_settings.port.to_string(),
                        snap.gw_settings.extra_allowed_hosts.join(", "),
                    ));
                }
                this.gateway_settings = snap.gw_settings;
                this.detected_clients = snap.detected_clients;
                this.snippet_clients = snap.snippet_clients;
                this.inspections = snap.inspections;
                this.managed_tools = snap.managed_tools;
                this.gateway_clients = snap.gateway_clients;
                this.gateway_rules = snap.gateway_rules;
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn open_connect_sheet(
        &mut self,
        initial_app: Option<ClientApp>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pick = initial_app
            .or_else(|| {
                self.detected_clients
                    .iter()
                    .find(|d| d.installed)
                    .map(|d| d.app)
            })
            .unwrap_or(ClientApp::ClaudeCode);
        self.open_connect_sheet_kind(ConnectToolKind::File(pick), window, cx);
    }

    pub fn open_connect_sheet_kind(
        &mut self,
        initial_tool: ConnectToolKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent = cx.entity().clone();
        let gw_url = self
            .overview
            .as_ref()
            .map(|o| o.status.base_url.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", self.gateway_settings.port));
        let available_models: Vec<(String, String)> = self
            .models
            .iter()
            .filter(|m| m.model.enabled)
            .map(|m| {
                let canonical = format!("{}/{}", m.model.provider_id, m.model.model_id);
                let display = format!(
                    "{}/{}",
                    self.provider_name(&m.model.provider_id),
                    m.model.model_id
                );
                (canonical, display)
            })
            .collect();
        let installed_apps: HashSet<ClientApp> = self
            .detected_clients
            .iter()
            .filter(|d| d.installed)
            .map(|d| d.app)
            .collect();
        let installed_snippets: HashSet<_> = self
            .snippet_clients
            .iter()
            .filter(|s| s.installed)
            .map(|s| s.client)
            .collect();
        let modal_ent = cx.new(|cx| {
            ConnectClientSheet::new(
                parent,
                initial_tool,
                gw_url,
                available_models,
                installed_apps,
                installed_snippets,
                window,
                cx,
            )
        });
        self.connect_modal = Some(modal_ent);
        cx.notify();
    }
}
