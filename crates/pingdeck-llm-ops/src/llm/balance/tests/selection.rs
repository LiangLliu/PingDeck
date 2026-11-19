//! Which adapter a target selects, and that "No balance API" sends nothing.

use super::*;

const ALL_FAMILIES: [GatewayFamily; 19] = [
    GatewayFamily::Official,
    GatewayFamily::NewApi,
    GatewayFamily::OneApi,
    GatewayFamily::Veloera,
    GatewayFamily::DoneHub,
    GatewayFamily::RixApi,
    GatewayFamily::Sub2Api,
    GatewayFamily::VoApi,
    GatewayFamily::LiteLlm,
    GatewayFamily::OpenRouter,
    GatewayFamily::CliProxyApi,
    GatewayFamily::VercelAiGateway,
    GatewayFamily::Ollama,
    GatewayFamily::LmStudio,
    GatewayFamily::Vllm,
    GatewayFamily::LlamaCpp,
    GatewayFamily::Sglang,
    GatewayFamily::LocalAi,
    GatewayFamily::UnknownCompatible,
];

/// Whether `family` alone selects an adapter. No wildcard arm: a new family fails to compile
/// here until it is classified (and added to [`ALL_FAMILIES`]).
const fn has_family_adapter(family: GatewayFamily) -> bool {
    match family {
        GatewayFamily::NewApi
        | GatewayFamily::OneApi
        | GatewayFamily::Veloera
        | GatewayFamily::DoneHub
        | GatewayFamily::RixApi
        | GatewayFamily::Sub2Api
        | GatewayFamily::VoApi
        | GatewayFamily::LiteLlm
        | GatewayFamily::OpenRouter => true,
        GatewayFamily::Official
        | GatewayFamily::CliProxyApi
        | GatewayFamily::VercelAiGateway
        | GatewayFamily::Ollama
        | GatewayFamily::LmStudio
        | GatewayFamily::Vllm
        | GatewayFamily::LlamaCpp
        | GatewayFamily::Sglang
        | GatewayFamily::LocalAi
        | GatewayFamily::UnknownCompatible => false,
    }
}

#[test]
fn every_adapter_id_is_reachable_by_a_preset_or_a_family() {
    let by_preset = [
        ("deepseek", AdapterId::DeepSeek),
        ("openrouter", AdapterId::OpenRouter),
        ("moonshot", AdapterId::Moonshot),
        ("siliconflow", AdapterId::SiliconFlow),
        ("stepfun", AdapterId::StepFun),
        ("zhipu", AdapterId::Zhipu),
    ];
    let by_family = [
        (GatewayFamily::NewApi, AdapterId::NewApi),
        (GatewayFamily::Veloera, AdapterId::NewApi),
        (GatewayFamily::RixApi, AdapterId::NewApi),
        (GatewayFamily::DoneHub, AdapterId::DoneHub),
        (GatewayFamily::OneApi, AdapterId::OneApi),
        (GatewayFamily::Sub2Api, AdapterId::Sub2Api),
        (GatewayFamily::LiteLlm, AdapterId::LiteLlm),
        (GatewayFamily::VoApi, AdapterId::VoApiV2),
    ];
    let mut reached = Vec::new();
    for (preset, expected) in by_preset {
        let mut t = target(GatewayFamily::Official);
        t.preset_id = Some(preset);
        assert_eq!(select_adapter(&t), Some(expected), "{preset}");
        reached.push(expected);
    }
    for (family, expected) in by_family {
        assert_eq!(
            select_adapter(&target(family)),
            Some(expected),
            "{family:?}"
        );
        reached.push(expected);
    }
    for adapter in AdapterId::ALL {
        assert!(reached.contains(&adapter), "{adapter:?} is unreachable");
    }
}

#[test]
fn families_and_unknown_hosts_without_an_adapter_say_no_balance_api() {
    for family in [
        GatewayFamily::Official,
        GatewayFamily::CliProxyApi,
        GatewayFamily::VercelAiGateway,
        GatewayFamily::Ollama,
        GatewayFamily::LmStudio,
        GatewayFamily::Vllm,
        GatewayFamily::LlamaCpp,
        GatewayFamily::Sglang,
        GatewayFamily::LocalAi,
        GatewayFamily::UnknownCompatible,
    ] {
        assert_eq!(select_adapter(&target(family)), None, "{family:?}");
    }
    for preset in ["anthropic", "openai", "google_gemini", "groq", "together"] {
        let mut t = vendor(preset, "https://api.example.com");
        t.family = GatewayFamily::Official;
        assert_eq!(select_adapter(&t), None, "{preset}");
    }
    // Every family is classified: the ones with an adapter select one, the rest select none.
    for family in ALL_FAMILIES {
        assert_eq!(
            select_adapter(&target(family)).is_some(),
            has_family_adapter(family),
            "{family:?}"
        );
    }
}

#[test]
fn a_known_vendor_host_selects_its_adapter_without_a_preset() {
    for (origin, adapter) in [
        ("https://api.deepseek.com", AdapterId::DeepSeek),
        ("https://platform.deepseek.com", AdapterId::DeepSeek),
        ("https://openrouter.ai", AdapterId::OpenRouter),
        ("https://api.moonshot.ai", AdapterId::Moonshot),
        ("https://open.bigmodel.cn", AdapterId::Zhipu),
        ("https://api.z.ai", AdapterId::Zhipu),
    ] {
        let mut t = target(GatewayFamily::Official);
        t.origin = origin;
        assert_eq!(select_adapter(&t), Some(adapter), "{origin}");
    }
    let mut lookalike = target(GatewayFamily::Official);
    lookalike.origin = "https://api.deepseek.com.evil.test";
    assert_eq!(select_adapter(&lookalike), None);
    // Created from the preset, whatever the host.
    let mut by_preset = target(GatewayFamily::Official);
    by_preset.preset_id = Some("stepfun");
    by_preset.origin = "https://stepfun-proxy.example.com";
    assert_eq!(select_adapter(&by_preset), Some(AdapterId::StepFun));
    // No preset, no known host, no family: a plain unknown host.
    let mut plain = target(GatewayFamily::UnknownCompatible);
    plain.origin = "https://random.example.com";
    assert_eq!(select_adapter(&plain), None);
}

#[tokio::test]
async fn no_adapter_sends_nothing() {
    let http = ScriptedHttp::new();
    let outcome = check(&http, &target(GatewayFamily::Ollama)).await;
    assert_eq!(outcome, BalanceOutcome::NoAdapter);
    assert_eq!(http.count(), 0);
}

#[tokio::test]
async fn proxy_host_moonshot_preset_sends_to_the_proxy_only_and_shows_unit_unknown() {
    let body = json!({"code": 0, "data": {"available_balance": 49.59, "voucher_balance": 46.59}});
    let http = ScriptedHttp::new()
        .route_on(
            "moonshot.example.com",
            SafeMethod::Get,
            "/v1/users/me/balance",
            ok(&body),
        )
        .route_on(
            "api.moonshot.cn",
            SafeMethod::Get,
            "/v1/users/me/balance",
            ok(&body),
        );
    let t = vendor("moonshot", "https://moonshot.example.com");
    let outcome = check(&http, &t).await;
    assert_eq!(http.count(), 1);
    assert_eq!(
        http.requests()[0].url,
        "https://moonshot.example.com/v1/users/me/balance"
    );
    let report = report_of(outcome);
    assert_eq!(
        (report.unit, report.remaining, report.remaining_usd),
        (BalanceUnit::Unsupported, Some(49.59), None)
    );
    assert!(
        report
            .labels
            .iter()
            .any(|label| label.contains("Unit unknown"))
    );
    assert!(
        http.requests()
            .iter()
            .all(|request| !request.url.contains("api.moonshot.cn"))
    );
}

#[tokio::test]
async fn reports_carry_the_evidence_grade_of_their_adapter() {
    let deepseek = ScriptedHttp::new().route(
        SafeMethod::Get,
        "/user/balance",
        ok(&json!({"is_available": true, "balance_infos": [{"currency": "CNY", "total_balance": "1"}]})),
    );
    let report = report_of(check(&deepseek, &vendor("deepseek", "https://api.deepseek.com")).await);
    assert_eq!(report.evidence, Some(Evidence::Documented));
    assert!(
        !report
            .evidence
            .is_some_and(Evidence::is_community_documented)
    );

    let zhipu = ScriptedHttp::new().route(
        SafeMethod::Get,
        "/api/biz/account/query-customer-account-report",
        ok(&json!({"success": true, "data": {"availableBalance": 12.5}})),
    );
    let report = report_of(check(&zhipu, &vendor("zhipu", "https://api.z.ai")).await);
    assert_eq!(report.evidence, Some(Evidence::ThirdPartySource));
    assert!(
        report
            .evidence
            .is_some_and(Evidence::is_community_documented)
    );
    assert_eq!(
        (report.unit, report.remaining),
        (BalanceUnit::Unsupported, Some(12.5)),
        "z.ai reads Unit unknown 12.5"
    );
}

#[test]
fn preset_balance_adapters_parse() {
    for (_, adapter) in crate::llm::presets::BALANCE_ADAPTERS {
        assert!(AdapterId::parse(adapter).is_some(), "{adapter}");
    }
}
