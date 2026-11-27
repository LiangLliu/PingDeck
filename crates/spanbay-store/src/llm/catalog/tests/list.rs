//! The paged list, its filters and default prices, and the user's own models.

use super::*;

/// The sample catalog synced plus the user's own `mine/m-1`.
fn listed_conn() -> Connection {
    let c = conn();
    apply_sync(&c, &sample_records(), 1, 1).unwrap();
    save_user_model(&c, user_model("mine", "m-1"), 5).unwrap();
    c
}

/// `beta/beta-own` is served by one of the user's providers.
fn used() -> HashMap<String, u32> {
    HashMap::from([("beta/beta-own".to_owned(), 1)])
}

fn query(
    c: &Connection,
    used: &HashMap<String, u32>,
    edit: &dyn Fn(&mut ModelCatalogQuery),
) -> ModelCatalogPage {
    let mut q = ModelCatalogQuery::default();
    edit(&mut q);
    list_page(c, &q, used).unwrap()
}

#[test]
fn the_default_list_holds_lab_user_and_in_use_models() {
    let c = listed_conn();
    let used = used();
    assert_eq!(
        list_page(&c, &ModelCatalogQuery::default(), &NONE_IN_USE)
            .unwrap()
            .total,
        9
    );
    let dist = list_page(
        &c,
        &ModelCatalogQuery {
            distributor_entries: Some(true),
            ..Default::default()
        },
        &NONE_IN_USE,
    )
    .unwrap();
    assert_eq!(ids(&dist), ["beta/beta-own"]);
    let in_use = list_page(
        &c,
        &ModelCatalogQuery {
            in_use: Some(true),
            ..Default::default()
        },
        &used,
    )
    .unwrap();
    assert_eq!(
        (ids(&in_use), in_use.rows[0].in_use),
        (vec!["beta/beta-own".to_owned()], Some(1))
    );

    let all = list_page(&c, &ModelCatalogQuery::default(), &used).unwrap();
    assert_eq!(
        ids(&all),
        [
            "alpha/alpha-nocost",
            "alpha/alpha-pro",
            "alpha/alpha-tiered",
            "alpha/alpha-free",
            "beta/beta-own",
            "alpha/alpha-old",
            "alpha/alpha-embed",
            "alpha/alpha-weird",
            "alpha/alpha-bare",
            "mine/m-1",
        ]
    );
    assert_eq!(
        (all.rows[1].model_key.as_str(), all.rows[1].provider_count),
        ("alpha/alpha-pro", 2)
    );
    assert_eq!(all.rows[9].model_key, "user:mine/m-1");
}

#[test]
fn the_list_filters_by_search_family_kind_source_and_model() {
    let c = listed_conn();
    let used = used();
    let query = |edit: &dyn Fn(&mut ModelCatalogQuery)| query(&c, &used, edit);
    assert_eq!(
        ids(&query(&|q| q.search = Some("  ALPHA pro ".into()))),
        ["alpha/alpha-pro"]
    );
    assert_eq!(
        ids(&query(&|q| q.search = Some("hub".into()))),
        ["alpha/alpha-pro", "beta/beta-own"]
    );
    // LIKE wildcards in the search match literally.
    assert_eq!(query(&|q| q.search = Some("%".into())).total, 0);
    assert_eq!(
        ids(&query(&|q| q.family = Some("beta".into()))),
        ["alpha/alpha-pro", "beta/beta-own"]
    );
    assert_eq!(query(&|q| q.kind = Some(CatalogKind::Embedding)).total, 1);
    assert_eq!(query(&|q| q.source = Some(CatalogSource::User)).total, 1);
    // A model's offers ignore the other filters.
    assert_eq!(
        ids(&query(&|q| {
            q.model_key = Some("alpha/alpha-pro".into());
            q.publisher = Some("nobody".into());
        })),
        ["alpha/alpha-pro", "beta/alpha-pro"]
    );
}

#[test]
fn the_list_pages_and_clamps_the_page_size() {
    let c = listed_conn();
    let used = used();
    let query = |edit: &dyn Fn(&mut ModelCatalogQuery)| query(&c, &used, edit);
    let paged = query(&|q| {
        q.offset = Some(8);
        q.limit = Some(5);
    });
    assert_eq!(
        (ids(&paged), paged.total, paged.families.len()),
        (
            vec!["alpha/alpha-bare".to_owned(), "mine/m-1".to_owned()],
            10,
            3
        )
    );
    assert_eq!(query(&|q| q.limit = Some(0)).rows.len(), 1);
    assert_eq!(query(&|q| q.limit = Some(100_000)).rows.len(), 10);
}

#[test]
fn the_mode_filters_follow_the_global_pricing_row() {
    let c = listed_conn();
    let used = used();
    c.execute_batch(
        "INSERT INTO model_pricing (provider_id, model_id, mode, per_call, currency, updated_at_ms)
             VALUES ('', 'alpha-pro', 'fixed', 0.05, 'CNY', 9);
         INSERT INTO model_pricing (provider_id, model_id, mode, input_per_mtok, output_per_mtok, updated_at_ms)
             VALUES ('p1', 'alpha-pro', 'custom', 1, 2, 9), ('p2', 'alpha-pro', 'dynamic', NULL, NULL, 9),
                    ('', 'beta-own', 'custom', 4, 5, 9);",
    )
    .unwrap();
    regroup(&c).unwrap();
    let page = list_page(&c, &ModelCatalogQuery::default(), &used).unwrap();
    let pro = page
        .rows
        .iter()
        .find(|r| r.model_id == "alpha-pro")
        .unwrap();
    assert_eq!(
        (
            pro.pricing.mode,
            pro.pricing.per_call,
            pro.pricing.currency.as_str(),
            pro.provider_overrides
        ),
        (PricingMode::Fixed, Some(0.05), "CNY", 2)
    );
    let query = |edit: &dyn Fn(&mut ModelCatalogQuery)| query(&c, &used, edit);
    assert_eq!(
        ids(&query(&|q| q.mode = Some(PricingFilter::Fixed))),
        ["alpha/alpha-pro"]
    );
    assert_eq!(
        ids(&query(&|q| q.mode = Some(PricingFilter::Custom))),
        ["beta/beta-own"]
    );
    assert_eq!(query(&|q| q.mode = Some(PricingFilter::Dynamic)).total, 8);
    assert_eq!(
        ids(&query(&|q| q.mode = Some(PricingFilter::None))),
        [
            "alpha/alpha-nocost",
            "alpha/alpha-weird",
            "alpha/alpha-bare"
        ]
    );
}

#[test]
fn listed_rows_carry_their_default_catalog_price() {
    let c = conn();
    apply_sync(
        &c,
        &[
            // The publisher's own priced offer: official.
            record("own", "priced"),
            // Only distributors of `lab/resold` price it: reference.
            ModelRecord {
                canonical_id: "lab/resold".into(),
                ..record("relay-a", "resold")
            },
            ModelRecord {
                canonical_id: "lab/resold".into(),
                ..record("relay-b", "resold")
            },
            // No offer has a price.
            ModelRecord {
                input_per_mtok: None,
                ..record("bare", "unpriced")
            },
        ],
        1,
        1,
    )
    .unwrap();
    for (family, key, basis) in [
        ("own", "own/priced", DefaultBasis::Official),
        ("relay-a", "lab/resold", DefaultBasis::Reference),
        ("bare", "bare/unpriced", DefaultBasis::None),
    ] {
        let page = query(&c, &NONE_IN_USE, &|q| q.family = Some(family.into()));
        let listed = page.rows.iter().find(|r| r.model_key == key).unwrap();
        let single = model_row(&c, key, 0).unwrap().unwrap();
        for row in [listed, &single] {
            assert_eq!(row.price_basis, Some(basis), "{key}");
            let referenced = basis == DefaultBasis::Reference;
            assert_eq!(
                row.basis_family.is_some(),
                basis != DefaultBasis::None,
                "{key}"
            );
            assert_eq!(!row.reference_from.is_empty(), referenced, "{key}");
        }
    }
    assert_eq!(model_row(&c, "nobody/none", 0).unwrap(), None);
}

#[test]
fn a_user_model_save_replaces_its_row_and_takes_the_family_name() {
    let c = listed_conn();
    let saved = save_user_model(&c, user_model(" mine ", "m-1"), 77).unwrap();
    assert_eq!(
        (saved.family.as_str(), saved.currency.as_str(), saved.source),
        ("mine", "USD", CatalogSource::User)
    );
    let mut again = user_model("mine", "m-1");
    again.name = String::new();
    again.input_per_mtok = None;
    let replaced = save_user_model(&c, again, 78).unwrap();
    assert_eq!(
        (replaced.name.as_str(), replaced.input_per_mtok),
        ("m-1", None)
    );
    assert_eq!(
        save_user_model(&c, user_model("alpha", "alpha-custom"), 79)
            .unwrap()
            .family_name,
        "Alpha Labs"
    );
}

#[test]
fn a_user_model_save_rejects_a_synced_owner_and_a_bad_input() {
    let c = listed_conn();
    assert!(matches!(
        save_user_model(&c, user_model("alpha", "alpha-pro"), 80),
        Err(AppError::Conflict { .. })
    ));
    let mut bad = user_model("mine", "bad");
    bad.input_per_mtok = Some(-1.0);
    assert!(matches!(
        save_user_model(&c, bad, 81),
        Err(AppError::Validation { .. })
    ));
}

#[test]
fn a_conflicting_user_model_save_changes_nothing() {
    let c = listed_conn();
    c.execute(
        "INSERT INTO model_pricing (provider_id, model_id, mode, input_per_mtok, output_per_mtok, updated_at_ms)
         VALUES ('', 'alpha-pro', 'custom', 7, 8, 1)",
        [],
    )
    .unwrap();
    regroup(&c).unwrap();
    let (catalog, pricing) = (
        table_rows(&c, "model_catalog"),
        table_rows(&c, "model_pricing"),
    );
    assert!(matches!(
        save_user_model(&c, user_model("alpha", "alpha-pro"), 80),
        Err(AppError::Conflict { .. })
    ));
    assert_eq!(table_rows(&c, "model_catalog"), catalog);
    assert_eq!(table_rows(&c, "model_pricing"), pricing);
}

#[test]
fn deleting_removes_only_a_user_model() {
    let c = listed_conn();
    assert!(!delete_user_model(&c, "alpha", "alpha-pro").unwrap());
    assert!(delete_user_model(&c, "mine", "m-1").unwrap());
    assert!(!delete_user_model(&c, "mine", "m-1").unwrap());
    assert_eq!(lookup_record(&c, "m-1").unwrap(), None);
}
