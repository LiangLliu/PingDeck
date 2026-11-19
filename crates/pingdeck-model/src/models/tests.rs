use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::validation::WorkspaceError;

#[test]
fn check_summary_counts_assertions_and_run_tests_and_is_none_when_nothing_ran() {
    let assertion = |passed| crate::assertion::AssertionResult {
        index: 0,
        label: "status".into(),
        passed,
        message: String::new(),
        expected: None,
        actual: None,
    };
    let test = |passed, skipped| crate::script_result::ScriptTestResult {
        name: "t".into(),
        passed,
        skipped,
        error: None,
        source: "test".into(),
    };
    assert_eq!(
        HistoryCheckSummary::of(
            &[assertion(true), assertion(false)],
            &[test(true, false), test(false, true)]
        ),
        Some(HistoryCheckSummary {
            passed: 2,
            failed: 1
        })
    );
    // Only skipped tests: nothing ran, so the row stays unchecked rather than "0 passed".
    assert_eq!(HistoryCheckSummary::of(&[], &[test(false, true)]), None);
}

#[test]
fn workspace_new_trims_name_and_stays_local_only() {
    let workspace = Workspace::new("ws_1", " Billing APIs ").expect("workspace should be valid");

    assert_eq!(workspace.id.as_str(), "ws_1");
    assert_eq!(workspace.name, "Billing APIs");
    assert_eq!(workspace.sync_state, SyncState::LocalOnly);
}

#[test]
fn workspace_rejects_blank_name() {
    let error = Workspace::new("ws_1", "   ").expect_err("blank names are invalid");

    assert_eq!(
        error,
        WorkspaceError::BlankName {
            entity: "workspace"
        }
    );
}

#[test]
fn request_assertions_reject_blank_fields() {
    let mut request = Request::new(
        "req_1",
        "ws_1",
        "List invoices",
        "https://api.example.com/v1/invoices",
    )
    .unwrap();

    request.assertions = vec![Assertion::StatusEquals { value: 200 }];
    assert!(request.validate_assertions().is_ok());

    request.assertions = vec![Assertion::HeaderExists {
        key: "   ".to_string(),
    }];
    assert!(request.validate_assertions().is_err());

    request.assertions = vec![Assertion::JsonPathExists {
        path: "".to_string(),
    }];
    assert!(request.validate_assertions().is_err());

    request.assertions = vec![Assertion::BodyContains {
        value: " ".to_string(),
    }];
    assert!(request.validate_assertions().is_err());
}

#[test]
fn network_options_legacy_json_defaults_to_sending_cookies() {
    let partial_options = serde_json::from_str::<NetworkOptions>(
        r#"{"timeout_ms":12000,"follow_redirects":false,"retry_count":2}"#,
    )
    .expect("partial legacy network options should deserialize");

    assert_eq!(partial_options.timeout_ms, 12_000);
    assert!(!partial_options.follow_redirects);
    assert_eq!(partial_options.retry_count, 2);
    assert!(partial_options.send_cookies);
    assert_eq!(partial_options.http_version, HttpVersionPreference::Auto);
    assert!(!partial_options.follow_original_method);
    assert!(!partial_options.follow_auth_header);
    assert!(!partial_options.remove_referer_on_redirect);
    assert!(partial_options.encode_url_automatically);
    assert_eq!(partial_options.max_redirects, 10);
    assert!(partial_options.disabled_tls_versions.is_empty());
    assert_eq!(partial_options.max_redirect_hops(), 0);

    let empty_options = serde_json::from_str::<NetworkOptions>(r#"{}"#)
        .expect("empty legacy options should deserialize");

    assert_eq!(empty_options, NetworkOptions::default());
    assert_eq!(empty_options.max_redirect_hops(), 10);
}

#[test]
fn network_options_inherit_wire_shape() {
    let own = serde_json::to_value(NetworkOptions::default()).expect("serializes");
    assert!(
        own.get("inherit").is_none(),
        "empty inherit stays off the wire"
    );

    let options = NetworkOptions {
        inherit: vec![NetworkField::TimeoutMs, NetworkField::ProxyUrl],
        ..NetworkOptions::default()
    };
    let json = serde_json::to_value(&options).expect("serializes");
    assert_eq!(
        json["inherit"],
        serde_json::json!(["timeout_ms", "proxy_url"])
    );

    let names: Vec<String> = NetworkField::ALL
        .iter()
        .map(|field| {
            serde_json::to_value(field)
                .expect("serializes")
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(
        names,
        [
            "timeout_ms",
            "retry_count",
            "http_version",
            "follow_redirects",
            "max_redirects",
            "verify_tls",
            "proxy_url"
        ]
    );

    let newer: NetworkOptions =
        serde_json::from_str(r#"{"inherit":["timeout_ms","from_the_future"]}"#)
            .expect("unknown names are ignored");
    assert_eq!(
        newer.inherit,
        vec![NetworkField::TimeoutMs, NetworkField::Unknown]
    );
}

#[test]
fn redirect_hops_are_clamped() {
    let options = NetworkOptions {
        max_redirects: 500,
        ..NetworkOptions::default()
    };
    assert_eq!(options.max_redirect_hops(), MAX_REDIRECT_HOPS);
}

#[test]
fn tls_certificate_validation_requires_paths_for_client_certificates() {
    let mut certificate = TlsCertificate {
        id: TlsCertificateId::new("cert_1").expect("id should be valid"),
        workspace_id: WorkspaceId::new("ws_billing").expect("workspace id should be valid"),
        kind: TlsCertificateKind::Client,
        host_pattern: "api.example.com:8443".to_owned(),
        cert_path: "/certs/client.pem".to_owned(),
        key_path: Some("/certs/client.key".to_owned()),
        passphrase_ref: None,
        enabled: true,
    };

    assert_eq!(certificate.validate(), Ok(()));

    certificate.cert_path = "  ".to_owned();
    assert_eq!(
        certificate.validate(),
        Err(TlsCertificateError::BlankCertPath)
    );
    certificate.cert_path = "/certs/client.pem".to_owned();

    certificate.key_path = None;
    assert_eq!(
        certificate.validate(),
        Err(TlsCertificateError::MissingKeyPath)
    );
    // A PKCS#12 bundle carries its own key.
    certificate.cert_path = "/certs/client.P12".to_owned();
    assert_eq!(certificate.validate(), Ok(()));
    certificate.cert_path = "/certs/client.pem".to_owned();
    certificate.key_path = Some("/certs/client.key".to_owned());

    certificate.host_pattern = String::new();
    assert_eq!(
        certificate.validate(),
        Err(TlsCertificateError::MissingHostPattern)
    );

    certificate.kind = TlsCertificateKind::Ca;
    certificate.key_path = None;
    certificate.host_pattern = String::new();
    assert_eq!(certificate.validate(), Ok(()));
}

#[test]
fn cookie_jar_rejects_explicit_domain_that_does_not_match_source_host() {
    let error = CookieJarEntry::from_response_cookie(
        "ws_billing",
        None,
        "https://api.example.com/account/profile",
        "session",
        "abc123",
        Some("evil.example.net"),
        Some("/"),
        None,
        None,
        true,
        true,
        None,
        "1700000000000",
    )
    .expect_err("cookie domains must match the response source host");

    assert_eq!(
        error,
        WorkspaceError::CookieDomainMismatch {
            source_host: "api.example.com".into(),
            cookie_domain: "evil.example.net".into()
        }
    );
}

#[test]
fn cookie_jar_rejects_public_suffix_domains_from_responses() {
    let error = CookieJarEntry::from_response_cookie(
        "ws_billing",
        None,
        "https://api.example.com/account/profile",
        "session",
        "abc123",
        Some(".COM"),
        Some("/"),
        None,
        None,
        true,
        false,
        None,
        "1700000000000",
    )
    .expect_err("public suffixes cannot set cookies");

    assert_eq!(
        error,
        WorkspaceError::CookiePublicSuffixDomain {
            domain: "com".into()
        }
    );
}

#[test]
fn resolved_cookie_rejects_public_suffix_domains() {
    let error = CookieJarEntry::from_resolved_cookie(
        "ws_billing",
        None,
        "session",
        "abc123",
        "co.uk",
        "/",
        false,
        None,
        None,
        true,
        false,
        None,
        "1700000000000",
    )
    .expect_err("public suffixes cannot be stored as domain cookies");

    assert_eq!(
        error,
        WorkspaceError::CookiePublicSuffixDomain {
            domain: "co.uk".into()
        }
    );
}

#[test]
fn resolved_cookie_keeps_hop_host_for_host_only_cookies() {
    let entry = CookieJarEntry::from_resolved_cookie(
        "ws_billing",
        None,
        "session",
        "abc123",
        "auth.example.com",
        "/login",
        true,
        None,
        None,
        true,
        false,
        None,
        "1700000000000",
    )
    .expect("host-only hop cookie should be valid");

    assert_eq!(entry.domain, "auth.example.com");
    assert!(entry.host_only);
    assert_eq!(
        entry.id.as_str(),
        "cookie:ws_billing:auth.example.com:/login:session"
    );
}

#[test]
fn cookie_jar_import_rejects_public_suffix_domains() {
    let mut entry = CookieJarEntry::from_response_cookie(
        "ws_billing",
        None,
        "https://api.example.com/",
        "session",
        "abc123",
        Some("api.example.com"),
        Some("/"),
        None,
        None,
        false,
        false,
        None,
        "1700000000000",
    )
    .unwrap();
    entry.domain = "co.uk".into();
    entry.host_only = false;
    let import = CookieJarImport::new("pingdeck.cookie_jar", 1, "ws_billing", vec![entry])
        .expect("payload envelope should be valid");

    assert_eq!(
        import
            .into_entries_for_workspace("ws_billing")
            .expect_err("public suffix domain should be rejected"),
        WorkspaceError::CookiePublicSuffixDomain {
            domain: "co.uk".into()
        }
    );
}

#[test]
fn cookie_jar_accepts_ip_literal_domains() {
    let entry = CookieJarEntry::from_response_cookie(
        "ws_billing",
        None,
        "http://127.0.0.1:8080/account/login",
        "session",
        "abc123",
        Some("127.0.0.1"),
        Some("/"),
        None,
        None,
        false,
        false,
        None,
        "1700000000000",
    )
    .expect("ip literal domains are not public suffixes");

    assert_eq!(entry.domain, "127.0.0.1");
    assert!(!entry.host_only);
}

#[test]
fn a_public_suffix_domain_equal_to_the_host_makes_a_host_only_cookie() {
    for host in ["localhost", "devbox"] {
        let (domain, host_only) = effective_cookie_domain(Some(host), host).unwrap();
        assert_eq!((domain.as_str(), host_only), (host, true));
    }
    assert!(matches!(
        effective_cookie_domain(Some("localhost"), "app.localhost"),
        Err(WorkspaceError::CookiePublicSuffixDomain { .. })
    ));
}

#[test]
fn a_response_cookie_carries_its_absolute_expiry_from_receipt() {
    let at = |expires: Option<&str>, max_age: Option<&str>| {
        CookieJarEntry::from_resolved_cookie(
            "ws_billing",
            None,
            "s",
            "v",
            "api.example.com",
            "/",
            true,
            expires,
            max_age,
            false,
            false,
            None,
            "1700000000000",
        )
        .unwrap()
    };
    // Max-Age wins over Expires and counts from receipt.
    let entry = at(Some("Wed, 21 Oct 2015 07:28:00 GMT"), Some("3600"));
    assert_eq!(entry.max_age, None);
    assert_eq!(
        entry.expires.as_deref(),
        Some("Tue, 14 Nov 2023 23:13:20 GMT")
    );
    // An edit bumps `updated_at` and never extends the lifetime.
    let mut edited = entry;
    edited.updated_at = "1800000000000".into();
    assert!(edited.is_expired_at(UNIX_EPOCH + Duration::from_secs(1_700_003_601)));
    assert!(at(None, Some("0")).is_expired_at(UNIX_EPOCH + Duration::from_secs(1)));
    // The dash form, a two-digit year and a wrong weekday read; garbage is a session cookie.
    assert_eq!(
        at(Some("Wed, 21-Oct-2025 07:28:00 GMT"), None)
            .expires
            .as_deref(),
        Some("Tue, 21 Oct 2025 07:28:00 GMT")
    );
    assert_eq!(
        at(Some("Wed, 21-Oct-15 07:28:00 GMT"), None)
            .expires
            .as_deref(),
        Some("Wed, 21 Oct 2015 07:28:00 GMT")
    );
    assert_eq!(at(Some("not a date"), None).expires, None);
    assert_eq!(
        at(None, Some("99999999999999")).expires.as_deref(),
        Some("Fri, 31 Dec 9999 23:59:59 GMT")
    );
}

#[test]
fn cookie_jar_export_wraps_versioned_payload() {
    let first = CookieJarEntry::from_response_cookie(
        "ws_billing",
        Some("req_invoices"),
        "https://api.example.com/account/profile",
        "session",
        "abc123",
        None,
        Some("/account"),
        None,
        None,
        true,
        true,
        Some("Lax"),
        "1700000000000",
    )
    .unwrap();
    let second = CookieJarEntry::from_response_cookie(
        "ws_billing",
        None,
        "https://docs.example.com/",
        "theme",
        "dark",
        None,
        Some("/"),
        None,
        Some("3600"),
        false,
        false,
        None,
        "1700000000001",
    )
    .unwrap();

    let export = CookieJarExport::new("ws_billing", vec![first.clone(), second.clone()])
        .expect("export should be valid");

    assert_eq!(export.format, "pingdeck.cookie_jar");
    assert_eq!(export.version, 1);
    assert_eq!(export.workspace_id.as_str(), "ws_billing");
    assert_eq!(export.cookies, vec![first, second]);

    let serialized = serde_json::to_value(&export).expect("export should serialize");
    assert_eq!(serialized["format"], "pingdeck.cookie_jar");
    assert_eq!(serialized["version"], 1);
    assert_eq!(serialized["workspace_id"], "ws_billing");
    assert_eq!(serialized["cookies"][0]["name"], "session");

    assert_eq!(
        CookieJarExport::new("   ", Vec::new()).expect_err("blank workspace id is invalid"),
        WorkspaceError::BlankId {
            entity: "workspace"
        }
    );
}

#[test]
fn cookie_jar_import_normalizes_entries_for_target_workspace() {
    let source = CookieJarEntry::from_response_cookie(
        "ws_billing",
        Some("req_invoices"),
        "https://api.example.com/account/profile",
        "session",
        "abc123",
        Some(".API.EXAMPLE.COM"),
        Some("account"),
        Some("Wed, 21 Oct 2030 07:28:00 GMT"),
        None,
        true,
        true,
        Some("Lax"),
        "1700000000000",
    )
    .unwrap();
    let import = CookieJarImport::new("pingdeck.cookie_jar", 1, "ws_billing", vec![source])
        .expect("import payload should be valid");

    let entries = import
        .into_entries_for_workspace("ws_billing")
        .expect("entries should normalize");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].workspace_id.as_str(), "ws_billing");
    assert_eq!(
        entries[0].id.as_str(),
        "cookie:ws_billing:api.example.com:/account:session"
    );
    assert_eq!(entries[0].source_request_id, None);
    assert_eq!(entries[0].name, "session");
    assert_eq!(entries[0].domain, "api.example.com");
    assert_eq!(entries[0].path, "/account");
    assert!(!entries[0].host_only);
    assert_eq!(entries[0].same_site.as_deref(), Some("Lax"));
    assert_eq!(entries[0].created_at, "1700000000000");
    assert_eq!(entries[0].updated_at, "1700000000000");
}

#[test]
fn cookie_jar_import_rejects_unsupported_payloads_before_entries_are_used() {
    let entry = CookieJarEntry::from_response_cookie(
        "ws_billing",
        None,
        "https://api.example.com/",
        "session",
        "abc123",
        None,
        Some("/"),
        None,
        None,
        false,
        false,
        None,
        "1700000000000",
    )
    .unwrap();

    assert_eq!(
        CookieJarImport::new("postman.cookie_jar", 1, "ws_billing", vec![entry.clone()])
            .expect_err("format should be rejected"),
        WorkspaceError::UnsupportedCookieJarFormat {
            format: "postman.cookie_jar".into()
        }
    );
    assert_eq!(
        CookieJarImport::new("pingdeck.cookie_jar", 2, "ws_billing", vec![entry.clone()])
            .expect_err("version should be rejected"),
        WorkspaceError::UnsupportedCookieJarVersion { version: 2 }
    );

    let import = CookieJarImport::new("pingdeck.cookie_jar", 1, "ws_billing", vec![entry])
        .expect("import payload should be valid");

    assert_eq!(
        import
            .into_entries_for_workspace("ws_other")
            .expect_err("workspace mismatch should be rejected"),
        WorkspaceError::CookieJarWorkspaceMismatch {
            expected: "ws_other".into(),
            actual: "ws_billing".into()
        }
    );

    let decoded: CookieJarImport = serde_json::from_value(serde_json::json!({
        "format": "pingdeck.cookie_jar",
        "version": 99,
        "workspace_id": "ws_billing",
        "cookies": []
    }))
    .expect("derived serde should decode envelope before validation");
    assert_eq!(
        decoded
            .into_entries_for_workspace("ws_billing")
            .expect_err("deserialized unsupported version should be rejected"),
        WorkspaceError::UnsupportedCookieJarVersion { version: 99 }
    );
}

#[test]
fn cookie_jar_import_rejects_invalid_cookie_metadata() {
    let mut entry = CookieJarEntry::from_response_cookie(
        "ws_billing",
        None,
        "https://api.example.com/",
        "session",
        "abc123",
        None,
        Some("/"),
        None,
        None,
        false,
        false,
        Some("Lax"),
        "1700000000000",
    )
    .unwrap();
    entry.same_site = Some("Loose".into());
    let import = CookieJarImport::new("pingdeck.cookie_jar", 1, "ws_billing", vec![entry])
        .expect("payload envelope should be valid");

    assert_eq!(
        import
            .into_entries_for_workspace("ws_billing")
            .expect_err("invalid SameSite should be rejected"),
        WorkspaceError::InvalidCookieSameSite {
            same_site: "Loose".into()
        }
    );
}

#[test]
fn secret5_history_masks_every_credential_header_and_keeps_version_headers() {
    let headers: Vec<KeyValue> = [
        ("X-Amz-Security-Token", "FwoGZXIvYXdzE"),
        ("x-relay-token", "tok-1"),
        ("X-Client-Secret", "cs-1"),
        ("Authorization", "Bearer abc"),
        ("x-api-version", "2024-01-01"),
        ("X-Request-Id", "r-1"),
    ]
    .into_iter()
    .map(|(key, value)| KeyValue {
        key: key.into(),
        value: value.into(),
        enabled: true,
        description: None,
    })
    .collect();
    let values: Vec<String> = capped_masked_headers(&headers)
        .0
        .into_iter()
        .map(|header| header.value)
        .collect();
    assert_eq!(
        values,
        [
            "[redacted]",
            "[redacted]",
            "[redacted]",
            "Bearer [redacted]",
            "2024-01-01",
            "r-1"
        ]
    );
}
