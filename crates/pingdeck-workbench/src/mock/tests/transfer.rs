//! Export file format and id re-issuing.

use std::collections::HashSet;

use crate::workspace::items::{
    MockCallback, MockDataBucket, MockRule, MockRuleOperator, MockRuleTarget,
};
use crate::workspace::models::HttpMethod;

use super::super::transfer::{
    EXPORT_VERSION, TransferError, export_json, parse_export, reissue_ids,
};
use super::support::*;

/// A server with a CRUD route over a bucket and a response firing a callback.
fn linked_server() -> crate::workspace::items::MockServer {
    let mut crud = crud_route("route_users", "/users", "bucket_users");
    crud.responses = vec![response(
        "resp_crud",
        200,
        "",
        vec![MockRule {
            id: "rule_1".into(),
            target: MockRuleTarget::Query,
            modifier: "a".into(),
            operator: MockRuleOperator::Exists,
            value: String::new(),
            invert: false,
            position: 0,
        }],
    )];
    let mut triggering = route(
        "route_trigger",
        "GET",
        "/trigger",
        vec![response("resp_trigger", 200, "ok", vec![])],
    );
    triggering.responses[0].callback_ids = vec!["callback_1".into(), "callback_missing".into()];
    let mut config = server(vec![crud, triggering]);
    config.data_buckets = vec![MockDataBucket {
        id: "bucket_users".into(),
        name: "Users".into(),
        documentation: String::new(),
        value_json: "[]".into(),
    }];
    config.callbacks = vec![MockCallback {
        id: "callback_1".into(),
        name: "notify".into(),
        documentation: String::new(),
        method: HttpMethod::Post,
        url: "http://127.0.0.1:9/notify".into(),
        delay_ms: 0,
        headers: Vec::new(),
        body: String::new(),
    }];
    config
}

#[test]
fn reissue_ids_and_export_round_trip_and_validation_hold() {
    let original = linked_server();
    let mut copy = original.clone();
    let mut counter = 0_u32;
    reissue_ids(&mut copy, |prefix| {
        counter += 1;
        format!("{prefix}_new{counter}")
    });
    let mut ids = vec![
        copy.id.clone(),
        copy.data_buckets[0].id.clone(),
        copy.callbacks[0].id.clone(),
    ];
    for route in &copy.routes {
        ids.push(route.id.clone());
        for response in &route.responses {
            ids.push(response.id.clone());
            ids.extend(response.rules.iter().map(|rule| rule.id.clone()));
        }
    }
    assert!(
        ids.iter().all(|id| id.contains("_new"))
            && ids.iter().collect::<HashSet<_>>().len() == ids.len()
    );
    assert_eq!(copy.routes[0].data_bucket_id, copy.data_buckets[0].id);
    assert_eq!(
        copy.routes[1].responses[0].callback_ids,
        [copy.callbacks[0].id.clone(), "callback_missing".into()]
    );
    assert_eq!(
        (&copy.routes[0].path, &copy.callbacks[0].url),
        (&original.routes[0].path, &original.callbacks[0].url)
    );

    let servers = vec![original];
    let json = export_json(servers.clone(), 1234).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        (
            doc["format"].as_str(),
            &doc["version"],
            doc["exported_at_ms"].as_i64()
        ),
        (
            Some("pingdeck-mock-servers"),
            &serde_json::json!(EXPORT_VERSION),
            Some(1234)
        )
    );
    assert_eq!(parse_export(&json).unwrap(), servers);

    for bad in [
        "nope",
        r#"{"format":"other","version":1,"servers":[]}"#,
        r#"{"format":"pingdeck-mock-servers","servers":[]}"#,
        r#"{"format":"pingdeck-mock-servers","version":1}"#,
    ] {
        assert!(matches!(parse_export(bad), Err(TransferError::Invalid(_))));
    }
    assert_eq!(
        parse_export(
            r#"{"format":"pingdeck-mock-servers","version":2,"servers":[{"future":true}]}"#
        ),
        Err(TransferError::Newer { found: 2 })
    );
}

#[test]
fn literal_credentials_in_headers_and_urls_are_left_out_of_an_export_and_counted() {
    use super::super::transfer::strip_literal_credentials;
    use crate::workspace::models::KeyValue;

    let header = |key: &str, value: &str| KeyValue {
        key: key.into(),
        value: value.into(),
        enabled: true,
        description: None,
    };
    let mut config = linked_server();
    config.proxy_req_headers = vec![
        header("Authorization", "Bearer sk-live"),
        header("X-Api-Key", "{{apiKey}}"),
        header("Accept", "application/json"),
        header("Cookie", "  "),
    ];
    config.callbacks[0].headers = vec![header("x-api-key", "abc123")];
    config.proxy_res_headers = vec![header("x-proxied", "yes")];

    config.proxy_base_url = "https://user:pass@upstream.example/v1?page=2&token=abc".into();
    config.callbacks[0].url = "https://hooks.example/x?api_key={{key}}&sig=s1".into();

    let mut servers = vec![config];
    assert_eq!(strip_literal_credentials(&mut servers), 5);
    assert_eq!(
        servers[0].proxy_base_url,
        "https://upstream.example/v1?page=2&token="
    );
    assert_eq!(
        servers[0].callbacks[0].url,
        "https://hooks.example/x?api_key={{key}}&sig="
    );
    let values: Vec<&str> = servers[0]
        .proxy_req_headers
        .iter()
        .map(|header| header.value.as_str())
        .collect();
    assert_eq!(values, ["", "{{apiKey}}", "application/json", "  "]);
    assert_eq!(servers[0].callbacks[0].headers[0].value, "");
    assert_eq!(servers[0].proxy_res_headers[0].value, "yes");
}

#[test]
fn file_body_paths_list_each_served_local_file_once() {
    use super::super::transfer::file_body_paths;
    use crate::workspace::items::MockBodySource;

    let file = |id: &str, path: &str| {
        let mut response = response(id, 200, path, vec![]);
        response.body_source = Some(MockBodySource::File);
        response
    };
    let config = server(vec![
        route(
            "route_a",
            "GET",
            "/a",
            vec![
                file("resp_a", " /Users/me/.aws/credentials "),
                response("resp_text", 200, "/etc/passwd", vec![]),
            ],
        ),
        route(
            "route_b",
            "GET",
            "/b",
            vec![
                file("resp_b", "/Users/me/.aws/credentials"),
                file("resp_c", "/srv/x.json"),
            ],
        ),
    ]);
    // A template body that happens to look like a path is not a file body.
    assert_eq!(
        file_body_paths(&config),
        ["/Users/me/.aws/credentials", "/srv/x.json"]
    );
}
