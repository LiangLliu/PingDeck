//! Model-list parsing, prefill and kind tests.

use serde_json::json;

use super::*;

fn ids(page: &ParsedPage) -> Vec<&str> {
    page.models.iter().map(|m| m.model.id.as_str()).collect()
}

#[test]
fn eight_body_shapes_parse() {
    let openai = parse_page(
        &json!({"object":"list","data":[{"id":"gpt-4o","owned_by":"openai"},{"id":"gpt-4o-mini"}]}),
    );
    assert_eq!(ids(&openai), ["gpt-4o", "gpt-4o-mini"]);
    assert_eq!(openai.models[0].model.owned_by.as_deref(), Some("openai"));

    let anthropic = parse_page(
        &json!({"data":[{"id":"claude-x","display_name":"Claude X","max_input_tokens":200000,"max_tokens":64000,
        "capabilities":{"image_input":{"supported":true},"pdf_input":{"supported":true},"thinking":{"supported":false}}}],
        "has_more":true,"last_id":"claude-x"}),
    );
    assert_eq!(anthropic.next, Some(ListCursor::AfterId("claude-x".into())));
    let m = &anthropic.models[0].model;
    assert_eq!(
        (m.context_window, m.max_output),
        (Some(200_000), Some(64_000))
    );
    assert_eq!(m.display_name.as_deref(), Some("Claude X"));
    assert_eq!(m.capabilities.known(Capability::Vision), Some(true));
    assert_eq!(m.capabilities.known(Capability::Document), Some(true));
    assert_eq!(m.capabilities.known(Capability::Reasoning), None);

    let gemini = parse_page(
        &json!({"models":[{"name":"models/gemini-3.2-pro","displayName":"Gemini 3.2 Pro","inputTokenLimit":1048576,
        "outputTokenLimit":65536,"supportedGenerationMethods":["generateContent","countTokens"],"thinking":true},
        {"name":"models/text-embedding-9","supportedGenerationMethods":["embedContent"]}],"nextPageToken":"t2"}),
    );
    assert_eq!(ids(&gemini), ["gemini-3.2-pro", "text-embedding-9"]);
    assert_eq!(gemini.next, Some(ListCursor::Token("t2".into())));
    assert_eq!(gemini.models[0].model.context_window, Some(1_048_576));
    assert_eq!(
        gemini.models[0]
            .model
            .capabilities
            .known(Capability::Reasoning),
        Some(true)
    );
    assert_eq!(gemini.models[1].declared_kind, Some(ModelKind::Embedding));

    let ollama = parse_page(
        &json!({"models":[{"name":"llama3:latest","model":"llama3:latest","details":{"parameter_size":"8B"}}]}),
    );
    assert_eq!(ids(&ollama), ["llama3:latest"]);

    let lm = parse_page(
        &json!({"data":[{"id":"qwen-vl","type":"vlm","max_context_length":32768,"capabilities":["tool_use"]},
        {"id":"nomic-embed","type":"embeddings","max_context_length":2048}]}),
    );
    assert_eq!(lm.models[0].model.context_window, Some(32_768));
    assert_eq!(
        lm.models[0].model.capabilities.known(Capability::Tools),
        Some(true)
    );
    assert_eq!(
        lm.models[0].model.capabilities.known(Capability::Vision),
        Some(true)
    );
    assert_eq!(lm.models[1].declared_kind, Some(ModelKind::Embedding));

    let vllm = parse_page(
        &json!({"object":"list","data":[{"id":"Qwen/Qwen3-8B","owned_by":"vllm","max_model_len":40960}]}),
    );
    assert_eq!(vllm.models[0].model.context_window, Some(40_960));

    let openrouter = parse_page(
        &json!({"data":[{"id":"openai/gpt-5","name":"OpenAI: GPT-5","context_length":400000,
        "pricing":{"prompt":"0.00000125","completion":"0.00001"},"architecture":{"input_modalities":["text","image","file"],"output_modalities":["text"]},
        "supported_parameters":["tools","reasoning","response_format"],"top_provider":{"max_completion_tokens":128000}},
        {"id":"img/model","architecture":{"output_modalities":["text","image"]},"pricing":{"prompt":"-1","completion":"-1"}}]}),
    );
    let m = &openrouter.models[0].model;
    assert_eq!(m.display_name.as_deref(), Some("OpenAI: GPT-5"));
    assert!((m.input_cost_per_mtok.unwrap_or_default() - 1.25).abs() < 1e-9);
    assert!((m.output_cost_per_mtok.unwrap_or_default() - 10.0).abs() < 1e-9);
    assert_eq!(
        (m.context_window, m.max_output),
        (Some(400_000), Some(128_000))
    );
    for capability in [
        Capability::Vision,
        Capability::Document,
        Capability::Tools,
        Capability::Reasoning,
        Capability::JsonSchema,
    ] {
        assert_eq!(
            m.capabilities.known(capability),
            Some(true),
            "{capability:?}"
        );
    }
    assert_eq!(openrouter.models[1].declared_kind, Some(ModelKind::Image));
    assert_eq!(openrouter.models[1].model.input_cost_per_mtok, None);
    // Only a list that states both prices declares them (a variable `-1` is no price).
    assert!(m.price_declared && !openrouter.models[1].model.price_declared);

    let newapi = parse_page(&json!({"object":"list","success":true,"data":[
        {"id":"gpt-4o","object":"model","owned_by":"openai","supported_endpoint_types":["openai"]},
        {"id":"text-embedding-3-small","supported_endpoint_types":["embeddings"]},
        {"id":"gpt-image-1","supported_endpoint_types":["image-generation"]}]}));
    assert_eq!(newapi.models[0].model.declared_endpoint_types, ["openai"]);
    assert_eq!(newapi.models[1].declared_kind, Some(ModelKind::Embedding));
    assert_eq!(newapi.models[2].declared_kind, Some(ModelKind::Image));
}

#[test]
fn tolerant_shapes_and_junk() {
    assert_eq!(
        ids(&parse_page(
            &json!(["a", "models/b", {"model": "c"}, {"slug": "d"}, 7, {}])
        )),
        ["a", "b", "c", "d"]
    );
    assert!(parse_page(&json!({"error": "nope"})).models.is_empty());
    assert!(parse_page(&json!("text")).models.is_empty());
    assert_eq!(
        parse_page(&json!({"data": [], "has_more": false})).next,
        None
    );
}

#[test]
fn truncated_lists_keep_the_complete_entries() {
    let full =
        r#"{"object":"list","data":[{"id":"a","tags":["x","y"]},{"id":"b \"q\" }"},{"id":"c","pri"#;
    let value = parse_truncated(full.as_bytes()).unwrap();
    let page = parse_page(&value);
    assert_eq!(ids(&page), ["a", "b \"q\" }"]);
    assert!(parse_truncated(b"{\"data\":[{\"id\":\"a").is_none());
    assert!(parse_truncated(b"garbage").is_none());
    // A multi-byte character cut in half does not break the repair.
    let mut bytes = r#"[{"id":"é1"},{"id":"é"#.as_bytes().to_vec();
    bytes.pop();
    assert_eq!(ids(&parse_page(&parse_truncated(&bytes).unwrap())), ["é1"]);
    // Multi-byte text and escaped quotes inside strings do not open or close anything.
    let cut = r#"{"data":[{"id":"é\"}]ü"},{"id":"ß{["#;
    assert_eq!(
        ids(&parse_page(&parse_truncated(cut.as_bytes()).unwrap())),
        ["é\"}]ü"]
    );
}

#[test]
fn an_object_id_of_only_the_models_prefix_is_blank() {
    // Regression guard (the object form used to give an empty id).
    assert!(ids(&parse_page(&json!([{"id": "models/"}]))).is_empty());
    assert_eq!(
        ids(&parse_page(&json!([{"id": "models/", "name": "x"}]))),
        ["x"]
    );
    assert!(ids(&parse_page(&json!(["models/"]))).is_empty());
}

#[test]
fn kind_table() {
    for (id, kind) in [
        ("gpt-4o", ModelKind::Chat),
        ("text-embedding-4", ModelKind::Embedding),
        ("glm-embedding-3", ModelKind::Embedding),
        ("gemini-3.2-flash-image", ModelKind::Image),
        ("dall-e-3", ModelKind::Image),
        ("gpt-image-1", ModelKind::Image),
        ("imagen-4", ModelKind::Image),
        ("black-forest-labs/FLUX.1-dev", ModelKind::Image),
        ("stable-diffusion-xl", ModelKind::Image),
        ("sdxl-turbo", ModelKind::Image),
        ("whisper-1", ModelKind::Audio),
        ("tts-1-hd", ModelKind::Audio),
        ("gpt-4o-audio-preview", ModelKind::Audio),
        ("bge-reranker", ModelKind::Rerank),
        ("jina-rerank-v2", ModelKind::Rerank),
        ("omni-moderation-latest", ModelKind::Moderation),
        ("claude-sonnet-5", ModelKind::Chat),
        ("deepseek-chat", ModelKind::Chat),
    ] {
        assert_eq!(kind_from_id(id), kind, "{id}");
    }
}

fn catalog_entry(mode: &str) -> CatalogEntry {
    CatalogEntry {
        model_id: "m".into(),
        mode: mode.into(),
        max_input_tokens: Some(8000),
        max_output_tokens: Some(1000),
        vision: true,
        input_cost_per_mtok: Some(2.0),
        ..CatalogEntry::default()
    }
}

#[test]
fn finalize_orders_declared_over_catalog_and_catalog_mode_over_declared_kind() {
    let raw = parse_page(&json!({"data":[
        {"id":"known","context_length":999,"supported_parameters":["tools"]},
        {"id":"vendor/emb-model","supported_endpoint_types":["embeddings"]},
        {"id":"gemini-x-image"},
        {"id":"known"}]}));
    let lookup = |id: &str| match id {
        "known" => Some(catalog_entry("chat")),
        "vendor/emb-model" => Some(catalog_entry("chat")),
        _ => None,
    };
    let models = finalize(raw.models, &lookup, None);
    assert_eq!(models.len(), 3, "duplicates keep the first");
    let known = &models[0];
    assert_eq!(
        known.context_window,
        Some(999),
        "declared beats the catalogue"
    );
    assert_eq!(known.max_output, Some(1000));
    assert_eq!(known.input_cost_per_mtok, Some(2.0));
    assert_eq!(known.capabilities.vision.source, CapabilitySource::Catalog);
    assert_eq!(known.capabilities.tools.source, CapabilitySource::Declared);
    assert!(!known.capabilities.has_solid_bit());
    // Catalogue mode wins over the declared kind.
    assert_eq!(models[1].kind, ModelKind::Chat);
    assert_eq!(models[2].kind, ModelKind::Image);
    assert!(known.default_enabled && !models[2].default_enabled);
}

#[test]
fn chat_and_embedding_start_enabled_for_lists_of_any_size() {
    let raw = parse_page(
        &json!({"data": [{"id":"a"},{"id":"dall-e-3"},{"id":"text-embedding-3"},
        {"id":"whisper-1"},{"id":"omni-moderation-latest"},{"id":"jina-rerank-v2"},{"id":"sora-2"}]}),
    );
    let models = finalize(raw.models, &|_| None, None);
    assert_eq!(
        models.iter().map(|m| m.default_enabled).collect::<Vec<_>>(),
        [true, false, true, false, false, false, false]
    );
    assert_eq!(models[6].kind, ModelKind::Other);
    // 312 models, 280 of them chat or embedding, all 280 enabled.
    let data: Vec<Value> = (0..312)
        .map(|i| match i % 39 {
            0..=3 => json!({"id": format!("img-model-{i}-image")}),
            _ => json!({"id": format!("m{i}")}),
        })
        .collect();
    let models = finalize(parse_page(&json!({"data": data})).models, &|_| None, None);
    let enabled = models.iter().filter(|m| m.default_enabled).count();
    assert_eq!(models.len(), 312);
    assert_eq!(
        enabled,
        312 - models.iter().filter(|m| m.kind == ModelKind::Image).count()
    );
    assert_eq!(enabled, 280);
}

#[test]
fn lm_studio_v1_keys_and_capabilities_prefill() {
    let page = parse_page(&json!({"models": [
        {"key": "qwen3-vl", "type": "llm", "max_context_length": 32768,
         "capabilities": {"vision": true, "trained_for_tool_use": true,
            "reasoning": {"allowed_options": ["off", "low", "high"], "default": "low"}}},
        {"key": "plain", "capabilities": {"vision": false, "trained_for_tool_use": false}}]}));
    let first = &page.models[0].model;
    assert_eq!(first.id, "qwen3-vl");
    assert_eq!(first.context_window, Some(32768));
    assert_eq!(first.reasoning_options, ["off", "low", "high"]);
    for capability in [Capability::Vision, Capability::Tools, Capability::Reasoning] {
        assert_eq!(
            first.capabilities.get(capability).source,
            CapabilitySource::Declared
        );
        assert_eq!(first.capabilities.known(capability), Some(true));
    }
    let plain = &page.models[1].model;
    assert!(plain.reasoning_options.is_empty());
    assert_eq!(plain.capabilities.known(Capability::Vision), None);
}

#[test]
fn preset_model_entries_fill_the_gaps_last() {
    let preset = crate::llm::presets::presets()
        .unwrap()
        .provider("deepseek")
        .unwrap();
    let hint = &preset.models[0];
    let raw = parse_page(&json!({"data":[{"id": hint.id.clone()}]}));
    let models = finalize(raw.models, &|_| None, Some(preset));
    assert_eq!(models[0].context_window, hint.context);
}

#[test]
fn the_public_page_parser_prefills_and_reports_the_cursor() {
    let body = br#"{"data":[{"id":"a"},{"id":"dall-e-3"},{"id":"a"}],"has_more":true,"last_id":"dall-e-3"}"#;
    let page = parse_list_page(body, false, &|_| None).unwrap();
    assert_eq!(
        page.models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "dall-e-3"]
    );
    assert_eq!(
        page.models
            .iter()
            .map(|m| m.default_enabled)
            .collect::<Vec<_>>(),
        [true, false]
    );
    assert_eq!(
        page.next.as_ref().map(ListCursor::query_pair),
        Some(("after_id", "dall-e-3"))
    );
    let gemini = parse_list_page(
        br#"{"models":[{"name":"models/g"}],"nextPageToken":"t"}"#,
        false,
        &|_| None,
    )
    .unwrap();
    assert_eq!(
        gemini.next.as_ref().map(ListCursor::query_pair),
        Some(("pageToken", "t"))
    );
    let cut = br#"{"data":[{"id":"x"},{"id":"y"},{"id":"z"#;
    assert_eq!(
        parse_list_page(cut, true, &|_| None).unwrap().models.len(),
        2
    );
    assert!(parse_list_page(cut, false, &|_| None).is_none());
    assert!(parse_list_page(br#"{"error":"nope"}"#, false, &|_| None).is_none());
    assert!(parse_list_page(b"<html>", false, &|_| None).is_none());
}

#[test]
fn a_scanned_kind_becomes_the_same_stored_kind() {
    use crate::llm::probe::listing::ModelKind as StoredKind;
    for (kind, stored) in [
        (ModelKind::Chat, StoredKind::Chat),
        (ModelKind::Embedding, StoredKind::Embedding),
        (ModelKind::Image, StoredKind::Image),
        (ModelKind::Audio, StoredKind::Audio),
        (ModelKind::Rerank, StoredKind::Rerank),
        (ModelKind::Moderation, StoredKind::Moderation),
        (ModelKind::Other, StoredKind::Other),
    ] {
        assert_eq!(StoredKind::from(kind), stored);
        // Both DTOs put the same string on the wire.
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            serde_json::to_string(&stored).unwrap()
        );
    }
}
