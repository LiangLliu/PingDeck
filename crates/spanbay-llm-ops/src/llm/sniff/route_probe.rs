//! Phase 3 of a scan: one zero-token `POST {}` per planned protocol (no model, no tokens),
//! classified into a route verdict, and the opt-in verification that sends one 1-token request
//! per existing route and upgrades its verdict to `Works`.

use serde_json::{Value, json};

use super::{
    auth::AuthFamily,
    exec::{Call, Purpose, Sent, Spend},
    plan::{ModelSource, RouteSpec, gemini_probe_model, route_path},
    route_verdict::classify_route,
    scan::Scan,
    types::RouteVerdict,
};
use crate::llm::{
    adapters::decision,
    safe_http::{SafeMethod, SafeResponse},
    wire::{AuthStyle, Protocol},
};

/// Characters of an error answer kept for the key verdict's quota check.
const ERROR_TEXT_CHARS: usize = 512;

/// The outcome of one protocol's probe.
#[derive(Debug, Clone)]
pub(super) struct RouteResult {
    /// The protocol.
    pub protocol: Protocol,
    /// The mount of the last attempt (the one the verdict is about).
    pub mount: String,
    /// The verdict.
    pub verdict: RouteVerdict,
    /// HTTP status of the last attempt.
    pub status: Option<u16>,
    /// Latency of the last attempt.
    pub latency_ms: Option<u64>,
    /// Every mount tried.
    pub tried: Vec<String>,
    /// The credential style in force for the protocol's family.
    pub auth: AuthStyle,
    /// Error text of the answer for quota classification (never reported).
    pub error_text: String,
    /// The probe path of the last attempt.
    pub path: String,
    /// The route answered 2xx to the empty request: it may have run a completion.
    pub lenient_2xx: bool,
}

/// What one probe answer said.
struct ProbeOutcome {
    verdict: RouteVerdict,
    status: Option<u16>,
    latency_ms: Option<u64>,
    error_text: String,
    lenient_2xx: bool,
}

impl ProbeOutcome {
    /// A probe that got no response (transport error): the route counts as absent.
    const fn no_answer() -> Self {
        Self {
            verdict: RouteVerdict::Absent,
            status: None,
            latency_ms: None,
            error_text: String::new(),
            lenient_2xx: false,
        }
    }
}

/// The progress-row label of `protocol`'s route probe.
pub(super) const fn route_label(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiChat => "Chat route",
        Protocol::AnthropicMessages => "Messages route",
        Protocol::Gemini => "Gemini route",
        Protocol::OpenaiResponses => "Responses route",
        Protocol::Embeddings => "Embeddings route",
        Protocol::DecisionSystemone | Protocol::DecisionOpenai => "Decision route",
    }
}

fn json_content_type() -> Vec<(String, String)> {
    vec![("content-type".to_owned(), "application/json".to_owned())]
}

/// The body of a 1-token verification request.
fn verify_body(protocol: Protocol, model: &str) -> Value {
    match protocol {
        Protocol::AnthropicMessages | Protocol::OpenaiChat => json!({
            "model": model, "max_tokens": 1,
            "messages": [{"role": "user", "content": "ping"}],
        }),
        Protocol::Gemini => json!({
            "contents": [{"role": "user", "parts": [{"text": "ping"}]}],
            "generationConfig": {"maxOutputTokens": 1},
        }),
        Protocol::OpenaiResponses => {
            json!({"model": model, "input": "ping", "max_output_tokens": 16})
        }
        Protocol::Embeddings => json!({"model": model, "input": "ping"}),
        Protocol::DecisionSystemone | Protocol::DecisionOpenai => {
            decision::probe_body(protocol, model)
        }
    }
}

/// Tokens a verification answer reports (OpenAI, Anthropic and Gemini usage fields). The
/// counts are untrusted, so sums saturate.
fn tokens_of(body: &Value) -> u64 {
    let usage = body.get("usage");
    let number = |value: Option<&Value>, key: &str| {
        value
            .and_then(|object| object.get(key))
            .and_then(Value::as_u64)
    };
    number(usage, "total_tokens")
        .or_else(|| {
            let sum = number(usage, "input_tokens")
                .unwrap_or(0)
                .saturating_add(number(usage, "output_tokens").unwrap_or(0));
            (sum > 0).then_some(sum)
        })
        .or_else(|| number(body.get("usageMetadata"), "totalTokenCount"))
        .unwrap_or(0)
}

/// The models a verification may name.
#[derive(Debug, Clone, Default)]
pub(super) struct VerifyTargets {
    /// Ids of the listed chat models.
    pub chat: Vec<String>,
    /// The first listed embedding model.
    pub embedding: Option<String>,
}

impl Scan<'_> {
    /// Mounts to try for `spec`, in order; one on a default scan.
    fn route_mounts(&self, spec: &RouteSpec) -> Vec<String> {
        let mut mounts = Vec::new();
        if spec.follows_models {
            let preferred = self
                .state()
                .models
                .preferred_mount(ModelSource::of(spec.protocol));
            mounts.extend(preferred);
        }
        for candidate in &spec.candidates {
            if !mounts.contains(candidate) {
                mounts.push(candidate.clone());
            }
        }
        if !self.input.extended {
            mounts.truncate(1);
        }
        mounts
    }

    /// The model a route probe names: only Gemini's path has one, but a verbatim mount may carry
    /// `{m}` for any protocol, so every probe works it out.
    fn probe_model(&self) -> String {
        let state = self.state();
        let listed = state.models.raw.iter().map(|raw| raw.model.id.as_str());
        gemini_probe_model(self.url.model_hint.as_deref(), listed)
    }

    /// The empty `POST {path}` of a route probe.
    fn route_call(
        &self,
        protocol: Protocol,
        path: String,
        tier: u8,
        hold: Option<u32>,
        row: Option<u32>,
    ) -> Call {
        Call {
            headers: json_content_type(),
            body: Some(b"{}".to_vec()),
            row,
            ..Call::new(
                Purpose::Route(protocol),
                route_label(protocol),
                SafeMethod::Post,
                path,
                self.ex.limits.probe,
                Spend::held(tier, hold),
            )
        }
    }

    /// Classifies one probe answer, after recording what it reveals about the host.
    fn route_outcome(&self, protocol: Protocol, response: &SafeResponse) -> ProbeOutcome {
        let json = response.json();
        self.observe_parsed(response, json.as_ref());
        let answered = self.state().models.list_answered(ModelSource::of(protocol));
        let verdict = classify_route(protocol, response, json.as_ref(), answered);
        ProbeOutcome {
            verdict,
            status: Some(response.status),
            latency_ms: Some(response.elapsed_ms()),
            error_text: if response.status >= 400 {
                String::from_utf8_lossy(&response.body)
                    .chars()
                    .take(ERROR_TEXT_CHARS)
                    .collect()
            } else {
                String::new()
            },
            lenient_2xx: verdict == RouteVerdict::Exists && response.is_success(),
        }
    }

    /// Phase 3 for one protocol: `POST {path}` with `{}`, then (extended scans only) the next
    /// mount while the route is absent. Only the first attempt uses `row` and `hold`.
    pub(super) async fn probe_route(
        &self,
        spec: &RouteSpec,
        row: u32,
        tier: u8,
        hold: Option<u32>,
    ) {
        let protocol = spec.protocol;
        let verbatim = self.plan.verbatim == Some(protocol);
        let model = self.probe_model();
        let family = AuthFamily::of(protocol);
        let mut row = Some(row);
        let mut hold = hold;
        let mut tried = Vec::new();
        let mut last: Option<(String, String, ProbeOutcome)> = None;
        for mount in self.route_mounts(spec) {
            let path = route_path(protocol, &mount, &model, verbatim);
            let call = self.route_call(protocol, path.clone(), tier, hold.take(), row.take());
            let Sent::Done { result: answer } = self.authed(family, call).await.0 else {
                break;
            };
            let outcome = match answer {
                Ok(response) => self.route_outcome(protocol, &response),
                Err(_) => ProbeOutcome::no_answer(),
            };
            tried.push(mount.clone());
            let absent = outcome.verdict == RouteVerdict::Absent;
            last = Some((mount, path, outcome));
            if !absent {
                break;
            }
        }
        let Some((mount, path, outcome)) = last else {
            return;
        };
        let result = RouteResult {
            protocol,
            mount,
            verdict: outcome.verdict,
            status: outcome.status,
            latency_ms: outcome.latency_ms,
            tried,
            auth: self.trials.get(family).current(),
            error_text: outcome.error_text,
            path,
            lenient_2xx: outcome.lenient_2xx,
        };
        self.state().routes.push(result);
    }

    /// `(protocol, mount, model)` of every route that exists and has a model to name.
    fn verification_plan(&self, targets: &VerifyTargets) -> Vec<(Protocol, String, String)> {
        let state = self.state();
        state
            .routes
            .iter()
            .filter(|route| route.verdict == RouteVerdict::Exists)
            .filter_map(|route| {
                let model = match route.protocol {
                    Protocol::Embeddings => targets.embedding.clone()?,
                    Protocol::Gemini => gemini_probe_model(
                        self.url.model_hint.as_deref(),
                        targets.chat.iter().map(String::as_str),
                    ),
                    _ => targets.chat.first()?.clone(),
                };
                Some((route.protocol, route.mount.clone(), model))
            })
            .collect()
    }

    /// Upgrades `protocol`'s route to `Works` with the verification's status and latency.
    fn mark_works(&self, protocol: Protocol, response: &SafeResponse) {
        let mut state = self.state();
        if let Some(route) = state
            .routes
            .iter_mut()
            .find(|route| route.protocol == protocol)
        {
            route.verdict = RouteVerdict::Works;
            route.status = Some(response.status);
            route.latency_ms = Some(response.elapsed_ms());
        }
    }

    /// The opt-in verification: one 1-token request per route that exists. Returns the tokens
    /// the answers report.
    pub(super) async fn verify(&self, targets: &VerifyTargets) -> u64 {
        let planned = self.verification_plan(targets);
        let verbatim = |protocol| self.plan.verbatim == Some(protocol);
        let calls = planned.iter().map(|(protocol, mount, model)| {
            let protocol = *protocol;
            let body = verify_body(protocol, model).to_string().into_bytes();
            let call = Call {
                headers: json_content_type(),
                body: Some(body),
                ..Call::new(
                    Purpose::Verify(protocol),
                    format!("{} (real request)", route_label(protocol)),
                    SafeMethod::Post,
                    route_path(protocol, mount, model, verbatim(protocol)),
                    self.ex.limits.models,
                    Spend::Verify,
                )
            };
            async move {
                (
                    protocol,
                    self.authed(AuthFamily::of(protocol), call).await.0,
                )
            }
        });
        let mut tokens: u64 = 0;
        for (protocol, sent) in futures_util::future::join_all(calls).await {
            let Some(response) = sent.into_response() else {
                continue;
            };
            if !response.is_success() {
                continue;
            }
            tokens = tokens.saturating_add(response.json().map_or(0, |body| tokens_of(&body)));
            self.mark_works(protocol, &response);
        }
        tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_fields_of_three_vendors() {
        assert_eq!(tokens_of(&json!({"usage": {"total_tokens": 9}})), 9);
        assert_eq!(
            tokens_of(&json!({"usage": {"input_tokens": 8, "output_tokens": 1}})),
            9
        );
        assert_eq!(
            tokens_of(&json!({"usageMetadata": {"totalTokenCount": 7}})),
            7
        );
        assert_eq!(tokens_of(&json!({})), 0);
    }

    #[test]
    fn untrusted_token_counts_saturate() {
        // Regression guard (the sum used to overflow).
        let body = json!({"usage": {"input_tokens": u64::MAX, "output_tokens": 1}});
        assert_eq!(tokens_of(&body), u64::MAX);
    }

    #[test]
    fn verification_bodies_are_one_token_pings() {
        for protocol in [Protocol::OpenaiChat, Protocol::AnthropicMessages] {
            let body = verify_body(protocol, "m");
            assert_eq!(body["max_tokens"], 1);
            assert_eq!(body["messages"][0]["content"], "ping");
        }
        assert_eq!(
            verify_body(Protocol::Gemini, "m")["generationConfig"]["maxOutputTokens"],
            1
        );
        assert!(verify_body(Protocol::Gemini, "m").get("model").is_none());
        assert_eq!(verify_body(Protocol::Embeddings, "e")["input"], "ping");
        let systemone = verify_body(Protocol::DecisionSystemone, "jev-latest");
        assert_eq!(systemone["model"], "jev-latest");
        assert!(systemone["state"].is_string() && systemone.get("messages").is_none());
        let openai = verify_body(Protocol::DecisionOpenai, "gpt-6-luna");
        assert_eq!(openai["model"], "gpt-6-luna");
        assert_eq!(openai["questions"][0]["type"], "predicate");
    }
}
