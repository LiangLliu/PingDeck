//! Optimistic versions (stale writes) and confirmed host changes of providers and keys.

use super::{connection, key, single_endpoint_provider};
use crate::llm::store::providers::save_provider_guarded;
use crate::llm::store::{
    Provider, ProviderKey, SaveProviderOptions, get_key, get_provider, reveal_key, save_key,
    save_key_guarded, save_provider, save_provider_with, set_key_management_token,
};
use pingdeck_base::error::AppError;

fn is_stale<T: std::fmt::Debug>(result: Result<T, AppError>) -> bool {
    matches!(result, Err(ref e @ AppError::Conflict { .. })
        if e.code() == Some(pingdeck_base::error::ErrorCode::StaleWrite))
}

#[test]
fn a_save_of_an_old_version_is_refused_and_writes_nothing() {
    let c = connection();
    let read = save_provider(&c, &single_endpoint_provider("p1"), 10).unwrap();
    let elsewhere = Provider {
        name: "Imported".into(),
        ..read.clone()
    };
    save_provider(&c, &elsewhere, 20).unwrap();
    let mine = Provider {
        name: "Mine".into(),
        ..read.clone()
    };
    assert!(is_stale(save_provider_guarded(
        &c,
        &mine,
        Some(read.updated_at_ms),
        30
    )));
    assert_eq!(get_provider(&c, "p1").unwrap().unwrap().name, "Imported");
    // A row deleted elsewhere is not recreated by a page that still holds it.
    assert!(is_stale(save_provider_guarded(
        &c,
        &single_endpoint_provider("gone"),
        Some(10),
        30
    )));
    assert!(get_provider(&c, "gone").unwrap().is_none());

    let key_read = save_key(&c, &key("k1", "p1"), Some("sk-one-secret"), 10).unwrap();
    save_key(&c, &key_read, None, 20).unwrap();
    let renamed = ProviderKey {
        name: "renamed".into(),
        ..key_read.clone()
    };
    assert!(is_stale(save_key_guarded(
        &c,
        &renamed,
        Some("sk-other-secret"),
        Some(key_read.updated_at_ms),
        30
    )));
    let stored = get_key(&c, "k1").unwrap().unwrap();
    assert_eq!(stored.name, "k1");
    assert_eq!(reveal_key(&c, "k1").unwrap(), "sk-one-secret");
}

#[test]
fn a_save_without_a_version_still_succeeds() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 10).unwrap();
    save_provider(&c, &single_endpoint_provider("p1"), 20).unwrap();
    let saved = save_provider_guarded(&c, &single_endpoint_provider("p1"), None, 30).unwrap();
    assert_eq!(saved.updated_at_ms, 30);
    save_key(&c, &key("k1", "p1"), None, 10).unwrap();
    let saved = save_key_guarded(&c, &key("k1", "p1"), None, None, 30).unwrap();
    assert_eq!(saved.updated_at_ms, 30);
    let current = save_key_guarded(&c, &saved, None, Some(30), 40).unwrap();
    assert_eq!(current.updated_at_ms, 40);
}

#[test]
fn two_writes_in_one_millisecond_never_share_a_version() {
    let c = connection();
    let first = save_provider(&c, &single_endpoint_provider("p1"), 5).unwrap();
    let second = save_provider(&c, &single_endpoint_provider("p1"), 5).unwrap();
    // A clock that went back still moves the version forward.
    let third = save_provider(&c, &single_endpoint_provider("p1"), 3).unwrap();
    assert_eq!(
        (
            first.updated_at_ms,
            second.updated_at_ms,
            third.updated_at_ms
        ),
        (5, 6, 7)
    );
    assert!(is_stale(save_provider_guarded(
        &c,
        &single_endpoint_provider("p1"),
        Some(6),
        7
    )));
    let k1 = save_key(&c, &key("k1", "p1"), None, 5).unwrap();
    let k2 = save_key(&c, &key("k1", "p1"), None, 5).unwrap();
    assert_eq!((k1.updated_at_ms, k2.updated_at_ms), (5, 6));
}

fn needs_host_confirm<T: std::fmt::Debug>(result: Result<T, AppError>) -> bool {
    matches!(result, Err(ref e @ AppError::Conflict { .. })
        if e.code() == Some(pingdeck_base::error::ErrorCode::HostChangeNeedsConfirm))
}

#[test]
fn a_new_path_on_the_same_host_needs_no_confirmation() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 10).unwrap();
    let moved = Provider {
        base_url: "https://EXAMPLE.test:443/api".into(),
        ..single_endpoint_provider("p1")
    };
    assert_eq!(
        save_provider(&c, &moved, 20).unwrap().base_url,
        "https://EXAMPLE.test:443/api"
    );
}

#[test]
fn another_host_without_confirmation_is_refused_and_writes_nothing() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 10).unwrap();
    save_key(&c, &key("k1", "p1"), Some("sk-kept-secret"), 10).unwrap();
    let moved = Provider {
        base_url: "https://evil.test".into(),
        ..single_endpoint_provider("p1")
    };
    assert!(needs_host_confirm(save_provider(&c, &moved, 20)));
    assert_eq!(
        get_provider(&c, "p1").unwrap().unwrap().base_url,
        "https://example.test"
    );
    assert!(get_key(&c, "k1").unwrap().unwrap().enabled);
}

#[test]
fn a_confirmed_host_change_switches_keys_off_and_forgets_the_old_host() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 10).unwrap();
    let stored = save_key(&c, &key("k1", "p1"), Some("sk-kept-secret"), 10).unwrap();
    save_key(
        &c,
        &ProviderKey {
            api_user_id: Some("42".into()),
            ..stored
        },
        None,
        11,
    )
    .unwrap();
    set_key_management_token(&c, "k1", Some("console-token"), 11).unwrap();
    c.execute(
        "UPDATE llm_keys SET balance_json = '{}' WHERE id = 'k1'",
        [],
    )
    .unwrap();
    c.execute(
        "UPDATE llm_providers SET quota_per_unit = 500000, gateway_family = 'new_api' WHERE id = 'p1'",
        [],
    )
    .unwrap();
    let moved = Provider {
        base_url: "https://other.test".into(),
        ..single_endpoint_provider("p1")
    };
    let options = SaveProviderOptions {
        confirm_host_change: true,
        ..SaveProviderOptions::default()
    };
    let saved = save_provider_with(&c, &moved, options, 20).unwrap();
    assert_eq!(saved.base_url, "https://other.test");
    assert!(saved.needs_resniff && saved.quota_per_unit.is_none());
    assert_eq!(
        saved.gateway_family,
        crate::llm::wire::GatewayFamily::UnknownCompatible
    );
    let key = get_key(&c, "k1").unwrap().unwrap();
    assert!(!key.enabled && key.api_user_id.is_none());
    let balance_gone: bool = c
        .query_row("SELECT balance_json IS NULL FROM llm_keys", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(balance_gone);
    assert!(!key.has_management_token);
    assert_eq!(reveal_key(&c, "k1").unwrap(), "sk-kept-secret");
}
