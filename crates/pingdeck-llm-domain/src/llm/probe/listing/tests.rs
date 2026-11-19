use serde_json::json;

use super::*;
use ModelKind as K;

fn ids(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn thirty_ids_have_the_expected_kinds() {
    let table = [
        ("gpt-6.1", K::Chat),
        ("gpt-6.1-mini", K::Chat),
        ("claude-haiku-5", K::Chat),
        ("deepseek-v4-pro", K::Chat),
        ("gemini-3.2-pro", K::Chat),
        ("gemini-3.2-flash-image", K::Image),
        ("glm-5", K::Chat),
        ("glm-embedding-3", K::Embedding),
        ("text-embedding-4", K::Embedding),
        ("text-embedding-3-large", K::Embedding),
        ("nomic-embed-text", K::Embedding),
        ("dall-e-3", K::Image),
        ("gpt-image-1", K::Image),
        ("imagen-4.0-generate", K::Image),
        ("flux-1.1-pro", K::Image),
        ("stable-diffusion-xl", K::Image),
        ("sdxl-turbo", K::Image),
        ("qwen-image-edit", K::Image),
        ("whisper-1", K::Audio),
        ("gpt-4o-mini-tts", K::Audio),
        ("gpt-4o-audio-preview", K::Audio),
        ("bge-reranker-v2", K::Rerank),
        ("rerank-3.5", K::Rerank),
        ("omni-moderation-latest", K::Moderation),
        ("text-moderation-stable", K::Moderation),
        ("kimi-k2.6", K::Chat),
        ("llama-4-70b", K::Chat),
        ("qwen3-max", K::Chat),
        ("openai/gpt-oss-120b", K::Chat),
        ("GPT-IMAGE-1", K::Image),
    ];
    assert_eq!(table.len(), 30);
    for (id, kind) in table {
        assert_eq!(model_kind(id, None, None), kind, "{id}");
    }
}

#[test]
fn decision_ids_are_recognised() {
    for id in [
        "jev-latest",
        "jev-1.14.0",
        "typesafe/jev-1.14",
        "Kev-4B",
        "kev",
        "gw/systemone",
    ] {
        assert!(is_decision_model(id), "{id}");
    }
    for id in [
        "gpt-6.1",
        "claude-haiku-5",
        "gemini-3.2-pro",
        "text-embedding-4",
        "gpt-oss-120b",
        "grok-5",
        "m",
    ] {
        assert!(!is_decision_model(id), "{id}");
    }
}

#[test]
fn the_key_list_failure_dto_keeps_its_wire_shape() {
    let mut drift = compute_drift(&[], &["a".to_owned()], 5);
    assert!(
        serde_json::to_value(&drift)
            .unwrap()
            .get("key_failures")
            .is_none(),
        "left out while empty, so the frozen ModelDriftDto samples keep their bytes"
    );
    drift.key_failures.push(KeyListFailure {
        key_id: "k1".into(),
        message: "the provider rejected the key on the model list (401)".into(),
    });
    assert_eq!(
        serde_json::to_value(&drift).unwrap()["key_failures"],
        json!([{"key_id": "k1", "message": "the provider rejected the key on the model list (401)"}])
    );
}

#[test]
fn an_incomplete_listing_is_on_the_wire_and_removes_nothing() {
    let mut drift = compute_drift(&ids(&["a", "b"]), &ids(&["c"]), 5);
    assert!(
        serde_json::to_value(&drift)
            .unwrap()
            .get("listing_complete")
            .is_none()
    );
    let stored: ModelDrift =
        serde_json::from_value(json!({"checked_at_ms": 1, "added": [], "removed": ["a"]})).unwrap();
    assert!(
        stored.listing_complete,
        "a drift stored before the field existed reads as complete"
    );
    drift.limit_removals(false, |_| true);
    assert!(drift.removed.is_empty());
    assert_eq!(
        serde_json::to_value(&drift).unwrap()["listing_complete"],
        json!(false)
    );
}

#[test]
fn catalogue_mode_wins_over_declared_and_patterns() {
    assert_eq!(model_kind("text-embedding-4", Some("chat"), None), K::Chat);
    assert_eq!(model_kind("m", Some("responses"), Some(K::Image)), K::Chat);
    assert_eq!(model_kind("m", Some("embedding"), None), K::Embedding);
    assert_eq!(model_kind("m", Some("image_generation"), None), K::Image);
    assert_eq!(model_kind("m", Some("audio_speech"), None), K::Audio);
    assert_eq!(model_kind("m", Some("ocr"), None), K::Other);
    // An unknown mode falls through to the next rule.
    assert_eq!(
        model_kind("m", Some("brand_new"), Some(K::Rerank)),
        K::Rerank
    );
}

#[test]
fn declared_wins_over_patterns() {
    assert_eq!(model_kind("text-embedding-4", None, Some(K::Chat)), K::Chat);
    assert_eq!(model_kind("plain", None, Some(K::Image)), K::Image);
}

#[test]
fn the_ui_kind_folds_rerank_and_moderation_into_other() {
    assert_eq!(K::Rerank.coarse(), K::Other);
    assert_eq!(K::Moderation.coarse(), K::Other);
    assert_eq!(K::Embedding.coarse(), K::Embedding);
}

#[test]
fn kind_identifiers_match_serde() {
    for kind in [
        K::Chat,
        K::Embedding,
        K::Image,
        K::Audio,
        K::Rerank,
        K::Moderation,
        K::Other,
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
    }
}

#[test]
fn drift_lists_new_and_missing_ids_once() {
    let stored = ids(&["a", "b", "c", "c"]);
    let listed = ids(&["b", "c", "d", "e", "d"]);
    let drift = compute_drift(&stored, &listed, 42);
    assert_eq!(drift.checked_at_ms, 42);
    assert_eq!(drift.added, ids(&["d", "e"]));
    assert_eq!(drift.removed, ids(&["a"]));
    assert!(!drift.is_empty());
}

#[test]
fn identical_lists_have_no_drift_and_ids_compare_exactly() {
    let same = ids(&["x", "y"]);
    assert!(compute_drift(&same, &same, 1).is_empty());
    let drift = compute_drift(&ids(&["GPT-6"]), &ids(&["gpt-6"]), 1);
    assert_eq!(drift.added, ids(&["gpt-6"]));
    assert_eq!(drift.removed, ids(&["GPT-6"]));
    assert!(compute_drift(&[], &[], 1).is_empty());
}

#[test]
fn presence_keeps_its_wire_name() {
    assert_eq!(
        serde_json::to_value(Presence::NotInProvider).unwrap(),
        "not_in_provider"
    );
}
