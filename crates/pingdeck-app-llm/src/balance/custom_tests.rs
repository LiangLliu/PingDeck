//! Tests of the custom balance query use-cases: save (scope, cross-host confirmation, header
//! secrets), delete, "Test query" and the way a saved query wins over the adapter.

use super::super::test_support::{MINUTE, NOW, SECRET, deepseek, events_of, header, open};
use super::tests::{deepseek_http, run};
use super::*;
use crate::llm::balance_state::{
    BalanceAuth, BalanceScope, BalanceUnit, CustomHeaderRef, UnitName,
};
use crate::llm::safe_http::SafeMethod;
use crate::llm::safe_http::scripted::{Reply, ScriptedHttp};
use pingdeck_store::storage::secrets;
use serde_json::json;

const TENANT: &str = "tenant-secret-value-7777";

fn query(url: &str) -> CustomBalanceQuery {
    CustomBalanceQuery {
        url: url.to_owned(),
        headers: Vec::new(),
        auth: BalanceAuth::Bearer,
        remaining_path: "$.data.available_balance".to_owned(),
        used_path: None,
        total_path: None,
        unit: UnitName::Cny,
        scope: Some(BalanceScope::Key),
        unit_label: None,
        divisor: None,
        cross_host: false,
        resolved_headers: ResolvedHeaders::default(),
    }
}

fn with_tenant_header(mut query: CustomBalanceQuery) -> CustomBalanceQuery {
    query.headers = vec![CustomHeaderRef {
        name: "X-Tenant".to_owned(),
        secret_ref: "ignored".to_owned(),
    }];
    query
}

fn tenant(value: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("X-Tenant".to_owned(), value.to_owned())])
}

fn own_url(path: &str) -> String {
    format!("https://api.deepseek.com{path}")
}

#[test]
fn custom_query_save_validates_scope_path_and_cross_host_confirmation() {
    let state = open();
    let (provider, _) = deepseek(&state);
    let mut no_scope = query(&own_url("/custom"));
    no_scope.scope = None;
    assert!(matches!(
        llm_balance_custom_save_impl(&state, &provider.id, no_scope, &BTreeMap::new(), false),
        Err(AppError::Validation { .. })
    ));

    let other = query("https://billing.other.example/api/balance");
    assert!(matches!(
        llm_balance_custom_save_impl(&state, &provider.id, other.clone(), &BTreeMap::new(), false),
        Err(AppError::ConfirmationRequired { .. })
    ));
    let saved = llm_balance_custom_save_impl(&state, &provider.id, other, &BTreeMap::new(), true)
        .expect("confirmed");
    assert!(saved.custom_balance.expect("stored").cross_host);
}

#[tokio::test]
async fn saved_query_overrides_adapter_stores_header_secrets_and_delete_restores_adapter() {
    let state = open();
    let (provider, _) = deepseek(&state);
    let seen = events_of(&state);
    let saved = llm_balance_custom_save_impl(
        &state,
        &provider.id,
        with_tenant_header(query(&own_url("/custom/balance"))),
        &tenant(TENANT),
        false,
    )
    .expect("save");
    assert!(!serde_json::to_string(&saved).unwrap().contains(TENANT));
    let reference = format!("llm.{}.balance.header.X-Tenant", provider.id);
    assert_eq!(
        secrets::get(state.store().connection(), &reference)
            .unwrap()
            .as_deref(),
        Some(TENANT)
    );

    let http = ScriptedHttp::new().route(
        SafeMethod::Get,
        "/custom/balance",
        vec![Reply::json(
            200,
            &json!({"data": {"available_balance": 12.5}}),
        )],
    );
    let custom = run(
        &state,
        &provider.id,
        None,
        RefreshTrigger::Manual,
        NOW,
        &http,
    )
    .await
    .expect("custom");
    let report = custom[0].report.as_ref().expect("report");
    assert_eq!(
        (report.unit, report.scope, report.remaining),
        (BalanceUnit::Cny, BalanceScope::Key, Some(12.5))
    );
    assert_eq!(
        header(&http.requests()[0], "x-tenant").as_deref(),
        Some(TENANT)
    );

    llm_balance_custom_delete_impl(&state, &provider.id).expect("delete");
    assert!(seen.lock().unwrap().contains(&LlmEvent::ProvidersChanged));
    let adapter_http = deepseek_http("86.40");
    let restored = run(
        &state,
        &provider.id,
        None,
        RefreshTrigger::Manual,
        NOW + MINUTE,
        &adapter_http,
    )
    .await
    .expect("adapter");
    assert_eq!(
        restored[0].report.as_ref().and_then(|r| r.remaining),
        Some(86.4)
    );
}

#[tokio::test]
async fn test_query_redacts_body_and_blocks_unconfirmed_hosts() {
    let state = open();
    let (provider, _) = deepseek(&state);
    let http = ScriptedHttp::new().route(
        SafeMethod::Get,
        "/custom",
        vec![Reply::json(
            200,
            &json!({"data": {"available_balance": 49.59}, "echo": SECRET, "tenant": TENANT}),
        )],
    );
    let dto = query_test(
        &state,
        &provider.id,
        None,
        with_tenant_header(query(&own_url("/custom"))),
        tenant(TENANT),
        Some(&http),
    )
    .await
    .expect("test");
    assert_eq!(dto.status, Some(200));
    assert_eq!(dto.preview.unwrap().remaining, Some(49.59));
    assert!(!dto.body.contains(SECRET) && !dto.body.contains(TENANT));

    let blocked_http = ScriptedHttp::new();
    let blocked = query_test(
        &state,
        &provider.id,
        None,
        query("https://billing.other.example/x"),
        BTreeMap::new(),
        Some(&blocked_http),
    )
    .await
    .expect("dto");
    assert_eq!(
        blocked.error.as_ref().map(|e| e.kind),
        Some(BalanceErrorKind::Blocked)
    );
    assert_eq!(blocked_http.count(), 0);
}
