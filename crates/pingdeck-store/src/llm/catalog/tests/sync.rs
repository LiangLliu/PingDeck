//! Applying a models.dev sync, recording failures and the automatic schedule.

use super::*;

#[test]
fn a_sync_counts_added_updated_and_unchanged_rows() {
    let c = conn();
    let records = sample_records();
    let first = apply_sync(&c, &records, 1_000, 42).unwrap();
    assert_eq!(
        (
            first.added,
            first.updated,
            first.unchanged,
            first.total_remote
        ),
        (10, 0, 0, 10)
    );

    // Only the sync stamp differs: nothing is rewritten.
    let mut later = sample_records();
    for r in &mut later {
        r.synced_at_ms = 2_000;
    }
    let second = apply_sync(&c, &later, 2_000, 5).unwrap();
    assert_eq!((second.added, second.updated, second.unchanged), (0, 0, 10));
    let min_stamp: i64 = c
        .query_row("SELECT MIN(synced_at_ms) FROM model_catalog", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(min_stamp, 100);
}

#[test]
fn a_sync_never_overwrites_user_rows_or_pricing() {
    let c = conn();
    apply_sync(&c, &sample_records(), 1_000, 42).unwrap();
    let mut mine = user_model("alpha", "my-private-model");
    mine.name = "Keep me".into();
    save_user_model(&c, mine, 11).unwrap();
    c.execute_batch(
        "INSERT INTO model_pricing (provider_id, model_id, mode, input_per_mtok, output_per_mtok, updated_at_ms)
         VALUES ('', 'alpha-pro', 'custom', 7, 8, 1), ('p1', 'my-private-model', 'custom', 1, 2, 1);",
    )
    .unwrap();

    let mut changed = sample_records();
    changed.retain(|r| r.model_id != "alpha-old");
    changed
        .iter_mut()
        .find(|r| r.model_id == "beta-own")
        .unwrap()
        .input_per_mtok = Some(9.0);
    changed.push(record("gamma", "new-model"));
    changed.push(ModelRecord {
        name: "Overwrite attempt".into(),
        input_per_mtok: Some(99.0),
        ..record("alpha", "my-private-model")
    });
    for r in &mut changed {
        r.synced_at_ms = 3_000;
    }
    let third = apply_sync(&c, &changed, 3_000, 5).unwrap();
    assert_eq!((third.added, third.updated, third.unchanged), (1, 1, 9));
    assert_eq!(
        lookup_record(&c, "beta-own")
            .unwrap()
            .unwrap()
            .input_per_mtok,
        Some(9.0)
    );
    assert!(lookup_record(&c, "alpha-old").unwrap().is_some());
    let kept = lookup_record(&c, "my-private-model").unwrap().unwrap();
    assert_eq!(
        (kept.name.as_str(), kept.input_per_mtok, kept.source),
        ("Keep me", Some(0.5), CatalogSource::User)
    );
    let pricing_count: i64 = c
        .query_row("SELECT COUNT(*) FROM model_pricing", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pricing_count, 2);
}

#[test]
fn a_failed_attempt_is_recorded_truncated_and_cleared_by_a_success() {
    let c = conn();
    apply_sync(&c, &sample_records(), 3_000, 5).unwrap();
    record_failure(&c, &"x".repeat(900), 4_000).unwrap();
    let st = sync_state(&c).unwrap();
    assert_eq!(
        (st.last_success_ms, st.last_attempt_ms),
        (Some(3_000), Some(4_000))
    );
    assert_eq!(st.last_error.as_ref().map(|e| e.chars().count()), Some(500));
    apply_sync(&c, &sample_records(), 5_000, 1).unwrap();
    assert_eq!(sync_state(&c).unwrap().last_error, None);
}

#[test]
fn the_status_classifies_the_last_failure() {
    let fresh = conn();
    record_failure(
        &fresh,
        "Model catalog download failed: HTTP 502 Bad Gateway",
        1,
    )
    .unwrap();
    let fst = sync_status(&fresh, false).unwrap();
    assert_eq!(
        (fst.last_error_kind, fst.last_error_http_status),
        (Some(K::Http), Some(502))
    );
}

#[test]
fn the_next_sync_is_due_one_interval_after_success_and_after_a_failure_backoff() {
    let mut s = SyncState {
        snapshot_at_ms: 10 * DAY,
        ..SyncState::default()
    };
    assert_eq!(s.next_due_ms(), 17 * DAY);
    s.last_success_ms = Some(20 * DAY);
    s.last_attempt_ms = Some(20 * DAY);
    assert_eq!(s.next_due_ms(), 27 * DAY);
    s.auto_interval_days = 1;
    s.last_attempt_ms = Some(21 * DAY + 3_600_000);
    s.last_error = Some("offline".into());
    assert_eq!(s.next_due_ms(), 21 * DAY + 3_600_000 + HOURS_6);
}

#[test]
fn the_sync_interval_is_1_7_or_30_days() {
    let c = conn();
    for days in [1, 7, 30] {
        save_config(&c, days).unwrap();
        assert_eq!(sync_state(&c).unwrap().auto_interval_days, days);
    }
    for bad in [0, 2, 14, 31] {
        assert!(matches!(
            save_config(&c, bad),
            Err(AppError::Validation { .. })
        ));
    }
}

#[test]
fn a_failed_attempt_backs_off_the_automatic_sync_by_6h() {
    let c = conn();
    apply_sync(&c, &sample_records(), 5_000, 1).unwrap();
    save_config(&c, 1).unwrap();
    assert!(!auto_sync_due(&c, 5_000 + DAY - 1).unwrap());
    assert!(auto_sync_due(&c, 5_000 + DAY).unwrap());
    record_failure(&c, "offline", 5_000 + DAY).unwrap();
    assert!(!auto_sync_due(&c, 5_000 + DAY + HOURS_6 - 1).unwrap());
    assert!(auto_sync_due(&c, 5_000 + DAY + HOURS_6).unwrap());
    assert_eq!(
        sync_status(&c, true).unwrap().next_due_ms,
        Some(5_000 + DAY + HOURS_6)
    );
    assert_eq!(sync_status(&c, false).unwrap().next_due_ms, None);
}
