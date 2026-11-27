//! Seeding the bundled snapshot and backfilling older rows.

use super::*;

/// A catalog seeded from the bundled snapshot.
fn seeded_conn() -> Connection {
    let c = conn();
    assert!(seed_bundled_if_empty(&c).unwrap());
    c
}

#[test]
fn the_catalog_lives_in_the_four_model_tables() {
    let c = conn();
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND (name LIKE 'model_%' OR name LIKE 'llm_catalog%' OR name = 'llm_price_overrides') ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        tables,
        [
            "model_alias",
            "model_catalog",
            "model_catalog_sync",
            "model_pricing"
        ]
    );
}

#[test]
fn seeding_an_empty_catalog_stores_the_bundled_snapshot_once() {
    let snap = models_dev::bundled_snapshot().unwrap();
    let at = snap.snapshot_at_ms.unwrap();
    let c = conn();
    assert_eq!(status(&c).unwrap(), None);
    assert_eq!(sync_state(&c).unwrap(), SyncState::default());
    assert!(seed_bundled_if_empty(&c).unwrap());
    assert!(!seed_bundled_if_empty(&c).unwrap());
    let seeded = status(&c).unwrap().unwrap();
    assert_eq!(
        (
            seeded.source.as_str(),
            seeded.fetched_at_ms,
            seeded.entry_count
        ),
        ("bundled", at, snap.records.len() as u64)
    );
    assert_eq!(
        (
            sync_state(&c).unwrap().origin,
            sync_state(&c).unwrap().snapshot_at_ms
        ),
        (CatalogOrigin::Builtin, at)
    );
    assert_eq!(lookup(&c, "claude-sonnet-5-5-nope").unwrap(), None);

    for (id, window) in [
        ("gpt-4o", 128_000),
        ("gpt-4.1", 1_047_576),
        ("claude-sonnet-4-5", 200_000),
        ("deepseek-v4-pro", 1_048_576),
        ("gemini-2.5-pro", 1_048_576),
    ] {
        let found = lookup_record(&c, id).unwrap().unwrap();
        assert!(found.has_price() && found.is_first_party());
        assert_eq!(found.context_tokens, Some(window), "{id}");
    }
    let embed = lookup_record(&c, "text-embedding-3-small")
        .unwrap()
        .unwrap();
    assert_eq!(
        (embed.kind, embed.family.as_str()),
        (CatalogKind::Embedding, "openai")
    );
}

#[test]
fn the_bundled_list_groups_offers_and_lists_distributor_entries() {
    let snap = models_dev::bundled_snapshot().unwrap();
    let c = seeded_conn();
    let all = list_page(&c, &ModelCatalogQuery::default(), &NONE_IN_USE).unwrap();
    assert_eq!((snap.records.len(), all.total), (937, 406));
    let dist = list_page(
        &c,
        &ModelCatalogQuery {
            distributor_entries: Some(true),
            ..Default::default()
        },
        &NONE_IN_USE,
    )
    .unwrap();
    assert_eq!(dist.total, 524 - 406);
    assert!(dist.rows.iter().all(|r| r.publisher == r.family));
    let opus = list_page(
        &c,
        &ModelCatalogQuery {
            search: Some("claude-opus-4-5".into()),
            publisher: Some("anthropic".into()),
            ..Default::default()
        },
        &NONE_IN_USE,
    )
    .unwrap();
    let row = opus
        .rows
        .iter()
        .find(|r| r.model_key == "anthropic/claude-opus-4-5")
        .unwrap();
    assert_eq!(
        (row.family.as_str(), row.publisher_name.as_str()),
        ("anthropic", "Anthropic")
    );
    assert!(row.provider_count >= 2 && row.release_date.as_deref() == Some("2025-11-24"));
}

#[test]
fn seeding_backfills_null_input_modalities() {
    let c = seeded_conn();
    c.execute_batch(
        "UPDATE model_catalog SET input_modalities = NULL;
         INSERT INTO model_catalog (family, model_id, source) VALUES ('openai', 'zz-gone', 'models.dev');",
    )
    .unwrap();
    assert!(!seed_bundled_if_empty(&c).unwrap());
    let zz: Option<String> = c
        .query_row(
            "SELECT input_modalities FROM model_catalog WHERE family = 'openai' AND model_id = 'zz-gone'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(zz.as_deref(), Some(""));
    assert!(
        CatalogIndex::load(&c)
            .unwrap()
            .lookup_record("gpt-4o")
            .unwrap()
            .input_modalities
            .contains(&"image".to_owned())
    );
}

#[test]
fn seeding_backfills_missing_model_keys() {
    let c = seeded_conn();
    let gpt_key = resolve(&c, None, "gpt-4o").unwrap().model_key;
    c.execute(
        "INSERT INTO model_pricing (provider_id, model_id, mode, updated_at_ms)
         VALUES ('', 'gpt-4o', 'dynamic', 1)",
        [],
    )
    .unwrap();
    let keys = |c: &Connection| -> Vec<(String, String, String)> {
        c.prepare("SELECT family, model_id, model_key FROM model_catalog ORDER BY family, model_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let pricing_key = |c: &Connection| -> String {
        c.query_row(
            "SELECT model_key FROM model_pricing WHERE model_id = 'gpt-4o'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let before = keys(&c);
    assert!(!gpt_key.is_empty() && before.iter().all(|(_, _, key)| !key.is_empty()));

    c.execute_batch(
        "UPDATE model_catalog SET model_key = '';
         UPDATE model_pricing SET model_key = '';",
    )
    .unwrap();
    assert!(!seed_bundled_if_empty(&c).unwrap());
    assert_eq!(keys(&c), before);
    assert_eq!(pricing_key(&c), gpt_key);
}

#[test]
fn seeding_keeps_user_rows_and_an_existing_sync_state() {
    let c = conn();
    save_user_model(&c, user_model("mine", "m-1"), 9).unwrap();
    assert!(seed_bundled_if_empty(&c).unwrap());
    let st = sync_status(&c, false).unwrap();
    assert_eq!(
        (st.user_count, st.entry_count),
        (1, st.models_dev_count + 1)
    );
    // Re-seeds once the models.dev rows are gone, keeping the recorded sync state.
    c.execute("DELETE FROM model_catalog WHERE source = 'models.dev'", [])
        .unwrap();
    c.execute(
        "UPDATE model_catalog_sync SET origin = 'models.dev', last_success_ms = 3",
        [],
    )
    .unwrap();
    assert!(seed_bundled_if_empty(&c).unwrap());
    assert_eq!(
        (
            sync_state(&c).unwrap().origin,
            sync_state(&c).unwrap().last_success_ms
        ),
        (CatalogOrigin::ModelsDev, Some(3))
    );
}
