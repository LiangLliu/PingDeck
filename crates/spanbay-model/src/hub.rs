//! The hub format: one protocol-neutral model of a chat exchange.
//!
//! Single responsibility: define the types every protocol adapter converts to and from. The shape
//! follows OpenAI Chat Completions semantics, widened where Anthropic, Gemini or the Responses API
//! carry more (reasoning blocks with signatures, tool results with error flags, cache usage).
//! Conversion goes `inbound protocol -> hub -> outbound protocol`, so adding a protocol costs one
//! adapter instead of one per pair.
//!
//! Conventions every adapter must keep:
//! - `HubUsage::input_tokens` counts the whole prompt, cached tokens included (OpenAI semantics).
//!   Anthropic reports cached and cache-write tokens next to `input_tokens`, so its adapter adds
//!   them in on decode and subtracts them on encode.
//! - Tool-call arguments are JSON *text* (`HubPart::ToolCall::arguments`). A protocol that carries
//!   an object parses it on encode; text that is not valid JSON becomes `{"_raw": "<text>"}`.
//! - A tool result lives in a message with [`HubRole::Tool`]; adapters regroup it into the
//!   protocol's own layout (a `tool` message, a `tool_result` block, a `functionResponse` part).
//! - All system and developer instructions fold into [`HubRequest::system`].
//! - **Signature provenance.** Signatures, encrypted payloads and item ids are only valid for the
//!   protocol that produced them. Every part that carries one also carries `origin`; a codec sends
//!   such data upstream only when [`signature_usable`] says the origin is its own protocol, and
//!   drops it otherwise (Gemini then substitutes its documented bypass signature on function
//!   calls). Decoders stamp their own protocol; the stream form is the [`HubEvent::Origin`] event.
//!   When a codec hands a signature to a *client*, [`wrap_signature`] tags foreign ones so the
//!   origin survives the client's round trip ([`unwrap_signature`] reads it back).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capabilities::ModelCapabilities;
use crate::wire::Protocol;

/// Who authored a [`HubMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubRole {
    /// The human (or the calling program).
    User,
    /// The model.
    Assistant,
    /// Results of tool calls the assistant made.
    Tool,
}

/// An image attached to a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HubImage {
    /// A remote image the provider fetches itself.
    Url {
        /// Absolute `http(s)` URL.
        url: String,
    },
    /// Inline bytes (a `data:` URL is decoded into this form).
    Base64 {
        /// MIME type such as `image/png`.
        media_type: String,
        /// Standard base64 without the `data:` prefix.
        data: String,
    },
    /// A content-addressed attachment in the agent's `AttachmentStore`. It has no wire form: the
    /// caller resolves it to [`HubImage::Base64`] before a request reaches a codec, and every
    /// codec rejects one that is still unresolved.
    AttachmentRef {
        /// Attachment id in the store.
        attachment_id: String,
        /// MIME type such as `image/png`.
        media_type: String,
    },
}

/// A document (PDF, text or code file) attached to a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubDocument {
    /// MIME type: `application/pdf`, `text/plain`, `text/markdown`, `text/csv`, …
    pub media_type: String,
    /// File name shown to the model when the protocol carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Where the content comes from.
    pub source: HubDocumentSource,
}

/// The content of a [`HubDocument`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HubDocumentSource {
    /// Inline bytes, standard base64.
    Base64 {
        /// The encoded bytes.
        data: String,
    },
    /// Already-extracted text (text-like media types).
    Text {
        /// The document text.
        text: String,
    },
    /// A remote document the provider fetches itself.
    Url {
        /// Absolute `http(s)` URL.
        url: String,
    },
    /// A content-addressed attachment; resolved to `Base64` or `Text` before a codec sees it.
    AttachmentRef {
        /// Attachment id in the store.
        attachment_id: String,
    },
}

/// A source a model cited in its answer (web search hit, grounding chunk, document span).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubCitation {
    /// Absolute source URL.
    pub url: String,
    /// Source title.
    pub title: String,
    /// Cited excerpt when the provider returns one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Start of the cited span in the answer text (UTF-16 offset as the provider reports it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_index: Option<u32>,
    /// End of the cited span in the answer text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_index: Option<u32>,
}

/// A tool the provider itself executes (the model searches the web and answers with sources).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubNativeTool {
    /// Provider-side web search: Anthropic `web_search_20250305`, OpenAI Responses `web_search`,
    /// OpenAI chat `web_search_options`, Gemini `google_search`.
    WebSearch {
        /// Most searches one answer may run (Anthropic `max_uses`); `None` leaves the default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_uses: Option<u32>,
        /// Restrict results to these domains (empty: any).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allowed_domains: Vec<String>,
        /// Never return results from these domains.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blocked_domains: Vec<String>,
    },
}

/// One piece of message content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubPart {
    /// Plain text.
    Text {
        /// The text.
        text: String,
        /// Opaque signature of this text part that must be echoed back (Gemini `thoughtSignature`
        /// on a non-thought text part). Only the protocol in `origin` understands it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// Protocol that produced `signature` (`None`: no signature, or stored before provenance
        /// existed; every codec then treats it as its own).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Protocol>,
        /// Sources the model cited for spans of this text.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<HubCitation>,
    },
    /// An image (user or tool-result content).
    Image {
        /// Where the bytes come from.
        image: HubImage,
    },
    /// A document: PDF, text or code file.
    Document {
        /// The document.
        document: HubDocument,
    },
    /// Spoken input audio.
    Audio {
        /// MIME type such as `audio/wav` or `audio/mpeg`.
        media_type: String,
        /// Standard base64 of the audio bytes.
        data: String,
    },
    /// A function call the assistant wants executed.
    ToolCall {
        /// Call id the matching [`HubPart::ToolResult`] references.
        id: String,
        /// Function name.
        name: String,
        /// Arguments as JSON text.
        arguments: String,
        /// Opaque provider signature that must be echoed back (Gemini `thoughtSignature`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// Protocol that produced `signature` (see [`HubPart::Text::origin`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Protocol>,
    },
    /// The outcome of a tool call.
    ToolResult {
        /// Id of the call this answers.
        call_id: String,
        /// Function name when known (Gemini requires it; OpenAI does not carry it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Result content: text, images and documents.
        content: Vec<HubPart>,
        /// Whether the tool failed.
        #[serde(default)]
        is_error: bool,
    },
    /// Model reasoning ("thinking") content.
    Reasoning {
        /// Readable reasoning or its summary; empty when only encrypted data exists.
        text: String,
        /// Provider signature authenticating the block (Anthropic `signature`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// Opaque encrypted or redacted payload (Anthropic `redacted_thinking`, OpenAI
        /// `encrypted_content`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted: Option<String>,
        /// Provider item id (OpenAI Responses reasoning item).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Protocol that produced `signature`, `encrypted` and `id`: only that protocol can verify
        /// them, so every other codec drops them (see [`signature_usable`]). `None`: nothing to
        /// verify, or stored before provenance existed (treated as the codec's own).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Protocol>,
    },
    /// A tool call the provider executed itself (see [`HubNativeTool`]): Anthropic
    /// `server_tool_use`, OpenAI `web_search_call`. Kept so a later turn can replay it to the same
    /// protocol; every other codec skips it, the answer text and citations carry the result.
    ServerToolUse {
        /// Provider call id.
        id: String,
        /// Tool name (`web_search`).
        name: String,
        /// The call's input as the provider reported it.
        input: Value,
        /// Protocol that produced this part (`None`: stored before provenance existed).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Protocol>,
    },
    /// What a provider-executed tool returned (Anthropic `web_search_tool_result`).
    ServerToolResult {
        /// Id of the [`HubPart::ServerToolUse`] this answers.
        call_id: String,
        /// Tool name.
        name: String,
        /// The provider's result payload, verbatim.
        payload: Value,
        /// Sources the result contains.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<HubCitation>,
        /// Protocol that produced this part (`None`: stored before provenance existed).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Protocol>,
    },
}

impl HubPart {
    /// A text part.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            signature: None,
            origin: None,
            citations: Vec::new(),
        }
    }
}

/// One turn of the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubMessage {
    /// Author of the turn.
    pub role: HubRole,
    /// Ordered content.
    pub content: Vec<HubPart>,
}

impl HubMessage {
    /// A user message with one text part.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: HubRole::User,
            content: vec![HubPart::text(text)],
        }
    }

    /// An assistant message with one text part.
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: HubRole::Assistant,
            content: vec![HubPart::text(text)],
        }
    }

    /// The concatenated text parts.
    #[must_use]
    pub fn text(&self) -> String {
        text_of(&self.content)
    }
}

/// A function the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HubTool {
    /// Function name.
    pub name: String,
    /// What the function does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON-schema of the arguments.
    pub parameters: Value,
    /// Strict schema adherence request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// How the model may use tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum HubToolChoice {
    /// The model decides.
    Auto,
    /// No tool may be called.
    None,
    /// Some tool must be called.
    Required,
    /// This function must be called.
    Tool {
        /// Function name.
        name: String,
    },
    /// One of these functions must be called.
    Allowed {
        /// Function names.
        names: Vec<String>,
    },
}

/// Requested output format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubResponseFormat {
    /// Free text (the default).
    Text,
    /// Any valid JSON object.
    JsonObject,
    /// JSON conforming to a schema.
    JsonSchema {
        /// Schema name.
        name: String,
        /// The JSON schema.
        schema: Value,
        /// Strict adherence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
}

/// Reasoning effort levels, ordered from least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubEffort {
    /// Smallest amount of thinking the provider offers.
    Minimal,
    /// Low effort.
    Low,
    /// Medium effort.
    Medium,
    /// High effort.
    High,
    /// Extra high (OpenAI and Claude `xhigh`); a model without this level gets `high`.
    #[serde(rename = "xhigh")]
    XHigh,
    /// Maximum effort (DeepSeek/Kimi `max`).
    Max,
}

/// Reasoning request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubReasoning {
    /// Explicit on/off for providers with a toggle; `None` leaves the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Effort level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<HubEffort>,
    /// Token budget for thinking (Anthropic `budget_tokens`, Gemini `thinkingBudget`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}

/// A protocol-neutral chat request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HubRequest {
    /// Model id (client-facing or upstream, depending on the stage).
    pub model: String,
    /// Folded system and developer instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The conversation.
    pub messages: Vec<HubMessage>,
    /// Callable functions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<HubTool>,
    /// Provider-executed tools (web search). Functions the caller executes stay in `tools`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_tools: Vec<HubNativeTool>,
    /// Tool-use policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<HubToolChoice>,
    /// Whether several tool calls may be emitted in one turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// Output token cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Top-k sampling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Stop sequences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    /// Sampling seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// Presence penalty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    /// Frequency penalty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    /// Whether the caller wants a stream.
    #[serde(default)]
    pub stream: bool,
    /// Output format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<HubResponseFormat>,
    /// Reasoning request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<HubReasoning>,
    /// End-user identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Prompt-cache hints; only in-app calls set them (the gateway forwards a client's own
    /// markers untouched).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheHint>,
}

/// Where a prompt cache may end and under which key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheHint {
    /// Cache breakpoints, in request order (Anthropic `cache_control`).
    pub breakpoints: Vec<CacheBreakpoint>,
    /// Routing key that keeps one conversation on one cache (OpenAI `prompt_cache_key`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// One place a cached prefix may end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheBreakpoint {
    /// After the system prompt and the tool definitions.
    SystemAndTools,
    /// After the last message (rolls forward every step).
    LastMessage,
}

impl HubRequest {
    /// A request for `model` with one user message.
    #[must_use]
    pub fn user_prompt(model: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: vec![HubMessage::user(prompt)],
            ..Self::default()
        }
    }

    /// Whether the conversation ends with an assistant turn (a "prefill": the model is asked to
    /// continue it). Only some protocols accept that; the others reject it with a clear error.
    #[must_use]
    pub fn ends_with_assistant(&self) -> bool {
        self.messages
            .last()
            .is_some_and(|message| message.role == HubRole::Assistant)
    }

    /// The tools for a protocol with no allow-list: when the choice is [`HubToolChoice::Allowed`],
    /// only the allowed ones (sent with `required`), otherwise all.
    pub fn tools_for_forced_choice(&self) -> impl Iterator<Item = &HubTool> {
        self.tools.iter().filter(|tool| match &self.tool_choice {
            Some(HubToolChoice::Allowed { names }) => names.contains(&tool.name),
            _ => true,
        })
    }
}

/// Why generation stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum HubFinish {
    /// Natural end or a stop sequence.
    Stop,
    /// The output token cap was hit.
    Length,
    /// The model asked for tool calls.
    ToolCalls,
    /// The provider's safety system cut the output.
    ContentFilter,
    /// The answer is not finished and the caller should continue it (Anthropic `pause_turn`,
    /// Gemini `CONTINUATION`): never a normal end.
    Pause,
    /// Any other provider-specific reason, verbatim.
    Other(String),
}

/// Token accounting. `None` means the provider did not report the figure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubUsage {
    /// Prompt tokens, cached tokens included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Generated tokens, reasoning included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Prompt tokens served from the provider's cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    /// Prompt tokens written to the provider's cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// The part of `cache_write_tokens` written at the one-hour tier (billed higher).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_long_tokens: Option<u64>,
    /// Tokens spent on reasoning (a subset of `output_tokens`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

impl HubUsage {
    /// Overlays every figure `newer` reports onto `self`.
    pub fn merge(&mut self, newer: &HubUsage) {
        self.input_tokens = newer.input_tokens.or(self.input_tokens);
        self.output_tokens = newer.output_tokens.or(self.output_tokens);
        self.cached_input_tokens = newer.cached_input_tokens.or(self.cached_input_tokens);
        self.cache_write_tokens = newer.cache_write_tokens.or(self.cache_write_tokens);
        self.cache_write_long_tokens = newer
            .cache_write_long_tokens
            .or(self.cache_write_long_tokens);
        self.reasoning_tokens = newer.reasoning_tokens.or(self.reasoning_tokens);
    }

    /// `input + output` when both are known.
    #[must_use]
    pub fn total_tokens(&self) -> Option<u64> {
        Some(self.input_tokens?.saturating_add(self.output_tokens?))
    }
}

/// A complete (non-streamed) model answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubResponse {
    /// Provider response id.
    pub id: String,
    /// Model that answered.
    pub model: String,
    /// Assistant content: text, reasoning and tool calls in order.
    pub content: Vec<HubPart>,
    /// Why generation stopped.
    pub finish: HubFinish,
    /// Token accounting.
    pub usage: HubUsage,
}

impl HubResponse {
    /// The concatenated text parts.
    #[must_use]
    pub fn text(&self) -> String {
        text_of(&self.content)
    }

    /// The tool calls as `(id, name, arguments)`.
    #[must_use]
    pub fn tool_calls(&self) -> Vec<(&str, &str, &str)> {
        self.content
            .iter()
            .filter_map(|part| match part {
                HubPart::ToolCall {
                    id,
                    name,
                    arguments,
                    ..
                } => Some((id.as_str(), name.as_str(), arguments.as_str())),
                _ => None,
            })
            .collect()
    }

    /// The assistant message to append to the conversation before sending tool results.
    #[must_use]
    pub fn into_message(self) -> HubMessage {
        HubMessage {
            role: HubRole::Assistant,
            content: self.content,
        }
    }
}

/// One attempt that did not serve, as the in-app client reports it with [`HubEvent::Served`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubAttempt {
    /// Provider id.
    pub provider_id: String,
    /// Upstream model id.
    pub model: String,
    /// The key used, when one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// The failure category (`FailureKind` name), when the attempt ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// HTTP status, when the upstream answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// What came next: `next_key`, `next_target`, `same_target_retry` or `skipped`.
    pub action: String,
    /// One redacted line about why.
    pub why: String,
}

/// One event of a streamed answer, in the order the model produced it.
///
/// A well-formed stream is `Start`, optionally `Origin`, any number of content events (with `Usage`
/// anywhere), `Finish`. `Error` ends the stream early.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubEvent {
    /// The answer begins.
    Start {
        /// Provider response id.
        id: String,
        /// Model that answers.
        model: String,
    },
    /// Names the protocol whose signatures the rest of the stream carries. Decoders emit it right
    /// after `Start`; [`collect_events`] stamps it on every part that gets a signature, an
    /// encrypted payload or an item id. Encoders ignore it.
    Origin {
        /// The protocol that produced the stream.
        protocol: Protocol,
    },
    /// The provider model the in-app client committed to, emitted once before the first content
    /// event (a fallback may differ from the requested model). Decoders never produce it;
    /// encoders and [`collect_events`] ignore it.
    Served {
        /// Provider id.
        provider_id: String,
        /// Upstream model id.
        model: String,
        /// The attempts that failed or were skipped before this target served, in order (a
        /// fallback took over when it is not empty).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attempts: Vec<HubAttempt>,
    },
    /// A source the model cited for the text streamed so far (it attaches to the current text
    /// part).
    Citation {
        /// The cited source.
        citation: HubCitation,
    },
    /// The provider started executing one of its own tools.
    ServerToolStart {
        /// Provider call id.
        id: String,
        /// Tool name.
        name: String,
        /// The call's input.
        input: Value,
    },
    /// A provider-executed tool returned.
    ServerToolResult {
        /// Id of the call this answers.
        call_id: String,
        /// Tool name.
        name: String,
        /// The provider's result payload, verbatim.
        payload: Value,
        /// Sources the result contains.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<HubCitation>,
    },
    /// A fragment of answer text.
    TextDelta {
        /// The fragment.
        text: String,
    },
    /// The signature of the text part being streamed (Gemini `thoughtSignature` on a text part; it
    /// may arrive on an otherwise empty text part).
    TextSignature {
        /// Opaque provider signature.
        signature: String,
    },
    /// A fragment of reasoning text.
    ReasoningDelta {
        /// The fragment.
        text: String,
    },
    /// The signature of the reasoning block being streamed.
    ReasoningSignature {
        /// Provider signature.
        signature: String,
    },
    /// Provider identity of the reasoning block just streamed (OpenAI Responses reasoning item id
    /// and encrypted content), needed to replay the block in a later tool turn.
    ReasoningItem {
        /// Provider item id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Opaque encrypted payload.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted: Option<String>,
    },
    /// A redacted reasoning block (Anthropic `redacted_thinking`): an opaque payload with no
    /// readable text. It is its own reasoning part, never merged into the neighbouring one.
    RedactedReasoning {
        /// The opaque payload, to be sent back verbatim.
        data: String,
    },
    /// A tool call begins; later deltas carry the same `index`.
    ToolCallStart {
        /// Position among the tool calls of this answer, starting at 0.
        index: u32,
        /// Call id.
        id: String,
        /// Function name.
        name: String,
        /// Echo-back signature (Gemini `thoughtSignature`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// A fragment of the JSON text of a tool call's arguments.
    ToolCallDelta {
        /// Tool call position.
        index: u32,
        /// The fragment.
        arguments: String,
    },
    /// Token accounting; later events override earlier ones figure by figure.
    Usage {
        /// The figures reported so far.
        usage: HubUsage,
    },
    /// Generation stopped.
    Finish {
        /// Why.
        reason: HubFinish,
    },
    /// The provider reported a failure inside the stream.
    Error {
        /// Human-readable message.
        message: String,
        /// Provider error type or code when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
}

/// The most tool-call argument text a stream may accumulate for one call.
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 4 * 1024 * 1024;
/// The most reasoning text a stream may accumulate for one block.
pub const MAX_REASONING_BYTES: usize = 4 * 1024 * 1024;
/// The most answer text a response stream encoder accumulates for one text item.
pub const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;

/// The error message for a stream whose `what` outgrew `cap` bytes.
#[must_use]
pub fn too_large(what: &str, cap: usize) -> String {
    format!(
        "the upstream stream's {what} exceed the {} MiB limit",
        cap / (1024 * 1024)
    )
}

/// Folds a finished event sequence back into a [`HubResponse`].
///
/// Used by `complete_chat` on streamed upstreams and by tests as the round-trip oracle. Adjacent
/// text deltas merge into one text part, reasoning deltas into one reasoning part; tool-call
/// fragments assemble by index. An `Error` event becomes the returned error message. Parts that
/// receive a signature, encrypted payload or item id are stamped with the stream's
/// [`HubEvent::Origin`].
///
/// # Errors
/// Returns the provider's message when the stream carried an `Error` event, or a size message
/// when answer text, one tool call's arguments or one reasoning block outgrew its limit.
pub fn collect_events(events: &[HubEvent]) -> Result<HubResponse, String> {
    let mut id = String::new();
    let mut model = String::new();
    let mut content: Vec<HubPart> = Vec::new();
    let mut finish = HubFinish::Stop;
    let mut usage = HubUsage::default();
    // Index of the tool-call part for each stream index.
    let mut calls: Vec<(u32, usize)> = Vec::new();
    // Protocol whose signatures the stream carries (`Origin` event).
    let mut stream_origin: Option<Protocol> = None;
    for event in events {
        match event {
            HubEvent::Start {
                id: new_id,
                model: new_model,
            } => {
                id.clone_from(new_id);
                model.clone_from(new_model);
            }
            HubEvent::Origin { protocol } => stream_origin = Some(*protocol),
            HubEvent::Served { .. } => {}
            // A signed text part is closed: Gemini says never to merge it with unsigned text.
            HubEvent::TextDelta { text } => match content.last_mut() {
                Some(HubPart::Text {
                    text: current,
                    signature: None,
                    ..
                }) => {
                    if current.len() + text.len() > MAX_TEXT_BYTES {
                        return Err(too_large("answer text", MAX_TEXT_BYTES));
                    }
                    current.push_str(text);
                }
                _ => {
                    if text.len() > MAX_TEXT_BYTES {
                        return Err(too_large("answer text", MAX_TEXT_BYTES));
                    }
                    content.push(HubPart::text(text.clone()));
                }
            },
            HubEvent::TextSignature { signature } => match content.last_mut() {
                Some(HubPart::Text {
                    signature: slot,
                    origin,
                    ..
                }) => {
                    *slot = Some(signature.clone());
                    *origin = stream_origin;
                }
                _ => content.push(HubPart::Text {
                    text: String::new(),
                    signature: Some(signature.clone()),
                    origin: stream_origin,
                    citations: Vec::new(),
                }),
            },
            // Only an unfinished block takes more text: once a block has its signature, item id or
            // encrypted payload it is closed and the next delta starts a new block.
            HubEvent::ReasoningDelta { text } => match content.last_mut() {
                Some(part @ HubPart::Reasoning { .. }) if is_open_reasoning(part) => {
                    if let HubPart::Reasoning { text: current, .. } = part {
                        if current.len() + text.len() > MAX_REASONING_BYTES {
                            return Err(too_large("reasoning text", MAX_REASONING_BYTES));
                        }
                        current.push_str(text);
                    }
                }
                _ => content.push(HubPart::Reasoning {
                    text: text.clone(),
                    signature: None,
                    encrypted: None,
                    id: None,
                    origin: None,
                }),
            },
            HubEvent::ReasoningSignature { signature } => {
                if let Some(part @ HubPart::Reasoning { .. }) = content.last_mut()
                    && is_open_reasoning(part)
                {
                    if let HubPart::Reasoning {
                        signature: slot,
                        origin,
                        ..
                    } = part
                    {
                        *slot = Some(signature.clone());
                        *origin = stream_origin;
                    }
                } else {
                    content.push(HubPart::Reasoning {
                        text: String::new(),
                        signature: Some(signature.clone()),
                        encrypted: None,
                        id: None,
                        origin: stream_origin,
                    });
                }
            }
            HubEvent::RedactedReasoning { data } => content.push(HubPart::Reasoning {
                text: String::new(),
                signature: None,
                encrypted: Some(data.clone()),
                id: None,
                origin: stream_origin,
            }),
            HubEvent::ReasoningItem {
                id: item_id,
                encrypted: item_encrypted,
            } => {
                if item_id.is_none() && item_encrypted.is_none() {
                    // Nothing to attach: never creates an empty part or erases anything.
                } else if let Some(
                    part @ HubPart::Reasoning {
                        signature: None, ..
                    },
                ) = content.last_mut()
                    && is_open_reasoning(part)
                    && let HubPart::Reasoning {
                        id,
                        encrypted,
                        origin,
                        ..
                    } = part
                {
                    // The item describes the block just streamed; a field it does not carry
                    // never erases one the block already has.
                    if item_id.is_some() {
                        id.clone_from(item_id);
                    }
                    if item_encrypted.is_some() {
                        encrypted.clone_from(item_encrypted);
                    }
                    *origin = stream_origin;
                } else {
                    content.push(HubPart::Reasoning {
                        text: String::new(),
                        signature: None,
                        encrypted: item_encrypted.clone(),
                        id: item_id.clone(),
                        origin: stream_origin,
                    });
                }
            }
            HubEvent::ToolCallStart {
                index,
                id,
                name,
                signature,
            } => {
                calls.push((*index, content.len()));
                content.push(HubPart::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                    signature: signature.clone(),
                    origin: signature.as_ref().and(stream_origin),
                });
            }
            HubEvent::ToolCallDelta { index, arguments } => {
                let slot = calls
                    .iter()
                    .rev()
                    .find(|(call_index, _)| call_index == index)
                    .map(|(_, position)| *position);
                if let Some(HubPart::ToolCall {
                    arguments: current, ..
                }) = slot.and_then(|position| content.get_mut(position))
                {
                    if current.len() + arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                        return Err(too_large("tool-call arguments", MAX_TOOL_ARGUMENT_BYTES));
                    }
                    current.push_str(arguments);
                }
            }
            HubEvent::Citation { citation } => match content.last_mut() {
                Some(HubPart::Text { citations, .. }) => citations.push(citation.clone()),
                _ => content.push(HubPart::Text {
                    text: String::new(),
                    signature: None,
                    origin: None,
                    citations: vec![citation.clone()],
                }),
            },
            HubEvent::ServerToolStart { id, name, input } => content.push(HubPart::ServerToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                origin: stream_origin,
            }),
            HubEvent::ServerToolResult {
                call_id,
                name,
                payload,
                citations,
            } => content.push(HubPart::ServerToolResult {
                call_id: call_id.clone(),
                name: name.clone(),
                payload: payload.clone(),
                citations: citations.clone(),
                origin: stream_origin,
            }),
            HubEvent::Usage { usage: newer } => usage.merge(newer),
            HubEvent::Finish { reason } => finish = reason.clone(),
            HubEvent::Error { message, .. } => return Err(message.clone()),
        }
    }
    Ok(HubResponse {
        id,
        model,
        content,
        finish,
        usage,
    })
}

/// Whether a signature (or encrypted payload, or item id) produced by `origin` may be sent to a
/// `target` protocol. Provenance `None` is unknown or legacy data and is accepted, so messages
/// stored before provenance existed keep working.
#[must_use]
pub fn signature_usable(origin: Option<Protocol>, target: Protocol) -> bool {
    origin.is_none_or(|origin| origin == target)
}

/// Prefix of a signature that crosses a client protocol under a foreign name.
const SIGNATURE_TAG: &str = "pdk1.";

/// The text a protocol puts in its own signature field when it hands a signature to a *client*.
///
/// A signature the client protocol itself produced (or of unknown origin) goes out unchanged. A
/// foreign one (Gemini's thought signature shown to an Anthropic client, say) is wrapped as
/// `pdk1.<origin>.<signature>`, so when the client echoes it back [`unwrap_signature`] still knows
/// who produced it and the codecs of other protocols can drop it instead of causing a 400.
#[must_use]
pub fn wrap_signature(origin: Option<Protocol>, signature: &str, own: Protocol) -> String {
    match origin {
        Some(origin) if origin != own => format!("{SIGNATURE_TAG}{}.{signature}", origin.as_str()),
        _ => signature.to_owned(),
    }
}

/// Reads a signature a client echoed back: returns its producer and the signature itself. An
/// untagged value was produced by `own`, the protocol the client speaks.
#[must_use]
pub fn unwrap_signature(raw: &str, own: Protocol) -> (Protocol, String) {
    if let Some(rest) = raw.strip_prefix(SIGNATURE_TAG)
        && let Some((name, signature)) = rest.split_once('.')
        && let Some(origin) = Protocol::parse(name)
    {
        return (origin, signature.to_owned());
    }
    (own, raw.to_owned())
}

/// Whether `part` is a reasoning block that is still being streamed: no signature, item id or
/// encrypted payload yet (a finished or redacted block never takes more text or metadata).
fn is_open_reasoning(part: &HubPart) -> bool {
    matches!(
        part,
        HubPart::Reasoning {
            signature: None,
            encrypted: None,
            id: None,
            ..
        }
    )
}

/// Concatenates the text parts of `parts`.
#[must_use]
pub fn text_of(parts: &[HubPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            HubPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Removes the provider-native tools `caps` says the model cannot run (`docs/architecture.md` §8): today
/// that is web search when `caps.web_search` is not supported. The caller then offers its own
/// function tool instead and tells the user. Returns whether anything was removed.
pub fn lower_native_tools(req: &mut HubRequest, caps: &ModelCapabilities) -> bool {
    let before = req.native_tools.len();
    req.native_tools.retain(|tool| match tool {
        HubNativeTool::WebSearch { .. } => caps.web_search.supported,
    });
    req.native_tools.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowering_drops_native_web_search_only_when_unsupported() {
        use crate::capabilities::{Capability, CapabilitySource};
        let search = || HubNativeTool::WebSearch {
            max_uses: None,
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
        };
        let mut request = HubRequest::user_prompt("m", "hi");
        request.native_tools = vec![search()];
        let mut supported = ModelCapabilities::default();
        supported.apply(Capability::WebSearch, true, CapabilitySource::Declared);
        assert!(!lower_native_tools(&mut request, &supported));
        assert_eq!(request.native_tools.len(), 1);
        // Unknown counts as unsupported: the function tool is the safe fallback.
        assert!(lower_native_tools(
            &mut request,
            &ModelCapabilities::default()
        ));
        assert!(request.native_tools.is_empty());
        assert!(!lower_native_tools(
            &mut request,
            &ModelCapabilities::default()
        ));
    }

    #[test]
    fn collect_events_merges_deltas_and_assembles_tool_calls() {
        let events = vec![
            HubEvent::Start {
                id: "r1".into(),
                model: "m".into(),
            },
            HubEvent::ReasoningDelta {
                text: "think ".into(),
            },
            HubEvent::ReasoningDelta {
                text: "hard".into(),
            },
            HubEvent::ReasoningSignature {
                signature: "sig".into(),
            },
            HubEvent::TextDelta { text: "Hel".into() },
            HubEvent::TextDelta { text: "lo".into() },
            HubEvent::ToolCallStart {
                index: 0,
                id: "c1".into(),
                name: "f".into(),
                signature: None,
            },
            HubEvent::ToolCallDelta {
                index: 0,
                arguments: "{\"a\":".into(),
            },
            HubEvent::ToolCallDelta {
                index: 0,
                arguments: "1}".into(),
            },
            HubEvent::Usage {
                usage: HubUsage {
                    input_tokens: Some(3),
                    ..HubUsage::default()
                },
            },
            HubEvent::Usage {
                usage: HubUsage {
                    output_tokens: Some(4),
                    ..HubUsage::default()
                },
            },
            HubEvent::Finish {
                reason: HubFinish::ToolCalls,
            },
        ];
        let response = collect_events(&events).unwrap();
        assert_eq!(response.id, "r1");
        assert_eq!(response.text(), "Hello");
        assert_eq!(response.tool_calls(), vec![("c1", "f", "{\"a\":1}")]);
        assert_eq!(response.finish, HubFinish::ToolCalls);
        assert_eq!(response.usage.total_tokens(), Some(7));
        assert!(matches!(
            &response.content[0],
            HubPart::Reasoning { text, signature: Some(sig), origin: None, .. }
                if text == "think hard" && sig == "sig"
        ));
    }

    #[test]
    fn collect_events_surfaces_stream_errors() {
        let events = vec![HubEvent::Error {
            message: "boom".into(),
            kind: None,
        }];
        assert_eq!(collect_events(&events).unwrap_err(), "boom");
    }

    #[test]
    fn collect_events_caps_tool_arguments_and_reasoning() {
        let chunk = "x".repeat(MAX_TOOL_ARGUMENT_BYTES);
        let calls = vec![
            HubEvent::ToolCallStart {
                index: 0,
                id: "c".into(),
                name: "f".into(),
                signature: None,
            },
            HubEvent::ToolCallDelta {
                index: 0,
                arguments: chunk.clone(),
            },
            HubEvent::ToolCallDelta {
                index: 0,
                arguments: "y".into(),
            },
        ];
        assert!(
            collect_events(&calls)
                .unwrap_err()
                .contains("tool-call arguments")
        );
        assert!(collect_events(&calls[..2]).is_ok());
        let thoughts: Vec<HubEvent> = [chunk.as_str(), "y"]
            .iter()
            .map(|text| HubEvent::ReasoningDelta {
                text: (*text).into(),
            })
            .collect();
        assert!(
            collect_events(&thoughts)
                .unwrap_err()
                .contains("reasoning text")
        );
    }

    #[test]
    fn collect_events_caps_answer_text() {
        let text = |text: &str| HubEvent::TextDelta { text: text.into() };
        let full = "x".repeat(MAX_TEXT_BYTES);
        let events = [text(&full), text("y")];
        assert!(collect_events(&events).unwrap_err().contains("answer text"));
        assert!(collect_events(&events[..1]).is_ok());
        let one_big = [text(&format!("{full}y"))];
        assert!(
            collect_events(&one_big)
                .unwrap_err()
                .contains("answer text")
        );
    }

    #[test]
    fn a_thinking_block_after_a_redacted_one_stays_separate() {
        let events = vec![
            HubEvent::RedactedReasoning {
                data: "blob".into(),
            },
            HubEvent::ReasoningDelta {
                text: "visible".into(),
            },
            HubEvent::ReasoningSignature {
                signature: "sig".into(),
            },
        ];
        let response = collect_events(&events).unwrap();
        assert_eq!(response.content.len(), 2);
        assert!(matches!(
            &response.content[0],
            HubPart::Reasoning { text, encrypted: Some(data), signature: None, .. }
                if text.is_empty() && data == "blob"
        ));
        assert!(matches!(
            &response.content[1],
            HubPart::Reasoning { text, signature: Some(sig), encrypted: None, .. }
                if text == "visible" && sig == "sig"
        ));
    }

    #[test]
    fn the_stream_origin_is_stamped_on_every_signed_part() {
        let events = vec![
            HubEvent::Origin {
                protocol: Protocol::Gemini,
            },
            HubEvent::ReasoningDelta { text: "t".into() },
            HubEvent::ReasoningSignature {
                signature: "r".into(),
            },
            HubEvent::TextDelta { text: "hi".into() },
            HubEvent::TextSignature {
                signature: "ts".into(),
            },
            HubEvent::ToolCallStart {
                index: 0,
                id: "c".into(),
                name: "f".into(),
                signature: Some("cs".into()),
            },
            HubEvent::ToolCallStart {
                index: 1,
                id: "d".into(),
                name: "g".into(),
                signature: None,
            },
            HubEvent::RedactedReasoning { data: "x".into() },
        ];
        let content = collect_events(&events).unwrap().content;
        let gemini = Some(Protocol::Gemini);
        assert!(matches!(
            &content[0],
            HubPart::Reasoning { signature: Some(_), origin, .. } if *origin == gemini
        ));
        assert!(matches!(
            &content[1],
            HubPart::Text { text, signature: Some(sig), origin, .. } if text == "hi" && sig == "ts" && *origin == gemini
        ));
        assert!(matches!(
            &content[2],
            HubPart::ToolCall { signature: Some(_), origin, .. } if *origin == gemini
        ));
        assert!(matches!(
            &content[3],
            HubPart::ToolCall {
                signature: None,
                origin: None,
                ..
            }
        ));
        assert!(matches!(
            &content[4],
            HubPart::Reasoning { encrypted: Some(_), origin, .. } if *origin == gemini
        ));
        // Without an Origin event nothing is stamped (unknown provenance).
        let plain = collect_events(&events[1..3]).unwrap().content;
        assert!(matches!(&plain[0], HubPart::Reasoning { origin: None, .. }));
    }

    #[test]
    fn a_second_thinking_block_after_a_signature_is_its_own_part() {
        let events = vec![
            HubEvent::ReasoningDelta { text: "one".into() },
            HubEvent::ReasoningSignature {
                signature: "s1".into(),
            },
            HubEvent::ReasoningDelta { text: "two".into() },
            HubEvent::ReasoningSignature {
                signature: "s2".into(),
            },
            // A signature with no thinking text of its own is a block as well.
            HubEvent::ReasoningSignature {
                signature: "s3".into(),
            },
        ];
        let content = collect_events(&events).unwrap().content;
        assert_eq!(content.len(), 3);
        let seen: Vec<(&str, &str)> = content
            .iter()
            .filter_map(|part| match part {
                HubPart::Reasoning {
                    text,
                    signature: Some(sig),
                    ..
                } => Some((text.as_str(), sig.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(seen, vec![("one", "s1"), ("two", "s2"), ("", "s3")]);
    }

    #[test]
    fn two_responses_reasoning_items_stay_separate() {
        let item = |id: &str, encrypted: Option<&str>| HubEvent::ReasoningItem {
            id: Some(id.into()),
            encrypted: encrypted.map(str::to_owned),
        };
        let events = vec![
            HubEvent::Origin {
                protocol: Protocol::OpenaiResponses,
            },
            HubEvent::ReasoningDelta {
                text: "first".into(),
            },
            item("rs_1", Some("enc1")),
            HubEvent::ReasoningDelta {
                text: "second".into(),
            },
            item("rs_2", None),
            HubEvent::TextDelta { text: "hi".into() },
        ];
        let content = collect_events(&events).unwrap().content;
        let responses = Some(Protocol::OpenaiResponses);
        assert!(matches!(
            &content[0],
            HubPart::Reasoning { text, id: Some(id), encrypted: Some(enc), origin, .. }
                if text == "first" && id == "rs_1" && enc == "enc1" && *origin == responses
        ));
        assert!(matches!(
            &content[1],
            HubPart::Reasoning { text, id: Some(id), encrypted: None, .. }
                if text == "second" && id == "rs_2"
        ));
        assert_eq!(content.len(), 3);
    }

    #[test]
    fn an_item_never_overwrites_metadata_a_block_already_has() {
        // A redacted block followed by an item: the item is its own part, the blob survives.
        let events = vec![
            HubEvent::RedactedReasoning {
                data: "blob".into(),
            },
            HubEvent::ReasoningItem {
                id: Some("rs_1".into()),
                encrypted: None,
            },
        ];
        let content = collect_events(&events).unwrap().content;
        assert_eq!(content.len(), 2);
        assert!(
            matches!(&content[0], HubPart::Reasoning { encrypted: Some(d), .. } if d == "blob")
        );
        assert!(
            matches!(&content[1], HubPart::Reasoning { id: Some(i), encrypted: None, .. } if i == "rs_1")
        );
        // An item with no payload leaves what the open block has.
        let events = vec![
            HubEvent::ReasoningDelta { text: "t".into() },
            HubEvent::ReasoningItem {
                id: Some("rs_1".into()),
                encrypted: Some("enc".into()),
            },
            HubEvent::ReasoningItem {
                id: None,
                encrypted: None,
            },
        ];
        let content = collect_events(&events).unwrap().content;
        assert!(matches!(
            &content[0],
            HubPart::Reasoning {
                id: Some(_),
                encrypted: Some(_),
                ..
            }
        ));
        assert_eq!(content.len(), 1, "an empty item is ignored, not an eraser");
    }

    #[test]
    fn text_after_a_signed_text_part_starts_a_new_part() {
        let events = vec![
            HubEvent::TextDelta { text: "a".into() },
            HubEvent::TextSignature {
                signature: "s".into(),
            },
            HubEvent::TextDelta { text: "b".into() },
            HubEvent::TextDelta { text: "c".into() },
        ];
        let content = collect_events(&events).unwrap().content;
        assert_eq!(content.len(), 2);
        assert!(
            matches!(&content[0], HubPart::Text { text, signature: Some(_), .. } if text == "a")
        );
        assert!(matches!(&content[1], HubPart::Text { text, signature: None, .. } if text == "bc"));
    }

    #[test]
    fn provenance_rules_and_the_client_wire_tag_round_trip() {
        assert!(signature_usable(None, Protocol::Gemini));
        assert!(signature_usable(Some(Protocol::Gemini), Protocol::Gemini));
        assert!(!signature_usable(
            Some(Protocol::AnthropicMessages),
            Protocol::Gemini
        ));
        let own = Protocol::AnthropicMessages;
        assert_eq!(wrap_signature(Some(own), "abc", own), "abc");
        assert_eq!(wrap_signature(None, "abc", own), "abc");
        let wrapped = wrap_signature(Some(Protocol::Gemini), "abc.def", own);
        assert_eq!(wrapped, "pdk1.gemini.abc.def");
        assert_eq!(
            unwrap_signature(&wrapped, own),
            (Protocol::Gemini, "abc.def".to_owned())
        );
        assert_eq!(
            unwrap_signature("raw-sig", own),
            (own, "raw-sig".to_owned())
        );
        // An unknown protocol name in a tag is not ours: the value is taken as the client's own.
        assert_eq!(
            unwrap_signature("pdk1.nope.sig", own),
            (own, "pdk1.nope.sig".to_owned())
        );
    }

    #[test]
    fn a_legacy_stored_message_without_provenance_deserializes_and_re_serializes_unchanged() {
        let old = r#"{"role":"assistant","content":[
            {"type":"reasoning","text":"t"},
            {"type":"text","text":"answer"},
            {"type":"tool_call","id":"c","name":"f","arguments":"{}"}
        ]}"#;
        let message: HubMessage = serde_json::from_str(old).unwrap();
        assert!(matches!(
            &message.content[0],
            HubPart::Reasoning {
                signature: None,
                origin: None,
                ..
            }
        ));
        assert!(matches!(
            &message.content[2],
            HubPart::ToolCall {
                signature: None,
                origin: None,
                ..
            }
        ));
        let again = serde_json::to_value(&message).unwrap();
        assert_eq!(again, serde_json::from_str::<Value>(old).unwrap());
    }

    #[test]
    fn old_stored_parts_without_provenance_still_deserialize() {
        let old = r#"[
            {"type":"text","text":"hi"},
            {"type":"tool_call","id":"c","name":"f","arguments":"{}","signature":"s"},
            {"type":"reasoning","text":"t","signature":"s"}
        ]"#;
        let parts: Vec<HubPart> = serde_json::from_str(old).unwrap();
        assert!(matches!(
            &parts[0],
            HubPart::Text {
                signature: None,
                origin: None,
                ..
            }
        ));
        assert!(matches!(&parts[1], HubPart::ToolCall { origin: None, .. }));
        assert!(matches!(&parts[2], HubPart::Reasoning { origin: None, .. }));
        let json = serde_json::to_string(&HubPart::text("x")).unwrap();
        assert_eq!(json, r#"{"type":"text","text":"x"}"#);
    }

    #[test]
    fn ends_with_assistant_checks_last_message_role() {
        let user_last = HubRequest {
            messages: vec![HubMessage::user("hi")],
            ..HubRequest::default()
        };
        assert!(!user_last.ends_with_assistant());
        let prefill = HubRequest {
            messages: vec![
                HubMessage::user("q"),
                HubMessage::assistant("The answer is"),
            ],
            ..HubRequest::default()
        };
        assert!(prefill.ends_with_assistant());
    }
}
