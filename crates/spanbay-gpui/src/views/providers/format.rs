//! Pure formatting and semantic-label helpers for the Providers, Gateway & Test Bench views.

use spanbay_desktop::core::app::{EffectivePriceState, KeyBalanceResultDto};
use spanbay_desktop::core::client_config::{ClientStatus, Drift, SnippetClient};
use spanbay_desktop::core::llm::balance_state::{BalanceReport, BalanceScope, BalanceUnit};
use spanbay_desktop::core::llm::probe::{ProbeStatus, ProbeTest};
use spanbay_desktop::core::llm::router::RuleStatus;
use spanbay_desktop::core::llm::status::{ProviderStatus, ReasonCode, StatusReason};
use spanbay_desktop::core::llm::store::ProviderKey;
use spanbay_desktop::core::llm::wire::{AuthStyle, GatewayFamily, Protocol, ReasoningStyle};
use spanbay_desktop::core::model::settings::Language;

use crate::ui::{StatusShape, Tone};

#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[must_use]
pub const fn provider_status_shape(status: ProviderStatus) -> StatusShape {
    match status {
        ProviderStatus::Healthy => StatusShape::Ok,
        ProviderStatus::NeedsAttention => StatusShape::Warn,
        ProviderStatus::Unreachable => StatusShape::Bad,
        ProviderStatus::Unverified | ProviderStatus::Disabled => StatusShape::Idle,
    }
}

#[must_use]
pub const fn provider_status_tone(status: ProviderStatus) -> Tone {
    match status {
        ProviderStatus::Healthy => Tone::Success,
        ProviderStatus::NeedsAttention => Tone::Warning,
        ProviderStatus::Unreachable => Tone::Danger,
        ProviderStatus::Unverified | ProviderStatus::Disabled => Tone::Neutral,
    }
}

#[must_use]
pub const fn provider_status_label(status: ProviderStatus, lang: Language) -> &'static str {
    let p = crate::ui::Tr(lang).providers();
    match status {
        ProviderStatus::Healthy => p.prov_fmt_status_healthy(),
        ProviderStatus::NeedsAttention => p.prov_fmt_status_attention(),
        ProviderStatus::Unreachable => p.prov_fmt_status_unreachable(),
        ProviderStatus::Unverified => p.prov_fmt_status_unverified(),
        ProviderStatus::Disabled => p.prov_fmt_status_disabled(),
    }
}

#[must_use]
pub fn format_reason(reason: &StatusReason, lang: Language) -> String {
    let p = crate::ui::Tr(lang).providers();
    let get = |k: &str| reason.params.get(k).copied().unwrap_or(0);
    match reason.code {
        ReasonCode::NoKey => p.prov_fmt_reason_no_key().into(),
        ReasonCode::LastVerified => p.prov_fmt_reason_last_verified(get("days")),
        ReasonCode::NoModelAnswered => p.prov_fmt_reason_no_model_answered().into(),
        ReasonCode::ConnectivityFailed => p.prov_fmt_reason_connectivity_failed().into(),
        ReasonCode::RecentFailures => p.prov_fmt_reason_recent_failures(get("count")),
        ReasonCode::AllKeysRejected => {
            let n = get("keys");
            if n > 1 {
                p.prov_fmt_reason_all_keys_rejected_n(n)
            } else {
                p.prov_fmt_reason_key_rejected().into()
            }
        }
        ReasonCode::ModelsFailed => p.prov_fmt_reason_models_failed(get("failed"), get("total")),
        ReasonCode::CircuitOpen => p.prov_fmt_reason_circuit_open(get("count")),
        ReasonCode::BalanceExhausted => p.prov_fmt_reason_balance_exhausted().into(),
        ReasonCode::AllKeysExhausted => p.prov_fmt_reason_all_keys_exhausted().into(),
        ReasonCode::BalanceAuthFailed => p.prov_fmt_reason_balance_auth_failed().into(),
        ReasonCode::HighFailureRate => p.prov_fmt_reason_high_failure_rate().into(),
    }
}

#[must_use]
pub const fn format_protocol(proto: Protocol, lang: Language) -> &'static str {
    match proto {
        Protocol::OpenaiChat => crate::ui::Tr(lang).providers().prov_format_openai_chat(),
        Protocol::OpenaiResponses => "Responses",
        Protocol::AnthropicMessages => "Messages",
        Protocol::Gemini => "Gemini",
        Protocol::Embeddings => "Embeddings",
        Protocol::DecisionSystemone => crate::ui::Tr(lang)
            .providers()
            .prov_format_decision_systemone(),
        Protocol::DecisionOpenai => crate::ui::Tr(lang)
            .providers()
            .prov_format_decision_openai(),
    }
}

#[must_use]
pub const fn format_auth(auth: AuthStyle, lang: Language) -> &'static str {
    match auth {
        AuthStyle::Bearer => "Bearer",
        AuthStyle::XApiKey => "x-api-key",
        AuthStyle::XGoogApiKey => "x-goog-api-key",
        AuthStyle::RawAuthorization => "Raw Authorization",
        AuthStyle::None => crate::ui::Tr(lang).providers().prov_format_none(),
    }
}

#[must_use]
pub const fn format_reasoning_style(style: ReasoningStyle, lang: Language) -> &'static str {
    let p = crate::ui::Tr(lang).providers();
    match style {
        ReasoningStyle::ReasoningEffort => p.prov_fmt_reasoning_effort(),
        ReasoningStyle::ReasoningObject => p.prov_fmt_reasoning_object(),
        ReasoningStyle::ThinkingType => p.prov_fmt_thinking_type(),
        ReasoningStyle::EnableThinking => p.prov_fmt_enable_thinking(),
        ReasoningStyle::ChatTemplateKwargs => p.prov_fmt_chat_template_kwargs(),
        ReasoningStyle::AnthropicAdaptive => p.prov_fmt_anthropic_adaptive(),
        ReasoningStyle::GeminiThinkingLevel => p.prov_fmt_gemini_thinking_level(),
        ReasoningStyle::None => p.prov_fmt_reasoning_none(),
    }
}

#[must_use]
pub const fn format_family(family: GatewayFamily, lang: Language) -> &'static str {
    match family {
        GatewayFamily::Official => crate::ui::Tr(lang).providers().sniff_picker_official(),
        GatewayFamily::NewApi => "new-api",
        GatewayFamily::OneApi => "one-api",
        GatewayFamily::Veloera => "Veloera",
        GatewayFamily::DoneHub => "done-hub",
        GatewayFamily::RixApi => "rix-api",
        GatewayFamily::Sub2Api => "sub2api",
        GatewayFamily::VoApi => "VoAPI",
        GatewayFamily::LiteLlm => "LiteLLM",
        GatewayFamily::OpenRouter => "OpenRouter",
        GatewayFamily::CliProxyApi => "CLIProxyAPI",
        GatewayFamily::VercelAiGateway => "Vercel AI Gateway",
        GatewayFamily::Ollama => "Ollama",
        GatewayFamily::LmStudio => "LM Studio",
        GatewayFamily::Vllm => "vLLM",
        GatewayFamily::LlamaCpp => "llama.cpp",
        GatewayFamily::Sglang => "SGLang",
        GatewayFamily::LocalAi => "LocalAI",
        GatewayFamily::UnknownCompatible => {
            crate::ui::Tr(lang).providers().prov_format_compatible()
        }
    }
}

#[must_use]
pub fn format_balance_report(rep: &BalanceReport, lang: Language) -> String {
    if !rep.supported {
        return crate::ui::Tr(lang)
            .providers()
            .prov_format_no_balance_api()
            .into();
    }
    match rep.unit {
        BalanceUnit::Unlimited => crate::ui::Tr(lang).providers().prov_bal_unlimited().into(),
        BalanceUnit::Usd => rep
            .remaining
            .map_or_else(|| "—".into(), |v| format!("${v:.2}")),
        BalanceUnit::Cny => rep
            .remaining
            .map_or_else(|| "—".into(), |v| format!("¥{v:.2}")),
        BalanceUnit::Quota => {
            if let Some(usd) = rep.remaining_usd {
                format!("${usd:.2}")
            } else if let Some(rem) = rep.remaining {
                crate::ui::Tr(lang)
                    .providers()
                    .prov_format_quota(format!("{rem:.0}"))
            } else {
                "—".into()
            }
        }
        BalanceUnit::Custom => {
            let lbl = rep.unit_label.as_deref().unwrap_or("");
            rep.remaining
                .map_or_else(|| "—".into(), |v| format!("{v:.2} {lbl}").trim().to_owned())
        }
        BalanceUnit::Unsupported => rep.remaining.map_or_else(
            || {
                crate::ui::Tr(lang)
                    .providers()
                    .prov_format_unknown_unit()
                    .into()
            },
            |v| format!("{v:.2}"),
        ),
    }
}

#[must_use]
pub fn provider_best_balance(
    keys: &[ProviderKey],
    category: &str,
    lang: Language,
) -> Option<String> {
    if category == "local" {
        return Some(
            crate::ui::Tr(lang)
                .providers()
                .prov_format_local_free()
                .into(),
        );
    }
    let best = keys
        .iter()
        .filter(|k| k.enabled)
        .filter_map(|k| k.balance.as_ref().and_then(|b| b.last_good.as_ref()))
        .find(|r| r.supported);
    best.map(|r| {
        let val = format_balance_report(r, lang);
        if r.scope == BalanceScope::Key && r.unit != BalanceUnit::Unlimited {
            crate::ui::Tr(lang).providers().prov_format_key(val)
        } else {
            val
        }
    })
}

#[must_use]
pub fn format_balance_dto_error(dto: &KeyBalanceResultDto, lang: Language) -> Option<String> {
    dto.error.as_ref().map(|e| {
        if e.message.is_empty() {
            crate::ui::Tr(lang)
                .providers()
                .prov_format_refresh_failed(format!("{:?}", e.kind))
        } else {
            e.message.clone()
        }
    })
}

#[must_use]
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        if (m - m.round()).abs() < 0.05 {
            format!("{}M", m.round() as u64)
        } else {
            format!("{m:.1}M")
        }
    } else if n >= 1_000 {
        let k = n as f64 / 1_000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{}K", k.round() as u64)
        } else {
            format!("{k:.1}K")
        }
    } else {
        n.to_string()
    }
}

#[must_use]
pub fn currency_symbol(code: &str) -> &str {
    match code.to_ascii_uppercase().as_str() {
        "CNY" | "RMB" => "¥",
        "EUR" => "€",
        "GBP" => "£",
        "JPY" => "¥",
        "HKD" => "HK$",
        _ => "$",
    }
}

#[must_use]
pub fn format_price_state(state: Option<&EffectivePriceState>, lang: Language) -> String {
    match state {
        Some(EffectivePriceState::Free) => crate::ui::Tr(lang).common().free().into(),
        Some(EffectivePriceState::Known(view)) => {
            let sym = currency_symbol(&view.price.currency);
            if let Some(per_call) = view.price.per_call {
                crate::ui::Tr(lang)
                    .providers()
                    .prov_format_call(sym, format!("{per_call:.3}"))
            } else {
                format!(
                    "{sym}{:.2} / {sym}{:.2}",
                    view.price.input_per_mtok, view.price.output_per_mtok
                )
            }
        }
        Some(EffectivePriceState::Unknown) | None => "—".into(),
    }
}

#[must_use]
pub fn format_relative_time(ts_ms: i64, lang: Language) -> String {
    if ts_ms <= 0 {
        return "—".into();
    }
    let delta_s = ((now_ms() - ts_ms).max(0)) / 1000;
    if delta_s < 45 {
        crate::ui::Tr(lang).common().time_just_now().into()
    } else if delta_s < 3600 {
        let m = (delta_s / 60).max(1);
        crate::ui::Tr(lang).providers().prov_format_m_ago(m)
    } else if delta_s < 86_400 {
        let h = delta_s / 3600;
        crate::ui::Tr(lang).providers().prov_format_h_ago(h)
    } else {
        let d = delta_s / 86_400;
        crate::ui::Tr(lang).providers().prov_format_d_ago(d)
    }
}

#[must_use]
pub fn format_uptime(secs: u64, lang: Language) -> String {
    if secs < 60 {
        crate::ui::Tr(lang).providers().prov_format_s(secs)
    } else if secs < 3600 {
        let m = secs / 60;
        crate::ui::Tr(lang).providers().prov_format_m(m)
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        crate::ui::Tr(lang).providers().prov_format_h_m(h, m)
    }
}

#[must_use]
pub const fn format_client_status(
    status: &ClientStatus,
    drift: &Drift,
    lang: Language,
) -> (&'static str, Tone) {
    let p = crate::ui::Tr(lang).providers();
    match status {
        ClientStatus::Verified { .. } => (p.gwa_gateway_writer_verified(), Tone::Success),
        ClientStatus::Configured => (p.connect_status_connected(), Tone::Success),
        ClientStatus::WaitingForFirstRequest => (p.prov_format_waiting_for_request(), Tone::Accent),
        ClientStatus::Overridden { .. } => (p.gwa_gateway_writer_overridden(), Tone::Warning),
        ClientStatus::ModelUnavailable => (p.gwa_gateway_writer_model_unavailable(), Tone::Warning),
        ClientStatus::NoRequest => (p.prov_format_no_request(), Tone::Neutral),
        ClientStatus::Rejected { .. } => (p.prov_format_rejected(), Tone::Danger),
        ClientStatus::Drifted => {
            let lbl = match drift {
                Drift::DifferentAddress { .. } => p.connect_status_different_address(),
                Drift::TokenNotRecognised => p.prov_format_token_unknown(),
                Drift::AliasMissing => p.connect_status_alias_missing(),
                _ => p.prov_format_drifted(),
            };
            (lbl, Tone::Warning)
        }
        ClientStatus::NotConfigured => (p.connect_status_not_configured(), Tone::Neutral),
    }
}

#[must_use]
pub const fn snippet_client_name(client: SnippetClient, lang: Language) -> &'static str {
    match client {
        SnippetClient::Zed => "Zed Editor",
        SnippetClient::Cursor => "Cursor",
        SnippetClient::CherryStudio => "Cherry Studio",
        SnippetClient::GenericEnv => crate::ui::Tr(lang).providers().prov_format_other_env_vars(),
    }
}

#[must_use]
pub const fn format_rule_status(status: RuleStatus, lang: Language) -> (&'static str, Tone) {
    let p = crate::ui::Tr(lang).providers();
    match status {
        RuleStatus::Ok => (p.gwa_rules_st_ok(), Tone::Success),
        RuleStatus::Disabled => (p.prov_status_disabled(), Tone::Neutral),
        RuleStatus::NoKey => (p.sniff_key_none(), Tone::Warning),
        RuleStatus::CircuitOpen => (p.prov_format_circuit_open(), Tone::Warning),
        RuleStatus::Unresolved => (p.prov_format_unresolved(), Tone::Danger),
    }
}

#[must_use]
pub const fn format_probe_status(status: ProbeStatus, lang: Language) -> (&'static str, Tone) {
    let t = crate::ui::Tr(lang);
    let p = t.providers();
    let c = t.common();
    match status {
        ProbeStatus::Ready => (c.status_ready(), Tone::Success),
        ProbeStatus::Rejected => (c.status_rejected(), Tone::Danger),
        ProbeStatus::RateLimited => (p.prov_format_rate_limited(), Tone::Warning),
        ProbeStatus::Unreachable => (c.status_unreachable(), Tone::Danger),
        ProbeStatus::InvalidResponse => (p.prov_format_invalid(), Tone::Warning),
        ProbeStatus::Unsupported => (c.status_unsupported(), Tone::Neutral),
        ProbeStatus::NotObserved => (c.status_not_observed(), Tone::Neutral),
        ProbeStatus::Skipped => (p.prov_format_skipped(), Tone::Neutral),
        ProbeStatus::TimedOut => (c.status_timed_out(), Tone::Warning),
    }
}

#[must_use]
pub const fn format_probe_test(test: ProbeTest, lang: Language) -> &'static str {
    let p = crate::ui::Tr(lang).providers();
    match test {
        ProbeTest::Connectivity => p.prov_fmt_probe_connectivity(),
        ProbeTest::Auth => p.prov_fmt_probe_auth(),
        ProbeTest::Models => p.prov_fmt_probe_models(),
        ProbeTest::Chat => p.prov_fmt_probe_chat(),
        ProbeTest::Stream => p.prov_fmt_probe_stream(),
        ProbeTest::Tools => p.prov_fmt_probe_tools(),
        ProbeTest::JsonSchema => "JSON",
        ProbeTest::Vision => p.prov_fmt_probe_vision(),
        ProbeTest::Reasoning => p.prov_fmt_probe_reasoning(),
        ProbeTest::Cache => p.prov_fmt_probe_cache(),
        ProbeTest::Embeddings => p.prov_fmt_probe_embed(),
        ProbeTest::Search => p.prov_fmt_probe_search(),
        ProbeTest::Pdf => "PDF",
        ProbeTest::Decision => p.prov_fmt_probe_decision(),
        ProbeTest::Context => p.prov_fmt_probe_context(),
    }
}
