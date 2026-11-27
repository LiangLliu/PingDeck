//! Identity scoring of the sniffer: which gateway family answers. Pure: the engine fills
//! [`Facts`] from responses and this module adds up the weights of the family table (a signal
//! of one family never adds to another). A winner is accepted at a score of at least 3 with a
//! lead of at least 2 over the runner-up; otherwise the host is "Compatible, unidentified".

use serde_json::Value;

use super::{
    local::LocalHit,
    types::{IdentityLabel, RunnerUp},
};
use crate::llm::{presets::KnownHost, wire::GatewayFamily};
pub(crate) use spanbay_llm_domain::llm::sniff_summary::clean_field;

/// The lowest score a winner is accepted at.
const ACCEPT_MIN_SCORE: i32 = 3;

/// The lead over the runner-up a winner needs to be accepted.
const ACCEPT_MIN_LEAD: i32 = 2;

/// Vercel's AI gateway, recognised by host without a preset.
const VERCEL_GATEWAY_HOST: &str = "ai-gateway.vercel.sh";

/// What the requests of a scan revealed; everything starts empty.
#[derive(Debug, Default)]
pub(super) struct Facts {
    /// The owner of the host by domain rule (+4).
    pub domain: Option<GatewayFamily>,
    /// The `/api/status` body when it was a 2xx JSON object.
    pub status_body: Option<Value>,
    /// `/api/status` answered 2xx with a body that is not JSON.
    pub status_unparsable: bool,
    /// `GET /api/v1/auth/me` without a key: status and JSON body.
    pub sub2api_me: Option<(u16, Option<Value>)>,
    /// `GET /model/info`: status and JSON body.
    pub model_info: Option<(u16, Option<Value>)>,
    /// Lower-case header names of every response seen.
    pub header_names: Vec<String>,
    /// Header values by lower-case name (only the ones the table reads).
    pub versions: Vec<(String, String)>,
    /// `error.type` of every JSON error body seen.
    pub error_types: Vec<String>,
    /// A 401 body `{"error":{"message":"Authentication Error…"}}` was seen.
    pub litellm_envelope: bool,
    /// The public model list answered with pricing and context fields.
    pub openrouter_anonymous_models: bool,
    /// `GET /api/user/info` with the console token answered `code:0` and a balance field.
    pub voapi_user_info: bool,
    /// The host is loopback on CLIProxyAPI's default port.
    pub cpa_default_port: bool,
    /// A local runtime recognised by its fingerprints.
    pub local: Option<LocalHit>,
}

impl Facts {
    /// Whether a header or the 401 envelope points at LiteLLM (`/model/info` is only asked
    /// then).
    pub(super) fn litellm_hinted(&self) -> bool {
        self.header_prefix("x-litellm-") || self.litellm_envelope
    }

    /// Records the headers of one response.
    pub(super) fn observe_headers(&mut self, headers: &[(String, String)]) {
        for (name, value) in headers {
            let name = name.to_ascii_lowercase();
            if matches!(
                name.as_str(),
                "x-new-api-version" | "x-litellm-version" | "x-cpa-version"
            ) {
                self.versions.push((name.clone(), value.clone()));
            }
            self.header_names.push(name);
        }
    }

    /// Records the error envelope of one JSON body.
    pub(super) fn observe_error(&mut self, body: &Value) {
        if let Some(kind) = body
            .get("error")
            .and_then(|error| error.get("type"))
            .and_then(Value::as_str)
        {
            self.error_types.push(kind.to_owned());
        }
        let message = body
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str);
        if message.is_some_and(|text| text.starts_with("Authentication Error")) {
            self.litellm_envelope = true;
        }
    }

    fn header(&self, name: &str) -> bool {
        self.header_names.iter().any(|candidate| candidate == name)
    }

    fn header_prefix(&self, prefix: &str) -> bool {
        self.header_names
            .iter()
            .any(|name| name.starts_with(prefix))
    }

    fn version_header(&self, name: &str) -> Option<&str> {
        self.versions
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }

    fn error_type(&self, kind: &str) -> bool {
        self.error_types.iter().any(|candidate| candidate == kind)
    }
}

/// The family a known host belongs to by domain rule: OpenRouter, the local runtime presets and
/// Vercel's gateway have their own; every other preset is `Official`.
#[must_use]
pub(super) fn domain_family(known: Option<&KnownHost>, host: &str) -> Option<GatewayFamily> {
    if host == VERCEL_GATEWAY_HOST {
        return Some(GatewayFamily::VercelAiGateway);
    }
    known.map(|known| match known.preset_id.as_str() {
        "openrouter" => GatewayFamily::OpenRouter,
        "ollama" => GatewayFamily::Ollama,
        "lmstudio" => GatewayFamily::LmStudio,
        "vllm" => GatewayFamily::Vllm,
        _ => GatewayFamily::Official,
    })
}

/// The outcome of the scoring.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Identity {
    /// The family to report: the winner, or `UnknownCompatible` when it was not accepted.
    pub family: GatewayFamily,
    /// Score of the winner.
    pub score: i32,
    /// The second-best family.
    pub runner_up: Option<RunnerUp>,
    /// Whether the winner was accepted.
    pub accepted: bool,
    /// Whether a response of the host carried a signal for the accepted winner.
    pub confirmed: bool,
    /// The label.
    pub label: IdentityLabel,
    /// Brand text from the family's field; `None` when the host has to be used.
    pub brand: Option<String>,
    /// Version text.
    pub version: Option<String>,
    /// `data.quota_per_unit` of `/api/status`.
    pub quota_per_unit: Option<f64>,
    /// `data.display_in_currency` of `/api/status`.
    pub display_in_currency: Option<bool>,
    /// done-hub: `quota` is what is left.
    pub quota_is_remaining: bool,
    /// The brand rests on third-party sources only (Veloera).
    pub third_party: bool,
}

/// One family's points on the board.
#[derive(Debug, Clone, Copy)]
struct Points {
    family: GatewayFamily,
    score: i32,
    /// Some of the points came from a response of the host (not the domain rule).
    from_response: bool,
}

/// The running scores, one entry per family in the order its first signal was added.
#[derive(Default)]
struct Board {
    scores: Vec<Points>,
}

impl Board {
    fn add(&mut self, family: GatewayFamily, points: i32, from_response: bool) {
        match self.scores.iter_mut().find(|entry| entry.family == family) {
            Some(entry) => {
                entry.score += points;
                entry.from_response |= from_response;
            }
            None => self.scores.push(Points {
                family,
                score: points,
                from_response,
            }),
        }
    }

    /// The winner and the runner-up. The sort is stable, so on equal scores the family added
    /// first ranks first: the order of the `add` calls is part of the result.
    fn ranking(mut self) -> (Option<Points>, Option<Points>) {
        self.scores
            .sort_by_key(|entry| std::cmp::Reverse(entry.score));
        (self.scores.first().copied(), self.scores.get(1).copied())
    }
}

fn non_empty_str<'a>(data: Option<&'a Value>, key: &str) -> Option<&'a str> {
    data.and_then(|data| data.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn has_field(data: Option<&Value>, key: &str) -> bool {
    data.is_some_and(|data| data.get(key).is_some_and(|value| !value.is_null()))
}

/// What `/api/status` and the new-api/one-api markers say.
struct StatusSignals<'a> {
    /// `data` of the status body.
    data: Option<&'a Value>,
    /// `success: true` with a numeric `data.quota_per_unit`: the shape new-api and one-api share.
    structural: bool,
    system_name: Option<&'a str>,
    version_field: Option<&'a str>,
    /// `system_name` names done-hub.
    done_token: bool,
    /// `system_name` or `version` names Veloera.
    veloera_token: bool,
    /// A RixAPI licence field.
    rix: bool,
    /// A new-api-only header, field or error type, and no fork signal.
    new_api_exclusive: bool,
    /// A one-api marker, no fork signal and nothing new-api-only.
    one_api_exclusive: bool,
}

impl<'a> StatusSignals<'a> {
    fn read(facts: &'a Facts) -> Self {
        let status = facts.status_body.as_ref();
        let data = status.and_then(|body| body.get("data"));
        let structural = status
            .and_then(|body| body.get("success"))
            .and_then(Value::as_bool)
            == Some(true)
            && data
                .and_then(|data| data.get("quota_per_unit"))
                .is_some_and(Value::is_number);
        let system_name = non_empty_str(data, "system_name");
        let version_field = non_empty_str(data, "version");
        let lower_name = system_name.unwrap_or_default().to_ascii_lowercase();
        let done_token = lower_name.contains("done-hub") || lower_name.contains("donehub");
        let veloera_token = lower_name.contains("veloera")
            || version_field.is_some_and(|v| v.to_ascii_lowercase().contains("veloera"));
        let rix = [
            "rix_version_message",
            "rixapi_license_type",
            "rix_license_enabled",
        ]
        .iter()
        .any(|key| has_field(data, key));
        // `new-api` and its forks inherit the `one-api` middleware, so `x-oneapi-request-id` and
        // `one_api_error` say nothing about which of the two answers.
        let fork_signal = done_token || veloera_token || rix;
        let new_api_exclusive = !fork_signal
            && (facts.header("x-new-api-version")
                || [
                    "quota_display_type",
                    "usd_exchange_rate",
                    "custom_currency_symbol",
                ]
                .iter()
                .any(|key| has_field(data, key))
                || facts.error_type("new_api_error"));
        let one_api_exclusive = !fork_signal
            && !new_api_exclusive
            && (facts.header("x-oneapi-request-id") || facts.error_type("one_api_error"));
        Self {
            data,
            structural,
            system_name,
            version_field,
            done_token,
            veloera_token,
            rix,
            new_api_exclusive,
            one_api_exclusive,
        }
    }

    /// A numeric or boolean `data` field, only on the shared status shape.
    fn structural_field<T>(&self, key: &str, read: impl Fn(&Value) -> Option<T>) -> Option<T> {
        if self.structural {
            self.data.and_then(|data| data.get(key)).and_then(read)
        } else {
            None
        }
    }
}

/// Points from `/api/status` and the new-api/one-api markers.
fn add_status_points(board: &mut Board, signals: &StatusSignals<'_>, facts: &Facts) {
    if signals.structural {
        board.add(GatewayFamily::NewApi, 3, true);
        board.add(GatewayFamily::OneApi, 3, true);
        if signals.done_token {
            board.add(GatewayFamily::DoneHub, 3, true);
        }
        if signals.veloera_token {
            board.add(GatewayFamily::Veloera, 3, true);
        }
    }
    if signals.new_api_exclusive {
        board.add(GatewayFamily::NewApi, 3, true);
    }
    let derived_family_signal =
        signals.done_token || signals.veloera_token || signals.rix || signals.one_api_exclusive;
    if signals.system_name.is_some() && !derived_family_signal {
        board.add(GatewayFamily::NewApi, 2, true);
    }
    if signals.one_api_exclusive {
        board.add(GatewayFamily::OneApi, 3, true);
    }
    if signals.structural && signals.system_name.is_none() {
        board.add(GatewayFamily::OneApi, 1, true);
    }
    if signals.done_token {
        board.add(GatewayFamily::DoneHub, 2, true);
    }
    if signals.veloera_token {
        board.add(GatewayFamily::Veloera, 2, true);
    }
    if signals.rix {
        let points = if signals.structural { 6 } else { 3 };
        board.add(GatewayFamily::RixApi, points, true);
    }
    if facts.status_unparsable {
        board.add(GatewayFamily::NewApi, -2, true);
        board.add(GatewayFamily::OneApi, -2, true);
    }
}

/// Points from the product probes: Sub2API's keyless 401, VoAPI's console, LiteLLM.
fn add_probe_points(board: &mut Board, facts: &Facts) {
    if let Some((401, Some(body))) = &facts.sub2api_me {
        let exactly_two_keys = body.as_object().is_some_and(|object| {
            object.len() == 2
                && object.get("code").and_then(Value::as_str) == Some("UNAUTHORIZED")
                && object.get("message").is_some()
        });
        if exactly_two_keys {
            board.add(GatewayFamily::Sub2Api, 3, true);
        }
    }
    if facts.voapi_user_info {
        board.add(GatewayFamily::VoApi, 3, true);
    }
    let mut litellm = 0;
    if facts.header_prefix("x-litellm-") {
        litellm += 3;
    }
    match &facts.model_info {
        Some((200, Some(body)))
            if body
                .get("data")
                .and_then(Value::as_array)
                .is_some_and(|rows| rows.iter().any(|row| row.get("litellm_params").is_some())) =>
        {
            litellm += 3;
        }
        Some((401, _)) => litellm += 2,
        _ => {}
    }
    if litellm > 0 && facts.litellm_envelope {
        litellm += 1;
    }
    if litellm > 0 {
        board.add(GatewayFamily::LiteLlm, litellm, true);
    }
}

/// Points from CLIProxyAPI headers, OpenRouter's anonymous list and a local runtime.
fn add_header_and_local_points(board: &mut Board, facts: &Facts) {
    if ["x-cpa-version", "x-cpa-commit", "x-cpa-build-date"]
        .iter()
        .any(|name| facts.header(name))
    {
        board.add(GatewayFamily::CliProxyApi, 3, true);
        if facts.cpa_default_port {
            board.add(GatewayFamily::CliProxyApi, 1, true);
        }
    }
    if facts.openrouter_anonymous_models {
        board.add(GatewayFamily::OpenRouter, 3, true);
    }
    if let Some(hit) = &facts.local {
        board.add(hit.family, if hit.port_hint { 4 } else { 3 }, true);
    }
}

/// The brand text `family` takes from the status body or its own name.
fn brand_for(family: GatewayFamily, signals: &StatusSignals<'_>) -> Option<String> {
    match family {
        GatewayFamily::NewApi
        | GatewayFamily::OneApi
        | GatewayFamily::DoneHub
        | GatewayFamily::RixApi => signals.system_name.map(clean_field),
        GatewayFamily::Veloera => Some("Veloera".to_owned()),
        GatewayFamily::OpenRouter => Some("OpenRouter".to_owned()),
        _ => None,
    }
}

/// The version text of `family`: its header or status field, or the local runtime's.
fn version_for(
    family: GatewayFamily,
    facts: &Facts,
    signals: &StatusSignals<'_>,
) -> Option<String> {
    match family {
        GatewayFamily::NewApi => facts
            .version_header("x-new-api-version")
            .or(signals.version_field)
            .map(clean_field),
        GatewayFamily::OneApi
        | GatewayFamily::DoneHub
        | GatewayFamily::Veloera
        | GatewayFamily::RixApi => signals.version_field.map(clean_field),
        GatewayFamily::LiteLlm => facts.version_header("x-litellm-version").map(clean_field),
        GatewayFamily::CliProxyApi => facts.version_header("x-cpa-version").map(clean_field),
        _ if facts.local.as_ref().is_some_and(|hit| hit.family == family) => {
            facts.local.as_ref().and_then(|hit| hit.version.clone())
        }
        _ => None,
    }
}

/// Scores every family from `facts` and picks the winner.
#[must_use]
pub(super) fn score(facts: &Facts) -> Identity {
    let signals = StatusSignals::read(facts);
    let mut board = Board::default();
    if let Some(family) = facts.domain {
        board.add(family, 4, false);
    }
    add_status_points(&mut board, &signals, facts);
    add_probe_points(&mut board, facts);
    add_header_and_local_points(&mut board, facts);

    let (winner, second) = board.ranking();
    let winner = winner.unwrap_or(Points {
        family: GatewayFamily::UnknownCompatible,
        score: 0,
        from_response: false,
    });
    let lead = winner.score - second.map_or(0, |points| points.score.max(0));
    let accepted = winner.score >= ACCEPT_MIN_SCORE && lead >= ACCEPT_MIN_LEAD;
    let family = if accepted {
        winner.family
    } else {
        GatewayFamily::UnknownCompatible
    };
    let label = match (accepted, winner.from_response) {
        (true, true) => IdentityLabel::Confirmed,
        (true, false) => IdentityLabel::Preset,
        (false, _) => IdentityLabel::CompatibleUnidentified,
    };
    Identity {
        family,
        // Only negative signals leave a negative winner; the reported score floors at 0.
        score: winner.score.max(0),
        runner_up: second
            .filter(|points| points.score > 0)
            .map(|points| RunnerUp {
                family: points.family,
                score: points.score,
            }),
        accepted,
        confirmed: accepted && winner.from_response,
        label,
        brand: brand_for(family, &signals).filter(|brand| !brand.is_empty()),
        version: version_for(family, facts, &signals).filter(|version| !version.is_empty()),
        quota_per_unit: signals.structural_field("quota_per_unit", Value::as_f64),
        display_in_currency: signals.structural_field("display_in_currency", Value::as_bool),
        quota_is_remaining: family == GatewayFamily::DoneHub,
        third_party: family == GatewayFamily::Veloera,
    }
}

#[cfg(test)]
mod tests;
