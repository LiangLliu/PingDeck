//! Async data loading, live `LlmEvent` subscription, and Connect Client sheet launcher for `GatewayView`.

use std::collections::HashMap;

use gpui_kit::{AppContext as _, Context, ParentElement as _, Window, px};
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
use crate::ui::WindowExt as _;

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
                let relevant = batch.iter().any(|ev| {
                    matches!(
                        ev.event,
                        LlmEvent::GatewayStatus { .. }
                            | LlmEvent::ProvidersChanged
                            | LlmEvent::ModelsChanged { .. }
                            | LlmEvent::ClientsChanged
                            | LlmEvent::GatewayLogBatch { .. }
                            | LlmEvent::Gap { .. }
                    )
                });
                if relevant {
                    this.reload_all(cx);
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
        self.open_connect_sheet_kind(
            ConnectToolKind::File(initial_app.unwrap_or(ClientApp::ClaudeCode)),
            window,
            cx,
        );
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
        let sheet_ent = cx.new(|cx| {
            ConnectClientSheet::new(parent, initial_tool, gw_url, available_models, window, cx)
        });
        window.open_sheet(cx, move |sheet, _, cx| {
            sheet
                .size(px(640.0))
                .title(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_loader_connect_local_ai_coding_tool(),
                )
                .child(sheet_ent.clone())
        });
    }
}
