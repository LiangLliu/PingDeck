//! A scan in progress: the executor, the input, the plan and everything the requests revealed,
//! plus the credential trial every keyed request goes through.
//!
//! The phases live next to this file: `identify_phase.rs` (local fingerprints, identify, the
//! LiteLLM read), `models_phase.rs` (the model list), `route_probe.rs` (route probes and the
//! opt-in verification) and `balance_hook.rs` (the balance step).

use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use super::{
    auth::{AuthFamily, AuthTrial, status_passed_auth},
    balance_hook::BalanceStep,
    exec::{Call, Exec, Sent, SkipReason},
    identify::Facts,
    local::LocalFacts,
    models::RawModel,
    plan::{ModelAttempt, ModelSource, Plan},
    route_probe::RouteResult,
    types::{SniffContext, SniffInput},
    url::NormalizedUrl,
};
use crate::llm::{
    safe_http::SafeResponse,
    wire::{AuthStyle, Protocol, Quirks, request_headers},
};

/// The first model-list request of a scan, for "Copy as curl".
#[derive(Debug, Clone)]
pub(super) struct FirstModelsRequest {
    /// Origin, path and query.
    pub url: String,
    /// The credential style it was sent with.
    pub style: AuthStyle,
    /// The protocol whose headers it carried.
    pub protocol: Protocol,
}

/// What the model-list phase learned.
#[derive(Debug, Default)]
pub(super) struct ModelsState {
    /// Parsed entries of every page.
    pub raw: Vec<RawModel>,
    /// The attempt that produced the list.
    pub winner: Option<ModelAttempt>,
    /// Status of the last credentialed model-list request.
    pub status: Option<u16>,
    /// Error text of the last model-list answer with status ≥ 400 (never reported).
    pub error_text: String,
    /// Per source: the mount whose list answered.
    pub answered: Vec<(ModelSource, String)>,
    /// Per source: the first mount that answered with anything but 404/405.
    pub first_alive: Vec<(ModelSource, String)>,
    /// The first model-list request.
    pub first_request: Option<FirstModelsRequest>,
}

impl ModelsState {
    /// The mount routes of `source`'s family start at: the one that answered a list, else the
    /// first that was alive.
    pub(super) fn preferred_mount(&self, source: ModelSource) -> Option<String> {
        let find = |list: &[(ModelSource, String)]| {
            list.iter()
                .find(|(candidate, _)| *candidate == source)
                .map(|(_, mount)| mount.clone())
        };
        find(&self.answered).or_else(|| find(&self.first_alive))
    }

    /// Whether a list of `source`'s family answered.
    pub(super) fn list_answered(&self, source: ModelSource) -> bool {
        self.answered
            .iter()
            .any(|(candidate, _)| *candidate == source)
    }
}

/// Everything the phases collect.
#[derive(Default)]
pub(super) struct State {
    pub facts: Facts,
    pub local: LocalFacts,
    pub models: ModelsState,
    pub routes: Vec<RouteResult>,
    /// The balance step's result.
    pub balance: Option<BalanceStep>,
    /// Tokens the real-request verification spent.
    pub tokens_spent: u64,
}

/// One credential trial per family, in [`AuthFamily::ALL`] order.
pub(super) struct Trials([AuthTrial; 3]);

impl Trials {
    pub(super) fn new(origin: &str, ctx: &SniffContext<'_>) -> Self {
        Self(AuthFamily::ALL.map(|family| {
            AuthTrial::new(family, ctx.auth_memory.get(origin, family.representative()))
        }))
    }

    pub(super) const fn get(&self, family: AuthFamily) -> &AuthTrial {
        let [openai, anthropic, gemini] = &self.0;
        match family {
            AuthFamily::OpenAi => openai,
            AuthFamily::Anthropic => anthropic,
            AuthFamily::Gemini => gemini,
        }
    }
}

/// A scan in progress: the executor, the input and what has been learned.
pub(super) struct Scan<'a> {
    pub ex: &'a Exec<'a>,
    pub input: &'a SniffInput,
    pub url: &'a NormalizedUrl,
    pub plan: &'a Plan,
    pub key: Option<&'a str>,
    pub trials: Trials,
    pub state: Mutex<State>,
}

impl Scan<'_> {
    pub(super) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reads headers and error envelopes of a response into the identity facts.
    pub(super) fn observe(&self, response: &SafeResponse) {
        let body = (response.status >= 400).then(|| response.json()).flatten();
        self.observe_parsed(response, body.as_ref());
    }

    /// [`Self::observe`] with the body already parsed (`None` when it is not JSON).
    pub(super) fn observe_parsed(&self, response: &SafeResponse, body: Option<&Value>) {
        let mut state = self.state();
        state.facts.observe_headers(&response.headers);
        if response.status >= 400
            && let Some(body) = body
        {
            state.facts.observe_error(body);
        }
    }

    /// Sends `call` with the credential header of `family`. After an explicit `401` the next
    /// variant is tried (at most two per family), never after `403` or `404`; a retry that is
    /// not sent (budget, stop, cancel) leaves the first answer standing. Without a key no
    /// credential is sent and nothing is retried.
    pub(super) async fn authed(&self, family: AuthFamily, call: Call) -> (Sent, AuthStyle) {
        let protocol = family.representative();
        let Some(key) = self.key else {
            let mut headers = request_headers(protocol, AuthStyle::None, "", &Quirks::default());
            headers.extend(call.headers.iter().cloned());
            let sent = self.ex.call(Call { headers, ..call }).await;
            return (sent, AuthStyle::None);
        };
        let trial = self.trials.get(family);
        // One request at a time finds out which variant works; the others wait for the answer.
        let _gate = if trial.is_settled() {
            None
        } else {
            self.ex.until_cancelled(trial.gate.lock()).await
        };
        let mut row = call.row;
        let mut spend = call.spend;
        let mut previous: Option<(Sent, AuthStyle)> = None;
        loop {
            let style = trial.current();
            let mut attempt = call.clone();
            let mut headers = request_headers(protocol, style, key, &Quirks::default());
            headers.append(&mut attempt.headers);
            attempt.headers = headers;
            attempt.row = row.take();
            attempt.spend = spend;
            spend = call.spend.retry();
            let sent = self.ex.call(attempt).await;
            if let Sent::Skipped(reason) = sent
                && let Some(earlier) = previous.take()
            {
                // The retry was not sent: the first answer stands, the variant untested.
                if reason == SkipReason::Budget {
                    self.ex.note_auth_budget_gap();
                }
                return earlier;
            }
            let Some(response) = sent.response() else {
                return (sent, style);
            };
            if status_passed_auth(response.status) {
                trial.accept(style);
                return (sent, style);
            }
            if response.status != 401 || trial.is_settled() || !trial.reject(style) {
                return (sent, style);
            }
            previous = Some((sent, style));
        }
    }
}
