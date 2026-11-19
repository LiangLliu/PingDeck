//! Authentication: bearer, basic, API key, AWS SigV4 and Digest.

use super::support::{
    TestServer, TestServerAction, environment_with_plain_values, unused_local_url,
};
use crate::executor::auth::{
    AwsCredentials, DigestChallenge, amz_timestamp, aws_sigv4_headers, canonical_query,
    canonical_uri, digest_authorization,
};
use crate::executor::send::execute_request;
use crate::executor::types::{ExecutionError, ExecutionHeader, ExecutorOptions};
use pingdeck_base::crypto;
use pingdeck_model::models::OAuth2GrantType;
use pingdeck_model::{
    ApiKeyLocation, AuthConfig, Environment, EnvironmentVariable, HttpMethod, KeyValue, Request,
    RequestBody,
};
use std::time::{Duration, UNIX_EPOCH};
use url::Url;

fn ok_actions(count: usize) -> Vec<TestServerAction> {
    (0..count)
        .map(|_| {
            TestServerAction::Respond(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into(),
            )
        })
        .collect()
}

#[tokio::test]
async fn bearer_basic_api_key_and_oauth2_auth_placement_masking_and_validation_hold() {
    let server = TestServer::spawn_sequence(ok_actions(7));
    let env = environment_with_plain_values(vec![
        ("token", "s3cr3t-token"),
        ("user", "user"),
        ("apiKey", "key-123"),
    ]);

    let mut req = Request::new(
        "r1",
        "ws_billing",
        "Bearer",
        format!("{}/secure", server.base_url),
    )
    .unwrap();
    req.auth = AuthConfig::Bearer {
        token_ref: "{{token}}".into(),
    };
    assert_eq!(
        execute_request(&req, Some(&env), ExecutorOptions::default())
            .await
            .unwrap()
            .status_code,
        200
    );

    req.headers = vec![KeyValue {
        key: "authorization".into(),
        value: "Bearer explicit-token".into(),
        enabled: true,
        description: None,
    }];
    execute_request(&req, Some(&env), ExecutorOptions::default())
        .await
        .unwrap();

    req.headers.clear();
    req.auth = AuthConfig::Basic {
        username: "{{user}}".into(),
        password_ref: "pass".into(),
    };
    execute_request(&req, Some(&env), ExecutorOptions::default())
        .await
        .unwrap();

    req.query_params = vec![KeyValue {
        key: "sig".into(),
        value: "{{sig}}".into(),
        enabled: true,
        description: None,
    }];
    req.auth = AuthConfig::Basic {
        username: "user".into(),
        password_ref: "{{password}}".into(),
    };
    let mut masked_env = Environment::new("env_local", "ws_billing", "Local").unwrap();
    masked_env.variables = vec![
        EnvironmentVariable::masked("password", "hunter2hunter2").unwrap(),
        EnvironmentVariable::masked("sig", "a+b/c=d").unwrap(),
    ];
    let masked_res = execute_request(&req, Some(&masked_env), ExecutorOptions::default())
        .await
        .unwrap();
    let sent = masked_res.sent_request.unwrap();
    assert_eq!(
        sent.headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case("authorization"))
            .unwrap()
            .value,
        "Basic ••••••"
    );
    assert!(
        sent.url.contains("sig=••••••")
            && !sent.url.contains("a%2Bb%2Fc%3Dd")
            && !sent.url.contains("a+b/c=d")
    );

    req.query_params.clear();
    req.auth = AuthConfig::ApiKey {
        key: "X-Api-Key".into(),
        value_ref: "{{apiKey}}".into(),
        location: ApiKeyLocation::Header,
    };
    execute_request(&req, Some(&env), ExecutorOptions::default())
        .await
        .unwrap();

    req.auth = AuthConfig::ApiKey {
        key: "X-Api-Key".into(),
        value_ref: "{{apiKey}}".into(),
        location: ApiKeyLocation::Query,
    };
    execute_request(&req, Some(&env), ExecutorOptions::default())
        .await
        .unwrap();

    req.query_params = vec![KeyValue {
        key: "X-Api-Key".into(),
        value: "explicit-key".into(),
        enabled: true,
        description: None,
    }];
    execute_request(&req, Some(&env), ExecutorOptions::default())
        .await
        .unwrap();

    let received = server.received_requests(7);
    assert!(received[0].contains("authorization: bearer s3cr3t-token"));
    assert!(
        received[1].contains("authorization: bearer explicit-token")
            && received[1].matches("authorization:").count() == 1
    );
    assert!(received[2].contains("authorization: basic dxnlcjpwyxnz"));
    assert!(received[3].contains("sig=a%2bb%2fc%3dd"));
    assert!(received[4].contains("x-api-key: key-123"));
    assert!(received[5].starts_with("get /secure?x-api-key=key-123 http/1.1"));
    assert!(
        received[6].starts_with("get /secure?x-api-key=explicit-key http/1.1")
            && received[6].matches("x-api-key=").count() == 1
    );

    let mut missing =
        Request::new("r_miss", "ws_billing", "Miss", "http://127.0.0.1:1/secure").unwrap();
    missing.auth = AuthConfig::Bearer {
        token_ref: "{{missingToken}}".into(),
    };
    let err = execute_request(&missing, None, ExecutorOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, ExecutionError::UnresolvedVariable { .. })
            && err.to_string().contains("missingToken")
    );

    missing.auth = AuthConfig::OAuth2 {
        grant_type: OAuth2GrantType::ClientCredentials,
        access_token_url: format!("{}/token?client_secret=hunter2", unused_local_url()),
        client_id: "client".into(),
        client_secret_ref: "secret".into(),
        username: String::new(),
        password_ref: String::new(),
        scope: String::new(),
        authorization_url: String::new(),
        redirect_port: None,
        client_auth: Default::default(),
        extra_params: Vec::new(),
    };
    let oauth_err = execute_request(&missing, None, ExecutorOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&oauth_err, ExecutionError::OAuth2Token { message } if !message.contains("hunter2"))
    );
}

#[tokio::test]
async fn aws_sigv4_and_digest_auth_signing_and_challenges_hold() {
    assert_eq!(
        amz_timestamp(UNIX_EPOCH + Duration::from_secs(1_440_938_160)),
        "20150830T123600Z"
    );
    let creds = AwsCredentials {
        access_key: "AKIDEXAMPLE".into(),
        secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
        region: "us-east-1".into(),
        service: "iam".into(),
        session_token: None,
    };
    let url = Url::parse("https://iam.amazonaws.com/?Action=ListUsers&Version=2010-05-08").unwrap();
    let headers = vec![ExecutionHeader {
        key: "Content-Type".into(),
        value: "application/x-www-form-urlencoded; charset=utf-8".into(),
    }];
    let pairs = aws_sigv4_headers(
        &creds,
        &reqwest::Method::GET,
        &url,
        &headers,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "20150830T123600Z",
    );
    assert_eq!(
        pairs.iter().find(|(k, _)| k == "Authorization").unwrap().1,
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
    );
    let enc_url =
        Url::parse("https://example.amazonaws.com/a b/c:d/100%25/e~f/%2f?z=last&a=%E4%BD%A0&a=1")
            .unwrap();
    assert_eq!(
        (
            canonical_uri(&enc_url).as_str(),
            canonical_query(&enc_url).as_str()
        ),
        ("/a%20b/c%3Ad/100%25/e~f/%2F", "a=%E4%BD%A0&a=1&z=last")
    );
    assert_eq!(
        canonical_uri(&Url::parse("https://example.com").unwrap()),
        "/"
    );

    let md5_ch = DigestChallenge::parse(
        "Digest realm=\"http-auth@example.org\", qop=\"auth, auth-int\", algorithm=MD5, nonce=\"7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v\", opaque=\"FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS\"",
    )
    .unwrap();
    let md5_auth = digest_authorization(
        &md5_ch,
        "Mufasa",
        "Circle of Life",
        &reqwest::Method::GET,
        "/dir/index.html",
        "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ",
        "00000001",
    );
    assert!(
        md5_auth.contains("response=\"8ca523f5e9506fed4657c9700eebdbec\"")
            && md5_auth.contains("qop=auth")
    );

    let sha_ch = DigestChallenge::parse("Digest realm=\"http-auth@example.org\", qop=\"auth\", algorithm=SHA-256, nonce=\"7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v\"").unwrap();
    let sha_auth = digest_authorization(
        &sha_ch,
        "Mufasa",
        "Circle of Life",
        &reqwest::Method::GET,
        "/dir/index.html",
        "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ",
        "00000001",
    );
    assert!(
        sha_auth.contains(
            "response=\"753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1\""
        ) && sha_auth.contains("algorithm=SHA-256")
    );

    let unauth = "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"test-realm\", nonce=\"abc123\", qop=\"auth\"\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond(
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"test-realm\"\r\nWWW-Authenticate: Digest realm=\"test-realm\", nonce=\"abc123\", qop=\"auth\", opaque=\"op-1\", algorithm=SHA-256\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into(),
        ),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".into()),
        TestServerAction::Respond(unauth.into()),
        TestServerAction::Respond(unauth.into()),
    ]);

    let mut aws_req = Request::new(
        "req_aws",
        "ws_billing",
        "AWS",
        format!("{}/prod/items", server.base_url),
    )
    .unwrap();
    aws_req.method = HttpMethod::Post;
    aws_req.body = RequestBody::RawText {
        language: None,
        content: "hello".into(),
    };
    aws_req.auth = AuthConfig::AwsSignatureV4 {
        access_key: "AKIDEXAMPLE".into(),
        secret_key_ref: Some("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into()),
        region: "us-east-1".into(),
        service: "execute-api".into(),
        session_token_ref: Some("session-token-value".into()),
    };
    let aws_res = execute_request(&aws_req, None, ExecutorOptions::default())
        .await
        .unwrap();
    let aws_sent = aws_res.sent_request.unwrap();
    assert_eq!(
        aws_sent
            .headers
            .iter()
            .find(|h| h.key == "Authorization")
            .unwrap()
            .value,
        "AWS4-HMAC-SHA256 ••••••"
    );
    assert_eq!(
        aws_sent
            .headers
            .iter()
            .find(|h| h.key == "X-Amz-Security-Token")
            .unwrap()
            .value,
        "••••••"
    );

    let mut dig_req = Request::new(
        "req_digest",
        "ws_billing",
        "Digest",
        format!("{}/secure", server.base_url),
    )
    .unwrap();
    dig_req.auth = AuthConfig::Digest {
        username: "Mufasa".into(),
        password_ref: Some("Circle of Life".into()),
    };
    let dig_res = execute_request(&dig_req, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert_eq!(dig_res.status_code, 200);
    assert_eq!(
        dig_res
            .sent_request
            .unwrap()
            .headers
            .iter()
            .find(|h| h.key == "Authorization")
            .unwrap()
            .value,
        "Digest ••••••"
    );

    let dig_loop = execute_request(&dig_req, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert_eq!(dig_loop.status_code, 401);

    let received = server.received_requests(5);
    assert!(received[0].contains("x-amz-security-token: session-token-value"));
    assert!(received[0].contains(&format!(
        "x-amz-content-sha256: {}",
        crypto::sha256_hex(b"hello")
    )));
    assert!(!received[1].contains("authorization:"));
    assert!(
        received[2].contains("authorization: digest username=\"mufasa\"")
            && received[2].contains("opaque=\"op-1\"")
            && received[2].contains("algorithm=sha-256")
    );
    assert!(received[4].contains("authorization: digest"));
}

#[tokio::test]
async fn secret5_sent_request_masks_every_credential_header_whole() {
    let server = TestServer::spawn_sequence(ok_actions(1));
    let mut req =
        Request::new("r_s5", "ws_billing", "S5", format!("{}/x", server.base_url)).unwrap();
    req.headers = [
        ("X-Amz-Security-Token", "FwoGZXIvYXdzE"),
        ("x-relay-token", "first second"),
        ("x-api-version", "2024-01-01"),
    ]
    .into_iter()
    .map(|(key, value)| KeyValue {
        key: key.into(),
        value: value.into(),
        enabled: true,
        description: None,
    })
    .collect();
    let sent = execute_request(&req, None, ExecutorOptions::default())
        .await
        .unwrap()
        .sent_request
        .unwrap();
    let value = |name: &str| {
        sent.headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case(name))
            .map(|h| h.value.clone())
    };
    assert_eq!(value("x-amz-security-token").as_deref(), Some("••••••"));
    assert_eq!(value("x-relay-token").as_deref(), Some("••••••"));
    assert_eq!(value("x-api-version").as_deref(), Some("2024-01-01"));
}

/// SigV4 hashes the bytes that actually go out, for a streamed binary file and for a
/// multipart body, so an upload's signature matches what the server receives.
#[tokio::test]
async fn sigv4_signs_the_streamed_bytes_of_binary_and_multipart_bodies() {
    let ok = || {
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".into(),
        )
    };
    let server = TestServer::spawn_sequence(vec![ok(), ok()]);
    let file = super::support::write_temp_file("upload.bin", &[0, 159, 146, 150, b'x']);
    let sigv4 = AuthConfig::AwsSignatureV4 {
        access_key: "AKIDEXAMPLE".into(),
        secret_key_ref: Some("secret".into()),
        region: "us-east-1".into(),
        service: "s3".into(),
        session_token_ref: None,
    };
    let mut binary = Request::new(
        "req_bin",
        "ws_billing",
        "Bin",
        format!("{}/b", server.base_url),
    )
    .unwrap();
    binary.method = HttpMethod::Put;
    binary.auth = sigv4.clone();
    binary.body = RequestBody::Binary {
        file_path: file.path().to_string_lossy().into_owned(),
        content_type: None,
    };
    let mut multipart = super::support::multipart_file_request(&server.base_url, file.path(), 0);
    multipart.auth = sigv4;
    if let RequestBody::Multipart { parts } = &mut multipart.body {
        parts.push(pingdeck_model::FormPart {
            key: "note".into(),
            value: "hi".into(),
            file_path: None,
            enabled: true,
        });
    }
    for request in [&binary, &multipart] {
        execute_request(request, None, ExecutorOptions::default())
            .await
            .unwrap();
    }

    for (index, raw) in server
        .received_request_bytes_many(2)
        .into_iter()
        .enumerate()
    {
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let (head, body) = raw.split_at(split);
        let head = String::from_utf8_lossy(head).to_ascii_lowercase();
        let signed = head
            .lines()
            .find_map(|line| line.strip_prefix("x-amz-content-sha256: "))
            .unwrap();
        assert_eq!(signed, crypto::sha256_hex(body), "request {index}");
        assert!(head.contains(&format!("content-length: {}\r\n", body.len())));
    }
}
