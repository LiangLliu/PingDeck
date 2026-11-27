//! The vendor wording the failure rules search: word lists and the predicates over them.
//!
//! Every list is matched as lowercase substrings of the first [`SCAN_BYTES`] of a body (see
//! [`scan_text`]); the wording changes with the providers, the rules in the parent module do not.

use super::FailureKind;

/// How much of an error body is searched (the interesting text is at the start).
pub(super) const SCAN_BYTES: usize = 16 * 1024;

/// The searched part of an error body: its first [`SCAN_BYTES`], cut at a character boundary,
/// lowercased. Idempotent, so an already scanned text may be scanned again.
pub(super) fn scan_text(body: &str) -> String {
    body[..body.floor_char_boundary(SCAN_BYTES)].to_ascii_lowercase()
}

/// Whether `text` contains any of `needles`.
pub(super) fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// Structured codes and stable message starts that mean the account is out of money, whatever
/// the status (Anthropic spend limits, OpenAI quota codes, Gemini prepaid credit, Moonshot
/// `exceeded_current_quota_error`, DashScope `Arrearage`, Volcengine Ark `AccountOverdueError`,
/// and the Chinese wording of Zhipu (code 1113), DeepSeek, new-api and one-api relays).
pub(super) const QUOTA_CODES: &[&str] = &[
    "enforced_spend_limit_reached",
    "credit_balance_exhausted",
    "organization_spend_limit_exceeded",
    "project_spend_limit_exceeded",
    "organization_usage_limit_exceeded",
    "insufficient_quota",
    "you have reached your specified api usage limits",
    "you have reached your specified workspace api usage limits",
    "exceeded_current_quota_error",
    "\"arrearage\"",
    "accountoverdueerror",
    "余额不足",
    "欠费",
    "额度不足",
    "额度已用尽",
    "额度已用完",
    "请充值",
];

/// A content or safety filter refused the request (OpenAI `content_policy_violation`, Gemini and
/// Azure safety wording, DashScope `DataInspectionFailed`, Volcengine Ark
/// `SensitiveContentDetected`, MiniMax `new_sensitive`, and the Chinese moderation wording of
/// Zhipu (code 1301), Baidu, Moonshot and the relays).
pub(super) const POLICY_WORDS: &[&str] = &[
    "content_policy_violation",
    "content policy",
    "safety policy",
    "blocked by policy",
    "blocked by safety",
    "safety filter",
    "datainspectionfailed",
    "sensitivecontentdetected",
    "new_sensitive",
    "敏感内容",
    "敏感信息",
    "不安全或敏感",
    "内容审核",
    "内容安全",
    "违规内容",
    "涉及违规",
];

/// A 401 or 403 about where the caller or the key is (OpenAI
/// `unsupported_country_region_territory`, keys bound to another region).
pub(super) const REGION_WORDS: &[&str] = &[
    "region",
    "country",
    "territory",
    "location",
    "not available in your",
    "geo",
];

/// Money words: a 429 without rate-limit wording, or a bare text, that mentions them is about the
/// account, not the rate.
pub(super) const QUOTA_WORDS: &[&str] = &[
    "insufficient",
    "quota",
    "credit",
    "balance",
    "billing",
    "余额",
    "额度",
    "充值",
];

/// A finished call without content (the client's and the probes' own messages).
const EMPTY_OUTPUT_WORDS: &[&str] = &["no content", "empty response"];

/// Rate-limit wording (OpenAI "Rate limit reached … requests per min", HTTP 429's reason phrase,
/// Moonshot `rate_limit_reached_error`, DashScope `Throttling`, Baidu "qps request limit", and the
/// Chinese wording of Zhipu (codes 1302, 1303) and the relays).
pub(super) const RATE_LIMIT_WORDS: &[&str] = &[
    "rate limit",
    "rate_limit",
    "too many requests",
    "requests per",
    "throttling",
    "qps request limit",
    "请求过于频繁",
    "请求频率",
    "频率超限",
    "频率过高",
    "并发数过高",
    "并发超限",
    "限流",
];

/// A missing, invalid or unauthorised key (OpenAI, Anthropic `x-api-key`, Gemini "API key not
/// valid", HTTP reason phrases).
const AUTH_WORDS: &[&str] = &[
    "invalid api key",
    "invalid x-api-key",
    "invalid key",
    "api key not valid",
    "unauthorized",
    "forbidden",
    "permission denied",
    "authentication",
    "invalidapikey",
    "身份验证失败",
    "鉴权失败",
    "认证失败",
    "无效的令牌",
    "令牌无效",
    "未授权",
];

/// A model the text says is not served.
const MODEL_UNAVAILABLE_WORDS: &[&str] = &[
    "model not found",
    "model does not exist",
    "model unavailable",
    "unknown model",
    "模型不存在",
    "模型不可用",
    "无权使用模型",
    "无权访问模型",
];

/// Timeout wording of clients, proxies and gRPC (`deadline exceeded`).
const TIMEOUT_WORDS: &[&str] = &["timed out", "timeout", "deadline"];

/// Transport wording of reqwest and the relays.
const NETWORK_WORDS: &[&str] = &["request failed", "network error", "connection", "dns"];

/// The upstream does not speak the protocol or route asked for (relay and gateway wording).
const PROTOCOL_WORDS: &[&str] = &[
    "stream error",
    "protocol",
    "endpoint has no upstream path",
    "does not support endpoint",
    "unsupported endpoint",
];

/// The provider failed (Anthropic `overloaded_error`, relay wording).
const PROVIDER_ERROR_WORDS: &[&str] = &["overloaded", "upstream failed"];

/// The kinds a text alone suggests, in the order they are tried; the first match decides.
const TEXT_KINDS: &[(&[&str], FailureKind)] = &[
    (EMPTY_OUTPUT_WORDS, FailureKind::EmptyOutput),
    (RATE_LIMIT_WORDS, FailureKind::RateLimit),
    (AUTH_WORDS, FailureKind::Auth),
    (QUOTA_WORDS, FailureKind::Quota),
    (MODEL_UNAVAILABLE_WORDS, FailureKind::ModelUnavailable),
    (TIMEOUT_WORDS, FailureKind::Timeout),
    (NETWORK_WORDS, FailureKind::Network),
    (PROTOCOL_WORDS, FailureKind::ProtocolMismatch),
    (PROVIDER_ERROR_WORDS, FailureKind::ProviderError),
];

/// Error codes that say the model is missing (OpenAI `model_not_found`, DeepSeek "Model Not
/// Exist", Moonshot "Not found the model", OpenRouter "No endpoints found").
const MISSING_MODEL_CODES: &[&str] = &[
    "model_not_found",
    "model_not_exist",
    "model not found",
    "model not exist",
    "model does not exist",
    "not found the model",
    "no such model",
    "unknown model",
    "unknown_model",
    "invalid model",
    "invalid_model",
    "no endpoints found",
    "invalidendpointormodel",
    "模型不存在",
    "未找到模型",
    "找不到模型",
];

/// Phrases that mean "missing" when the text also talks about a model (Gemini `models/x is not
/// found`, Anthropic `not_found_error`).
const MISSING_MODEL_PHRASES: &[&str] = &[
    "does not exist",
    "do not exist",
    "not exist",
    "not found",
    "not_found",
    "is not supported",
    "not available",
    "no access",
    "no available channel",
    "无可用渠道",
    "不存在",
    "不支持",
    "无权",
];

/// Context-length and request-size wording (see [`names_context_limit`]).
const CONTEXT_LIMIT_PHRASES: &[&str] = &[
    "context_length_exceeded",
    "string_above_max_length",
    "request_too_large",
    "context length",
    "context window",
    "context limit",
    "prompt is too long",
    "prompt too long",
    "input is too long",
    "input too long",
    "too many tokens",
    "too many input tokens",
    "reduce the length of the messages",
    "exceeds the maximum number of tokens",
    "exceeded model token limit",
    "exceeds the model token limit",
    "input token count",
    "maximum number of input tokens",
    "上下文长度",
    "超出最大长度",
    "超过最大长度",
    "prompt 超长",
    "prompt超长",
    "输入过长",
    "输入长度超",
];

/// The request fields that name an output cap (Chat, Responses, Messages, Gemini).
const OUTPUT_CAP_FIELDS: &[&str] = &[
    "max_tokens",
    "max_completion_tokens",
    "max_output_tokens",
    "maxoutputtokens",
];

/// Wording that says an output cap is above what the model allows (Anthropic "maximum allowed
/// number of output tokens", OpenAI "too large … supports at most", DeepSeek "valid range").
const OUTPUT_CAP_WORDS: &[&str] = &[
    "exceed",
    "too large",
    "greater than",
    "less than or equal",
    "maximum allowed",
    "at most",
    "valid range",
    "超过",
    "超出",
    "最大",
];

/// Whether a 400 says the request's output cap is above the model's: a model-level limit, so the
/// next target (with a larger cap) may take the same request.
pub(super) fn output_cap_in(text: &str) -> bool {
    contains_any(text, OUTPUT_CAP_FIELDS) && contains_any(text, OUTPUT_CAP_WORDS)
}

/// What an "exceeds the maximum" sentence must also name to be a size limit.
const SIZE_WORDS: &[&str] = &["token", "length", "size", "bytes", "context"];

/// Whether `text` talks about a model, ignoring URL paths (`/v1/models`) but not Gemini's
/// `models/<id>` resource names; the Chinese word for model (`模型`) counts too.
pub(super) fn mentions_model(text: &str) -> bool {
    text.contains("模型")
        || text
            .split(|c: char| c.is_whitespace() || "\",{}:`'()[]".contains(c))
            .filter(|token| !token.contains('/') || token.starts_with("models/"))
            .any(|token| token.contains("model"))
}

/// [`names_missing_model`] of an already scanned text.
pub(super) fn missing_model_in(text: &str) -> bool {
    contains_any(text, MISSING_MODEL_CODES)
        || (mentions_model(text) && contains_any(text, MISSING_MODEL_PHRASES))
}

/// [`names_context_limit`] of an already scanned text.
pub(super) fn context_limit_in(text: &str) -> bool {
    contains_any(text, CONTEXT_LIMIT_PHRASES)
        || (text.contains("exceeds the maximum") && contains_any(text, SIZE_WORDS))
}

/// Whether a 404 body says the *model* was not found: an error code (`model_not_found`), a
/// message about a model that "does not exist" / "is not found" (OpenAI, Gemini `models/x is not
/// found`, Moonshot "Not found the model", DeepSeek "Model Not Exist"), Anthropic's
/// `not_found_error` on `model: <id>`, or OpenRouter's "No endpoints found". A 404 about a
/// missing route or file is not a model error.
#[must_use]
pub fn names_missing_model(body: &str) -> bool {
    missing_model_in(&scan_text(body))
}

/// Whether a 400 or 413 body names a context-length (or request-size) limit: OpenAI
/// `context_length_exceeded` / `string_above_max_length` / "maximum context length", Anthropic
/// "prompt is too long" and `request_too_large`, Gemini "exceeds the maximum number of tokens",
/// DeepSeek and OpenRouter "maximum context length", Moonshot "exceeded model token limit".
/// A malformed body, a bad parameter or an output-token cap is not a context error. This is the
/// only word list for context limits.
#[must_use]
pub fn names_context_limit(body: &str) -> bool {
    context_limit_in(&scan_text(body))
}

/// The kind a text alone suggests (no failing status: in-stream errors, probe messages).
pub(super) fn text_kind(text: &str) -> FailureKind {
    TEXT_KINDS
        .iter()
        .find(|(words, _)| contains_any(text, words))
        .map_or(FailureKind::Unknown, |(_, kind)| *kind)
}
