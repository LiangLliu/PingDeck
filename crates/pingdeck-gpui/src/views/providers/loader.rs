//! Async data loading, view navigation, and slide-over sheet launchers for `ProvidersView`.

use std::collections::HashMap;

use gpui_kit::{AppContext as _, Context, ParentElement as _, Window, px};
use pingdeck_desktop::core::app::{
    ClientInspectDto, GatewayOverviewDto, GatewayRule, ManagedTool, ProviderModelView,
    ProviderView, llm_client_detect_impl, llm_client_inspect_impl, llm_client_managed_list_impl,
    llm_gateway_client_list_impl, llm_gateway_overview_impl, llm_gateway_rules_impl,
    llm_key_list_impl, llm_log_list_impl, llm_model_catalog_list_impl, llm_model_list_view_impl,
    llm_presets_impl, llm_probe_results_impl, llm_provider_health_impl,
    llm_provider_list_view_impl, llm_provider_pricing_list_impl, llm_provider_status_list_impl,
    llm_subscribe_impl, settings_get_impl,
};
use pingdeck_desktop::core::client_config::{ClientApp, DetectedClient, DetectedSnippetClient};
use pingdeck_desktop::core::llm::events::LlmEvent;
use pingdeck_desktop::core::llm::presets::ProviderPreset;
use pingdeck_desktop::core::llm::store::{
    GatewayClient, LogFilter, ProbeResult, ProviderKey, RequestLogEntry,
};
use pingdeck_desktop::core::model::events::Sequenced;
use pingdeck_desktop::core::model::model_catalog::ModelCatalogQuery;
use pingdeck_desktop::core::model::settings::AiGatewaySettings;
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};
use crate::ui::WindowExt as _;

use super::connect_sheet::ConnectToolKind;
use super::{
    LeftSelection, ProviderTab, ProvidersView, bench, connect_sheet, detail_config, sniff_sheet,
};

struct ReloadSnapshot {
    providers: Vec<ProviderView>,
    keys: Vec<ProviderKey>,
    models: Vec<ProviderModelView>,
    presets: Vec<ProviderPreset>,
    overview: Option<GatewayOverviewDto>,
    gw_settings: AiGatewaySettings,
    detected_clients: Vec<DetectedClient>,
    snippet_clients: Vec<DetectedSnippetClient>,
    inspections: HashMap<ClientApp, ClientInspectDto>,
    managed_tools: Vec<ManagedTool>,
    gateway_clients: Vec<GatewayClient>,
    gateway_rules: Vec<GatewayRule>,
    probe_results: Vec<ProbeResult>,
    recent_logs: Vec<RequestLogEntry>,
}

impl ProvidersView {
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
                            | LlmEvent::ProviderStatusChanged { .. }
                            | LlmEvent::ProvidersChanged
                            | LlmEvent::KeysChanged { .. }
                            | LlmEvent::ModelsChanged { .. }
                            | LlmEvent::CatalogChanged
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

    pub fn provider_name(&self, id: &str) -> String {
        self.providers
            .iter()
            .find(|p| p.provider.id == id)
            .map_or_else(|| id.to_owned(), |p| p.provider.name.clone())
    }

    pub fn reload_all(&mut self, cx: &mut Context<Self>) {
        let selected_pid = match &self.selection {
            LeftSelection::Provider(id) => Some(id.clone()),
            _ => None,
        };
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
                let recent_logs = selected_pid
                    .as_deref()
                    .and_then(|pid| {
                        llm_log_list_impl(
                            state,
                            Some(LogFilter {
                                provider_id: Some(pid.to_owned()),
                                ..LogFilter::default()
                            }),
                            Some(8),
                            None,
                        )
                        .ok()
                    })
                    .unwrap_or_default();
                ReloadSnapshot {
                    providers: llm_provider_list_view_impl(state).unwrap_or_default(),
                    keys: llm_key_list_impl(state, None).unwrap_or_default(),
                    models: llm_model_list_view_impl(state, None).unwrap_or_default(),
                    presets: llm_presets_impl().map(|c| c.providers).unwrap_or_default(),
                    overview: llm_gateway_overview_impl(state).ok(),
                    gw_settings: settings_get_impl(state).ai.gateway,
                    detected_clients,
                    snippet_clients,
                    inspections,
                    managed_tools: llm_client_managed_list_impl(state).unwrap_or_default(),
                    gateway_clients: llm_gateway_client_list_impl(state).unwrap_or_default(),
                    gateway_rules: llm_gateway_rules_impl(state).unwrap_or_default(),
                    probe_results: llm_probe_results_impl(state, None).unwrap_or_default(),
                    recent_logs,
                }
            },
            |this, snap, cx| {
                if let LeftSelection::Provider(id) = &this.selection
                    && !snap.providers.iter().any(|p| &p.provider.id == id)
                {
                    this.selection = LeftSelection::Gateway;
                }
                this.providers = snap.providers;
                this.keys = snap.keys;
                this.models = snap.models;
                this.presets = snap.presets;
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
                this.probe_results = snap.probe_results;
                this.recent_logs = snap.recent_logs;
                cx.notify();
            },
        );
        self._tasks.push(task);

        let health_pid = match &self.selection {
            LeftSelection::Provider(id) => Some(id.clone()),
            _ => None,
        };
        let status_task = spawn_async_into(
            cx,
            move |state| async move {
                let statuses = llm_provider_status_list_impl(&state)
                    .await
                    .unwrap_or_default();
                let health = match health_pid {
                    Some(pid) => llm_provider_health_impl(&state, &pid).await.ok(),
                    None => None,
                };
                let pricing = llm_provider_pricing_list_impl(&state)
                    .await
                    .unwrap_or_default();
                let families = llm_model_catalog_list_impl(
                    &state,
                    ModelCatalogQuery {
                        limit: Some(1),
                        ..ModelCatalogQuery::default()
                    },
                )
                .await
                .map(|pg| pg.families)
                .unwrap_or_default();
                (statuses, health, pricing, families)
            },
            |this, (statuses, health, pricing, families), cx| {
                this.statuses = statuses
                    .into_iter()
                    .map(|s| (s.provider_id.clone(), s))
                    .collect();
                this.provider_health = health;
                this.provider_pricing = pricing
                    .into_iter()
                    .map(|p| (p.provider_id.clone(), p))
                    .collect();
                if !families.is_empty() {
                    this.catalog_families = families;
                }
                cx.notify();
            },
        );
        self._tasks.push(status_task);
    }

    pub fn select_provider(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.selection = LeftSelection::Provider(id.clone());
        self.provider_tab = ProviderTab::Overview;
        self.selected_models.clear();
        self.adding_key_open = false;
        self.adding_model_open = false;
        self.revealed_provider_key = None;
        self.models_page = 0;
        self.ensure_config_form(&id, window, cx);
        self.reload_all(cx);
    }

    pub fn ensure_config_form(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .config_form
            .as_ref()
            .is_some_and(|f| f.read(cx).provider_id() == id)
        {
            return;
        }
        if let Some(pv) = self.providers.iter().find(|p| p.provider.id == id).cloned() {
            let parent = cx.entity().clone();
            self.config_form =
                Some(cx.new(|cx| detail_config::ProviderConfigForm::new(parent, pv, window, cx)));
        }
    }

    pub fn open_bench(
        &mut self,
        provider_id: Option<String>,
        models: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selection = LeftSelection::Bench;
        if let Some(b) = &self.bench_view {
            b.update(cx, |bv, cx| bv.focus_provider(provider_id, models, cx));
        } else {
            let parent = cx.entity().clone();
            self.bench_view =
                Some(cx.new(|cx| bench::BenchView::new(parent, provider_id, models, window, cx)));
        }
        cx.notify();
    }

    pub fn open_sniff_sheet(
        &mut self,
        resniff_provider_id: Option<String>,
        initial_url: Option<String>,
        initial_preset: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_resniff = resniff_provider_id.is_some();
        let parent = cx.entity().clone();
        let presets = self.presets.clone();
        let (resniff_url, resniff_name) = resniff_provider_id
            .as_deref()
            .and_then(|id| self.providers.iter().find(|p| p.provider.id == id))
            .map(|p| {
                (
                    Some(p.provider.base_url.clone()),
                    Some(p.provider.name.clone()),
                )
            })
            .unwrap_or((initial_url, None));
        let sheet_ent = cx.new(|cx| {
            sniff_sheet::SniffProviderSheet::new(
                parent,
                presets,
                resniff_provider_id,
                resniff_url,
                resniff_name,
                initial_preset,
                window,
                cx,
            )
        });
        window.open_sheet(cx, move |sheet, _, cx| {
            sheet
                .size(px(640.0))
                .title(if is_resniff {
                    crate::ui::tr(cx).providers().prov_loader_re_scan_provider()
                } else {
                    crate::ui::tr(cx)
                        .providers()
                        .prov_loader_add_ai_provider_smart_sniff()
                })
                .child(sheet_ent.clone())
        });
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
            connect_sheet::ConnectClientSheet::new(
                parent,
                initial_tool,
                gw_url,
                available_models,
                window,
                cx,
            )
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
