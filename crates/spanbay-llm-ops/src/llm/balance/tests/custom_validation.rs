//! The scope rule of a custom query: the query's scope is the report's, and a query without one
//! sends nothing.

use super::*;

#[tokio::test]
async fn the_scope_of_the_query_is_the_scope_of_the_report_and_is_required_to_run() {
    let http = ScriptedHttp::new().route(SafeMethod::Get, ZHIPU_PATH, ok(&zhipu_body()));
    for scope in [BalanceScope::Key, BalanceScope::Account] {
        let mut custom = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
        custom.scope = Some(scope);
        let t = zhipu_with(&custom);
        assert_eq!(report_of(check(&http, &t).await).scope, scope);
    }
    let mut unscoped = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
    unscoped.scope = None;
    let before = http.count();
    let t = zhipu_with(&unscoped);
    assert_eq!(
        error_of(check(&http, &t).await).kind,
        BalanceErrorKind::Blocked
    );
    assert_eq!(http.count(), before, "an unscoped query sends nothing");
    let stored = serde_json::to_value(&unscoped).unwrap_or_default();
    assert_eq!(stored["scope"], Value::Null);
    let restored: CustomBalanceQuery =
        serde_json::from_value(stored).unwrap_or_else(|e| panic!("{e}"));
    assert!(restored.validate().is_err());
}
