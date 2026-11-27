//! The probe plan of a scan: which requests, in what priority, at which mounts.
//!
//! From the normalised URL and the input it derives the request budget, the route probes with
//! their mount candidates, the model-list attempts in source order, and the paths and templates
//! of each probe. Pure: no I/O, so the budget rules are testable without a transport.

use std::fmt::Write as _;

use super::{
    types::{SniffError, SniffInput},
    url::{NormalizedUrl, same_origin_path},
};
use crate::llm::{
    presets::{self, KnownHost},
    wire::{EndpointUrlMode, Protocol, endpoint_url},
};

/// Budget with a key.
const BUDGET_WITH_KEY: u32 = 12;
/// Budget without a key.
const BUDGET_WITHOUT_KEY: u32 = 8;
/// Budget of an extended scan.
const BUDGET_EXTENDED: u32 = 20;

/// The route probe order: Chat, Messages, Gemini, Responses, Embeddings, Systemone, Decisions.
const ROUTE_PRIORITY: [Protocol; 7] = [
    Protocol::OpenaiChat,
    Protocol::AnthropicMessages,
    Protocol::Gemini,
    Protocol::OpenaiResponses,
    Protocol::Embeddings,
    Protocol::DecisionSystemone,
    Protocol::DecisionOpenai,
];

/// The API family a model list comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModelSource {
    /// `{mount}/models`, `{data:[{id}]}`.
    OpenAi,
    /// `{mount}/models?limit=1000`, `has_more` + `last_id`.
    Anthropic,
    /// `{mount}/models?pageSize=1000`, `nextPageToken`.
    Gemini,
    /// `/api/tags` of Ollama.
    OllamaTags,
}

impl ModelSource {
    /// The list family whose answer says "a list of `protocol`'s family answered".
    pub(super) const fn of(protocol: Protocol) -> Self {
        match protocol {
            Protocol::AnthropicMessages => Self::Anthropic,
            Protocol::Gemini => Self::Gemini,
            Protocol::OpenaiChat
            | Protocol::OpenaiResponses
            | Protocol::Embeddings
            | Protocol::DecisionSystemone
            | Protocol::DecisionOpenai => Self::OpenAi,
        }
    }

    /// The protocol whose credential convention the source uses.
    pub(super) const fn protocol(self) -> Protocol {
        match self {
            Self::OpenAi | Self::OllamaTags => Protocol::OpenaiChat,
            Self::Anthropic => Protocol::AnthropicMessages,
            Self::Gemini => Protocol::Gemini,
        }
    }

    /// The query of the first page.
    const fn first_page_query(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("limit=1000"),
            Self::Gemini => Some("pageSize=1000"),
            Self::OpenAi | Self::OllamaTags => None,
        }
    }
}

/// One model-list request candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelAttempt {
    /// Source family.
    pub source: ModelSource,
    /// Path relative to the origin, without query.
    pub path: String,
    /// The mount the list hangs off (what the routes of the family reuse when it answers).
    pub mount: String,
    /// Query of the typed models URL (`?x=y` of Advanced), without the `?`.
    pub query: Option<String>,
}

impl ModelAttempt {
    /// The list of `source` at `{mount}/models`, with the source's first-page query.
    fn at_mount(source: ModelSource, mount: String) -> Self {
        Self {
            source,
            path: format!("{mount}/models"),
            mount,
            query: source.first_page_query().map(str::to_owned),
        }
    }
}

/// One route probe with the mounts it may be tried at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RouteSpec {
    /// The protocol.
    pub protocol: Protocol,
    /// Mounts in trial order (one on a default scan; more only in an extended scan).
    pub candidates: Vec<String>,
    /// Whether the first mount is the one the model list answered at (unknown hosts), not a
    /// fixed preset mount.
    pub follows_models: bool,
}

/// The plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    /// Request budget.
    pub budget: u32,
    /// Send `GET /api/status` (public unknown hosts only).
    pub identify: bool,
    /// Run the local-runtime fingerprints (loopback and private hosts only).
    pub local: bool,
    /// Route probes in priority order.
    pub routes: Vec<RouteSpec>,
    /// Model-list requests in source order; empty = no list to ask for.
    pub model_attempts: Vec<ModelAttempt>,
    /// The typed URL is the request URL of this protocol (`#` plus a pasted tail).
    pub verbatim: Option<Protocol>,
    /// The preset region that owns the host.
    pub known: Option<KnownHost>,
}

/// Mount candidates of an unknown host, per API family.
struct Candidates {
    openai: Vec<String>,
    messages: Vec<String>,
    gemini: Vec<String>,
}

impl Candidates {
    fn of(&self, source: ModelSource) -> &[String] {
        match source {
            ModelSource::OpenAi => &self.openai,
            ModelSource::Anthropic => &self.messages,
            ModelSource::Gemini => &self.gemini,
            ModelSource::OllamaTags => &[],
        }
    }
}

/// The budget of a scan.
const fn budget(has_key: bool, extended: bool) -> u32 {
    if extended {
        BUDGET_EXTENDED
    } else if has_key {
        BUDGET_WITH_KEY
    } else {
        BUDGET_WITHOUT_KEY
    }
}

fn source_order(hint: Option<Protocol>) -> [ModelSource; 3] {
    match hint {
        Some(Protocol::Gemini) => [
            ModelSource::Gemini,
            ModelSource::OpenAi,
            ModelSource::Anthropic,
        ],
        Some(Protocol::AnthropicMessages) => [
            ModelSource::Anthropic,
            ModelSource::OpenAi,
            ModelSource::Gemini,
        ],
        _ => [
            ModelSource::OpenAi,
            ModelSource::Anthropic,
            ModelSource::Gemini,
        ],
    }
}

/// The mount of `protocol` in the preset region of `known`.
fn preset_mount(known: &KnownHost, protocol: Protocol) -> Option<String> {
    presets::region_mount(&known.preset_id, &known.region_id, protocol)
}

/// The route probes in priority order: the one verbatim protocol, a known host's preset mounts,
/// or every protocol with candidates at an unknown host. `protocol_override` keeps one.
fn route_specs(
    url: &NormalizedUrl,
    input: &SniffInput,
    verbatim: Option<Protocol>,
    known: Option<&KnownHost>,
    candidates: &Candidates,
) -> Vec<RouteSpec> {
    let mut routes = Vec::new();
    for protocol in ROUTE_PRIORITY {
        if input.protocol_override.is_some_and(|only| only != protocol)
            || verbatim.is_some_and(|only| only != protocol)
        {
            continue;
        }
        let spec = if verbatim == Some(protocol) {
            RouteSpec {
                protocol,
                candidates: vec![url.typed_mount.clone()],
                follows_models: false,
            }
        } else if let Some(known) = known {
            let Some(mount) = preset_mount(known, protocol) else {
                continue;
            };
            RouteSpec {
                protocol,
                candidates: vec![mount],
                follows_models: false,
            }
        } else {
            let mounts = candidates.of(ModelSource::of(protocol));
            if mounts.is_empty() {
                continue;
            }
            RouteSpec {
                protocol,
                candidates: mounts.to_vec(),
                follows_models: true,
            }
        };
        routes.push(spec);
    }
    routes
}

/// The one attempt of a typed Models URL, which must stay on the typed origin.
fn typed_models_url_attempt(url: &NormalizedUrl, typed: &str) -> Result<ModelAttempt, SniffError> {
    let target = same_origin_path(&url.origin, typed)?;
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_owned(), Some(query.to_owned())),
        None => (target, None),
    };
    Ok(ModelAttempt {
        source: ModelSource::OpenAi,
        path,
        mount: String::new(),
        query,
    })
}

/// The list attempts of a known host, in source order. A preset without a list endpoint means
/// models are typed by hand, so it has none.
fn known_host_attempts(known: &KnownHost, hint: Option<Protocol>) -> Vec<ModelAttempt> {
    let Some(path) = presets::region_models_path(&known.preset_id, &known.region_id) else {
        return Vec::new();
    };
    let mount_of = |protocol| preset_mount(known, protocol);
    // A preset that speaks no chat protocol (TypeSafe) still declares its list; a known host
    // does not use the mount, so the attempt goes out with an empty one.
    let chatless = Protocol::ALL
        .into_iter()
        .filter(|protocol| protocol.is_chat())
        .all(|protocol| mount_of(protocol).is_none());
    let mut attempts = Vec::new();
    for source in source_order(hint) {
        let mount = match source {
            ModelSource::OpenAi => mount_of(Protocol::OpenaiChat)
                .or_else(|| mount_of(Protocol::OpenaiResponses))
                .or_else(|| mount_of(Protocol::Embeddings)),
            ModelSource::Anthropic => mount_of(Protocol::AnthropicMessages),
            ModelSource::Gemini => mount_of(Protocol::Gemini),
            ModelSource::OllamaTags => None,
        };
        let mount = match mount {
            Some(mount) => mount,
            None if source == ModelSource::OpenAi && chatless => String::new(),
            None => continue,
        };
        let attempt = if source == ModelSource::OpenAi {
            ModelAttempt {
                source,
                path: path.clone(),
                mount,
                query: source.first_page_query().map(str::to_owned),
            }
        } else {
            ModelAttempt::at_mount(source, mount)
        };
        attempts.push(attempt);
    }
    attempts
}

/// The list attempts of an unknown host: every mount candidate of every source, in source
/// order.
fn discovered_attempts(candidates: &Candidates, hint: Option<Protocol>) -> Vec<ModelAttempt> {
    source_order(hint)
        .into_iter()
        .flat_map(|source| {
            candidates
                .of(source)
                .iter()
                .map(move |mount| ModelAttempt::at_mount(source, mount.clone()))
        })
        .collect()
}

/// Builds the plan of a scan.
///
/// # Errors
/// [`SniffError::InvalidInput`] for a Models URL on another origin.
pub(super) fn build(url: &NormalizedUrl, input: &SniffInput) -> Result<Plan, SniffError> {
    let extended = input.extended;
    let known = url.known_host.clone();
    let verbatim = url.as_typed.then_some(url.tail_hint).flatten();
    let candidates = Candidates {
        openai: url.openai_candidates(),
        messages: url.messages_candidates(extended),
        gemini: url.gemini_candidates(extended),
    };
    let routes = route_specs(url, input, verbatim, known.as_ref(), &candidates);
    let hint = url.tail_hint.or(input.protocol_override);
    let typed_models_url = input
        .models_url
        .as_deref()
        .filter(|text| !text.trim().is_empty());
    let model_attempts = if let Some(typed) = typed_models_url {
        vec![typed_models_url_attempt(url, typed)?]
    } else if let Some(known) = &known {
        known_host_attempts(known, hint)
    } else {
        discovered_attempts(&candidates, hint)
    };
    Ok(Plan {
        budget: budget(input.key().is_some(), extended),
        identify: known.is_none() && !url.host_class.is_local(),
        local: url.host_class.is_local(),
        routes,
        model_attempts,
        verbatim,
        known,
    })
}

/// Percent-encodes a path segment (a model id): everything but ASCII letters, digits and
/// `-_.~:` becomes an upper-case `%XX` per byte.
fn encode_segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b':') {
            out.push(char::from(byte));
        } else {
            // Writing to a `String` cannot fail.
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The request path of a route probe at `mount`; `model` fills Gemini's `{m}`. A verbatim
/// typed URL is used as typed (its `{m}`, if any, filled the same way).
#[must_use]
pub(super) fn route_path(protocol: Protocol, mount: &str, model: &str, verbatim: bool) -> String {
    if verbatim {
        return mount.replace("{m}", &encode_segment(model));
    }
    endpoint_url(
        "",
        mount,
        EndpointUrlMode::Standard,
        protocol,
        Some(&encode_segment(model)),
        false,
    )
    .unwrap_or_else(|_| format!("{mount}{}", protocol.suffix()))
}

/// The probe path with `{m}` for the model.
#[must_use]
pub(super) fn route_template(protocol: Protocol, mount: &str, verbatim: bool) -> String {
    if verbatim {
        return mount.to_owned();
    }
    // `route_path` percent-encodes the model, so its `{m}` comes back as `%7Bm%7D`.
    route_path(protocol, mount, "{m}", false).replace("%7Bm%7D", "{m}")
}

/// The base a user would paste into an SDK: Chat, Responses and Embeddings keep the mount;
/// Messages and Gemini move the trailing version to the request path.
#[must_use]
pub(super) fn sdk_base_url(
    origin: &str,
    protocol: Protocol,
    mount: &str,
    verbatim: bool,
) -> String {
    let mount = if verbatim {
        mount
    } else {
        match protocol {
            Protocol::AnthropicMessages => mount.strip_suffix("/v1").unwrap_or(mount),
            Protocol::Gemini => mount
                .strip_suffix("/v1beta")
                .or_else(|| mount.strip_suffix("/v1"))
                .unwrap_or(mount),
            _ => mount,
        }
    };
    format!("{origin}{mount}")
}

/// The model id the Gemini probe names: the pasted hint, the first listed id containing
/// `gemini`, the first listed id, else `gemini-probe`.
#[must_use]
pub(super) fn gemini_probe_model<'a, I>(hint: Option<&str>, listed: I) -> String
where
    I: IntoIterator<Item = &'a str>,
    I::IntoIter: Clone,
{
    let mut listed = listed.into_iter();
    hint.or_else(|| listed.clone().find(|id| id.contains("gemini")))
        .or_else(|| listed.next())
        .unwrap_or("gemini-probe")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::sniff::url::normalize_base_url;

    fn plan(input: &str, key: bool, extended: bool) -> Plan {
        let mut typed = SniffInput::new(input);
        typed.api_key = key.then(|| "sk-test-1234567890".to_owned());
        typed.extended = extended;
        build(&normalize_base_url(input).unwrap(), &typed).unwrap()
    }

    fn plan_of(input: &SniffInput) -> Result<Plan, SniffError> {
        build(&normalize_base_url(&input.base_url).unwrap(), input)
    }

    fn protocols(plan: &Plan) -> Vec<Protocol> {
        plan.routes.iter().map(|route| route.protocol).collect()
    }

    #[test]
    fn budgets_and_identify_rules() {
        assert_eq!(plan("https://relay.acme.dev", true, false).budget, 12);
        assert_eq!(plan("https://relay.acme.dev", false, false).budget, 8);
        assert_eq!(plan("https://relay.acme.dev", false, true).budget, 20);
        let relay = plan("https://relay.acme.dev", true, false);
        assert!(relay.identify && !relay.local);
        let deepseek = plan("https://api.deepseek.com", true, false);
        assert!(
            !deepseek.identify && !deepseek.local,
            "a known host skips identify"
        );
        let local = plan("http://192.168.1.20:8080/v1", true, false);
        assert!(!local.identify && local.local);
        assert!(plan("http://localhost:11434", false, false).local);
    }

    #[test]
    fn unknown_host_probes_every_route_in_priority_order() {
        let relay = plan("https://relay.acme.dev", true, false);
        assert_eq!(
            protocols(&relay),
            [
                Protocol::OpenaiChat,
                Protocol::AnthropicMessages,
                Protocol::Gemini,
                Protocol::OpenaiResponses,
                Protocol::Embeddings,
                Protocol::DecisionSystemone,
                Protocol::DecisionOpenai
            ]
        );
        let first: Vec<(&str, &[String])> = relay
            .model_attempts
            .iter()
            .map(|a| (a.path.as_str(), std::slice::from_ref(&a.mount)))
            .collect();
        assert_eq!(
            first.iter().map(|(path, _)| *path).collect::<Vec<_>>(),
            ["/v1/models", "/models", "/v1/models", "/v1beta/models"]
        );
        assert_eq!(relay.model_attempts[2].source, ModelSource::Anthropic);
        assert_eq!(relay.routes[0].candidates, ["/v1", ""]);
        assert_eq!(relay.routes[1].candidates, ["/v1"]);
        assert_eq!(relay.routes[2].candidates, ["/v1beta"]);
    }

    #[test]
    fn known_hosts_probe_only_what_the_preset_lists() {
        let deepseek = plan("https://api.deepseek.com", true, false);
        assert_eq!(
            protocols(&deepseek),
            [
                Protocol::OpenaiChat,
                Protocol::AnthropicMessages,
                Protocol::OpenaiResponses
            ]
        );
        assert_eq!(deepseek.routes[1].candidates, ["/anthropic/v1"]);
        assert_eq!(deepseek.model_attempts[0].path, "/models");
        let zhipu = plan("https://api.z.ai", true, false);
        assert!(
            zhipu.model_attempts.is_empty(),
            "the preset has no list endpoint"
        );
        assert_eq!(zhipu.routes[0].candidates, ["/api/paas/v4"]);
        let anthropic = plan("https://api.anthropic.com/v1/messages", true, false);
        assert_eq!(protocols(&anthropic), [Protocol::AnthropicMessages]);
        assert_eq!(anthropic.model_attempts[0].source, ModelSource::Anthropic);
        assert_eq!(anthropic.model_attempts[0].path, "/v1/models");
        let ollama = plan("http://localhost:11434", false, false);
        assert_eq!(ollama.model_attempts[0].path, "/api/tags");
    }

    #[test]
    fn a_decision_only_preset_lists_its_models_and_probes_the_decision_route() {
        let typesafe = plan("https://api.typesafe.ai", true, false);
        assert_eq!(protocols(&typesafe), [Protocol::DecisionSystemone]);
        assert_eq!(typesafe.routes[0].candidates, ["/v1"]);
        assert!(!typesafe.routes[0].follows_models);
        assert_eq!(typesafe.model_attempts.len(), 1);
        assert_eq!(typesafe.model_attempts[0].source, ModelSource::OpenAi);
        assert_eq!(typesafe.model_attempts[0].path, "/v1/models");
        assert_eq!(typesafe.model_attempts[0].mount, "");
        // The pasted operation tail narrows the same preset to that one route.
        let verbatim = plan("https://api.typesafe.ai/v1/systemone#", true, false);
        assert_eq!(verbatim.verbatim, Some(Protocol::DecisionSystemone));
        assert_eq!(protocols(&verbatim), [Protocol::DecisionSystemone]);
    }

    #[test]
    fn hints_override_and_typed_as_is_narrow_the_plan() {
        let gemini = plan(
            "https://x.test/v1beta/models/g-1:generateContent",
            true,
            false,
        );
        assert_eq!(gemini.model_attempts[0].source, ModelSource::Gemini);
        let messages = plan("https://relay.acme.dev/v1/messages", true, false);
        assert_eq!(messages.model_attempts[0].source, ModelSource::Anthropic);
        let mut input = SniffInput::new("https://relay.acme.dev");
        input.protocol_override = Some(Protocol::AnthropicMessages);
        let overridden = plan_of(&input).unwrap();
        assert_eq!(protocols(&overridden), [Protocol::AnthropicMessages]);
        let verbatim = plan("https://gw.test/v1/chat/completions#", true, false);
        assert_eq!(verbatim.verbatim, Some(Protocol::OpenaiChat));
        assert_eq!(protocols(&verbatim), [Protocol::OpenaiChat]);
        assert_eq!(verbatim.routes[0].candidates, ["/v1/chat/completions"]);
        assert_eq!(verbatim.model_attempts[0].path, "/v1/models");
        let extended = plan("https://relay.acme.dev", true, true);
        assert!(extended.routes[1].candidates.len() > 1);
        assert_eq!(extended.routes[2].candidates, ["/v1beta", "/v1"]);
    }

    #[test]
    fn models_url_override_stays_on_the_origin() {
        let mut input = SniffInput::new("https://relay.acme.dev");
        input.models_url = Some("/custom/list?all=1".into());
        let built = plan_of(&input).unwrap();
        assert_eq!(built.model_attempts.len(), 1);
        assert_eq!(built.model_attempts[0].path, "/custom/list");
        assert_eq!(built.model_attempts[0].query.as_deref(), Some("all=1"));
        input.models_url = Some("https://evil.test/models".into());
        assert!(plan_of(&input).is_err());
    }

    #[test]
    fn paths_templates_and_sdk_bases() {
        assert_eq!(
            route_path(Protocol::OpenaiChat, "/v1", "", false),
            "/v1/chat/completions"
        );
        assert_eq!(
            route_path(Protocol::Embeddings, "", "", false),
            "/embeddings"
        );
        assert_eq!(
            route_path(Protocol::Gemini, "/v1beta", "gemini-3.2-pro", false),
            "/v1beta/models/gemini-3.2-pro:generateContent"
        );
        assert_eq!(
            route_template(Protocol::Gemini, "/v1beta", false),
            "/v1beta/models/{m}:generateContent"
        );
        assert_eq!(
            route_path(Protocol::Gemini, "/v1beta", "a b/c", false),
            "/v1beta/models/a%20b%2Fc:generateContent"
        );
        assert_eq!(
            sdk_base_url("https://x.test", Protocol::OpenaiChat, "/v1", false),
            "https://x.test/v1"
        );
        assert_eq!(
            sdk_base_url("https://x.test", Protocol::AnthropicMessages, "/v1", false),
            "https://x.test"
        );
        assert_eq!(
            sdk_base_url(
                "https://x.test",
                Protocol::AnthropicMessages,
                "/api/anthropic/v1",
                false
            ),
            "https://x.test/api/anthropic"
        );
        assert_eq!(
            sdk_base_url("https://x.test", Protocol::Gemini, "/v1beta", false),
            "https://x.test"
        );
        assert_eq!(
            gemini_probe_model(None, ["gpt-4o", "gemini-3.2-pro"]),
            "gemini-3.2-pro"
        );
        assert_eq!(gemini_probe_model(Some("h"), []), "h");
        assert_eq!(gemini_probe_model(None, ["a"]), "a");
        assert_eq!(gemini_probe_model(None, []), "gemini-probe");
    }

    #[test]
    fn path_segments_keep_only_the_unreserved_set() {
        let printable: String = (0x20u8..=0x7e).map(char::from).collect();
        assert_eq!(
            encode_segment(&format!("{printable}é{{m}}")),
            "%20%21%22%23%24%25%26%27%28%29%2A%2B%2C-.%2F0123456789:%3B%3C%3D%3E%3F%40ABCDEFGHIJKLMNOPQRSTUVWXYZ%5B%5C%5D%5E_%60abcdefghijklmnopqrstuvwxyz%7B%7C%7D~%C3%A9%7Bm%7D"
        );
    }
}
