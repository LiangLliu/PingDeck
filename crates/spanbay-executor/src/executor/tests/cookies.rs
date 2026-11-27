//! Cookie replay, expiry, capture and per-redirect-hop session cookie behaviour.

use super::support::{TestServer, TestServerAction};
use crate::executor::cookies::{
    capture_hop_cookies, cookie_header_for_url, cookie_is_expired, cookie_matches_url,
    mask_cookie_header,
};
use crate::executor::send::execute_request;
use crate::executor::types::CookieRejectReason;
use crate::executor::types::ExecutorOptions;
use reqwest::header::{HeaderMap, HeaderValue, SET_COOKIE};
use spanbay_base::secrets::REDACTED;
use spanbay_model::models::COOKIE_MAX_BYTES;
use spanbay_model::{CookieJarEntry, HttpMethod, KeyValue, ProtocolKind, Request, RequestBody};
use url::Url;

#[allow(clippy::too_many_arguments)]
fn jar_entry(
    name: &str,
    value: &str,
    base_url: &str,
    domain: Option<&str>,
    path: Option<&str>,
    secure: bool,
    expires: Option<&str>,
    max_age: Option<&str>,
    created_at: &str,
) -> CookieJarEntry {
    CookieJarEntry::from_response_cookie(
        "ws_billing",
        None,
        format!("{base_url}/account/login"),
        name,
        value,
        domain,
        path,
        expires,
        max_age,
        secure,
        false,
        None,
        created_at,
    )
    .unwrap()
}

#[tokio::test]
async fn cookie_replay_handles_matching_explicit_disabled_graphql_boundaries_and_expiry() {
    let actions: Vec<_> = (0..7)
        .map(|_| {
            TestServerAction::Respond(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into(),
            )
        })
        .collect();
    let server = TestServer::spawn_sequence(actions);
    let base = &server.base_url;
    let jar = vec![
        jar_entry(
            "session",
            "abc123",
            base,
            None,
            Some("/account"),
            false,
            None,
            None,
            "1700000000000",
        ),
        jar_entry(
            "theme",
            "dark",
            base,
            Some("127.0.0.1"),
            Some("/"),
            false,
            None,
            None,
            "1700000000000",
        ),
        jar_entry(
            "skip_path",
            "no",
            base,
            None,
            Some("/admin"),
            false,
            None,
            None,
            "1700000000000",
        ),
        jar_entry(
            "expired_max_age",
            "old",
            base,
            None,
            Some("/account"),
            false,
            None,
            Some("0"),
            "1700000000000",
        ),
        jar_entry(
            "expired_expires",
            "old",
            base,
            None,
            Some("/account"),
            false,
            Some("Wed, 21 Oct 2015 07:28:00 GMT"),
            None,
            "1700000000000",
        ),
    ];
    let opts = || ExecutorOptions {
        cookie_jar: jar.clone(),
        ..ExecutorOptions::default()
    };

    let req = Request::new(
        "r1",
        "ws_billing",
        "Replay",
        format!("{base}/account/profile"),
    )
    .unwrap();
    execute_request(&req, None, opts()).await.unwrap();

    let mut explicit = req.clone();
    explicit.headers = vec![KeyValue {
        key: "Cookie".into(),
        value: "manual=1".into(),
        enabled: true,
        description: None,
    }];
    execute_request(&explicit, None, opts()).await.unwrap();

    let mut disabled = req.clone();
    disabled.network_options.send_cookies = false;
    execute_request(&disabled, None, opts()).await.unwrap();

    let mut disabled_manual = explicit;
    disabled_manual.network_options.send_cookies = false;
    execute_request(&disabled_manual, None, opts())
        .await
        .unwrap();

    let mut gql = Request::new("r_gql", "ws_billing", "GQL", format!("{base}/graphql")).unwrap();
    gql.protocol = ProtocolKind::Graphql;
    gql.method = HttpMethod::Post;
    gql.body = RequestBody::Graphql {
        query: "{ viewer { id } }".into(),
        variables_json: Some("{}".into()),
        operation_name: None,
    };
    execute_request(&gql, None, opts()).await.unwrap();

    let boundary = Request::new(
        "r_bound",
        "ws_billing",
        "Boundary",
        format!("{base}/accountant/profile"),
    )
    .unwrap();
    execute_request(
        &boundary,
        None,
        ExecutorOptions {
            cookie_jar: vec![jar_entry(
                "session",
                "abc123",
                base,
                None,
                Some("/account"),
                false,
                None,
                None,
                "1700000000000",
            )],
            ..ExecutorOptions::default()
        },
    )
    .await
    .unwrap();

    execute_request(&req, None, opts()).await.unwrap();

    let received = server.received_requests(7);
    assert!(received[0].contains("cookie: session=abc123; theme=dark"));
    for skipped in ["skip_path=no", "expired_max_age=old", "expired_expires=old"] {
        assert!(!received[0].contains(skipped) && !received[6].contains(skipped));
    }
    assert!(received[1].contains("cookie: manual=1") && !received[1].contains("session=abc123"));
    assert!(!received[2].contains("cookie:"));
    assert!(received[3].contains("cookie: manual=1"));
    assert!(received[4].contains("post /graphql") && received[4].contains("cookie: theme=dark"));
    assert!(!received[5].contains("cookie:"));
}

#[tokio::test]
async fn cookie_capture_and_redirect_session_jar_hold() {
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nSet-Cookie: session=abc123; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=3600\r\nSet-Cookie: theme=dark; Domain=127.0.0.1; Expires=Wed, 21 Oct 2030 07:28:00 GMT\r\nSet-Cookie: foreign=skip; Domain=example.com; Path=/\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into(),
        ),
        TestServerAction::Respond("HTTP/1.1 302 Found\r\nLocation: /next\r\nSet-Cookie: session=abc123; Path=/next\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 302 Found\r\nLocation: /next/again\r\nSet-Cookie: session=gone; Path=next; Max-Age=0\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".into()),
    ]);

    let cap_req = Request::new(
        "r_cap",
        "ws_billing",
        "Cap",
        format!("{}/cookie", server.base_url),
    )
    .unwrap();
    let cap_res = execute_request(&cap_req, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert_eq!(cap_res.cookies.len(), 2);
    let c0 = &cap_res.cookies[0];
    assert_eq!(
        (
            c0.name.as_str(),
            c0.value.as_str(),
            c0.path.as_deref(),
            c0.domain.as_deref(),
            c0.max_age.as_deref(),
            c0.same_site.as_deref()
        ),
        (
            "session",
            "abc123",
            Some("/"),
            Some("127.0.0.1"),
            Some("3600"),
            Some("Lax")
        )
    );
    assert!(c0.host_only && c0.secure && c0.http_only);
    let c1 = &cap_res.cookies[1];
    assert!(!c1.host_only && c1.expires.as_deref() == Some("Wed, 21 Oct 2030 07:28:00 GMT"));

    let redir_req = Request::new(
        "r_redir",
        "ws_billing",
        "Redir",
        format!("{}/start", server.base_url),
    )
    .unwrap();
    let opts = ExecutorOptions {
        cookie_jar: vec![jar_entry(
            "scope",
            "1",
            &server.base_url,
            None,
            Some("/next"),
            false,
            None,
            None,
            "1700000000000",
        )],
        ..ExecutorOptions::default()
    };
    execute_request(&redir_req, None, opts).await.unwrap();
    let received = server.received_requests(4);
    assert!(!received[1].contains("cookie:"));
    assert!(received[2].contains("cookie: scope=1; session=abc123"));
    assert!(received[3].contains("cookie: scope=1") && !received[3].contains("session="));
}

#[test]
fn cookie_expiry_and_domain_suffix_rules_hold() {
    let now = std::time::UNIX_EPOCH + std::time::Duration::from_millis(4102444801000);
    let max_age_wins = jar_entry(
        "m",
        "1",
        "https://api.example.com",
        None,
        Some("/"),
        false,
        Some("Wed, 21 Oct 2015 07:28:00 GMT"),
        Some("3600"),
        "4102444800000",
    );
    assert!(!cookie_is_expired(&max_age_wins, now));

    // An edit bumps `updated_at`; the lifetime still counts from receipt.
    let mut edited = jar_entry(
        "r",
        "1",
        "https://api.example.com",
        None,
        Some("/"),
        false,
        None,
        Some("3600"),
        "1700000000000",
    );
    edited.updated_at = "4102444800000".into();
    assert!(cookie_is_expired(&edited, now));

    let bad_max = jar_entry(
        "b1",
        "1",
        "https://api.example.com",
        None,
        Some("/"),
        false,
        None,
        Some("nan"),
        "4102444800000",
    );
    let dashed = |name: &str, expires: &str| {
        jar_entry(
            name,
            "1",
            "https://api.example.com",
            None,
            Some("/"),
            false,
            Some(expires),
            None,
            "4102444800000",
        )
    };
    assert!(cookie_is_expired(
        &dashed("d1", "Wed, 21-Oct-2015 07:28:00 GMT"),
        now
    ));
    assert!(!cookie_is_expired(
        &dashed("d2", "Fri, 31-Dec-2100 00:00:00 GMT"),
        now
    ));
    let bad_exp = jar_entry(
        "b3",
        "1",
        "https://api.example.com",
        None,
        Some("/"),
        false,
        Some("bad"),
        None,
        "4102444800000",
    );
    assert!(!cookie_is_expired(&bad_max, now) && !cookie_is_expired(&bad_exp, now));

    let url = Url::parse("https://api.example.com/invoices").unwrap();
    let mut domain_cookie = jar_entry(
        "s",
        "1",
        "http://127.0.0.1:1",
        None,
        Some("/"),
        false,
        None,
        None,
        "1700000000000",
    );
    domain_cookie.domain = "example.com".into();
    domain_cookie.host_only = false;
    assert!(cookie_matches_url(&domain_cookie, &url, true));
    domain_cookie.domain = "com".into();
    assert!(!cookie_matches_url(&domain_cookie, &url, true));
}

#[test]
fn set_cookie_with_non_visible_ascii_keeps_utf8_and_drops_the_rest() {
    let url = Url::parse("http://example.com/").expect("url");
    let mut headers = HeaderMap::new();
    for raw in [
        "café=crème; Path=/".as_bytes(),
        b"latin=\xe9t\xe9".as_slice(),
        b"plain=1",
    ] {
        headers.append(SET_COOKIE, HeaderValue::from_bytes(raw).expect("header"));
    }
    let (mut captured, mut session, mut rejected) = (Vec::new(), Vec::new(), Vec::new());
    capture_hop_cookies(&headers, &url, &mut captured, &mut session, &mut rejected);
    assert_eq!(
        captured
            .iter()
            .map(|cookie| (cookie.name.as_str(), cookie.value.as_str()))
            .collect::<Vec<_>>(),
        [("café", "crème"), ("plain", "1")]
    );
    assert_eq!(
        cookie_header_for_url(&url, &session, true).as_deref(),
        Some("café=crème; plain=1")
    );
    assert_eq!(
        rejected
            .iter()
            .map(|cookie| (cookie.name.as_str(), cookie.reason))
            .collect::<Vec<_>>(),
        [("", CookieRejectReason::Malformed)]
    );
}

#[test]
fn local_development_cookies_are_kept_and_every_rejection_is_reported() {
    let url = Url::parse("http://localhost:3000/login").unwrap();
    let mut headers = HeaderMap::new();
    let big = format!("big={}", "x".repeat(COOKIE_MAX_BYTES));
    for raw in [
        "sid=1; Domain=localhost; Secure",
        "intranet=1; Domain=devbox",
        "foreign=1; Domain=example.com",
        "novalue",
        big.as_str(),
    ] {
        headers.append(SET_COOKIE, HeaderValue::from_str(raw).unwrap());
    }
    let (mut captured, mut session, mut rejected) = (Vec::new(), Vec::new(), Vec::new());
    capture_hop_cookies(&headers, &url, &mut captured, &mut session, &mut rejected);
    let kept: Vec<_> = captured
        .iter()
        .map(|cookie| {
            (
                cookie.name.as_str(),
                cookie.domain.as_deref(),
                cookie.host_only,
            )
        })
        .collect();
    assert_eq!(kept, [("sid", Some("localhost"), true)]);
    let reasons: Vec<_> = rejected
        .iter()
        .map(|cookie| (cookie.name.as_str(), cookie.host.as_str(), cookie.reason))
        .collect();
    assert_eq!(
        reasons,
        [
            ("intranet", "localhost", CookieRejectReason::PublicSuffix),
            ("foreign", "localhost", CookieRejectReason::ForeignDomain),
            ("", "localhost", CookieRejectReason::Malformed),
            ("big", "localhost", CookieRejectReason::TooLarge),
        ]
    );
    // A loopback host is a secure context: the Secure cookie goes back over http.
    let api = Url::parse("http://localhost:3000/api").unwrap();
    assert_eq!(
        cookie_header_for_url(&api, &session, true).as_deref(),
        Some("sid=1")
    );

    let secure = |host: &str| {
        let mut entry = jar_entry(
            "s",
            "1",
            "https://api.example.com",
            None,
            Some("/"),
            true,
            None,
            None,
            "1700000000000",
        );
        entry.domain = host.into();
        entry
    };
    // (url, sent when dialled directly, sent through a proxy)
    for (url, direct, proxied) in [
        ("http://127.0.0.1:8080/", true, false),
        ("http://[::1]:8080/", true, false),
        ("http://localhost:8080/", true, false),
        // Not pinned to loopback by the resolver, so not a secure context.
        ("http://app.localhost/", false, false),
        ("http://api.example.com/", false, false),
        ("https://api.example.com/", true, true),
    ] {
        let url = Url::parse(url).unwrap();
        let cookie = secure(url.host_str().unwrap());
        assert_eq!(cookie_matches_url(&cookie, &url, true), direct, "{url}");
        assert_eq!(
            cookie_matches_url(&cookie, &url, false),
            proxied,
            "{url} via proxy"
        );
    }
}

#[test]
fn a_sent_cookie_header_keeps_names_and_masks_values() {
    assert_eq!(
        mask_cookie_header("session=abc123; theme=dark=1;  ; flag"),
        format!("session={REDACTED}; theme={REDACTED}; {REDACTED}")
    );
}
