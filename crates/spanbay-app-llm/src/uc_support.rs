//! Test fixture shared by the gateway, client, log and start-routine use-case tests.
//!
//! Single responsibility: an on-disk [`AppState`] in a temp folder with an injected home
//! directory, a fake chat upstream (`llm::gateway::tests::Fake`), the provider/key/alias wiring
//! through the real use cases, and a small HTTP client for the real gateway. Nothing touches the
//! network or the user's data folder.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::{Value, json};

use super::{
    SaveProviderKeyInput, llm_gateway_start_impl, llm_gateway_stop_impl, llm_key_save_impl,
    llm_model_save_impl, llm_provider_save_impl,
};
use crate::app::{AppState, settings_update_impl};
use crate::llm::gateway::tests::{Canned, Fake, Reply, canned_json, canned_sse, free_port};
use crate::llm::store::{self, ModelRef, ModelSource, Provider, ProviderModel};
use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint, Quirks};
use crate::settings::SettingsSection;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// The upstream key every fixture provider uses.
pub(super) const UPSTREAM_SECRET: &str = "sk-upstream-secret-0123456789abcdef";

/// An app state over a temp folder, an injected home and the gateway on a free port.
#[allow(dead_code)]
pub(super) struct Fx {
    pub(super) state: AppState,
    /// The home directory the writers are pointed at.
    pub(super) home: PathBuf,
    /// The data folder (`<root>/spanbay.sqlite3`, `<root>/backups`, …).
    pub(super) root: PathBuf,
}

/// What [`Fx::wire`] created.
pub(super) struct Wired {
    pub(super) provider_id: String,
    pub(super) key_id: String,
}

impl Fx {
    pub(super) fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pd-uc-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let state = AppState::open(root.join("spanbay.sqlite3")).unwrap();
        state.on_start().unwrap();
        settings_update_impl(
            &state,
            SettingsSection::Ai,
            &json!({"gateway": {"port": free_port()}}),
        )
        .unwrap();
        Self { state, home, root }
    }

    /// Starts the gateway and returns its port.
    pub(super) fn start(&self) -> u16 {
        llm_gateway_start_impl(&self.state).unwrap().port
    }

    /// A provider speaking Chat Completions at `upstream`'s port with one key and the models
    /// `models`, plus an alias `alias` whose single target is `models[0]`.
    pub(super) fn wire(&self, alias: &str, upstream: &Fake, models: &[&str]) -> Wired {
        let wired = self.provider("Relay", upstream.port, models);
        self.alias(
            alias,
            &[(&wired.provider_id, Some(&wired.key_id), models[0])],
        );
        wired
    }

    /// Provider `Relay` (port 1, never called), model `m` and alias `smart`, for tests that only
    /// read configuration.
    pub(super) fn wire_without_upstream(&self) -> Wired {
        let wired = self.provider("Relay", 1, &["m"]);
        self.alias("smart", &[(&wired.provider_id, Some(&wired.key_id), "m")]);
        wired
    }

    /// A provider with one enabled key and the given models.
    pub(super) fn provider(&self, name: &str, port: u16, models: &[&str]) -> Wired {
        let provider = llm_provider_save_impl(
            &self.state,
            Provider {
                name: name.to_owned(),
                category: "custom".to_owned(),
                base_url: format!("http://127.0.0.1:{port}"),
                auth: AuthStyle::Bearer,
                endpoints: vec![ProviderEndpoint {
                    protocol: Protocol::OpenaiChat,
                    path_prefix: "/v1".to_owned(),
                    auth: None,
                    url_mode: Default::default(),
                    enabled: true,
                }],
                quirks: Quirks::default(),
                enabled: true,
                ..Default::default()
            },
        )
        .unwrap();
        let key = llm_key_save_impl(
            &self.state,
            SaveProviderKeyInput {
                provider_id: provider.id.clone(),
                name: "k".to_owned(),
                secret: Some(UPSTREAM_SECRET.to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        for model in models {
            llm_model_save_impl(
                &self.state,
                ProviderModel {
                    provider_id: provider.id.clone(),
                    model_id: (*model).to_owned(),
                    source: ModelSource::Manual,
                    enabled: true,
                    ..Default::default()
                },
            )
            .unwrap();
        }
        Wired {
            provider_id: provider.id,
            key_id: key.id,
        }
    }

    /// A routed name the way v46 leaves an old alias: `name` resolves to the first
    /// `(provider, key, model)` triple, the others become its fallback chain (key pins ignored).
    pub(super) fn alias(&self, name: &str, targets: &[(&str, Option<&str>, &str)]) {
        let refs: Vec<ModelRef> = targets
            .iter()
            .map(|(provider, _, model)| ModelRef {
                provider_id: (*provider).to_owned(),
                model: (*model).to_owned(),
            })
            .collect();
        let store = self.state.store();
        store::set_model_fallbacks(store.connection(), &refs[0], &refs[1..]).unwrap();
        store::save_legacy_name(store.connection(), name, &refs[0]).unwrap();
        drop(store);
        self.state.llm.touch();
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = llm_gateway_stop_impl(&self.state);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A fake chat upstream that answers every request `text`, streamed or not.
pub(super) async fn upstream_saying(text: &'static str) -> Fake {
    Fake::start(move |_, request| {
        let canned = Canned::text(&[text]);
        if request.body["stream"] == json!(true) {
            Reply::Sse(canned_sse(Protocol::OpenaiChat, "m", &canned))
        } else {
            Reply::Json(200, canned_json(Protocol::OpenaiChat, "m", &canned))
        }
    })
    .await
}

/// The answer of one gateway call.
pub(super) struct Answer {
    pub(super) status: u16,
    pub(super) body: String,
}

/// POSTs `body` to the real gateway with `token` as Bearer and reads the whole answer.
pub(super) async fn post(port: u16, path: &str, token: Option<&str>, body: &Value) -> Answer {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut call = client
        .post(format!("http://127.0.0.1:{port}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string());
    if let Some(token) = token {
        call = call.header("authorization", format!("Bearer {token}"));
    }
    let response = call.send().await.unwrap();
    Answer {
        status: response.status().as_u16(),
        body: response.text().await.unwrap(),
    }
}

/// A Chat Completions request for `model`.
pub(super) fn chat(model: &str) -> Value {
    json!({"model": model, "messages": [{"role": "user", "content": "hi"}], "max_tokens": 4})
}

/// The stored log rows the gateway announced since [`watch_rows`] was called.
pub(super) struct RowWatch {
    rows: std::sync::mpsc::Receiver<crate::llm::store::RequestLogEntry>,
    leftover: std::cell::RefCell<Vec<crate::llm::store::RequestLogEntry>>,
}

impl RowWatch {
    /// Blocks (event-driven, 10 s deadline) until `count` rows arrived in total, oldest first.
    pub(super) fn rows(&self, count: usize) -> Vec<crate::llm::store::RequestLogEntry> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = self.leftover.borrow_mut();
        while seen.len() < count {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Ok(row) = self.rows.recv_timeout(left) else {
                panic!("only {} of {count} log rows arrived", seen.len());
            };
            seen.push(row);
        }
        seen.clone()
    }
}

/// Listens to the hub for stored log rows (subscribe before the request that makes them).
pub(super) fn watch_rows(fx: &Fx) -> RowWatch {
    let (sender, rows) = std::sync::mpsc::channel();
    super::llm_subscribe_impl(
        &fx.state,
        std::sync::Arc::new(
            move |event: crate::events::Sequenced<crate::llm::events::LlmEvent>| {
                if let crate::llm::events::LlmEvent::GatewayLogBatch { entries } = event.event {
                    for entry in entries {
                        if sender.send(entry).is_err() {
                            return false;
                        }
                    }
                }
                true
            },
        ),
        None,
    )
    .unwrap();
    RowWatch {
        rows,
        leftover: std::cell::RefCell::default(),
    }
}

/// Listens to the hub for gateway status events; they arrive in order.
pub(super) fn watch_status(
    fx: &Fx,
) -> std::sync::mpsc::Receiver<crate::llm::gateway::GatewayStatus> {
    let (sender, receiver) = std::sync::mpsc::channel();
    super::llm_subscribe_impl(
        &fx.state,
        std::sync::Arc::new(
            move |event: crate::events::Sequenced<crate::llm::events::LlmEvent>| {
                if let crate::llm::events::LlmEvent::GatewayStatus { status } = event.event {
                    return sender.send(status).is_ok();
                }
                true
            },
        ),
        None,
    )
    .unwrap();
    receiver
}

/// The next status event satisfying `wanted` within 10 s (earlier ones are skipped).
pub(super) fn wait_status(
    heard: &std::sync::mpsc::Receiver<crate::llm::gateway::GatewayStatus>,
    wanted: impl Fn(&crate::llm::gateway::GatewayStatus) -> bool,
) -> crate::llm::gateway::GatewayStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let status = heard
            .recv_timeout(left)
            .expect("no matching gateway status event");
        if wanted(&status) {
            return status;
        }
    }
}
