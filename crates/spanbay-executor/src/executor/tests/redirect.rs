//! Redirect following: method switching, header stripping and redirect options.

use super::support::{TestServer, TestServerAction};
use crate::executor::send::execute_request;
use crate::executor::types::ExecutorOptions;
use spanbay_model::{AuthConfig, HttpMethod, KeyValue, Request, RequestBody};

#[tokio::test]
async fn same_origin_redirects_handle_methods_bodies_cookies_max_redirects_and_referer() {
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond("HTTP/1.1 302 Found\r\nLocation: /final\r\nSet-Cookie: session=abc123; Path=/\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".into()),
        TestServerAction::Respond("HTTP/1.1 307 Temporary Redirect\r\nLocation: /replay\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".into()),
        TestServerAction::Respond("HTTP/1.1 302 Found\r\nLocation: /next\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 302 Found\r\nLocation: /final\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".into()),
        TestServerAction::Respond("HTTP/1.1 302 Found\r\nLocation: /middle\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 302 Found\r\nLocation: /final\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".into()),
    ]);

    let mut post_req = Request::new(
        "r1",
        "ws_billing",
        "R",
        format!("{}/start", server.base_url),
    )
    .unwrap();
    post_req.method = HttpMethod::Post;
    post_req.body = RequestBody::Json {
        content: "{\"invoice\":\"in_123\"}".into(),
    };
    let res_302 = execute_request(&post_req, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert!(
        res_302.status_code == 200 && res_302.url.ends_with("/final") && res_302.cookies.len() == 1
    );

    execute_request(&post_req, None, ExecutorOptions::default())
        .await
        .unwrap();

    let mut no_follow = Request::new(
        "r_max0",
        "ws_billing",
        "Max0",
        format!("{}/start", server.base_url),
    )
    .unwrap();
    no_follow.network_options.max_redirects = 0;
    assert_eq!(
        execute_request(&no_follow, None, ExecutorOptions::default())
            .await
            .unwrap()
            .status_code,
        302
    );

    let mut follow_method = post_req;
    follow_method.network_options.follow_original_method = true;
    execute_request(&follow_method, None, ExecutorOptions::default())
        .await
        .unwrap();

    let mut ref_req = Request::new(
        "r_ref",
        "ws_billing",
        "Ref",
        format!("{}/start", server.base_url),
    )
    .unwrap();
    ref_req.headers = vec![KeyValue {
        key: "Referer".into(),
        value: "https://example.com/origin".into(),
        enabled: true,
        description: None,
    }];
    ref_req.network_options.remove_referer_on_redirect = true;
    execute_request(&ref_req, None, ExecutorOptions::default())
        .await
        .unwrap();

    let received = server.received_requests(10);
    assert!(received[0].starts_with("post /start") && received[0].contains("in_123"));
    assert!(
        received[1].starts_with("get /final")
            && !received[1].contains("in_123")
            && received[1].contains("cookie: session=abc123")
    );
    assert!(received[3].starts_with("post /replay") && received[3].contains("in_123"));
    assert!(received[4].starts_with("get /start"));
    assert!(received[6].starts_with("post /final") && received[6].contains("in_123"));
    assert!(
        received[7].contains("referer: https://example.com/origin")
            && !received[8].contains("referer:")
            && !received[9].contains("referer:")
    );
}

#[tokio::test]
async fn cross_origin_redirects_strip_credentials_cookies_and_aws_signatures_unless_configured() {
    let target = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".into(),
        ),
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".into(),
        ),
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".into(),
        ),
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".into(),
        ),
    ]);
    let redir = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}/landing\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        target.base_url
    );
    let origin = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(redir.clone()),
        TestServerAction::Respond(redir.clone()),
        TestServerAction::Respond(redir.clone()),
        TestServerAction::Respond(redir),
    ]);

    let mut hdr_auth = Request::new(
        "r1",
        "ws_billing",
        "H",
        format!("{}/start", origin.base_url),
    )
    .unwrap();
    hdr_auth.headers = vec![KeyValue {
        key: "Authorization".into(),
        value: "Bearer top-secret".into(),
        enabled: true,
        description: None,
    }];
    execute_request(&hdr_auth, None, ExecutorOptions::default())
        .await
        .unwrap();

    let mut saved_auth = Request::new(
        "r2",
        "ws_billing",
        "S",
        format!("{}/start", origin.base_url),
    )
    .unwrap();
    saved_auth.auth = AuthConfig::Bearer {
        token_ref: "cross-secret".into(),
    };
    execute_request(&saved_auth, None, ExecutorOptions::default())
        .await
        .unwrap();

    let mut follow_auth = hdr_auth;
    follow_auth.headers.push(KeyValue {
        key: "Cookie".into(),
        value: "manual=1".into(),
        enabled: true,
        description: None,
    });
    follow_auth.network_options.follow_auth_header = true;
    execute_request(&follow_auth, None, ExecutorOptions::default())
        .await
        .unwrap();

    let mut aws_req = Request::new(
        "r4",
        "ws_billing",
        "AWS",
        format!("{}/start", origin.base_url),
    )
    .unwrap();
    aws_req.auth = AuthConfig::AwsSignatureV4 {
        access_key: "AKIDEXAMPLE".into(),
        secret_key_ref: Some("secret".into()),
        region: "us-east-1".into(),
        service: "s3".into(),
        session_token_ref: None,
    };
    execute_request(&aws_req, None, ExecutorOptions::default())
        .await
        .unwrap();

    let orig_reqs = origin.received_requests(4);
    let targ_reqs = target.received_requests(4);
    assert!(
        orig_reqs[0].contains("authorization: bearer top-secret")
            && !targ_reqs[0].contains("authorization:")
    );
    assert!(
        orig_reqs[1].contains("authorization: bearer cross-secret")
            && !targ_reqs[1].contains("authorization:")
    );
    assert!(
        targ_reqs[2].contains("authorization: bearer top-secret")
            && !targ_reqs[2].contains("manual=1")
    );
    assert!(orig_reqs[3].contains("authorization: aws4-hmac-sha256"));
    assert!(
        !targ_reqs[3].contains("authorization:")
            && !targ_reqs[3].contains("x-amz-date:")
            && !targ_reqs[3].contains("x-amz-content-sha256:")
    );
}
