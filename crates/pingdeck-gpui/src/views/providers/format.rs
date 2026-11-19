//! Pure formatting and semantic-label helpers for the Providers, Gateway & Test Bench views.

use pingdeck_desktop::core::app::{EffectivePriceState, KeyBalanceResultDto};
use pingdeck_desktop::core::client_config::{ClientStatus, Drift, SnippetClient};
use pingdeck_desktop::core::llm::balance_state::{BalanceReport, BalanceScope, BalanceUnit};
use pingdeck_desktop::core::llm::probe::{ProbeStatus, ProbeTest};
use pingdeck_desktop::core::llm::router::RuleStatus;
use pingdeck_desktop::core::llm::status::{ProviderStatus, ReasonCode, StatusReason};
use pingdeck_desktop::core::llm::store::ProviderKey;
use pingdeck_desktop::core::llm::wire::{AuthStyle, GatewayFamily, Protocol, ReasoningStyle};

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
pub const fn provider_status_label(status: ProviderStatus, zh: bool) -> &'static str {
    match (status, zh) {
        (ProviderStatus::Healthy, true) => "正常",
        (ProviderStatus::Healthy, false) => "Healthy",
        (ProviderStatus::NeedsAttention, true) => "需要关注",
        (ProviderStatus::NeedsAttention, false) => "Needs attention",
        (ProviderStatus::Unreachable, true) => "无法连接",
        (ProviderStatus::Unreachable, false) => "Unreachable",
        (ProviderStatus::Unverified, true) => "未验证",
        (ProviderStatus::Unverified, false) => "Unverified",
        (ProviderStatus::Disabled, true) => "已停用",
        (ProviderStatus::Disabled, false) => "Disabled",
    }
}

#[must_use]
pub fn format_reason(reason: &StatusReason, zh: bool) -> String {
    if !zh {
        return reason.text.clone();
    }
    let get = |k: &str| reason.params.get(k).copied().unwrap_or(0);
    match reason.code {
        ReasonCode::NoKey => "没有密钥".into(),
        ReasonCode::LastVerified => format!("上次验证在 {} 天前", get("days")),
        ReasonCode::NoModelAnswered => "还没有模型应答过".into(),
        ReasonCode::ConnectivityFailed => "上次连通性检查失败".into(),
        ReasonCode::RecentFailures => format!("最近 {} 个请求都失败了", get("count")),
        ReasonCode::AllKeysRejected => {
            let n = get("keys");
            if n > 1 {
                format!("全部 {n} 个密钥都被拒绝")
            } else {
                "密钥被拒绝".into()
            }
        }
        ReasonCode::ModelsFailed => {
            format!("{} / {} 模型测试失败", get("failed"), get("total"))
        }
        ReasonCode::CircuitOpen => format!("有 {} 个熔断已打开", get("count")),
        ReasonCode::BalanceExhausted => "余额已用完".into(),
        ReasonCode::AllKeysExhausted => "所有密钥额度已用完".into(),
        ReasonCode::BalanceAuthFailed => "无法读取余额：密钥被拒绝".into(),
        ReasonCode::HighFailureRate => "近期失败率过高".into(),
    }
}

#[must_use]
pub fn format_protocol(proto: Protocol, zh: bool) -> &'static str {
    match proto {
        Protocol::OpenaiChat => {
            if zh {
                "OpenAI 对话"
            } else {
                "OpenAI Chat"
            }
        }
        Protocol::OpenaiResponses => "Responses",
        Protocol::AnthropicMessages => "Messages",
        Protocol::Gemini => "Gemini",
        Protocol::Embeddings => "Embeddings",
        Protocol::DecisionSystemone => {
            if zh {
                "决策 (SystemOne)"
            } else {
                "Decision (SystemOne)"
            }
        }
        Protocol::DecisionOpenai => {
            if zh {
                "决策 (OpenAI)"
            } else {
                "Decision (OpenAI)"
            }
        }
    }
}

#[must_use]
pub const fn format_auth(auth: AuthStyle, zh: bool) -> &'static str {
    match auth {
        AuthStyle::Bearer => "Bearer",
        AuthStyle::XApiKey => "x-api-key",
        AuthStyle::XGoogApiKey => "x-goog-api-key",
        AuthStyle::RawAuthorization => "Raw Authorization",
        AuthStyle::None => {
            if zh {
                "无认证"
            } else {
                "None"
            }
        }
    }
}

#[must_use]
pub const fn format_reasoning_style(style: ReasoningStyle, zh: bool) -> &'static str {
    match (style, zh) {
        (ReasoningStyle::ReasoningEffort, true) => "推理强度 (reasoning_effort)",
        (ReasoningStyle::ReasoningEffort, false) => "reasoning_effort",
        (ReasoningStyle::ReasoningObject, true) => "推理对象 (reasoning)",
        (ReasoningStyle::ReasoningObject, false) => "reasoning object",
        (ReasoningStyle::ThinkingType, true) => "思考类型 (thinking.type)",
        (ReasoningStyle::ThinkingType, false) => "thinking.type",
        (ReasoningStyle::EnableThinking, true) => "思考开关 (enable_thinking)",
        (ReasoningStyle::EnableThinking, false) => "enable_thinking",
        (ReasoningStyle::ChatTemplateKwargs, true) => "模板参数 (chat_template_kwargs)",
        (ReasoningStyle::ChatTemplateKwargs, false) => "chat_template_kwargs",
        (ReasoningStyle::AnthropicAdaptive, true) => "Anthropic 自适应",
        (ReasoningStyle::AnthropicAdaptive, false) => "Anthropic adaptive",
        (ReasoningStyle::GeminiThinkingLevel, true) => "Gemini 思考级别",
        (ReasoningStyle::GeminiThinkingLevel, false) => "Gemini thinkingLevel",
        (ReasoningStyle::None, true) => "不发送推理参数",
        (ReasoningStyle::None, false) => "None",
    }
}

#[must_use]
pub const fn format_family(family: GatewayFamily, zh: bool) -> &'static str {
    match family {
        GatewayFamily::Official => {
            if zh {
                "官方"
            } else {
                "Official"
            }
        }
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
            if zh {
                "兼容接口"
            } else {
                "Compatible"
            }
        }
    }
}

#[must_use]
pub fn format_balance_report(rep: &BalanceReport, zh: bool) -> String {
    if !rep.supported {
        return if zh {
            "无余额接口".into()
        } else {
            "No balance API".into()
        };
    }
    match rep.unit {
        BalanceUnit::Unlimited => {
            if zh {
                "不限额".into()
            } else {
                "Unlimited".into()
            }
        }
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
                if zh {
                    format!("{rem:.0} 额度")
                } else {
                    format!("{rem:.0} quota")
                }
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
                if zh {
                    "单位未知".into()
                } else {
                    "Unknown unit".into()
                }
            },
            |v| format!("{v:.2}"),
        ),
    }
}

#[must_use]
pub fn provider_best_balance(keys: &[ProviderKey], category: &str, zh: bool) -> Option<String> {
    if category == "local" {
        return Some(if zh {
            "本地免费".into()
        } else {
            "Local · Free".into()
        });
    }
    let best = keys
        .iter()
        .filter(|k| k.enabled)
        .filter_map(|k| k.balance.as_ref().and_then(|b| b.last_good.as_ref()))
        .find(|r| r.supported);
    best.map(|r| {
        let val = format_balance_report(r, zh);
        if r.scope == BalanceScope::Key && r.unit != BalanceUnit::Unlimited {
            if zh {
                format!("密钥 {val}")
            } else {
                format!("Key {val}")
            }
        } else {
            val
        }
    })
}

#[must_use]
pub fn format_balance_dto_error(dto: &KeyBalanceResultDto, zh: bool) -> Option<String> {
    dto.error.as_ref().map(|e| {
        if e.message.is_empty() {
            if zh {
                format!("刷新失败 ({:?})", e.kind)
            } else {
                format!("Refresh failed ({:?})", e.kind)
            }
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
pub fn format_price_state(state: Option<&EffectivePriceState>, zh: bool) -> String {
    match state {
        Some(EffectivePriceState::Free) => {
            if zh {
                "免费".into()
            } else {
                "Free".into()
            }
        }
        Some(EffectivePriceState::Known(view)) => {
            let sym = currency_symbol(&view.price.currency);
            if let Some(per_call) = view.price.per_call {
                if zh {
                    format!("{sym}{per_call:.3}/次")
                } else {
                    format!("{sym}{per_call:.3}/call")
                }
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
pub fn format_relative_time(ts_ms: i64, zh: bool) -> String {
    if ts_ms <= 0 {
        return "—".into();
    }
    let delta_s = ((now_ms() - ts_ms).max(0)) / 1000;
    if delta_s < 45 {
        if zh {
            "刚刚".into()
        } else {
            "just now".into()
        }
    } else if delta_s < 3600 {
        let m = (delta_s / 60).max(1);
        if zh {
            format!("{m} 分钟前")
        } else {
            format!("{m}m ago")
        }
    } else if delta_s < 86_400 {
        let h = delta_s / 3600;
        if zh {
            format!("{h} 小时前")
        } else {
            format!("{h}h ago")
        }
    } else {
        let d = delta_s / 86_400;
        if zh {
            format!("{d} 天前")
        } else {
            format!("{d}d ago")
        }
    }
}

#[must_use]
pub fn format_uptime(secs: u64, zh: bool) -> String {
    if secs < 60 {
        if zh {
            format!("{secs} 秒")
        } else {
            format!("{secs}s")
        }
    } else if secs < 3600 {
        let m = secs / 60;
        if zh {
            format!("{m} 分钟")
        } else {
            format!("{m}m")
        }
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if zh {
            format!("{h} 小时 {m} 分")
        } else {
            format!("{h}h {m}m")
        }
    }
}

#[must_use]
pub fn format_client_status(
    status: &ClientStatus,
    drift: &Drift,
    zh: bool,
) -> (&'static str, Tone) {
    match status {
        ClientStatus::Verified { .. } => (if zh { "已验证" } else { "Verified" }, Tone::Success),
        ClientStatus::Configured => (if zh { "已连接" } else { "Connected" }, Tone::Success),
        ClientStatus::WaitingForFirstRequest => (
            if zh {
                "等待首个请求"
            } else {
                "Waiting for request"
            },
            Tone::Accent,
        ),
        ClientStatus::Overridden { .. } => {
            (if zh { "被覆盖" } else { "Overridden" }, Tone::Warning)
        }
        ClientStatus::ModelUnavailable => (
            if zh {
                "模型不可用"
            } else {
                "Model unavailable"
            },
            Tone::Warning,
        ),
        ClientStatus::NoRequest => (if zh { "未收到请求" } else { "No request" }, Tone::Neutral),
        ClientStatus::Rejected { .. } => (if zh { "请求被拒" } else { "Rejected" }, Tone::Danger),
        ClientStatus::Drifted => {
            let lbl = match drift {
                Drift::DifferentAddress { .. } => {
                    if zh {
                        "地址不同"
                    } else {
                        "Different address"
                    }
                }
                Drift::TokenNotRecognised => {
                    if zh {
                        "令牌无法识别"
                    } else {
                        "Token unknown"
                    }
                }
                Drift::AliasMissing => {
                    if zh {
                        "模型不存在"
                    } else {
                        "Model missing"
                    }
                }
                _ => {
                    if zh {
                        "配置已漂移"
                    } else {
                        "Drifted"
                    }
                }
            };
            (lbl, Tone::Warning)
        }
        ClientStatus::NotConfigured => {
            (if zh { "未配置" } else { "Not configured" }, Tone::Neutral)
        }
    }
}

#[must_use]
pub const fn snippet_client_name(client: SnippetClient, zh: bool) -> &'static str {
    match client {
        SnippetClient::Zed => "Zed Editor",
        SnippetClient::Cursor => "Cursor",
        SnippetClient::CherryStudio => "Cherry Studio",
        SnippetClient::GenericEnv => {
            if zh {
                "其他（环境变量）"
            } else {
                "Other (Env vars)"
            }
        }
    }
}

#[must_use]
pub const fn format_rule_status(status: RuleStatus, zh: bool) -> (&'static str, Tone) {
    match status {
        RuleStatus::Ok => (if zh { "生效" } else { "Active" }, Tone::Success),
        RuleStatus::Disabled => (if zh { "已停用" } else { "Disabled" }, Tone::Neutral),
        RuleStatus::NoKey => (if zh { "无密钥" } else { "No key" }, Tone::Warning),
        RuleStatus::CircuitOpen => (if zh { "熔断中" } else { "Circuit open" }, Tone::Warning),
        RuleStatus::Unresolved => (if zh { "未解析" } else { "Unresolved" }, Tone::Danger),
    }
}

#[must_use]
pub const fn format_probe_status(status: ProbeStatus, zh: bool) -> (&'static str, Tone) {
    match status {
        ProbeStatus::Ready => (if zh { "就绪" } else { "Ready" }, Tone::Success),
        ProbeStatus::Rejected => (if zh { "被拒绝" } else { "Rejected" }, Tone::Danger),
        ProbeStatus::RateLimited => (if zh { "限流" } else { "Rate limited" }, Tone::Warning),
        ProbeStatus::Unreachable => (if zh { "无法连接" } else { "Unreachable" }, Tone::Danger),
        ProbeStatus::InvalidResponse => (if zh { "响应无效" } else { "Invalid" }, Tone::Warning),
        ProbeStatus::Unsupported => (if zh { "不支持" } else { "Unsupported" }, Tone::Neutral),
        ProbeStatus::NotObserved => (if zh { "未观察到" } else { "Not observed" }, Tone::Neutral),
        ProbeStatus::Skipped => (if zh { "跳过" } else { "Skipped" }, Tone::Neutral),
        ProbeStatus::TimedOut => (if zh { "超时" } else { "Timed out" }, Tone::Warning),
    }
}

#[must_use]
pub const fn format_probe_test(test: ProbeTest, zh: bool) -> &'static str {
    match (test, zh) {
        (ProbeTest::Connectivity, true) => "连通性",
        (ProbeTest::Connectivity, false) => "Connectivity",
        (ProbeTest::Auth, true) => "鉴权",
        (ProbeTest::Auth, false) => "Auth",
        (ProbeTest::Models, true) => "模型列表",
        (ProbeTest::Models, false) => "Models",
        (ProbeTest::Chat, true) => "对话",
        (ProbeTest::Chat, false) => "Chat",
        (ProbeTest::Stream, true) => "流式",
        (ProbeTest::Stream, false) => "Stream",
        (ProbeTest::Tools, true) => "工具",
        (ProbeTest::Tools, false) => "Tools",
        (ProbeTest::JsonSchema, true) => "JSON",
        (ProbeTest::JsonSchema, false) => "JSON",
        (ProbeTest::Vision, true) => "视觉",
        (ProbeTest::Vision, false) => "Vision",
        (ProbeTest::Reasoning, true) => "推理",
        (ProbeTest::Reasoning, false) => "Reasoning",
        (ProbeTest::Cache, true) => "缓存",
        (ProbeTest::Cache, false) => "Cache",
        (ProbeTest::Embeddings, true) => "嵌入",
        (ProbeTest::Embeddings, false) => "Embed",
        (ProbeTest::Search, true) => "搜索",
        (ProbeTest::Search, false) => "Search",
        (ProbeTest::Pdf, true) => "PDF",
        (ProbeTest::Pdf, false) => "PDF",
        (ProbeTest::Decision, true) => "决策",
        (ProbeTest::Decision, false) => "Decision",
        (ProbeTest::Context, true) => "上下文",
        (ProbeTest::Context, false) => "Context",
    }
}
