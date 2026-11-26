//! Async data loading, view navigation, and Smart Sniff slide-over launcher for `ProvidersView`.

use gpui_kit::{AppContext as _, Context, ParentElement as _, Window, px};
use pingdeck_desktop::core::app::{
    ProviderModelView, ProviderView, llm_key_list_impl, llm_log_list_impl,
    llm_model_catalog_list_impl, llm_model_list_view_impl, llm_presets_impl,
    llm_probe_results_impl, llm_provider_health_impl, llm_provider_list_view_impl,
    llm_provider_pricing_list_impl, llm_provider_status_list_impl, llm_subscribe_impl,
};
use pingdeck_desktop::core::llm::events::LlmEvent;
use pingdeck_desktop::core::llm::presets::ProviderPreset;
use pingdeck_desktop::core::llm::store::{LogFilter, ProbeResult, ProviderKey, RequestLogEntry};
use pingdeck_desktop::core::model::events::Sequenced;
use pingdeck_desktop::core::model::model_catalog::ModelCatalogQuery;
use pingdeck_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};
use crate::ui::WindowExt as _;

use super::{LeftSelection, ProviderTab, ProvidersView, bench, detail_config, sniff_sheet};

struct ReloadSnapshot {
    providers: Vec<ProviderView>,
    keys: Vec<ProviderKey>,
    models: Vec<ProviderModelView>,
    presets: Vec<ProviderPreset>,
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
                        LlmEvent::ProviderStatusChanged { .. }
                            | LlmEvent::ProvidersChanged
                            | LlmEvent::KeysChanged { .. }
                            | LlmEvent::ModelsChanged { .. }
                            | LlmEvent::CatalogChanged
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
        let selected_pid = match &self.selection {
            LeftSelection::Provider(id) => Some(id.clone()),
            _ => None,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let providers = llm_provider_list_view_impl(state).unwrap_or_default();
                let effective_pid = selected_pid
                    .filter(|id| providers.iter().any(|p| &p.provider.id == id))
                    .or_else(|| providers.first().map(|p| p.provider.id.clone()));
                let recent_logs = effective_pid
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
                    providers,
                    keys: llm_key_list_impl(state, None).unwrap_or_default(),
                    models: llm_model_list_view_impl(state, None).unwrap_or_default(),
                    presets: llm_presets_impl().map(|c| c.providers).unwrap_or_default(),
                    probe_results: llm_probe_results_impl(state, None).unwrap_or_default(),
                    recent_logs,
                }
            },
            |this, snap, cx| {
                match &this.selection {
                    LeftSelection::Empty => {
                        if let Some(first) = snap.providers.first() {
                            this.selection = LeftSelection::Provider(first.provider.id.clone());
                        }
                    }
                    LeftSelection::Provider(id)
                        if !snap.providers.iter().any(|p| &p.provider.id == id) =>
                    {
                        this.selection = snap.providers.first().map_or(LeftSelection::Empty, |p| {
                            LeftSelection::Provider(p.provider.id.clone())
                        });
                    }
                    _ => {}
                }
                this.providers = snap.providers;
                this.keys = snap.keys;
                this.models = snap.models;
                this.presets = snap.presets;
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
                let effective_pid =
                    health_pid.or_else(|| statuses.first().map(|s| s.provider_id.clone()));
                let health = match effective_pid {
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
        window.open_sheet(cx, move |sheet, w, cx| {
            let sheet_w =
                crate::ui::responsive_sheet_width(640.0, f32::from(w.viewport_size().width));
            sheet
                .size(px(sheet_w))
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
}
