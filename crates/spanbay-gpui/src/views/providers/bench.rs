//! Model Test Bench (`BenchView` entity):
//! State management, streaming `ChannelSink<ProbeEvent>` runner, live cost estimation,
//! stop/retry, and Apply Results + Undo. Rendering lives in `bench_table.rs`.

use std::collections::{BTreeMap, BTreeSet};

use gpui_kit::{AppContext as _, Context, Entity, IntoElement, Render, Subscription, Task, Window};
use spanbay_desktop::core::app::{
    ProbeApplyPreviewDto, ProbeCellDto, ProbeEstimateDto, ProbeEvent, ProbeExportFilter,
    ProbeExportFormat, ProbeItem, ProbeRunInput, ProbeSummary, llm_key_list_impl,
    llm_model_list_impl, llm_probe_apply_impl, llm_probe_apply_preview_impl,
    llm_probe_apply_undo_impl, llm_probe_estimate_impl, llm_probe_export_impl,
    llm_probe_results_impl, llm_probe_run_impl, llm_provider_list_impl,
};
use spanbay_desktop::core::llm::probe::plan::{CheckSelection, CheckSet};
use spanbay_desktop::core::llm::probe::{ProbeStatus, ProbeTest};
use spanbay_desktop::core::llm::store::{ProbeResult, Provider, ProviderKey, ProviderModel};
use spanbay_desktop::services::{FileFilter, HostDialog, RfdDialog, SaveFileSpec};
use spanbay_desktop::streams::ChannelSink;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};
use crate::ui::{InputEvent, InputState, Tone};

use super::ProvidersView;
use super::bench_table::render_bench;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum BenchFilter {
    #[default]
    All,
    Failed,
    Slow,
}

pub struct BenchView {
    parent: Entity<ProvidersView>,
    pub(super) providers: Vec<Provider>,
    pub(super) keys: Vec<ProviderKey>,
    pub(super) models: Vec<ProviderModel>,
    pub(super) selected_provider_id: Option<String>,
    pub(super) selected_key_id: Option<String>,
    pub(super) check_set: CheckSet,
    pub(super) concurrency: u8,
    pub(super) include_disabled: bool,
    pub(super) filter: BenchFilter,
    pub(super) bench_page: usize,
    pub(super) bench_page_size: usize,
    pub(super) search_input: Entity<InputState>,
    pub(super) selected_models: BTreeSet<String>,
    /// `(model_id, test_name) -> ProbeCellDto` for the active or loaded run.
    pub(super) cells: BTreeMap<(String, String), ProbeCellDto>,
    pub(super) estimate: Option<ProbeEstimateDto>,
    pub(super) pending_cost_confirm: Option<Option<String>>,
    pub(super) pending_apply_preview: Option<ProbeApplyPreviewDto>,
    pub(super) running: bool,
    cancel_token: Option<String>,
    pub(super) run_id: Option<String>,
    pub(super) progress_done: u32,
    pub(super) progress_total: u32,
    pub(super) last_summary: Option<ProbeSummary>,
    pub(super) last_apply_id: Option<String>,
    pub(super) selected_cell: Option<(String, String)>,
    pub(super) flash: Option<(Tone, String)>,
    _subscriptions: Vec<Subscription>,
    _stream_task: Option<Task<()>>,
    _tasks: Vec<Task<()>>,
}

impl BenchView {
    pub fn new(
        parent: Entity<ProvidersView>,
        initial_provider: Option<String>,
        initial_models: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::ui::tr(cx).providers().prov_bench_search_models())
        });
        let sub = cx.subscribe(&search_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.bench_page = 0;
                cx.notify();
            }
        });
        let mut view = Self {
            parent,
            providers: Vec::new(),
            keys: Vec::new(),
            models: Vec::new(),
            selected_provider_id: initial_provider,
            selected_key_id: None,
            check_set: CheckSet::Quick,
            concurrency: 4,
            include_disabled: false,
            filter: BenchFilter::All,
            bench_page: 0,
            bench_page_size: 50,
            search_input,
            selected_models: initial_models.into_iter().collect(),
            cells: BTreeMap::new(),
            estimate: None,
            pending_cost_confirm: None,
            pending_apply_preview: None,
            running: false,
            cancel_token: None,
            run_id: None,
            progress_done: 0,
            progress_total: 0,
            last_summary: None,
            last_apply_id: None,
            selected_cell: None,
            flash: None,
            _subscriptions: vec![sub],
            _stream_task: None,
            _tasks: Vec::new(),
        };
        view.reload(cx);
        view
    }

    pub fn focus_provider(
        &mut self,
        provider_id: Option<String>,
        models: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(pid) = provider_id {
            self.selected_provider_id = Some(pid);
            self.selected_key_id = None;
            self.bench_page = 0;
        }
        if !models.is_empty() {
            self.selected_models = models.into_iter().collect();
        }
        self.reload(cx);
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            |state| {
                let providers = llm_provider_list_impl(state).unwrap_or_default();
                let keys = llm_key_list_impl(state, None).unwrap_or_default();
                let models = llm_model_list_impl(state, None).unwrap_or_default();
                let results = llm_probe_results_impl(state, None).unwrap_or_default();
                (providers, keys, models, results)
            },
            |this, (providers, keys, models, results), cx| {
                if this.selected_provider_id.is_none() {
                    this.selected_provider_id = providers.first().map(|p| p.id.clone());
                }
                this.providers = providers;
                this.keys = keys;
                this.models = models;
                this.hydrate_stored_results(&results);
                this.recompute_estimate(cx);
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn hydrate_stored_results(&mut self, results: &[ProbeResult]) {
        let Some(pid) = &self.selected_provider_id else {
            return;
        };
        let key_ids: BTreeSet<&str> = self
            .keys
            .iter()
            .filter(|k| &k.provider_id == pid)
            .map(|k| k.id.as_str())
            .collect();
        if self.running {
            return;
        }
        self.cells.clear();
        for res in results {
            if !key_ids.contains(res.key_id.as_str()) {
                continue;
            }
            let Some(test) = parse_probe_test(&res.test) else {
                continue;
            };
            let status = if res.ok {
                ProbeStatus::Ready
            } else {
                match res.status_word.as_str() {
                    "rejected" => ProbeStatus::Rejected,
                    "rate_limited" => ProbeStatus::RateLimited,
                    "unreachable" => ProbeStatus::Unreachable,
                    "unsupported" => ProbeStatus::Unsupported,
                    "not_observed" => ProbeStatus::NotObserved,
                    "skipped" => ProbeStatus::Skipped,
                    "timed_out" => ProbeStatus::TimedOut,
                    _ => ProbeStatus::InvalidResponse,
                }
            };
            let entry_key = (res.model_id.clone(), res.test.clone());
            let should_replace = self
                .cells
                .get(&entry_key)
                .and_then(|c| c.result.as_ref())
                .is_none_or(|old| res.checked_at_ms >= old.checked_at_ms);
            if should_replace {
                self.cells.insert(
                    entry_key,
                    ProbeCellDto {
                        provider_id: pid.clone(),
                        key_id: res.key_id.clone(),
                        model_id: res.model_id.clone(),
                        test,
                        status,
                        reason: res.message.clone(),
                        skip: None,
                        result: Some(res.clone()),
                    },
                );
            }
        }
    }

    fn build_run_input(
        &self,
        retry_of: Option<String>,
        confirmed_cost: bool,
    ) -> Option<ProbeRunInput> {
        let pid = self.selected_provider_id.as_ref()?;
        let active_keys: Vec<&ProviderKey> = self
            .keys
            .iter()
            .filter(|k| {
                &k.provider_id == pid
                    && k.enabled
                    && self
                        .selected_key_id
                        .as_deref()
                        .is_none_or(|sel| sel == k.id)
            })
            .collect();
        let first_key = active_keys.first()?;
        let target_models: Vec<String> = if !self.selected_models.is_empty() {
            self.selected_models.iter().cloned().collect()
        } else {
            self.models
                .iter()
                .filter(|m| &m.provider_id == pid && (self.include_disabled || m.enabled))
                .map(|m| m.model_id.clone())
                .collect()
        };
        let items: Vec<ProbeItem> = if retry_of.is_some() {
            Vec::new()
        } else {
            target_models
                .into_iter()
                .take(100)
                .map(|model_id| ProbeItem {
                    key_id: first_key.id.clone(),
                    model_id,
                })
                .collect()
        };
        if retry_of.is_none() && items.is_empty() {
            return None;
        }
        Some(ProbeRunInput {
            items,
            checks: CheckSelection::Set {
                set: self.check_set,
            },
            protocol: None,
            concurrency: Some(self.concurrency),
            retry_of,
            remaining_of: None,
            alias: None,
            confirmed_cost,
        })
    }

    pub(super) fn recompute_estimate(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.build_run_input(None, false) else {
            self.estimate = None;
            return;
        };
        let task = spawn_async_into(
            cx,
            move |state| async move { llm_probe_estimate_impl(&state, input).await.ok() },
            |this, est, cx| {
                this.estimate = est;
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn request_start_run(&mut self, retry_of: Option<String>, cx: &mut Context<Self>) {
        if retry_of.is_none() && self.estimate.as_ref().is_some_and(|e| e.needs_confirmation) {
            self.pending_cost_confirm = Some(retry_of);
            cx.notify();
            return;
        }
        self.start_run(retry_of, false, cx);
    }

    pub(super) fn start_run(
        &mut self,
        retry_of: Option<String>,
        confirmed_cost: bool,
        cx: &mut Context<Self>,
    ) {
        self.pending_cost_confirm = None;
        let Some(input) = self.build_run_input(retry_of, confirmed_cost) else {
            self.flash = Some((
                Tone::Warning,
                crate::ui::tr(cx)
                    .providers()
                    .prov_bench_need_key_and_model()
                    .into(),
            ));
            cx.notify();
            return;
        };
        let tok = format!("probe-{}", super::format::now_ms());
        self.running = true;
        self.cancel_token = Some(tok.clone());
        self.progress_done = 0;
        self.progress_total = 0;
        self.last_summary = None;
        self.flash = None;
        cx.notify();

        let (sink, rx) = ChannelSink::<ProbeEvent>::pair();
        self._stream_task = Some(subscribe_stream_into(cx, rx, 24, |this, batch, cx| {
            for ev in batch {
                match ev {
                    ProbeEvent::RunStarted { run_id, total, .. } => {
                        this.run_id = Some(run_id);
                        this.progress_total = total;
                    }
                    ProbeEvent::Cell { cell, .. } => {
                        let key = (cell.model_id.clone(), test_key_name(cell.test).to_owned());
                        this.cells.insert(key, cell);
                    }
                    ProbeEvent::Progress { done, total } => {
                        this.progress_done = done;
                        this.progress_total = total;
                    }
                    ProbeEvent::RunFinished { summary } => {
                        this.run_id = Some(summary.run_id.clone());
                        this.last_summary = Some(summary);
                    }
                }
            }
            cx.notify();
        }));

        let parent = self.parent.clone();
        let task = spawn_async_into(
            cx,
            move |state| async move { llm_probe_run_impl(&state, input, Some(tok), &sink).await },
            move |this, res, cx| {
                this.running = false;
                this.cancel_token = None;
                this._stream_task = None;
                match res {
                    Ok(summary) => {
                        this.run_id = Some(summary.run_id.clone());
                        this.flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_bench_done_passed_failed_rate_limited(
                                    summary.passed,
                                    summary.failed,
                                    summary.rate_limited,
                                ),
                        ));
                        this.last_summary = Some(summary);
                        parent.update(cx, |p, cx| p.reload_all(cx));
                    }
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn stop_run(&mut self, cx: &mut Context<Self>) {
        if let Some(tok) = self.cancel_token.clone() {
            runtime(cx).state().cancellations.cancel(&tok);
        }
    }

    pub(super) fn open_apply_preview(&mut self, cx: &mut Context<Self>) {
        let Some(rid) = self.run_id.clone() else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| llm_probe_apply_preview_impl(state, &rid),
            |this, res, cx| {
                match res {
                    Ok(preview) => this.pending_apply_preview = Some(preview),
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn confirm_apply_results(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.pending_apply_preview.take() else {
            return;
        };
        let rid = preview.run_id;
        let sel = preview.default_selection;
        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_probe_apply_impl(state, &rid, &sel),
            move |this, res, cx| {
                match res {
                    Ok(rep) => {
                        this.last_apply_id = rep.apply_id;
                        this.flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_bench_applied_enabled_disabled_created(
                                    rep.enabled.len(),
                                    rep.disabled.len(),
                                    rep.created.len(),
                                ),
                        ));
                        parent.update(cx, |p, cx| p.reload_all(cx));
                    }
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn export_probe_results(
        &mut self,
        format: ProbeExportFormat,
        cx: &mut Context<Self>,
    ) {
        let Some(rid) = self.run_id.clone() else {
            return;
        };
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let file =
                    llm_probe_export_impl(state, &rid, &ProbeExportFilter::default(), format)?;
                let spec = SaveFileSpec {
                    title: None,
                    starting_dir: None,
                    file_name: Some(file.file_name),
                    filters: vec![FileFilter {
                        name: "CSV / JSON".into(),
                        extensions: vec!["csv".into(), "json".into()],
                    }],
                };
                let Some(dest) = RfdDialog.save_file(&spec) else {
                    return Ok::<Option<String>, spanbay_desktop::core::error::AppError>(None);
                };
                std::fs::write(&dest, file.bytes).map_err(|e| {
                    spanbay_desktop::core::error::AppError::io(format!("export write failed: {e}"))
                })?;
                Ok(Some(dest.display().to_string()))
            },
            move |this, res, cx| {
                match res {
                    Ok(Some(path)) => {
                        this.flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_bench_exported_report_to(path),
                        ));
                    }
                    Ok(None) => {}
                    Err(e) => this.flash = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn undo_apply(&mut self, cx: &mut Context<Self>) {
        let Some(aid) = self.last_apply_id.clone() else {
            return;
        };
        let parent = self.parent.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_probe_apply_undo_impl(state, &aid),
            move |this, res, cx| {
                match res {
                    Ok(u) => {
                        this.last_apply_id = None;
                        this.flash = Some((
                            Tone::Neutral,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_bench_undone_models_restored(u.restored.len()),
                        ));
                        parent.update(cx, |p, cx| p.reload_all(cx));
                    }
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}

impl Render for BenchView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render_bench(self, cx)
    }
}

fn parse_probe_test(s: &str) -> Option<ProbeTest> {
    match s {
        "connectivity" => Some(ProbeTest::Connectivity),
        "auth" => Some(ProbeTest::Auth),
        "models" => Some(ProbeTest::Models),
        "chat" => Some(ProbeTest::Chat),
        "stream" => Some(ProbeTest::Stream),
        "tools" => Some(ProbeTest::Tools),
        "json_schema" => Some(ProbeTest::JsonSchema),
        "vision" => Some(ProbeTest::Vision),
        "reasoning" => Some(ProbeTest::Reasoning),
        "cache" => Some(ProbeTest::Cache),
        "embeddings" => Some(ProbeTest::Embeddings),
        "search" => Some(ProbeTest::Search),
        "pdf" => Some(ProbeTest::Pdf),
        "decision" => Some(ProbeTest::Decision),
        "context" => Some(ProbeTest::Context),
        _ => None,
    }
}

const fn test_key_name(t: ProbeTest) -> &'static str {
    match t {
        ProbeTest::Connectivity => "connectivity",
        ProbeTest::Auth => "auth",
        ProbeTest::Models => "models",
        ProbeTest::Chat => "chat",
        ProbeTest::Stream => "stream",
        ProbeTest::Tools => "tools",
        ProbeTest::JsonSchema => "json_schema",
        ProbeTest::Vision => "vision",
        ProbeTest::Reasoning => "reasoning",
        ProbeTest::Cache => "cache",
        ProbeTest::Embeddings => "embeddings",
        ProbeTest::Search => "search",
        ProbeTest::Pdf => "pdf",
        ProbeTest::Decision => "decision",
        ProbeTest::Context => "context",
    }
}
