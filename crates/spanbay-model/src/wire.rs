//! Wire facts about upstream protocols: endpoint suffixes, authentication, URL building.
//!
//! Single responsibility: everything needed to address one provider endpoint, and nothing about
//! message conversion (that lives in `adapters`). The identifiers serialize exactly as the
//! provider presets (`fixtures/llm-presets.json`) spell them.
//!
//! A request URL is `base_url + path_prefix + suffix`. The suffix belongs to the protocol:
//! `/chat/completions`, `/responses`, `/messages`, `/embeddings`, `/systemone`, `/decisions`, and
//! for Gemini `/models/{model}:generateContent` or `:streamGenerateContent?alt=sse`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use spanbay_base::error::AppError;

/// Default `anthropic-version` header when a provider does not pin one.
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// An upstream (and gateway-facing) API family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// OpenAI Chat Completions.
    OpenaiChat,
    /// OpenAI Responses.
    OpenaiResponses,
    /// Anthropic Messages.
    AnthropicMessages,
    /// Gemini native (`generateContent`, `embedContent`).
    Gemini,
    /// OpenAI-style embeddings.
    Embeddings,
    /// TypeSafe-style decision API (`/systemone`).
    DecisionSystemone,
    /// OpenAI decision API (`/decisions`).
    DecisionOpenai,
}

impl Protocol {
    /// Every protocol, in the order the UI lists them.
    pub const ALL: [Protocol; 7] = [
        Protocol::OpenaiChat,
        Protocol::OpenaiResponses,
        Protocol::AnthropicMessages,
        Protocol::Gemini,
        Protocol::Embeddings,
        Protocol::DecisionSystemone,
        Protocol::DecisionOpenai,
    ];

    /// The identifier used in presets, the database and DTOs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiChat => "openai_chat",
            Self::OpenaiResponses => "openai_responses",
            Self::AnthropicMessages => "anthropic_messages",
            Self::Gemini => "gemini",
            Self::Embeddings => "embeddings",
            Self::DecisionSystemone => "decision_systemone",
            Self::DecisionOpenai => "decision_openai",
        }
    }

    /// Parses [`Protocol::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|protocol| protocol.as_str() == text)
    }

    /// Whether the protocol carries chat (the hub-convertible families).
    #[must_use]
    pub const fn is_chat(self) -> bool {
        matches!(
            self,
            Self::OpenaiChat | Self::OpenaiResponses | Self::AnthropicMessages | Self::Gemini
        )
    }

    /// Whether the protocol is a decision API (it answers typed questions, not chat).
    #[must_use]
    pub const fn is_decision(self) -> bool {
        matches!(self, Self::DecisionSystemone | Self::DecisionOpenai)
    }

    /// The fixed URL suffix; Gemini's depends on the model (see [`endpoint_url`]).
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::OpenaiChat => "/chat/completions",
            Self::OpenaiResponses => "/responses",
            Self::AnthropicMessages => "/messages",
            Self::Gemini => "/models/{model}:generateContent",
            Self::Embeddings => "/embeddings",
            Self::DecisionSystemone => "/systemone",
            Self::DecisionOpenai => "/decisions",
        }
    }

    /// Authentication the protocol's own vendor uses when a provider does not say otherwise.
    #[must_use]
    pub const fn default_auth(self) -> AuthStyle {
        match self {
            Self::AnthropicMessages => AuthStyle::XApiKey,
            Self::Gemini => AuthStyle::XGoogApiKey,
            _ => AuthStyle::Bearer,
        }
    }
}

/// How the secret travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>`. On the Anthropic Messages protocol this also sends
    /// `x-api-key`, because compat layers accept either and vendors disagree on which.
    #[default]
    Bearer,
    /// `x-api-key: <key>` only.
    XApiKey,
    /// `x-goog-api-key: <key>`.
    XGoogApiKey,
    /// `Authorization: <key>` with no scheme (Zhipu and compatible endpoints).
    RawAuthorization,
    /// No credential (local runtimes).
    None,
}

/// How a provider wants reasoning requested on OpenAI-compatible chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningStyle {
    /// Top-level `reasoning_effort`.
    ReasoningEffort,
    /// `reasoning: {"effort": …}` (OpenRouter, Together).
    ReasoningObject,
    /// `thinking: {"type": "enabled"|"disabled"}` (DeepSeek, Zhipu, Doubao, Hunyuan, Qianfan).
    ThinkingType,
    /// `enable_thinking` plus `thinking_budget` (Qwen, SiliconFlow).
    EnableThinking,
    /// `chat_template_kwargs: {"enable_thinking": …}` (vLLM).
    ChatTemplateKwargs,
    /// Anthropic `thinking` with adaptive/budget settings.
    AnthropicAdaptive,
    /// Gemini `generationConfig.thinkingConfig.thinkingLevel`.
    GeminiThinkingLevel,
    /// Send no reasoning controls at all (the provider rejects or ignores them).
    None,
}

/// Per-provider behaviour differences the adapters honour. Every flag is optional: `None`
/// follows the provider's preset, then the codec's own rule. `drop_sampling_params` and
/// `reasoning_request` act on every chat codec, `anthropic_version` on Messages, the rest on Chat
/// Completions only; `no_done_sentinel` is read by no codec.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quirks {
    /// Pinned `anthropic-version` header value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_version: Option<String>,
    /// How reasoning is requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_request: Option<ReasoningStyle>,
    /// Name of the response field carrying reasoning text (`reasoning_content`, `reasoning`,
    /// `thinking`); `None` means the adapter's default set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_response_field: Option<String>,
    /// Sampling parameters (`temperature`, `top_p`, …) are rejected or ignored: do not send them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_sampling_params: Option<bool>,
    /// Reasoning text must be sent back with tool results or the provider answers 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub echo_reasoning_in_tool_turns: Option<bool>,
    /// The stream ends without a `[DONE]` sentinel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_done_sentinel: Option<bool>,
    /// Headers added to every request.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_headers: BTreeMap<String, String>,
    /// Whether the gateway lists this provider's models by direct id in `GET /v1/models`
    /// (`None`: on up to 50 enabled models, off above; direct ids still resolve either way).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_in_models: Option<bool>,
    /// Whether `developer`-role messages are sent as `system` (`None`: Auto, the codec's own rule
    /// per provider and model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub developer_role_as_system: Option<bool>,
    /// Whether streams ask for token usage (`stream_options.include_usage` on chat); `None` is
    /// Auto, the codec's own behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_usage: Option<bool>,
    /// The Chat Completions field that carries the output cap; `None` is Auto (the model rule).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,
    /// Whether a request with tools goes over Responses when the provider speaks it; `None` is
    /// Auto (see [`Quirks::tools_via_responses_for`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_via_responses: Option<bool>,
    /// How the relay prices its models against the official prices. Read by no codec: the
    /// price resolver applies it to every price that is not the provider's own row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_pricing: Option<crate::model_catalog::RelayPricing>,
}

impl Quirks {
    /// Whether sampling parameters are left out.
    #[must_use]
    pub fn drops_sampling(&self) -> bool {
        self.drop_sampling_params == Some(true)
    }

    /// Whether reasoning text goes back with tool results (Chat Completions).
    #[must_use]
    pub fn echoes_reasoning(&self) -> bool {
        self.echo_reasoning_in_tool_turns == Some(true)
    }

    /// These quirks with every unset flag taken from `preset` (Auto follows the preset).
    /// An explicit value, `false` included, stays.
    #[must_use]
    pub fn or_preset(mut self, preset: &Quirks) -> Self {
        fn fill<T: Clone>(slot: &mut Option<T>, preset: &Option<T>) {
            if slot.is_none() {
                slot.clone_from(preset);
            }
        }
        fill(&mut self.anthropic_version, &preset.anthropic_version);
        fill(&mut self.reasoning_request, &preset.reasoning_request);
        fill(
            &mut self.reasoning_response_field,
            &preset.reasoning_response_field,
        );
        fill(&mut self.drop_sampling_params, &preset.drop_sampling_params);
        fill(
            &mut self.echo_reasoning_in_tool_turns,
            &preset.echo_reasoning_in_tool_turns,
        );
        fill(&mut self.no_done_sentinel, &preset.no_done_sentinel);
        fill(&mut self.list_in_models, &preset.list_in_models);
        fill(
            &mut self.developer_role_as_system,
            &preset.developer_role_as_system,
        );
        fill(&mut self.stream_usage, &preset.stream_usage);
        fill(&mut self.max_tokens_field, &preset.max_tokens_field);
        fill(&mut self.tools_via_responses, &preset.tools_via_responses);
        self
    }

    /// Whether a request with tools to `model` goes over Responses: the setting, else
    /// Auto, which says yes for OpenAI's reasoning models (`gpt-5…`, `o1`, `o3`, `o4…` on a
    /// provider that asks for reasoning with `reasoning_effort`): their Chat Completions endpoint
    /// refuses function tools together with reasoning.
    #[must_use]
    pub fn tools_via_responses_for(&self, model: &str) -> bool {
        if let Some(setting) = self.tools_via_responses {
            return setting;
        }
        let id = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        let reasoning_family = id.starts_with("gpt-5")
            || ["o1", "o3", "o4"]
                .iter()
                .any(|family| id == *family || id.starts_with(&format!("{family}-")));
        self.reasoning_request == Some(ReasoningStyle::ReasoningEffort) && reasoning_family
    }
}

/// The Chat Completions field that carries the output-token cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    /// `max_tokens` (the classic field most compatible servers read).
    MaxTokens,
    /// `max_completion_tokens` (OpenAI reasoning models and newer servers).
    MaxCompletionTokens,
}

/// One protocol a provider speaks, with where it lives and how it authenticates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEndpoint {
    /// The API family.
    pub protocol: Protocol,
    /// Path between `base_url` and the protocol suffix (`/v1`, `/api/paas/v4`, or empty).
    #[serde(default)]
    pub path_prefix: String,
    /// Authentication for this endpoint; `None` uses the provider's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthStyle>,
    /// How `base_url + path_prefix` becomes a request URL.
    #[serde(default, skip_serializing_if = "EndpointUrlMode::is_standard")]
    pub url_mode: EndpointUrlMode,
    /// Whether the endpoint is in use. A disabled endpoint keeps its prefix and auth override but
    /// is no available protocol for routing, probes or listings.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

/// Serde default of [`ProviderEndpoint::enabled`]: rows written before the field load enabled.
const fn default_true() -> bool {
    true
}

/// Whether `value` is `true` (serde skips the default).
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_true(value: &bool) -> bool {
    *value
}

/// How an endpoint's typed address turns into the request URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointUrlMode {
    /// `base_url + path_prefix + protocol suffix` (the default).
    #[default]
    Standard,
    /// The user typed the complete request URL (a trailing `#` in other tools): `base_url +
    /// path_prefix` is used verbatim, nothing is appended. A Gemini `{model}` placeholder is
    /// still filled.
    AsTyped,
}

impl EndpointUrlMode {
    /// Whether this is [`EndpointUrlMode::Standard`] (serde skips the default).
    #[must_use]
    pub const fn is_standard(&self) -> bool {
        matches!(self, Self::Standard)
    }
}

/// The kind of gateway software (or vendor) behind a base URL, as the sniffer identifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayFamily {
    /// The model vendor's own API.
    Official,
    /// QuantumNous new-api.
    NewApi,
    /// one-api.
    OneApi,
    /// Veloera.
    Veloera,
    /// done-hub.
    DoneHub,
    /// rix-api.
    RixApi,
    /// sub2api.
    Sub2Api,
    /// VoAPI.
    VoApi,
    /// LiteLLM proxy.
    LiteLlm,
    /// OpenRouter.
    OpenRouter,
    /// CLIProxyAPI.
    CliProxyApi,
    /// Vercel AI Gateway.
    VercelAiGateway,
    /// Ollama.
    Ollama,
    /// LM Studio.
    LmStudio,
    /// vLLM.
    Vllm,
    /// llama.cpp server.
    LlamaCpp,
    /// SGLang.
    Sglang,
    /// LocalAI.
    LocalAi,
    /// An OpenAI-compatible service that matched no known family.
    UnknownCompatible,
}

/// Builds the request URL for `protocol` against `base_url` + `path_prefix`.
///
/// `model` fills Gemini's `{model}` (a leading `models/` is dropped). `stream` selects Gemini's
/// `:streamGenerateContent?alt=sse`; other protocols stream by a body flag, not by URL.
///
/// # Errors
/// [`AppError::Validation`] when a Gemini URL is requested without a model.
pub fn endpoint_url(
    base_url: &str,
    path_prefix: &str,
    mode: EndpointUrlMode,
    protocol: Protocol,
    model: Option<&str>,
    stream: bool,
) -> Result<String, AppError> {
    let base = base_url.trim().trim_end_matches('/');
    let prefix = path_prefix.trim().trim_end_matches('/');
    let prefix = if prefix.is_empty() || prefix.starts_with('/') {
        prefix.to_owned()
    } else {
        format!("/{prefix}")
    };
    let gemini_model = || {
        model
            .map(|m| m.trim().trim_start_matches("models/"))
            .filter(|m| !m.is_empty())
            .ok_or_else(|| AppError::validation("a Gemini request needs a model id"))
    };
    if mode == EndpointUrlMode::AsTyped {
        let typed = format!("{base}{prefix}");
        return Ok(
            if protocol == Protocol::Gemini && typed.contains("{model}") {
                typed.replace("{model}", gemini_model()?)
            } else {
                typed
            },
        );
    }
    let suffix = match protocol {
        Protocol::Gemini => {
            let model = gemini_model()?;
            let action = if stream {
                "streamGenerateContent?alt=sse"
            } else {
                "generateContent"
            };
            format!("/models/{model}:{action}")
        }
        other => other.suffix().to_owned(),
    };
    Ok(format!("{base}{prefix}{suffix}"))
}

/// URL of Gemini's single-text embedding method for `model`.
///
/// # Errors
/// [`AppError::Validation`] when `model` is empty.
pub fn gemini_embed_url(
    base_url: &str,
    path_prefix: &str,
    model: &str,
) -> Result<String, AppError> {
    let url = endpoint_url(
        base_url,
        path_prefix,
        EndpointUrlMode::Standard,
        Protocol::Gemini,
        Some(model),
        false,
    )?;
    Ok(url.replace(":generateContent", ":embedContent"))
}

/// URL of a provider's model-list endpoint (`models_endpoint` is relative to `base_url`).
#[must_use]
pub fn models_url(base_url: &str, models_endpoint: &str) -> String {
    let endpoint = models_endpoint.trim();
    let separator = if endpoint.starts_with('/') { "" } else { "/" };
    format!(
        "{}{separator}{endpoint}",
        base_url.trim().trim_end_matches('/')
    )
}

/// Request headers for one upstream call: authentication, version pins and the provider's extras.
///
/// `style` is the resolved style (endpoint override, else provider, else the protocol default).
#[must_use]
pub fn request_headers(
    protocol: Protocol,
    style: AuthStyle,
    secret: &str,
    quirks: &Quirks,
) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    let secret = secret.trim();
    if !secret.is_empty() {
        match style {
            AuthStyle::Bearer => {
                headers.push(("authorization".to_owned(), format!("Bearer {secret}")));
                if protocol == Protocol::AnthropicMessages {
                    headers.push(("x-api-key".to_owned(), secret.to_owned()));
                }
            }
            AuthStyle::XApiKey => headers.push(("x-api-key".to_owned(), secret.to_owned())),
            AuthStyle::XGoogApiKey => {
                headers.push(("x-goog-api-key".to_owned(), secret.to_owned()));
            }
            AuthStyle::RawAuthorization => {
                headers.push(("authorization".to_owned(), secret.to_owned()));
            }
            AuthStyle::None => {}
        }
    }
    if protocol == Protocol::AnthropicMessages {
        let version = quirks
            .anthropic_version
            .as_deref()
            .unwrap_or(DEFAULT_ANTHROPIC_VERSION);
        headers.push(("anthropic-version".to_owned(), version.to_owned()));
    }
    for (name, value) in &quirks.extra_headers {
        headers.push((name.to_ascii_lowercase(), value.clone()));
    }
    headers
}

/// Rewrites the model of a same-protocol request body in place (Gemini carries it in the URL).
pub fn rewrite_model(protocol: Protocol, body: &mut Value, model: &str) {
    if protocol == Protocol::Gemini {
        return;
    }
    if let Some(object) = body.as_object_mut() {
        object.insert("model".to_owned(), Value::String(model.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quirk_flags_keep_an_explicit_off_and_follow_the_preset_when_unset() {
        // A stored `false` is the user's Off; a missing flag is Auto and is not written.
        let stored: Quirks = serde_json::from_str(r#"{"drop_sampling_params":false}"#).unwrap();
        let preset = Quirks {
            drop_sampling_params: Some(true),
            echo_reasoning_in_tool_turns: Some(true),
            ..Quirks::default()
        };
        let effective = stored.clone().or_preset(&preset);
        assert!(!effective.drops_sampling());
        assert!(effective.echoes_reasoning());
        assert_eq!(
            serde_json::to_string(&stored).unwrap(),
            r#"{"drop_sampling_params":false}"#
        );
    }

    #[test]
    fn urls_are_base_plus_prefix_plus_suffix() {
        let url = |base, prefix, protocol, model, stream| {
            endpoint_url(
                base,
                prefix,
                EndpointUrlMode::Standard,
                protocol,
                model,
                stream,
            )
            .unwrap()
        };
        assert_eq!(
            url(
                "https://api.openai.com/",
                "/v1",
                Protocol::OpenaiChat,
                None,
                false
            ),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            url(
                "https://api.deepseek.com",
                "",
                Protocol::OpenaiResponses,
                None,
                false
            ),
            "https://api.deepseek.com/responses"
        );
        assert_eq!(
            url(
                "https://api.anthropic.com",
                "/v1",
                Protocol::AnthropicMessages,
                None,
                true
            ),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            url(
                "https://qianfan.baidubce.com/v2",
                "",
                Protocol::Embeddings,
                None,
                false
            ),
            "https://qianfan.baidubce.com/v2/embeddings"
        );
        assert_eq!(
            url(
                "https://api.typesafe.ai",
                "v1",
                Protocol::DecisionSystemone,
                None,
                false
            ),
            "https://api.typesafe.ai/v1/systemone"
        );
    }

    #[test]
    fn gemini_urls_carry_model_and_action() {
        let base = "https://generativelanguage.googleapis.com";
        let standard = EndpointUrlMode::Standard;
        assert_eq!(
            endpoint_url(
                base,
                "/v1beta",
                standard,
                Protocol::Gemini,
                Some("models/gemini-x"),
                false
            )
            .unwrap(),
            format!("{base}/v1beta/models/gemini-x:generateContent")
        );
        assert_eq!(
            endpoint_url(
                base,
                "/v1beta",
                standard,
                Protocol::Gemini,
                Some("gemini-x"),
                true
            )
            .unwrap(),
            format!("{base}/v1beta/models/gemini-x:streamGenerateContent?alt=sse")
        );
        assert_eq!(
            gemini_embed_url(base, "/v1beta", "gemini-embedding-2").unwrap(),
            format!("{base}/v1beta/models/gemini-embedding-2:embedContent")
        );
        assert!(endpoint_url(base, "/v1beta", standard, Protocol::Gemini, None, false).is_err());
    }

    #[test]
    fn an_as_typed_endpoint_is_used_verbatim() {
        let typed = |base, protocol, model| {
            endpoint_url(base, "", EndpointUrlMode::AsTyped, protocol, model, true).unwrap()
        };
        assert_eq!(
            typed("https://gw.example/custom/chat", Protocol::OpenaiChat, None),
            "https://gw.example/custom/chat"
        );
        assert_eq!(
            typed(
                "https://gw.example/v1beta/models/{model}:gen",
                Protocol::Gemini,
                Some("g-1")
            ),
            "https://gw.example/v1beta/models/g-1:gen"
        );
        assert!(
            endpoint_url(
                "https://gw/{model}",
                "",
                EndpointUrlMode::AsTyped,
                Protocol::Gemini,
                None,
                false
            )
            .is_err()
        );
    }

    #[test]
    fn raw_authorization_sends_the_key_without_a_scheme() {
        let headers = request_headers(
            Protocol::OpenaiChat,
            AuthStyle::RawAuthorization,
            " k-1 ",
            &Quirks::default(),
        );
        assert_eq!(headers, [("authorization".to_owned(), "k-1".to_owned())]);
    }

    #[test]
    fn models_url_is_relative_to_the_base() {
        assert_eq!(
            models_url("https://api.groq.com/", "/openai/v1/models"),
            "https://api.groq.com/openai/v1/models"
        );
    }

    #[test]
    fn auth_headers_follow_the_style_and_protocol() {
        let quirks = Quirks::default();
        let headers = request_headers(Protocol::OpenaiChat, AuthStyle::Bearer, " k ", &quirks);
        assert_eq!(
            headers,
            vec![("authorization".to_owned(), "Bearer k".to_owned())]
        );

        let headers = request_headers(Protocol::AnthropicMessages, AuthStyle::Bearer, "k", &quirks);
        assert!(headers.contains(&("x-api-key".to_owned(), "k".to_owned())));
        assert!(headers.contains(&("authorization".to_owned(), "Bearer k".to_owned())));
        assert!(headers.contains(&(
            "anthropic-version".to_owned(),
            DEFAULT_ANTHROPIC_VERSION.to_owned()
        )));

        let headers = request_headers(
            Protocol::AnthropicMessages,
            AuthStyle::XApiKey,
            "k",
            &quirks,
        );
        assert!(!headers.iter().any(|(name, _)| name == "authorization"));

        let mut pinned = Quirks {
            anthropic_version: Some("2026-10-21".into()),
            ..Quirks::default()
        };
        pinned.extra_headers.insert("X-Beta".into(), "1".into());
        let headers = request_headers(
            Protocol::AnthropicMessages,
            AuthStyle::XApiKey,
            "k",
            &pinned,
        );
        assert!(headers.contains(&("anthropic-version".to_owned(), "2026-10-21".to_owned())));
        assert!(headers.contains(&("x-beta".to_owned(), "1".to_owned())));

        assert!(request_headers(Protocol::OpenaiChat, AuthStyle::None, "k", &quirks).is_empty());
        assert!(request_headers(Protocol::OpenaiChat, AuthStyle::Bearer, "  ", &quirks).is_empty());
        assert_eq!(
            request_headers(Protocol::Gemini, AuthStyle::XGoogApiKey, "g", &quirks),
            vec![("x-goog-api-key".to_owned(), "g".to_owned())]
        );
    }

    #[test]
    fn protocol_identifiers_round_trip() {
        for protocol in Protocol::ALL {
            assert_eq!(Protocol::parse(protocol.as_str()), Some(protocol));
            let json = serde_json::to_string(&protocol).unwrap();
            assert_eq!(json, format!("\"{}\"", protocol.as_str()));
        }
        assert_eq!(Protocol::parse("nope"), None);
    }

    #[test]
    fn rewrite_model_skips_gemini() {
        let mut body = serde_json::json!({"model": "a"});
        rewrite_model(Protocol::OpenaiChat, &mut body, "b");
        assert_eq!(body["model"], "b");
        let mut body = serde_json::json!({"contents": []});
        rewrite_model(Protocol::Gemini, &mut body, "b");
        assert!(body.get("model").is_none());
    }

    #[test]
    fn old_quirks_json_loads_with_the_new_flags_on_auto() {
        let old: Quirks = serde_json::from_str(
            r#"{"reasoning_request":"thinking_type","drop_sampling_params":true,"extra_headers":{"X":"{{secret:a}}"}}"#,
        )
        .unwrap();
        assert_eq!(old.developer_role_as_system, None);
        assert_eq!(old.stream_usage, None);
        assert_eq!(old.max_tokens_field, None);
        assert_eq!(old.reasoning_request, Some(ReasoningStyle::ThinkingType));
        let empty: Quirks = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Quirks::default());
    }

    #[test]
    fn auto_quirks_serialize_nothing_and_set_ones_use_snake_case() {
        assert_eq!(serde_json::to_string(&Quirks::default()).unwrap(), "{}");
        let set = Quirks {
            developer_role_as_system: Some(true),
            stream_usage: Some(false),
            max_tokens_field: Some(MaxTokensField::MaxCompletionTokens),
            reasoning_request: Some(ReasoningStyle::None),
            ..Quirks::default()
        };
        let json = serde_json::to_value(&set).unwrap();
        assert_eq!(json["developer_role_as_system"], true);
        assert_eq!(json["stream_usage"], false);
        assert_eq!(json["max_tokens_field"], "max_completion_tokens");
        assert_eq!(json["reasoning_request"], "none");
        assert_eq!(serde_json::from_value::<Quirks>(json).unwrap(), set);
    }

    #[test]
    fn max_tokens_field_identifiers() {
        assert_eq!(
            serde_json::to_string(&MaxTokensField::MaxTokens).unwrap(),
            "\"max_tokens\""
        );
        assert_eq!(
            serde_json::from_str::<MaxTokensField>("\"max_completion_tokens\"").unwrap(),
            MaxTokensField::MaxCompletionTokens
        );
        assert!(serde_json::from_str::<MaxTokensField>("\"auto\"").is_err());
    }

    #[test]
    fn an_endpoint_without_enabled_loads_enabled_and_a_disabled_one_round_trips() {
        let old: ProviderEndpoint =
            serde_json::from_str(r#"{"protocol":"openai_chat","path_prefix":"/v1"}"#).unwrap();
        assert!(old.enabled);
        assert_eq!(
            serde_json::to_value(&old).unwrap(),
            serde_json::json!({"protocol": "openai_chat", "path_prefix": "/v1"}),
            "enabled endpoints keep the old JSON shape"
        );
        let off = ProviderEndpoint {
            enabled: false,
            ..old
        };
        let json = serde_json::to_value(&off).unwrap();
        assert_eq!(json["enabled"], false);
        assert_eq!(
            serde_json::from_value::<ProviderEndpoint>(json).unwrap(),
            off
        );
    }

    #[test]
    fn openai_reasoning_models_with_tools_choose_responses() {
        let official = Quirks {
            reasoning_request: Some(ReasoningStyle::ReasoningEffort),
            ..Quirks::default()
        };
        for model in ["gpt-5.1", "openai/gpt-5-mini", "o3", "o4-mini"] {
            assert!(official.tools_via_responses_for(model), "{model}");
        }
        for model in ["gpt-4o", "o3x", "omni"] {
            assert!(!official.tools_via_responses_for(model), "{model}");
        }
        // Another provider's gpt-5 alias is left alone; the setting wins either way.
        assert!(!Quirks::default().tools_via_responses_for("gpt-5"));
        let off = Quirks {
            tools_via_responses: Some(false),
            ..official.clone()
        };
        assert!(!off.tools_via_responses_for("gpt-5"));
    }
}
