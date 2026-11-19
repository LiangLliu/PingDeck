//! Keys: balance persistence, the write-only management token, last-use times.

use super::{connection, key, provider, secret, seeded, single_endpoint_provider};
use crate::llm::balance_state::{
    BalanceErrorKind, BalanceErrorRecord, BalanceReport, BalanceUnit, StoredBalance,
};
use crate::llm::store::{
    KeyStrategy, LogWrite, RequestLogEntry, ResolvedKey, delete_key, delete_provider, get_key,
    get_resolved_key, insert_log, key_last_used, list_keys, list_resolved_keys, reveal_key,
    save_key, save_provider, set_key_balance, set_key_management_token, unique_key_name,
};
use crate::storage::secrets;
use pingdeck_base::error::AppError;
use pingdeck_base::secrets::SecretRef;

fn report(checked_at_ms: i64, remaining: f64) -> BalanceReport {
    BalanceReport {
        supported: true,
        unit: BalanceUnit::Usd,
        remaining: Some(remaining),
        remaining_usd: Some(remaining),
        checked_at_ms,
        ..BalanceReport::default()
    }
}

#[test]
fn balance_and_management_token_persistence() {
    let c = connection();
    assert!(matches!(
        set_key_balance(&c, "nope", &StoredBalance::default(), 1),
        Err(AppError::NotFound { .. })
    ));
    assert!(matches!(
        set_key_management_token(&c, "nope", Some("t"), 1),
        Err(AppError::NotFound { .. })
    ));
    seeded(&c, "p1", "k1");
    save_key(&c, &key("k10", "p1"), Some("sk-tenth-key-123"), 1).unwrap();
    set_key_management_token(&c, "k10", Some("console-token-10"), 1).unwrap();

    let good = StoredBalance {
        last_good: Some(report(1_000, 12.4)),
        error: None,
        stale: false,
    };
    set_key_balance(&c, "k1", &good, 1_005).unwrap();
    set_key_balance(
        &c,
        "k1",
        &StoredBalance {
            last_good: good.last_good.clone(),
            error: Some(BalanceErrorRecord {
                at_ms: 2_000,
                kind: BalanceErrorKind::Auth,
                status: Some(401),
                message: "rejected".into(),
                retry_at_ms: None,
            }),
            stale: true,
        },
        2_000,
    )
    .unwrap();
    let read = get_key(&c, "k1").unwrap().unwrap();
    assert_eq!(read.balance_attempt_ms, Some(2_000));
    let bal = read.balance.unwrap();
    assert_eq!(bal.last_good, good.last_good);
    assert!(bal.stale && bal.error.is_some());

    // Management token is write-only, preserved on UI save alongside balance, and deleted with key.
    set_key_management_token(&c, "k1", Some("console-token-value-1"), 2).unwrap();
    let mut edited = key("k1", "p1");
    edited.name = "Renamed".into();
    edited.api_user_id = Some("  12 ".into());
    let saved = save_key(&c, &edited, None, 3).unwrap();
    assert_eq!(saved.api_user_id.as_deref(), Some("12"));
    assert!(saved.balance.is_some() && saved.has_management_token);
    assert!(
        !saved
            .management_token_hint
            .contains("console-token-value-1")
    );
    assert!(
        !serde_json::to_string(&saved)
            .unwrap()
            .contains("console-token-value-1")
    );

    let resolved = get_resolved_key(&c, "k1").unwrap().unwrap();
    assert_eq!(
        resolved.management_token.as_deref(),
        Some("console-token-value-1")
    );
    assert!(!format!("{resolved:?}").contains("console-token-value-1"));
    assert_eq!(list_resolved_keys(&c).unwrap().len(), 2);

    // Recovery, clearing, corrupt JSON, and prefix-safe delete.
    set_key_balance(
        &c,
        "k1",
        &StoredBalance {
            last_good: Some(report(3_000, 4.0)),
            error: None,
            stale: false,
        },
        3_000,
    )
    .unwrap();
    assert!(
        get_key(&c, "k1")
            .unwrap()
            .unwrap()
            .balance
            .unwrap()
            .error
            .is_none()
    );
    set_key_balance(&c, "k1", &StoredBalance::default(), 4_000).unwrap();
    assert!(get_key(&c, "k1").unwrap().unwrap().balance.is_none());
    c.execute(
        "UPDATE llm_keys SET balance_json = 'oops', balance_error_json = '{'",
        [],
    )
    .unwrap();
    assert!(list_keys(&c, None).unwrap()[0].balance.is_none());

    set_key_management_token(&c, "k1", Some("   "), 4).unwrap();
    assert!(!get_key(&c, "k1").unwrap().unwrap().has_management_token);
    set_key_management_token(&c, "k1", Some("console-token-value-1"), 5).unwrap();
    delete_key(&c, "k1").unwrap();
    assert_eq!(secret(&c, "llm.key.k1"), None);
    assert_eq!(secret(&c, "llm.key.k1.management_token"), None);
    assert!(secret(&c, "llm.key.k10.management_token").is_some());
}

#[test]
fn revealing_a_key_never_echoes_it_on_error() {
    let c = connection();
    seeded(&c, "p1", "k1");
    let value = "sk-reveal-0123456789abcdef";
    save_key(&c, &key("k2", "p1"), Some(value), 1).unwrap();
    assert_eq!(reveal_key(&c, "k2").unwrap(), value);
    assert!(
        !reveal_key(&c, "nope")
            .unwrap_err()
            .message()
            .contains(value)
    );
}

#[test]
fn last_use_is_the_newest_row_per_key() {
    let c = connection();
    seeded(&c, "p1", "k1");
    save_key(&c, &key("k2", "p1"), Some("sk-reveal-0123456789abcdef"), 1).unwrap();
    for (key_id, ts, source) in [
        ("k1", 100, "gateway"),
        ("k1", 300, "agent"),
        ("k2", 50, "probe"),
    ] {
        let entry = RequestLogEntry {
            ts_ms: ts,
            protocol_in: "openai_chat".into(),
            requested_model: "m".into(),
            key_id: Some(key_id.into()),
            status: 200,
            outcome: "success".into(),
            source: source.into(),
            ..RequestLogEntry::default()
        };
        insert_log(
            &c,
            &LogWrite {
                entry: &entry,
                capture: None,
                rollup: None,
            },
        )
        .unwrap();
    }
    let used = key_last_used(&c, 0).unwrap();
    assert_eq!((used.get("k1"), used.get("k2")), (Some(&300), Some(&50)));
}

#[test]
fn key_names_and_notes_are_validated_and_trimmed() {
    let c = connection();
    seeded(&c, "p1", "k1");
    let mut k = key("k1", "p1");
    k.name = "x".repeat(61);
    assert!(matches!(
        save_key(&c, &k, None, 2),
        Err(AppError::Validation { .. })
    ));
    k.name = "x".repeat(60);
    k.notes = Some("n".repeat(201));
    assert!(matches!(
        save_key(&c, &k, None, 2),
        Err(AppError::Validation { .. })
    ));
    k.notes = Some("  backup  ".into());
    assert_eq!(
        save_key(&c, &k, None, 2).unwrap().notes.as_deref(),
        Some("backup")
    );
}

#[test]
fn omitted_key_strategy_fields_keep_the_stored_values() {
    let c = connection();
    seeded(&c, "p1", "k1");
    let mut p = provider("p1");
    p.key_strategy = Some(KeyStrategy::RoundRobin);
    p.key_cooldown = Some(false);
    save_provider(&c, &p, 2).unwrap();
    p.key_strategy = None;
    p.key_cooldown = None;
    let kept = save_provider(&c, &p, 3).unwrap();
    assert_eq!(
        (kept.key_strategy, kept.key_cooldown),
        (Some(KeyStrategy::RoundRobin), Some(false))
    );
}

#[test]
fn debug_output_hides_the_resolved_key_secret() {
    let resolved = ResolvedKey {
        key: key("k1", "p1"),
        secret: "sk-test-secret".into(),
        management_token: Some("mgmt-token-value".into()),
    };
    let text = format!("{resolved:?}");
    assert!(
        !text.contains("sk-test-secret") && text.contains("***"),
        "{text}"
    );
}

#[test]
fn key_secrets_round_trip_keep_clear_and_never_serialize() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 1).unwrap();
    let saved = save_key(&c, &key("k1", "p1"), Some("sk-live-abcdef9f2a"), 5).unwrap();
    assert!(!saved.secret_hint.is_empty());
    assert!(
        !saved.secret_hint.contains("sk-live-abcdef9f2a"),
        "{}",
        saved.secret_hint
    );
    let resolved = get_resolved_key(&c, "k1").unwrap().unwrap();
    assert_eq!(resolved.secret, "sk-live-abcdef9f2a");
    assert_eq!(resolved.key, saved);
    assert_eq!(
        list_resolved_keys(&c).unwrap()[0].secret,
        "sk-live-abcdef9f2a"
    );
    // Nothing that is listed or serialized carries the plaintext, and `llm_keys` never stores it.
    let json = serde_json::to_string(&list_keys(&c, None).unwrap()).unwrap();
    assert!(
        !json.contains("sk-live-abcdef9f2a") && json.contains("secret_hint"),
        "{json}"
    );
    let leaked: i64 = c
        .query_row(
            "SELECT count(*) FROM llm_keys \
             WHERE instr(name || secret_hint || notes || group_name, 'sk-live-abcdef9f2a') > 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leaked, 0);

    // `None` keeps the secret and its hint; `Some(new)` replaces; `Some("")` clears.
    let kept = save_key(&c, &key("k1", "p1"), None, 6).unwrap();
    assert_eq!(kept.secret_hint, saved.secret_hint);
    assert_eq!(
        get_resolved_key(&c, "k1").unwrap().unwrap().secret,
        "sk-live-abcdef9f2a"
    );
    save_key(&c, &key("k1", "p1"), Some("sk-other-123456"), 7).unwrap();
    assert_eq!(
        get_resolved_key(&c, "k1").unwrap().unwrap().secret,
        "sk-other-123456"
    );
    let cleared = save_key(&c, &key("k1", "p1"), Some(""), 8).unwrap();
    assert_eq!(cleared.secret_hint, "");
    assert_eq!(get_resolved_key(&c, "k1").unwrap().unwrap().secret, "");
}

#[test]
fn deleting_a_key_or_provider_removes_its_secrets() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 1).unwrap();
    save_key(&c, &key("k1", "p1"), Some("sk-one-111111"), 1).unwrap();
    save_key(&c, &key("k2", "p1"), Some("sk-two-222222"), 1).unwrap();
    let stored = |id: &str| secrets::get(&c, SecretRef::llm_key(id).as_str()).unwrap();
    assert_eq!(stored("k1").as_deref(), Some("sk-one-111111"));
    delete_key(&c, "k1").unwrap();
    assert_eq!(stored("k1"), None);
    assert_eq!(stored("k2").as_deref(), Some("sk-two-222222"));
    delete_provider(&c, "p1").unwrap();
    assert_eq!(stored("k2"), None);
}

#[test]
fn saving_a_key_for_a_missing_provider_is_not_found() {
    let c = connection();
    assert!(matches!(
        save_key(&c, &key("k1", "ghost"), Some("sk-test"), 1),
        Err(AppError::NotFound { .. })
    ));
    assert!(list_keys(&c, None).unwrap().is_empty());
}

#[test]
fn a_key_never_moves_to_another_provider() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 10).unwrap();
    save_provider(&c, &single_endpoint_provider("p2"), 10).unwrap();
    save_key(&c, &key("k1", "p1"), Some("sk-of-p1"), 10).unwrap();
    assert!(matches!(
        save_key(&c, &key("k1", "p2"), Some("sk-from-p2"), 20),
        Err(AppError::Conflict { .. })
    ));
    assert_eq!(reveal_key(&c, "k1").unwrap(), "sk-of-p1");
}

#[test]
fn a_new_key_after_a_deletion_sorts_last_with_a_name_of_its_own() {
    // Positions and default names never collide with the keys that remain.
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 1).unwrap();
    for id in ["k1", "k2", "k3"] {
        save_key(&c, &key(id, "p1"), Some("sk-x"), 1).unwrap();
    }
    delete_key(&c, "k2").unwrap();
    // The page sends `keys.length`, which is now 2: the new key still goes after k3.
    let mut fresh = key("k4", "p1");
    fresh.position = 2;
    save_key(&c, &fresh, Some("sk-y"), 1).unwrap();
    let order: Vec<(String, i64)> = list_keys(&c, Some("p1"))
        .unwrap()
        .into_iter()
        .map(|k| (k.id, k.position))
        .collect();
    assert_eq!(
        order,
        [("k1".into(), 0), ("k3".into(), 2), ("k4".into(), 3)]
    );
    assert_eq!(unique_key_name(&c, "p1", "k1").unwrap(), "k1 2");
    assert_eq!(unique_key_name(&c, "p1", "Default").unwrap(), "Default");
}
