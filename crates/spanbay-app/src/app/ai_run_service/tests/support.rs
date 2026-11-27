//! Fixtures: an on-disk app state in a temp folder, providers and requests, fake upstreams and an
//! event recorder. Nothing touches the network or the user's data folder.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::app::{
    AiRequestRunInput, AppState, ai_request_run_impl, settings_update_impl, workspace_create_impl,
};
use crate::error::AppError;
use crate::llm::ai_run::{AiResultDto, AiRunEvent};
use crate::llm::gateway::tests::{Canned, Fake, Reply, canned_json, canned_sse};
use crate::llm::store::{self, LogDetail, Provider, ProviderKey};
use crate::llm::usage::PriceOverride;
use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint, Quirks};
use crate::settings::SettingsSection;
use crate::workspace::ai_request::{AiRequestConfig, AiTarget, non_routable_label};
use crate::workspace::{HttpMethod, Request, RequestBody};

/// The provider key every fixture provider uses.
pub(super) const KEY: &str = "sk-ai-run-secret-0123456789abcdef";
/// The workspace of the fixtures.
pub(super) const WS: &str = "ws_ai";

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// An app state over a temp folder, with one workspace.
pub(super) struct Fx {
    pub(super) state: AppState,
    pub(super) root: PathBuf,
}

impl Fx {
    pub(super) fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pd-ai-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = AppState::open(root.join("spanbay.sqlite3")).unwrap();
        state.on_start().unwrap();
        workspace_create_impl(&state, WS.into(), "AI".into()).unwrap();
        Self { state, root }
    }

    /// A provider at `port` speaking `endpoints` (`protocol`, path prefix, auth override), with
    /// one enabled key.
    pub(super) fn provider(
        &self,
        name: &str,
        port: u16,
        auth: AuthStyle,
        endpoints: &[(Protocol, &str, Option<AuthStyle>)],
    ) -> String {
        self.provider_with(name, port, auth, endpoints, Quirks::default())
    }

    pub(super) fn provider_with(
        &self,
        name: &str,
        port: u16,
        auth: AuthStyle,
        endpoints: &[(Protocol, &str, Option<AuthStyle>)],
        quirks: Quirks,
    ) -> String {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("p-{n}");
        let key_id = format!("k-{n}");
        let endpoints: Vec<ProviderEndpoint> = endpoints
            .iter()
            .map(|(protocol, prefix, auth)| ProviderEndpoint {
                protocol: *protocol,
                path_prefix: (*prefix).to_owned(),
                auth: *auth,
                url_mode: Default::default(),
                enabled: true,
            })
            .collect();
        let provider = self
            .state
            .with_llm_connection(|connection| {
                let saved = store::save_provider(
                    connection,
                    &Provider {
                        id: id.clone(),
                        name: name.to_owned(),
                        category: "custom".to_owned(),
                        base_url: format!("http://127.0.0.1:{port}"),
                        auth,
                        endpoints,
                        quirks,
                        enabled: true,
                        ..Default::default()
                    },
                    1,
                )?;
                store::save_key(
                    connection,
                    &ProviderKey {
                        id: key_id,
                        provider_id: saved.id.clone(),
                        name: "k".to_owned(),
                        enabled: true,
                        ..Default::default()
                    },
                    Some(KEY),
                    1,
                )?;
                Ok(saved)
            })
            .unwrap();
        self.state.llm().touch();
        provider.id
    }

    pub(super) fn save_price(&self, price: PriceOverride) {
        self.state
            .with_llm_connection(|connection| store::save_price_override(connection, price, 1))
            .unwrap();
        self.state.llm().touch();
    }

    /// A provider speaking Chat Completions under `/v1` with a bearer key.
    pub(super) fn chat_provider(&self, name: &str, port: u16) -> String {
        self.provider(
            name,
            port,
            AuthStyle::Bearer,
            &[(Protocol::OpenaiChat, "/v1", None)],
        )
    }

    /// Saves an AI request with a provider target.
    pub(super) fn provider_request(
        &self,
        id: &str,
        provider_id: &str,
        protocol: Protocol,
        model: &str,
        body: &str,
    ) -> Request {
        let target = AiTarget::Provider {
            provider_id: provider_id.to_owned(),
            provider_name: provider_id.to_owned(),
            base_url_hint: String::new(),
            model: model.to_owned(),
        };
        let path = match protocol {
            Protocol::Gemini => format!("/models/{model}:generateContent"),
            other => other.suffix().to_owned(),
        };
        let request = ai_request(
            id,
            protocol,
            target,
            &non_routable_label(provider_id, &path),
            body,
        );
        self.save(&request);
        request
    }

    /// Saves an AI request with a custom URL.
    pub(super) fn custom_request(
        &self,
        id: &str,
        url: &str,
        protocol: Protocol,
        model: &str,
        body: &str,
    ) -> Request {
        let request = ai_request(
            id,
            protocol,
            AiTarget::Custom {
                model: model.to_owned(),
            },
            url,
            body,
        );
        self.save(&request);
        request
    }

    pub(super) fn save(&self, request: &Request) {
        self.state.store().save_request(request).unwrap();
    }

    /// Runs the saved request `id` with an event recorder.
    pub(super) async fn run(&self, id: &str) -> Run {
        self.run_with(id, None, None, true).await
    }

    pub(super) async fn run_with(
        &self,
        id: &str,
        environment_id: Option<&str>,
        confirmed: Option<bool>,
        keep_exchange: bool,
    ) -> Run {
        let events = Events::default();
        let sink = events.sink();
        let result = ai_request_run_impl(
            &self.state,
            AiRequestRunInput {
                workspace_id: WS.to_owned(),
                request_id: Some(id.to_owned()),
                request: None,
                environment_id: environment_id.map(str::to_owned),
                confirmed,
                keep_exchange,
            },
            None,
            &sink,
        )
        .await;
        Run { result, events }
    }

    /// The stored log row and capture `dto` points at.
    pub(super) fn log(&self, dto: &AiResultDto) -> LogDetail {
        let id = dto.log_row_id.expect("the run has a log row");
        let store = self.state.store();
        store::get_log(store.connection(), id)
            .unwrap()
            .expect("the log row exists")
    }

    /// Sets the gateway capture policy (`off`, `errors_only`, `all`).
    pub(super) fn capture_mode(&self, mode: &str) {
        settings_update_impl(
            &self.state,
            SettingsSection::Ai,
            &json!({"gateway": {"capture_mode": mode}}),
        )
        .unwrap();
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The result of a run and the events it sent.
pub(super) struct Run {
    pub(super) result: Result<AiResultDto, AppError>,
    pub(super) events: Events,
}

impl Run {
    /// The result, which must have succeeded.
    pub(super) fn ok(&self) -> &AiResultDto {
        self.result.as_ref().expect("the run produced a result")
    }
}

/// Records the events of a run.
#[derive(Clone, Default)]
pub(super) struct Events(Arc<Mutex<Vec<AiRunEvent>>>);

impl Events {
    pub(super) fn sink(&self) -> impl Fn(AiRunEvent) -> bool + Send + Sync + use<> {
        let events = Arc::clone(&self.0);
        move |event| {
            events.lock().unwrap().push(event);
            true
        }
    }

    pub(super) fn all(&self) -> Vec<AiRunEvent> {
        self.0.lock().unwrap().clone()
    }

    /// The number of `Frames` events and the frames they hold.
    pub(super) fn frame_batches(&self) -> (usize, usize) {
        self.all()
            .iter()
            .fold((0, 0), |(batches, frames), event| match event {
                AiRunEvent::Frames { items } => (batches + 1, frames + items.len()),
                _ => (batches, frames),
            })
    }
}

/// An AI request over the HTTP fields of its target.
pub(super) fn ai_request(
    id: &str,
    protocol: Protocol,
    target: AiTarget,
    url: &str,
    body: &str,
) -> Request {
    let mut request = Request::new(id, WS, "AI call", url).unwrap();
    request.method = HttpMethod::Post;
    request.body = RequestBody::Json {
        content: body.to_owned(),
    };
    request.mode = crate::workspace::ai_request::RequestMode::AiApi;
    request.ai = Some(AiRequestConfig {
        protocol,
        target,
        builder: None,
        compare_models: Vec::new(),
    });
    request.network_options.timeout_ms = 10_000;
    request
}

/// A request body of `protocol`, streaming or not.
pub(super) fn body_for(protocol: Protocol, model: &str, stream: bool) -> String {
    match protocol {
        Protocol::OpenaiChat => {
            json!({"model": model, "stream": stream, "max_tokens": 64, "messages": [{"role": "user", "content": "ping"}]})
        }
        Protocol::OpenaiResponses => {
            json!({"model": model, "stream": stream, "input": [{"role": "user", "content": [{"type": "input_text", "text": "ping"}]}]})
        }
        Protocol::AnthropicMessages => {
            json!({"model": model, "stream": stream, "max_tokens": 64, "messages": [{"role": "user", "content": [{"type": "text", "text": "ping"}]}]})
        }
        Protocol::Gemini => {
            json!({"contents": [{"role": "user", "parts": [{"text": "ping"}]}]})
        }
        _ => json!({"model": model, "input": ["ping"]}),
    }
    .to_string()
}

/// A fake upstream that answers every call of `protocol` with `canned`, streamed or not.
pub(super) async fn upstream(protocol: Protocol, canned: Canned) -> Fake {
    Fake::start(move |_, request| {
        let streaming =
            request.body["stream"] == json!(true) || request.path.contains("streamGenerateContent");
        if streaming {
            Reply::Sse(canned_sse(protocol, "m", &canned))
        } else {
            Reply::Json(200, canned_json(protocol, "m", &canned))
        }
    })
    .await
}

/// Chat Completions saying `pong` with usage 9 in / 2 out.
pub(super) fn pong() -> Canned {
    Canned {
        text: vec!["po", "ng"],
        usage: (9, 2),
        ..Canned::default()
    }
}
