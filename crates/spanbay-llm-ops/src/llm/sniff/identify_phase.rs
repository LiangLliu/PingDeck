//! The identify requests of a scan: the local-runtime fingerprints, `GET /api/status` with the
//! Sub2API and VoAPI fallbacks, and LiteLLM's `/model/info`. What they reveal goes into the
//! identity facts of [`Scan`].

use std::borrow::Cow;

use futures_util::future::join_all;
use serde_json::Value;

use super::{
    auth::AuthFamily,
    exec::{Call, Purpose, RowPlan, Sent, Spend, TIER_IDENTIFY, note_text},
    local::{FIRST_PROBES, FOLLOW_UPS},
    scan::Scan,
};
use crate::llm::safe_http::{SafeMethod, SafeResponse};
use spanbay_llm_domain::llm::sniff_summary::SYSTEM_NAME_NOTE;

/// Row label of a local-runtime fingerprint.
const LOCAL_LABEL: &str = "Local runtime check";

/// Header-name prefixes of gateway products; a 404 status page with one of them is not a
/// Sub2API or VoAPI host.
const PRODUCT_HEADER_PREFIXES: [&str; 4] = ["x-litellm-", "x-cpa-", "x-new-api", "x-oneapi"];

/// The sentence of an identify answer: what the body harvested ("quota_per_unit 500000, …").
fn describe_status(response: &SafeResponse) -> Option<String> {
    let body = response.json()?;
    let data = body.get("data")?;
    let mut parts = Vec::new();
    if let Some(quota) = data.get("quota_per_unit").and_then(Value::as_f64) {
        parts.push(format!("quota_per_unit {quota}"));
    }
    if let Some(name) = note_text(data.get("system_name")).filter(|name| !name.is_empty()) {
        parts.push(format!("{SYSTEM_NAME_NOTE}{name}"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Whether a header names a gateway product (ASCII case-insensitive prefix match).
fn has_product_header(response: &SafeResponse) -> bool {
    response.headers.iter().any(|(name, _)| {
        PRODUCT_HEADER_PREFIXES.iter().any(|prefix| {
            name.get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })
    })
}

/// Whether a VoAPI `/api/user/info` body is its console's (`code` 0 with a balance field).
fn is_voapi_console(body: &Value) -> bool {
    body.get("code").and_then(Value::as_i64) == Some(0)
        && body.get("data").is_some_and(|data| {
            data.get("basicBalance").is_some() || data.get("bindBalance").is_some()
        })
}

impl Scan<'_> {
    /// The local-runtime fingerprints: five GETs, at most three at a time, then the SGLang,
    /// vLLM and LocalAI follow-ups when `/health` answered and nothing else decided.
    pub(super) async fn local(&self) {
        self.run_local_batch(&FIRST_PROBES).await;
        if self.state().local.needs_followups() {
            self.run_local_batch(&FOLLOW_UPS).await;
        }
    }

    /// Plans `paths` as one batch of uncounted rows, sends them and reads the answers.
    async fn run_local_batch(&self, paths: &[&'static str]) {
        let rows: Vec<RowPlan<'_>> = paths
            .iter()
            .map(|path| RowPlan {
                method: SafeMethod::Get.as_str(),
                path: Cow::Borrowed(path),
                label: LOCAL_LABEL,
                counted: false,
            })
            .collect();
        let ids = self.ex.plan(&rows);
        let sent = join_all(paths.iter().zip(ids).map(|(path, row)| {
            self.ex.call(Call {
                row: Some(row),
                html_ok: true,
                ..Call::new(
                    Purpose::Local,
                    LOCAL_LABEL,
                    SafeMethod::Get,
                    *path,
                    self.ex.limits.local,
                    Spend::Free,
                )
            })
        }))
        .await;
        for (path, sent) in paths.iter().zip(sent) {
            self.read_local(path, &sent);
        }
    }

    fn read_local(&self, path: &str, sent: &Sent) {
        let Some(response) = sent.response() else {
            return;
        };
        self.observe(response);
        let ok = response.is_success() && !response.looks_like_html();
        let body = ok.then(|| response.json()).flatten();
        let local = &mut self.state().local;
        match path {
            "/api/version" => local.api_version = body,
            "/props" => local.props = body,
            "/api/v1/models" => local.lm_studio_v1_models = body,
            "/api/v0/models" => local.lm_studio_models = body,
            "/health" => local.health = ok,
            "/readyz" => local.readyz = ok,
            "/get_model_info" => local.get_model_info = ok,
            "/version" => local.vllm_version = ok,
            _ => {}
        }
    }

    /// `GET /api/status` and, only when it is a 404 on a host without any product header,
    /// Sub2API's `GET /api/v1/auth/me` and (with a typed management token) VoAPI's console.
    pub(super) async fn identify(&self, row: u32, hold: Option<u32>) {
        let call = Call {
            row: Some(row),
            describe: describe_status,
            ..Call::new(
                Purpose::Identify,
                "Identify",
                SafeMethod::Get,
                "/api/status",
                self.ex.limits.probe,
                Spend::held(TIER_IDENTIFY, hold),
            )
        };
        let Some(response) = self.ex.call(call).await.into_response() else {
            return;
        };
        self.identify_status(&response);
        if response.status == 404 && !has_product_header(&response) {
            self.identify_sub2api().await;
            self.identify_voapi().await;
        }
    }

    /// Keeps what `/api/status` answered.
    fn identify_status(&self, response: &SafeResponse) {
        self.observe(response);
        if response.is_success() && !response.looks_like_html() {
            let body = response.json().filter(Value::is_object);
            let mut state = self.state();
            state.facts.status_unparsable = body.is_none();
            state.facts.status_body = body;
        }
    }

    /// An identify request after the status page: keyless, unreserved, plain status line.
    fn identify_fallback(&self, label: &str, path: &str) -> Call {
        Call::new(
            Purpose::Identify,
            label,
            SafeMethod::Get,
            path,
            self.ex.limits.probe,
            Spend::tier(TIER_IDENTIFY),
        )
    }

    /// Sub2API's `GET /api/v1/auth/me`.
    async fn identify_sub2api(&self) {
        let call = self.identify_fallback("Identify (Sub2API)", "/api/v1/auth/me");
        if let Some(answer) = self.ex.call(call).await.into_response() {
            self.observe(&answer);
            let body = answer.json();
            self.state().facts.sub2api_me = Some((answer.status, body));
        }
    }

    /// VoAPI v2 is only recognised through its console: a typed management token is sent to
    /// the typed origin and nowhere else. Nothing is sent without a token.
    async fn identify_voapi(&self) {
        let Some(token) = self
            .input
            .management_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
        else {
            return;
        };
        let call = Call {
            headers: vec![("authorization".to_owned(), format!("Bearer {token}"))],
            ..self.identify_fallback("Identify (VoAPI)", "/api/user/info")
        };
        if let Some(answer) = self.ex.call(call).await.into_response() {
            self.observe(&answer);
            let console = answer.json().is_some_and(|body| is_voapi_console(&body));
            self.state().facts.voapi_user_info = console;
        }
    }

    /// LiteLLM's `GET /model/info`, only when a header or the 401 envelope already points at it.
    pub(super) async fn litellm_model_info(&self) {
        if !self.state().facts.litellm_hinted() {
            return;
        }
        let call = Call::new(
            Purpose::Identify,
            "Identify (LiteLLM)",
            SafeMethod::Get,
            "/model/info",
            self.ex.limits.probe,
            Spend::tier(TIER_IDENTIFY),
        );
        let (sent, _) = self.authed(AuthFamily::OpenAi, call).await;
        if let Some(response) = sent.into_response() {
            self.observe(&response);
            self.state().facts.model_info = Some((response.status, response.json()));
        }
    }
}
