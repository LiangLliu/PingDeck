//! Which row describes a model id: SQL lookup, the in-memory index and search.

use super::*;

#[test]
fn strip_version_drops_a_date_or_version_suffix() {
    for (raw, stripped) in [
        ("m-2024-11-21", Some("m")),
        ("m-20241121", Some("m")),
        ("m@v2", Some("m")),
        ("m-2024-1121", None),
        ("m-2411", None),
        ("20241121", None),
        ("@x", None),
    ] {
        assert_eq!(strip_version(raw), stripped, "{raw}");
    }
}

#[test]
fn lookup_ranks_first_party_then_priced_then_family() {
    let c = conn();
    let rows = vec![
        ModelRecord {
            canonical_id: "orig/gpt".into(),
            ..record("a", "gpt")
        },
        ModelRecord {
            input_per_mtok: None,
            ..record("orig", "gpt")
        },
        record("z", "gpt"),
        record("c", "tie"),
        record("b", "tie"),
        ModelRecord {
            canonical_id: "q/solo".into(),
            input_per_mtok: None,
            ..record("k", "solo")
        },
        ModelRecord {
            canonical_id: "q/solo".into(),
            ..record("m", "solo")
        },
        ModelRecord {
            canonical_id: "q/solo".into(),
            ..record("l", "solo")
        },
    ];
    apply_sync(&c, &rows, 1, 1).unwrap();
    let fam = |id: &str| lookup_record(&c, id).unwrap().map(|r| r.family);
    assert_eq!(fam("gpt").as_deref(), Some("z"));
    assert_eq!(fam("tie").as_deref(), Some("b"));
    assert_eq!(fam("solo").as_deref(), Some("l"));
    let entry = lookup(&c, "gpt").unwrap().unwrap();
    assert_eq!(
        (
            entry.provider.as_str(),
            entry.mode.as_str(),
            entry.input_cost_per_mtok
        ),
        ("z", "chat", Some(1.0))
    );
}

#[test]
fn index_matches_sql_lookup_for_every_fallback_form() {
    let c = conn();
    let rows: Vec<_> = [
        ("p", "gpt-4o", "", true),
        ("q", "gpt-4o", "other/gpt-4o", true),
        ("r", "gpt-4o", "", false),
        ("p", "openai/gpt-4o", "", true),
        ("p", "vendor/deep/GPT-4o", "", true),
        ("p", "x/Claude-Opus", "", true),
        ("q", "x/Claude-Opus", "", true),
        ("p", "anthropic/claude-opus-long-name/claude-opus", "", true),
        ("p", "Mixed-Case", "", true),
        ("q", "mixed-case", "", false),
        ("p", "a/b/c/model-x", "", true),
        ("q", "z/model-x", "q/z", true),
        ("p", "gemini-2.5-pro", "", true),
        ("p", "claude-3-5-sonnet", "", true),
        ("p", "mistral-large", "", true),
        ("p", "azure/", "", true),
        ("p", "/leading", "", true),
    ]
    .into_iter()
    .map(|(f, id, can, pr)| ModelRecord {
        canonical_id: can.into(),
        input_per_mtok: pr.then_some(1.0),
        ..record(f, id)
    })
    .collect();
    apply_sync(&c, &rows, 1, 1).unwrap();
    let index = CatalogIndex::load(&c).unwrap();
    for q in [
        "gpt-4o",
        "GPT-4O",
        "  gpt-4o ",
        "claude-opus",
        "CLAUDE-OPUS",
        "x/claude-opus",
        "mixed-case",
        "MIXED-CASE",
        "model-x",
        "c/model-x",
        "b/c/model-x",
        "deep/gpt-4o",
        "gemini-2.5-pro@001",
        "claude-3-5-sonnet-20241022",
        "mistral-large-2024-11-21",
        "mistral-large-2411",
        "nothing-20241022",
        "leading",
        "azure",
        "",
        "@x",
        "models/gemini-2.5-pro",
        "Anthropic/claude-3-5-sonnet-20241022",
        "relay/mistral-large-2024-11-21",
        "a/b/MODEL-X",
        "trailing/",
        "unknown",
    ] {
        assert_eq!(
            index.lookup_record(q).cloned(),
            lookup_record(&c, q).unwrap(),
            "{q:?}"
        );
        assert_eq!(index.lookup(q), lookup(&c, q).unwrap(), "{q:?}");
    }
    assert_eq!(
        index
            .lookup_record("claude-opus")
            .map(|r| r.model_id.as_str()),
        Some("x/Claude-Opus")
    );
    assert_eq!(
        index
            .lookup_record("gemini-2.5-pro@001")
            .map(|r| r.model_id.as_str()),
        Some("gemini-2.5-pro")
    );
    assert_eq!(
        index
            .lookup_record("Anthropic/claude-3-5-sonnet-20241022")
            .map(|r| r.model_id.as_str()),
        Some("claude-3-5-sonnet")
    );
}

#[test]
fn search_lists_each_id_once_and_matches_like_wildcards_literally() {
    let c = conn();
    let mut rows: Vec<ModelRecord> = ["b/GPT-4o", "a-gpt-mini", "gpt_x", "gptyx", "llama"]
        .map(|id| record("p", id))
        .into();
    rows.push(ModelRecord {
        canonical_id: "p/gpt_x".into(),
        ..record("a", "gpt_x")
    });
    apply_sync(&c, &rows, 1, 1).unwrap();
    let found: Vec<_> = search(&c, "gpt", 10)
        .unwrap()
        .into_iter()
        .map(|e| e.model_id)
        .collect();
    assert_eq!(found, ["a-gpt-mini", "b/GPT-4o", "gpt_x", "gptyx"]);
    assert_eq!(
        search(&c, "gpt_", 10)
            .unwrap()
            .into_iter()
            .map(|e| (e.model_id, e.provider))
            .collect::<Vec<_>>(),
        [("gpt_x".to_owned(), "p".to_owned())]
    );
    assert_eq!(search(&c, "GPT", 2).unwrap().len(), 2);
    assert!(search(&c, "%", 10).unwrap().is_empty());
}
