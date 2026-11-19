use super::*;
use crate::llm::wire::Protocol;

#[test]
fn the_embedded_presets_parse_and_every_enum_value_maps_to_a_variant() {
    let catalog = presets().unwrap();
    assert_eq!(catalog.version, 1);
    assert_eq!(catalog.providers.len(), 27);
    assert_eq!(
        catalog
            .providers
            .iter()
            .map(|p| p.models.len())
            .sum::<usize>(),
        118
    );
    for provider in &catalog.providers {
        // The preset name is the default provider name: no `/` or quotes.
        assert!(
            crate::llm::providers::validate_provider_chars(&provider.name).is_ok(),
            "{} is not a usable provider name",
            provider.name
        );
        assert!(
            !provider.regions.is_empty(),
            "{} has no region",
            provider.id
        );
        assert!(
            !provider.endpoints.is_empty(),
            "{} has no endpoint",
            provider.id
        );
        for endpoint in &provider.endpoints {
            assert!(Protocol::ALL.contains(&endpoint.protocol));
            assert_eq!(endpoint.auth, None, "presets set auth per provider");
        }
    }
}

#[test]
fn preset_ids_are_unique_and_each_protocol_appears_once_per_provider() {
    let catalog = presets().unwrap();
    let mut ids: Vec<&str> = catalog.providers.iter().map(|p| p.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), catalog.providers.len());
    for provider in &catalog.providers {
        let mut protocols: Vec<Protocol> = provider.endpoints.iter().map(|e| e.protocol).collect();
        protocols.sort_unstable();
        protocols.dedup();
        assert_eq!(protocols.len(), provider.endpoints.len(), "{}", provider.id);
    }
}

#[test]
fn resolved_urls_match_the_documented_conventions() {
    use crate::llm::wire::{EndpointUrlMode::Standard, endpoint_url};
    let catalog = presets().unwrap();
    let openai = catalog.provider("openai").unwrap();
    assert_eq!(
        endpoint_url(
            &openai.regions[0].base_url,
            &openai.endpoints[0].path_prefix,
            Standard,
            Protocol::OpenaiChat,
            None,
            false
        )
        .unwrap(),
        "https://api.openai.com/v1/chat/completions"
    );
    let qianfan = catalog.provider("baidu_qianfan").unwrap();
    assert_eq!(qianfan.endpoints[0].path_prefix, "");
    assert_eq!(
        endpoint_url(
            &qianfan.regions[0].base_url,
            "",
            Standard,
            Protocol::OpenaiChat,
            None,
            false
        )
        .unwrap(),
        "https://qianfan.baidubce.com/v2/chat/completions"
    );
    let gemini = catalog.provider("google_gemini").unwrap();
    assert_eq!(gemini.endpoints.len(), 1);
    assert_eq!(gemini.endpoints[0].protocol, Protocol::Gemini);
    assert_eq!(gemini.endpoints[0].path_prefix, "/v1beta");
    assert_eq!(gemini.auth, AuthStyle::XGoogApiKey);
}

#[test]
fn manual_model_providers_have_no_models_and_placeholders_must_be_filled() {
    let catalog = presets().unwrap();
    for id in ["azure_foundry", "vllm"] {
        assert!(catalog.provider(id).unwrap().models.is_empty());
    }
    let azure = catalog.provider("azure_foundry").unwrap();
    let base = &azure.regions[0].base_url;
    assert!(fill_placeholders(base, &[]).is_err());
    assert_eq!(
        fill_placeholders(base, &[("resource", " mine ")]).unwrap(),
        "https://mine.openai.azure.com"
    );
    let bedrock = catalog.provider("aws_bedrock").unwrap();
    assert!(
        fill_placeholders(&bedrock.regions[0].base_url, &[])
            .unwrap_err()
            .message()
            .contains("{region}")
    );
    assert!(catalog.provider("zhipu").unwrap().models_endpoint.is_none());
}

#[test]
fn a_placeholder_value_must_be_one_host_label() {
    let base = "https://{resource}.openai.azure.com";
    for bad in [
        "evil.example/",
        "evil.example",
        "x@evil.example",
        "a:8443",
        "a/b",
        "a b",
        "{region}",
        "-é-",
        &"a".repeat(64),
    ] {
        let error = fill_placeholders(base, &[("resource", bad)]).unwrap_err();
        assert!(matches!(error, AppError::Validation { .. }), "{bad}");
        assert!(error.message().contains("{resource}"), "{bad}");
    }
    // An unused placeholder value is not looked at.
    assert_eq!(
        fill_placeholders(
            base,
            &[("resource", "my-res-01"), ("region", "evil.example/")]
        )
        .unwrap(),
        "https://my-res-01.openai.azure.com"
    );
    assert_eq!(
        fill_placeholders(
            "https://bedrock-runtime.{region}.amazonaws.com",
            &[("region", "us-east-1")]
        )
        .unwrap(),
        "https://bedrock-runtime.us-east-1.amazonaws.com"
    );
}

#[test]
fn known_hosts_map_every_concrete_region_host_and_adapters_name_presets() {
    let deepseek = known_host("api.deepseek.com", None).unwrap();
    assert_eq!(
        (deepseek.preset_id.as_str(), deepseek.region_id.as_str()),
        ("deepseek", "global")
    );
    assert_eq!(deepseek.balance_adapter.as_deref(), Some("deepseek"));
    assert_eq!(
        known_host("api.moonshot.ai", None).unwrap().region_id,
        "global"
    );
    assert_eq!(
        known_host("api.moonshot.cn", None).unwrap().region_id,
        "china"
    );
    // Ports are part of the key: `localhost:11434` is Ollama, bare `localhost` is nothing.
    assert_eq!(
        known_host("localhost", Some(11434)).unwrap().preset_id,
        "ollama"
    );
    assert!(known_host("localhost", None).is_none());
    assert!(known_host("relay.acme.dev", None).is_none());
    // Placeholder hosts never match.
    assert!(known_host("openai.azure.com", None).is_none());
    let catalog = presets().unwrap();
    let region_hosts = catalog
        .providers
        .iter()
        .flat_map(|provider| provider.regions.iter())
        .filter_map(|region| url::Url::parse(&region.base_url).ok())
        .count();
    assert_eq!(KNOWN_HOSTS.len(), region_hosts);
    for (preset, _) in BALANCE_ADAPTERS {
        assert!(catalog.provider(preset).is_some(), "{preset}");
    }
}

#[test]
fn region_mounts_include_the_region_path_and_placeholder_hosts_match_a_picked_preset() {
    assert_eq!(
        region_mount("baidu_qianfan", "china", Protocol::OpenaiChat).as_deref(),
        Some("/v2")
    );
    assert_eq!(
        region_mount("deepseek", "global", Protocol::AnthropicMessages).as_deref(),
        Some("/anthropic/v1")
    );
    assert_eq!(region_mount("deepseek", "global", Protocol::Gemini), None);
    assert_eq!(
        region_models_path("deepseek", "global").as_deref(),
        Some("/models")
    );
    assert_eq!(region_models_path("zhipu", "global"), None);
    let azure = preset_region_for_host("azure_foundry", None, "mine.openai.azure.com", None);
    assert_eq!(
        azure.map(|owner| owner.region_id),
        Some("resource".to_owned())
    );
    assert!(preset_region_for_host("azure_foundry", None, "openai.azure.com", None).is_none());
    assert!(preset_region_for_host("deepseek", None, "relay.acme.dev", None).is_none());
}
