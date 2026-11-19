//! Shared test helpers: an app state over a temp database, fake upstreams registered as
//! providers with one key, run inputs and an event collector.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::app::AppState;
use crate::error::AppError;
use crate::events::EventSink;
use crate::llm::probe::plan::{CheckSelection, CheckSet};
use crate::llm::probe::test_support::{Fake, Reply, Seen, openai_fake, spawn_fake};
use crate::llm::probe::{ProbeStatus, ProbeTest};
use crate::llm::store::Provider;
use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint, Quirks};
use crate::{
    ProbeCellDto, ProbeEvent, ProbeItem, ProbeRunInput, ProbeSummary, SaveProviderKeyInput,
    llm_key_save_impl, llm_probe_run_impl, llm_provider_save_impl,
};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A fresh app state over a database file of its own, shared so a run can be spawned.
pub(super) fn open_state() -> Arc<AppState> {
    let dir = std::env::temp_dir().join(format!(
        "pd-probe-uc-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path: PathBuf = dir.join("pingdeck.sqlite3");
    Arc::new(AppState::open(path).expect("state"))
}

/// A fake upstream registered as a provider with one enabled key.
pub(super) struct Upstream {
    pub fake: Fake,
    pub key_id: String,
    pub secret: String,
}

impl Upstream {
    /// The items for `models` with this upstream's key.
    pub fn items(&self, models: &[&str]) -> Vec<ProbeItem> {
        models
            .iter()
            .map(|model| ProbeItem {
                key_id: self.key_id.clone(),
                model_id: (*model).to_owned(),
            })
            .collect()
    }

    /// POST requests the upstream received, in order.
    pub fn posts(&self) -> Vec<Seen> {
        self.fake
            .requests()
            .into_iter()
            .filter(|seen| seen.method == "POST")
            .collect()
    }

    /// GET requests the upstream received, in order.
    pub fn gets(&self) -> Vec<Seen> {
        self.fake
            .requests()
            .into_iter()
            .filter(|seen| seen.method == "GET")
            .collect()
    }
}

/// The custom provider of `base_url`, speaking OpenAI chat, with `/models` as its list.
pub(super) fn provider(id: &str, base_url: &str) -> Provider {
    Provider {
        id: id.to_owned(),
        name: format!("Relay {id}"),
        preset_id: None,
        category: "custom".to_owned(),
        base_url: base_url.to_owned(),
        auth: AuthStyle::Bearer,
        endpoints: vec![ProviderEndpoint {
            protocol: Protocol::OpenaiChat,
            path_prefix: "/v1".to_owned(),
            auth: None,
            url_mode: Default::default(),
            enabled: true,
        }],
        models_endpoint: Some("/models".to_owned()),
        quirks: Quirks::default(),
        enabled: true,
        ..Default::default()
    }
}

/// Starts a fake upstream and saves a provider `id` and a key `k-<id>` for it.
pub(super) async fn upstream(
    state: &AppState,
    id: &str,
    handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static,
) -> Upstream {
    attach(state, id, spawn_fake(handler).await, Quirks::default())
}

/// [`upstream`] over an already started fake (for shared in-flight counters).
pub(super) fn attach(state: &AppState, id: &str, fake: Fake, quirks: Quirks) -> Upstream {
    let mut saved = provider(id, &format!("http://{}", fake.addr));
    saved.quirks = quirks;
    llm_provider_save_impl(state, saved).expect("provider");
    let secret = format!("sk-{id}-secret-0123456789");
    let key = llm_key_save_impl(
        state,
        SaveProviderKeyInput {
            id: format!("k-{id}"),
            provider_id: id.to_owned(),
            name: "k".to_owned(),
            secret: Some(secret.clone()),
            ..Default::default()
        },
    )
    .expect("key");
    Upstream {
        fake,
        key_id: key.id,
        secret,
    }
}

pub(super) use crate::test_support::add_model;

/// The default input: the Standard set over `items`.
pub(super) fn standard(items: Vec<ProbeItem>) -> ProbeRunInput {
    with_set(items, CheckSet::Standard)
}

/// The Quick set over `items`.
pub(super) fn quick(items: Vec<ProbeItem>) -> ProbeRunInput {
    with_set(items, CheckSet::Quick)
}

fn with_set(items: Vec<ProbeItem>, set: CheckSet) -> ProbeRunInput {
    ProbeRunInput {
        items,
        checks: CheckSelection::Set { set },
        ..empty_input()
    }
}

/// A list of exactly `tests` over `items`.
pub(super) fn custom(items: Vec<ProbeItem>, tests: &[ProbeTest]) -> ProbeRunInput {
    ProbeRunInput {
        items,
        checks: CheckSelection::Custom {
            tests: tests.to_vec(),
        },
        ..empty_input()
    }
}

/// An input with no rows and the Standard set: the base the other modes fill in.
pub(super) fn empty_input() -> ProbeRunInput {
    ProbeRunInput {
        items: Vec::new(),
        checks: CheckSelection::Set {
            set: CheckSet::Standard,
        },
        protocol: None,
        concurrency: None,
        retry_of: None,
        remaining_of: None,
        alias: None,
        confirmed_cost: false,
    }
}

/// Collects every event of a run.
#[derive(Default)]
pub(super) struct Collect(pub Mutex<Vec<ProbeEvent>>);

impl EventSink<ProbeEvent> for Collect {
    fn emit(&self, event: ProbeEvent) -> bool {
        self.0.lock().unwrap().push(event);
        true
    }
}

impl Collect {
    pub fn events(&self) -> Vec<ProbeEvent> {
        self.0.lock().unwrap().clone()
    }

    pub fn cells(&self) -> Vec<ProbeCellDto> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                ProbeEvent::Cell { cell, .. } => Some(cell),
                _ => None,
            })
            .collect()
    }
}

/// Runs `input` and returns the summary with the events.
pub(super) async fn run(
    state: &AppState,
    input: ProbeRunInput,
) -> Result<(ProbeSummary, Collect), AppError> {
    let events = Collect::default();
    let summary = llm_probe_run_impl(state, input, None, &events).await?;
    Ok((summary, events))
}

/// [`run`] that must succeed.
pub(super) async fn run_ok(state: &AppState, input: ProbeRunInput) -> (ProbeSummary, Collect) {
    run(state, input).await.expect("run")
}

/// The model of a request body (`"model": "..."`).
pub(super) fn model_of(seen: &Seen) -> String {
    serde_json::from_str::<serde_json::Value>(&seen.body)
        .ok()
        .and_then(|body| body["model"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The status of the cell (`model`, `test`) of a run's events.
pub(super) fn status_of(cells: &[ProbeCellDto], model: &str, test: ProbeTest) -> ProbeStatus {
    cells
        .iter()
        .find(|cell| cell.model_id == model && cell.test == test)
        .unwrap_or_else(|| panic!("no cell {model} {test:?}"))
        .status
}

/// An upstream that answers like [`openai_fake`] except for POSTs of `model`, which get `reply`.
pub(super) fn failing(
    model: &'static str,
    status: u16,
) -> impl Fn(&Seen) -> Reply + Send + Sync + 'static {
    move |seen| {
        if seen.method == "POST" && model_of(seen) == model {
            Reply::json(
                status,
                &serde_json::json!({"error": {"message": "refused"}}),
            )
        } else {
            openai_fake(seen)
        }
    }
}

/// The fake answering everything like a healthy OpenAI upstream.
pub(super) fn healthy(seen: &Seen) -> Reply {
    openai_fake(seen)
}
