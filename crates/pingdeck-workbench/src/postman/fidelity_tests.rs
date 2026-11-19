//! Postman round trips of what v2.1 has no field for: assertions, network options, examples,
//! raw languages, and the export's environment merge.

use serde_json::{Value, json};

use super::*;
use crate::workspace::models::{
    Assertion, HttpVersionPreference, KeyValue, NetworkField, RawLanguage, RequestBody,
    RequestExample,
};

fn request(id: &str, name: &str) -> Request {
    Request::new(id, "ws_1", name, "https://x.test/a")
        .unwrap()
        .with_collection("col_1")
        .unwrap()
}

fn export(requests: &[Request], environment: &[EnvironmentVariable]) -> PostmanExport {
    let collection = Collection::new("col_1", "ws_1", "Fidelity").unwrap();
    export_collection(
        &collection,
        &[],
        requests,
        environment,
        &PostmanExportContext::default(),
    )
    .unwrap()
}

#[test]
fn assertions_network_options_examples_and_languages_survive_a_round_trip() {
    let mut checked = request("req_1", "Checked");
    checked.test_script = "console.log('mine');".into();
    checked.assertions = vec![
        Assertion::StatusEquals { value: 201 },
        Assertion::JsonPathEquals {
            path: "$.items[0]['id']".into(),
            value: json!(7),
        },
        // Not plain member and index steps: Postman cannot run it.
        Assertion::JsonPathExists {
            path: "$..id".into(),
        },
        // The test name holds the marker text; the line must still read back.
        Assertion::BodyContains {
            value: "x // pingdeck:assertion {\"StatusEquals\":{\"value\":1}}".into(),
        },
    ];
    let options = &mut checked.network_options;
    options.follow_redirects = false;
    options.verify_tls = false;
    options.max_redirects = 4;
    options.http_version = HttpVersionPreference::Http2;
    options.timeout_ms = 5_000;
    options.retry_count = 2;
    checked.examples = vec![RequestExample {
        name: "Created".into(),
        status: 201,
        headers: vec![KeyValue {
            key: "Content-Type".into(),
            value: "application/json".into(),
            enabled: true,
            description: None,
        }],
        body: "{\"id\":7}".into(),
    }];
    let mut xml = request("req_2", "Xml");
    xml.body = RequestBody::RawText {
        content: "<a/>".into(),
        language: Some(RawLanguage::Xml),
    };
    // A field that follows the global settings is not written as the request's own.
    xml.network_options.verify_tls = false;
    xml.network_options.inherit = vec![NetworkField::VerifyTls];

    let exported = export(&[checked.clone(), xml], &[]);
    let envelope: Value = serde_json::from_str(&exported.json).unwrap();
    let item = &envelope["item"][0];
    let behavior = &item["protocolProfileBehavior"];
    assert_eq!(behavior["followRedirects"], false);
    assert_eq!(behavior["strictSSL"], false);
    assert_eq!(behavior["maxRedirects"], 4);
    assert_eq!(behavior["protocolVersion"], "http2");
    assert!(envelope["item"][1].get("protocolProfileBehavior").is_none());
    assert_eq!(item["response"][0]["code"], 201);
    let test_lines = item["event"][0]["script"]["exec"].as_array().unwrap();
    assert_eq!(test_lines[0], "console.log('mine');");
    assert!(test_lines.iter().any(|line| {
        line.as_str()
            .unwrap()
            .starts_with("pm.test(\"Status is 201\"")
    }));
    let warnings = exported.warnings.join("\n");
    for needle in ["1 assertion cannot run", "own timeout", "own retry count"] {
        assert!(warnings.contains(needle), "{warnings}");
    }

    let back = import_collection(&exported.json, "ws_1").unwrap();
    let checked_back = back.requests.iter().find(|r| r.name == "Checked").unwrap();
    assert_eq!(checked_back.assertions, checked.assertions);
    assert_eq!(checked_back.test_script, "console.log('mine');");
    assert_eq!(checked_back.examples, checked.examples);
    let options = &checked_back.network_options;
    // The file asks to skip certificate checks; only the user may weaken the transport.
    assert!(!options.follow_redirects && options.verify_tls);
    assert!(
        back.warnings.iter().any(|w| w.contains("'Checked'")
            && w.contains("strictSSL: false")
            && w.contains("not applied")),
        "{:?}",
        back.warnings
    );
    assert_eq!(options.max_redirects, 4);
    assert_eq!(options.http_version, HttpVersionPreference::Http2);
    let xml_back = back.requests.iter().find(|r| r.name == "Xml").unwrap();
    assert_eq!(
        xml_back.body,
        RequestBody::RawText {
            content: "<a/>".into(),
            language: Some(RawLanguage::Xml)
        }
    );
    assert!(xml_back.network_options.verify_tls);
}

#[test]
fn imported_behavior_is_inherited_and_unknown_keys_are_reported_where_declared() {
    let payload = json!({
        "info": {"name": "Inherit"},
        "protocolProfileBehavior": {"strictSSL": false, "insecureHTTPParser": true},
        "item": [{
            "name": "Folder",
            "protocolProfileBehavior": {"followRedirects": false},
            "item": [
                {"name": "Own", "protocolProfileBehavior": {"followRedirects": true},
                 "request": {"method": "GET", "url": "https://x.test/1"}},
                {"name": "Inherits", "request": {"method": "GET", "url": "https://x.test/2"},
                 "response": (0..22).map(|n| json!({"name": format!("E{n}"), "code": 200, "body": ""})).collect::<Vec<_>>()}
            ]
        }]
    })
    .to_string();
    let imported = import_collection(&payload, "ws_1").unwrap();
    let own = imported.requests.iter().find(|r| r.name == "Own").unwrap();
    let inherits = imported
        .requests
        .iter()
        .find(|r| r.name == "Inherits")
        .unwrap();
    assert!(own.network_options.follow_redirects && own.network_options.verify_tls);
    assert!(!inherits.network_options.follow_redirects && inherits.network_options.verify_tls);
    assert_eq!(inherits.examples.len(), 20);
    let warnings = imported.warnings.join("\n");
    assert_eq!(
        warnings.matches("insecureHTTPParser").count(),
        1,
        "{warnings}"
    );
    assert!(
        warnings.contains("only the first 20 saved examples"),
        "{warnings}"
    );
    // Reported once where declared, though every request inherits it.
    assert_eq!(
        warnings.matches("strictSSL: false").count(),
        1,
        "{warnings}"
    );
}

#[test]
fn imported_behavior_cannot_forward_authorization_across_redirects() {
    let payload = json!({
        "info": {"name": "Hostile"},
        "item": [{
            "name": "Token",
            "protocolProfileBehavior": {"followAuthorizationHeader": true, "strictSSL": false},
            "request": {"method": "GET", "url": "https://x.test/1"}
        }]
    })
    .to_string();
    let imported = import_collection(&payload, "ws_1").unwrap();
    let options = &imported.requests[0].network_options;
    assert!(!options.follow_auth_header && options.verify_tls);
    let warnings = imported.warnings.join("\n");
    assert!(
        warnings.contains("followAuthorizationHeader") && warnings.contains("not applied"),
        "{warnings}"
    );
}

#[test]
fn a_line_separator_in_an_assertion_cannot_end_the_marker_comment() {
    let mut checked = request("req_1", "Separator");
    checked.assertions = vec![Assertion::BodyContains {
        value: "x\u{2028};pm.sendRequest('https://evil.test/');//\u{2029}".into(),
    }];
    let exported = export(std::slice::from_ref(&checked), &[]);
    let envelope: Value = serde_json::from_str(&exported.json).unwrap();
    let lines = envelope["item"][0]["event"][0]["script"]["exec"]
        .as_array()
        .unwrap();
    for line in lines {
        let line = line.as_str().unwrap();
        assert!(!line.contains(['\u{2028}', '\u{2029}']), "{line:?}");
    }
    let back = import_collection(&exported.json, "ws_1").unwrap();
    assert_eq!(back.requests[0].assertions, checked.assertions);
}

#[test]
fn an_environment_overrides_and_extends_the_collection_variables() {
    let mut collection = Collection::new("col_1", "ws_1", "Vars").unwrap();
    collection.variables = vec![
        EnvironmentVariable::plain("baseUrl", "https://collection.test").unwrap(),
        EnvironmentVariable::plain("page", "1").unwrap(),
    ];
    let mut disabled = EnvironmentVariable::plain("page", "99").unwrap();
    disabled.enabled = false;
    let environment = vec![
        EnvironmentVariable::plain("baseUrl", "https://staging.test").unwrap(),
        EnvironmentVariable::masked("token", "sk-live").unwrap(),
        disabled,
    ];
    let exported = export_collection(
        &collection,
        &[],
        &[],
        &environment,
        &PostmanExportContext::default(),
    )
    .unwrap();
    let envelope: Value = serde_json::from_str(&exported.json).unwrap();
    let pairs: Vec<(String, String)> = envelope["variable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["key"].as_str().unwrap().to_owned(),
                v["value"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        [
            ("baseUrl".to_owned(), "https://staging.test".to_owned()),
            ("page".to_owned(), "1".to_owned()),
            // A secret value never leaves the app.
            ("token".to_owned(), String::new()),
        ]
    );
}
