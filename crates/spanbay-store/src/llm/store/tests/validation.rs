//! Provider input validation: names, base URLs, paths, extra headers and proxies.

use super::{connection, single_endpoint_provider};
use crate::llm::providers::{MAX_EXTRA_HEADERS, MAX_HEADER_VALUE_BYTES};
use crate::llm::store::{
    Provider, get_provider, list_providers, save_provider, unique_provider_name,
};
use spanbay_base::error::{AppError, ErrorCode};

#[test]
fn proxy_urls_are_validated_and_stored() {
    let c = connection();
    let mut with_proxy = single_endpoint_provider("p1");
    with_proxy.proxy_url = " http://127.0.0.1:7890 ".into();
    let saved = save_provider(&c, &with_proxy, 1).unwrap();
    assert_eq!(saved.proxy_url, "http://127.0.0.1:7890");
    with_proxy.proxy_url = "not a proxy".into();
    assert!(matches!(
        save_provider(&c, &with_proxy, 2),
        Err(AppError::Validation { .. })
    ));
    assert_eq!(
        get_provider(&c, "p1").unwrap().unwrap().proxy_url,
        "http://127.0.0.1:7890"
    );
}

#[test]
fn provider_names_are_unique_ignoring_case_and_never_another_providers_id() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 1).unwrap();
    let named = |id: &str, name: &str| Provider {
        name: name.into(),
        ..single_endpoint_provider(id)
    };
    save_provider(&c, &named("p2", "Hub"), 1).unwrap();
    for (name, holder) in [("hub", "p2"), (" HUB ", "p2"), ("P1", "p1")] {
        let error = save_provider(&c, &named("p3", name), 1).unwrap_err();
        assert!(matches!(error, AppError::Conflict { .. }));
        assert_eq!(error.code(), Some(ErrorCode::NameTaken));
        assert!(
            matches!(&error, AppError::Conflict { detail, .. } if detail.subject.as_deref() == Some(holder))
        );
    }
    let slash = save_provider(&c, &named("p3", "a/b"), 1).unwrap_err();
    assert_eq!(slash.code(), Some(ErrorCode::NameHasSlash));
    for name in ["a\"b", "a`b", "a\\b", "a\u{7}b"] {
        assert!(matches!(
            save_provider(&c, &named("p3", name), 1),
            Err(AppError::Validation { .. })
        ));
    }
    // Renaming itself to another case is fine; spaces, parentheses and Chinese are allowed.
    save_provider(&c, &named("p2", "HUB"), 1).unwrap();
    save_provider(&c, &named("p3", "中转 (Jev) 2"), 1).unwrap();
    assert_eq!(unique_provider_name(&c, "p4", "hub").unwrap(), "hub 2");
    assert_eq!(unique_provider_name(&c, "p4", "Fresh").unwrap(), "Fresh");
}

fn validation_message(result: Result<Provider, AppError>) -> String {
    match result {
        Err(AppError::Validation { message, .. }) => message,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn a_blank_name_or_base_url_is_rejected() {
    let c = connection();
    let mut blank_name = single_endpoint_provider("p1");
    blank_name.name = "   ".into();
    assert!(validation_message(save_provider(&c, &blank_name, 1)).contains("name"));
    let mut blank_url = single_endpoint_provider("p1");
    blank_url.base_url = "  ".into();
    assert!(validation_message(save_provider(&c, &blank_url, 1)).contains("base URL"));
    assert!(list_providers(&c).unwrap().is_empty());
}

#[test]
fn base_urls_must_be_plain_http_hosts() {
    let c = connection();
    for bad in [
        "not a url",
        "ftp://example.test",
        "file:///etc/passwd",
        "https://user:pw@example.test",
        "https://user@example.test",
        "https://example.test/v1?key=abc",
        "https://example.test/v1#frag",
    ] {
        let mut candidate = single_endpoint_provider("p1");
        candidate.base_url = bad.into();
        let message = validation_message(save_provider(&c, &candidate, 1));
        assert!(
            !message.contains("pw") && !message.contains("abc"),
            "{message}"
        );
    }
    assert!(list_providers(&c).unwrap().is_empty());
    let mut good = single_endpoint_provider("p1");
    good.base_url = " http://127.0.0.1:8080/api ".into();
    assert_eq!(
        save_provider(&c, &good, 1).unwrap().base_url,
        "http://127.0.0.1:8080/api"
    );
}

#[test]
fn base_urls_never_point_at_link_local_or_metadata_hosts() {
    let c = connection();
    for bad in [
        "http://169.254.169.254/latest",
        "http://[fe80::1]/",
        "http://[fd00:ec2::254]/",
        "http://100.100.100.200/",
        "http://metadata.google.internal/computeMetadata/v1",
        "http://0.0.0.0:8080",
        "http://2852039166/",
    ] {
        let mut candidate = single_endpoint_provider("p1");
        candidate.base_url = bad.into();
        let message = validation_message(save_provider(&c, &candidate, 1));
        assert!(message.contains("link-local"), "{bad}: {message}");
    }
    assert!(list_providers(&c).unwrap().is_empty());
    for (n, good) in [
        "http://192.168.1.20:11434",
        "http://100.64.0.7:11434",
        "http://ollama.lan:11434",
        "http://[::1]:8080",
    ]
    .into_iter()
    .enumerate()
    {
        // One provider each: moving a stored one to another host needs a confirmation.
        let mut candidate = single_endpoint_provider(&format!("p{n}"));
        candidate.base_url = good.into();
        save_provider(&c, &candidate, 1).expect(good);
    }
}

#[test]
fn model_endpoint_and_path_prefix_must_be_relative_paths() {
    let c = connection();
    for bad in [
        "https://evil.test/models",
        "//evil.test/models",
        "/a b",
        "/a#b",
    ] {
        let mut candidate = single_endpoint_provider("p1");
        candidate.models_endpoint = Some(bad.into());
        validation_message(save_provider(&c, &candidate, 1));
        let mut candidate = single_endpoint_provider("p1");
        candidate.endpoints[0].path_prefix = bad.into();
        validation_message(save_provider(&c, &candidate, 1));
    }
    let mut query = single_endpoint_provider("p1");
    query.endpoints[0].path_prefix = "/v1?x=1".into();
    validation_message(save_provider(&c, &query, 1));
    let mut query_endpoint = single_endpoint_provider("p1");
    query_endpoint.models_endpoint = Some("/models?limit=5".into());
    assert!(save_provider(&c, &query_endpoint, 1).is_ok());
}

#[test]
fn extra_headers_are_validated_without_echoing_values() {
    let c = connection();
    let with = |name: &str, value: &str| {
        let mut candidate = single_endpoint_provider("p1");
        candidate
            .quirks
            .extra_headers
            .insert(name.to_owned(), value.to_owned());
        candidate
    };
    for name in [
        "Host",
        "content-length",
        "Transfer-Encoding",
        "connection",
        "Authorization",
        "X-Api-Key",
        "x-goog-api-key",
        "api-key",
        "anthropic-version",
        "bad name",
        "",
    ] {
        validation_message(save_provider(&c, &with(name, "v"), 1));
    }
    let message = validation_message(save_provider(&c, &with("x-beta", "top\nsecret"), 1));
    assert!(!message.contains("secret"), "{message}");
    validation_message(save_provider(
        &c,
        &with("x-beta", &"v".repeat(MAX_HEADER_VALUE_BYTES + 1)),
        1,
    ));
    let mut many = single_endpoint_provider("p1");
    for index in 0..=MAX_EXTRA_HEADERS {
        many.quirks
            .extra_headers
            .insert(format!("x-h{index}"), "1".into());
    }
    validation_message(save_provider(&c, &many, 1));
    assert!(save_provider(&c, &with("X-Beta", "1"), 1).is_ok());
}
