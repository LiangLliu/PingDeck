//! The stored shapes of balance readings and their rules: what the custom-query form may save,
//! the stored form of its headers, the same-origin rule, the error class names, the stale rule and
//! the evidence grades.

use serde_json::json;

use super::*;

/// A header value the custom-query tests resolve from the secret store.
const SECRET_VALUE: &str = "super-secret-header-value-42";
/// The Zhipu account endpoint the custom-query tests call.
const ZHIPU_URL: &str = "https://open.bigmodel.cn/api/biz/account/query-customer-account-report";
/// Milliseconds per minute.
const MIN: i64 = 60_000;

/// A valid custom query of `url` reading `$.data.availableBalance` for the account.
fn query(url: &str, auth: BalanceAuth, unit: UnitName) -> CustomBalanceQuery {
    CustomBalanceQuery {
        url: url.to_owned(),
        headers: Vec::new(),
        auth,
        remaining_path: "$.data.availableBalance".to_owned(),
        used_path: None,
        total_path: None,
        unit,
        scope: Some(BalanceScope::Account),
        unit_label: None,
        divisor: None,
        cross_host: false,
        resolved_headers: ResolvedHeaders::default(),
    }
}

#[test]
fn validation_refuses_what_the_form_must_not_save() {
    let good = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
    assert_eq!(good.validate(), Ok(()));
    let bad = |edit: &dyn Fn(&mut CustomBalanceQuery)| {
        let mut query = good.clone();
        edit(&mut query);
        query.validate().err().unwrap_or_default()
    };
    assert!(
        bad(&|q| q.scope = None).contains("key or the account"),
        "a query without a scope cannot be saved"
    );
    assert!(!bad(&|q| q.url = "ftp://x.test/a".to_owned()).is_empty());
    assert!(!bad(&|q| q.url = "not a url".to_owned()).is_empty());
    assert!(!bad(&|q| q.url = "https://user:pw@x.test/a".to_owned()).is_empty());
    assert!(
        bad(&|q| q.url = "https://x.test/a?api_key=abcdef123456".to_owned()).contains("credential")
    );
    assert!(
        bad(&|q| q.url = "https://x.test/a?token=abcdef123456".to_owned()).contains("credential")
    );
    assert!(!bad(&|q| q.remaining_path = "$..[".to_owned()).is_empty());
    assert!(!bad(&|q| q.used_path = Some("data.x[".to_owned())).is_empty());
    assert!(
        !bad(&|q| q.unit = UnitName::Custom).is_empty(),
        "a custom unit needs a label"
    );
    assert!(
        !bad(&|q| {
            q.unit = UnitName::Custom;
            q.unit_label = Some("x".repeat(25));
        })
        .is_empty()
    );
    assert!(
        !bad(&|q| q.divisor = Some(500_000.0)).is_empty(),
        "a divisor needs the quota unit"
    );
    assert!(
        !bad(&|q| {
            q.unit = UnitName::Quota;
            q.divisor = Some(0.0);
        })
        .is_empty()
    );
    for name in ["Host", "Cookie", "Content-Length", "bad name", "", "Accept"] {
        let header = CustomHeaderRef {
            name: name.to_owned(),
            secret_ref: "ref".to_owned(),
        };
        assert!(
            !bad(&|q| q.headers = vec![header.clone()]).is_empty(),
            "{name}"
        );
    }
    let twice = vec![
        CustomHeaderRef {
            name: "X-A".to_owned(),
            secret_ref: "r".to_owned(),
        },
        CustomHeaderRef {
            name: "x-a".to_owned(),
            secret_ref: "r2".to_owned(),
        },
    ];
    assert!(!bad(&|q| q.headers = twice.clone()).is_empty());
    let collides = vec![CustomHeaderRef {
        name: "Authorization".to_owned(),
        secret_ref: "r".to_owned(),
    }];
    assert!(
        !bad(&|q| q.headers = collides.clone()).is_empty(),
        "the auth header is set by auth"
    );
    let mut no_auth = good.clone();
    no_auth.auth = BalanceAuth::None;
    no_auth.headers = collides;
    assert_eq!(
        no_auth.validate(),
        Ok(()),
        "with auth none the user may name Authorization"
    );
    let mut labelled = good;
    labelled.unit = UnitName::Custom;
    labelled.unit_label = Some("credits".to_owned());
    assert_eq!(labelled.validate(), Ok(()));
}

#[test]
fn the_stored_form_never_holds_a_header_value() {
    let mut custom = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
    custom.headers = vec![CustomHeaderRef {
        name: "X-Org".to_owned(),
        secret_ref: "llm.custom.1.x-org".to_owned(),
    }];
    custom.resolved_headers = ResolvedHeaders(vec![("X-Org".to_owned(), SECRET_VALUE.to_owned())]);
    let stored = serde_json::to_string(&custom).unwrap_or_default();
    assert!(
        !stored.contains(SECRET_VALUE) && !stored.contains("sk-"),
        "{stored}"
    );
    assert!(stored.contains("llm.custom.1.x-org"));
    assert!(!format!("{custom:?}").contains(SECRET_VALUE));
    let restored: CustomBalanceQuery =
        serde_json::from_str(&stored).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(restored.resolved_headers, ResolvedHeaders::default());
    assert_eq!(restored.headers, custom.headers);
    assert!(
        serde_json::from_str::<CustomBalanceQuery>(
            &stored.replace("\"cross_host\"", "\"resolved_headers\":[],\"cross_host\"")
        )
        .is_err(),
        "resolved values are not part of the stored shape"
    );
    let unit_json = serde_json::to_value(&custom).unwrap_or_default();
    assert_eq!(
        (unit_json["unit"].clone(), unit_json["auth"].clone()),
        (json!("CNY"), json!("raw"))
    );
}

#[test]
fn the_same_origin_rule_covers_scheme_port_and_every_stored_origin() {
    let q = |url: &str| query(url, BalanceAuth::Bearer, UnitName::Usd);
    let origin = ["https://relay.acme.dev".to_owned()];
    assert!(!q("https://relay.acme.dev/a").is_cross_host(&origin));
    assert!(!q("https://RELAY.acme.dev:443/a?x=1").is_cross_host(&origin));
    assert!(
        q("http://relay.acme.dev/a").is_cross_host(&origin),
        "a scheme downgrade is another origin"
    );
    assert!(q("https://relay.acme.dev:8443/a").is_cross_host(&origin));
    assert!(q("https://relay.acme.dev.evil.test/a").is_cross_host(&origin));
    assert!(q("https://evil.test/a").is_cross_host(&["not a url".to_owned()]));

    // A second stored endpoint origin is not cross-host; no stored origin matches nothing.
    let stored = [
        "https://a.acme.dev".to_owned(),
        "https://b.acme.dev:8443".to_owned(),
    ];
    assert!(!q("https://b.acme.dev:8443/balance").is_cross_host(&stored));
    assert!(q("https://c.acme.dev/balance").is_cross_host(&stored));
    assert!(
        q("https://a.acme.dev/balance").is_cross_host(&[]),
        "no stored origin, no match"
    );
}

#[test]
fn every_class_name_is_its_serde_form() {
    for kind in [
        BalanceErrorKind::Auth,
        BalanceErrorKind::NotFound,
        BalanceErrorKind::SchemaMismatch,
        BalanceErrorKind::RateLimited,
        BalanceErrorKind::Transient,
        BalanceErrorKind::Network,
        BalanceErrorKind::Blocked,
        BalanceErrorKind::Cancelled,
        BalanceErrorKind::NeedsConsoleToken,
        BalanceErrorKind::ConsoleTokenExpired,
    ] {
        assert_eq!(
            serde_json::to_value(kind).ok(),
            Some(serde_json::Value::from(kind.as_str())),
            "{kind:?}"
        );
    }
}

#[test]
fn sixty_one_minutes_is_stale_and_fifty_nine_is_not() {
    let stored = StoredBalance {
        last_good: Some(BalanceReport::default()),
        ..StoredBalance::default()
    };
    assert!(!stored.is_stale_at(59 * MIN));
    assert!(!stored.is_stale_at(60 * MIN));
    assert!(stored.is_stale_at(61 * MIN));
}

#[test]
fn third_party_and_inferred_endpoints_are_community_documented() {
    assert!(Evidence::Inferred.is_community_documented());
    assert!(Evidence::ThirdPartySource.is_community_documented());
    assert!(!Evidence::Documented.is_community_documented());
    assert!(!Evidence::UpstreamSource.is_community_documented());
}

/// The stored shapes have no redaction pattern of their own; the one shared table
/// (`spanbay_base::secrets`) does the work.
#[test]
fn the_stored_shapes_have_no_redaction_pattern_of_their_own() {
    let sources = [
        include_str!("custom.rs"),
        include_str!("evidence.rs"),
        include_str!("record.rs"),
        include_str!("types.rs"),
    ];
    for (index, source) in sources.iter().enumerate() {
        let production = source.split("#[cfg(test)]").next().unwrap_or_default();
        for forbidden in ["Regex", "regex::", "LazyLock<Vec<(", "\"sk-", "BODY_KEYS"] {
            assert!(
                !production.contains(forbidden),
                "file #{index} contains {forbidden}"
            );
        }
    }
}
