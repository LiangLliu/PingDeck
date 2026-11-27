//! cURL transport flags and the Insomnia, Bruno and Hoppscotch importers, plus OpenAPI response
//! examples on requests.

use serde_json::json;

use super::*;
use crate::workspace::models::{
    ApiKeyLocation, Assertion, AuthConfig, HttpMethod, HttpVersionPreference, ProtocolKind,
    RawLanguage, Request, RequestBody,
};

fn named<'a>(imported: &'a ImportedCollection, name: &str) -> &'a Request {
    imported
        .requests
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("{name} not imported"))
}

#[test]
fn curl_transport_flags_become_network_options_and_the_rest_is_reported() {
    let imported = import_curl(
        "curl -L --max-redirs 3 -k -m 1.5 --retry 2 -x proxy.test:3128 --http1.1 --compressed \
         --connect-timeout 4 --cert me.pem -s -o out.json https://x.test/a",
        "ws_1",
    )
    .unwrap();
    let options = &imported.requests[0].network_options;
    assert!(options.follow_redirects && !options.verify_tls);
    assert_eq!(options.max_redirects, 3);
    assert_eq!(options.total_timeout_ms, Some(1_500));
    assert_eq!(options.retry_count, 2);
    assert_eq!(options.proxy_url.as_deref(), Some("http://proxy.test:3128"));
    assert_eq!(options.http_version, HttpVersionPreference::Http11);
    let warnings = imported.warnings.join("\n");
    // Output and decompression flags change nothing about the request; the others say why.
    assert!(
        !warnings.contains("--compressed") && !warnings.contains("'-o'"),
        "{warnings}"
    );
    assert!(warnings.contains("--connect-timeout"), "{warnings}");
    assert!(warnings.contains("Settings › Certificates"), "{warnings}");

    let bad = import_curl("curl -m soon https://x.test/a", "ws_1").unwrap();
    assert_eq!(bad.requests[0].network_options.total_timeout_ms, None);
    assert!(bad.warnings.iter().any(|w| w.contains("-m 'soon'")));
}

#[test]
fn insomnia_export_becomes_a_collection_with_folders_and_base_variables() {
    let payload = json!({
        "_type": "export", "__export_format": 4,
        "resources": [
            {"_id": "wrk_1", "_type": "workspace", "name": "Shop", "description": "d"},
            {"_id": "env_base", "_type": "environment", "parentId": "wrk_1", "data": {"baseUrl": "https://shop.test"}},
            {"_id": "env_sub", "_type": "environment", "parentId": "env_base", "data": {"baseUrl": "https://staging"}},
            {"_id": "fld_1", "_type": "request_group", "parentId": "wrk_1", "name": "Orders", "metaSortKey": 1},
            {"_id": "req_2", "_type": "request", "parentId": "fld_1", "name": "Create", "method": "POST",
             "url": "{{ _.baseUrl }}/orders?draft=1", "metaSortKey": 2,
             "headers": [{"name": "X-Off", "value": "1", "disabled": true}],
             "body": {"mimeType": "application/json", "text": "{\"sku\": \"{{ _.sku }}\"}"},
             "authentication": {"type": "apikey", "key": "X-Key", "value": "k", "addTo": "queryParams"}},
            {"_id": "req_1", "_type": "request", "parentId": "fld_1", "name": "List", "method": "GET",
             "url": "{{baseUrl}}/orders/{% uuid %}", "metaSortKey": 1, "authentication": {}},
            {"_id": "req_3", "_type": "request", "parentId": "wrk_1", "name": "Xml", "method": "PUT",
             "url": "https://shop.test/x", "body": {"mimeType": "application/xml", "text": "<a/>"}},
            {"_id": "grpc_1", "_type": "grpc_request", "parentId": "wrk_1", "name": "Stream"}
        ]
    })
    .to_string();
    let imported = import_insomnia(&payload, "ws_1").unwrap();
    assert_eq!(imported.collection.name, "Shop");
    assert_eq!(imported.collection.variables[0].key, "baseUrl");
    assert_eq!(imported.folders.len(), 1);
    let create = named(&imported, "Create");
    assert_eq!(create.url, "{{baseUrl}}/orders");
    assert_eq!(create.query_params[0].key, "draft");
    assert!(!create.headers[0].enabled);
    assert_eq!(
        create.body,
        RequestBody::Json {
            content: "{\"sku\": \"{{sku}}\"}".into()
        }
    );
    assert!(matches!(
        &create.auth,
        AuthConfig::ApiKey {
            location: ApiKeyLocation::Query,
            ..
        }
    ));
    let list = named(&imported, "List");
    // `metaSortKey` orders the folder: List before Create.
    assert!(list.position < create.position);
    assert_eq!(list.auth, AuthConfig::Inherit);
    assert!(matches!(
        named(&imported, "Xml").body,
        RequestBody::RawText {
            language: Some(RawLanguage::Xml),
            ..
        }
    ));
    let warnings = imported.warnings.join("\n");
    for needle in ["template tags", "1 sub-environment", "grpc_request"] {
        assert!(warnings.contains(needle), "{warnings}");
    }
    assert!(matches!(
        import_insomnia("{\"_type\": \"export\", \"__export_format\": 5}", "ws_1"),
        Err(ImportError::UnsupportedDocument { .. })
    ));
}

#[test]
fn bruno_collection_imports_folders_params_auth_and_status_assertions() {
    let payload = json!({
        "name": "Bruno API", "version": "1",
        "root": {"request": {"auth": {"mode": "bearer", "bearer": {"token": "{{token}}"}}}},
        "environments": [{"name": "Local", "variables": []}],
        "items": [
            {"type": "folder", "name": "Users", "items": [
                {"type": "http-request", "name": "Get user", "seq": 2, "request": {
                    "url": "{{baseUrl}}/users/:id?verbose=true", "method": "GET",
                    "params": [{"name": "verbose", "value": "true", "type": "query", "enabled": true},
                               {"name": "id", "value": "42", "type": "path", "enabled": true}],
                    "headers": [{"name": "Accept", "value": "application/json", "enabled": true}],
                    "auth": {"mode": "inherit"},
                    "script": {"req": "bru.setVar('a', 1)"},
                    "assertions": [{"name": "res.status", "value": "eq 200", "enabled": true},
                                   {"name": "res.body.id", "value": "isDefined", "enabled": true}]
                }},
                {"type": "graphql-request", "name": "Query", "seq": 1, "request": {
                    "url": "{{baseUrl}}/graphql", "method": "POST",
                    "body": {"mode": "graphql", "graphql": {"query": "{ me { id } }", "variables": ""}}
                }}
            ]}
        ]
    })
    .to_string();
    let imported = import_bruno(&payload, "ws_1").unwrap();
    assert!(matches!(
        imported.collection.auth,
        AuthConfig::Bearer { .. }
    ));
    let user = named(&imported, "Get user");
    assert_eq!(user.url, "{{baseUrl}}/users/:id");
    assert_eq!(user.query_params[0].key, "verbose");
    assert_eq!(user.path_variables[0].value, "42");
    assert_eq!(
        user.assertions,
        vec![Assertion::StatusEquals { value: 200 }]
    );
    assert_eq!(user.pre_request_script, "bru.setVar('a', 1)");
    let query = named(&imported, "Query");
    assert_eq!(query.protocol, ProtocolKind::Graphql);
    // `seq` orders the folder.
    assert!(query.position < user.position);
    let warnings = imported.warnings.join("\n");
    for needle in ["Bruno scripts", "1 Bruno assertion", "1 Bruno environment"] {
        assert!(warnings.contains(needle), "{warnings}");
    }
}

#[test]
fn hoppscotch_collections_convert_variables_and_nest_several_as_folders() {
    let collection = |name: &str| {
        json!({
            "v": 2, "name": name, "folders": [{"v": 2, "name": "Inner", "folders": [], "requests": [
                {"name": "Form", "method": "POST", "endpoint": "<<base>>/form",
                 "params": [], "headers": [{"key": "X-A", "value": "<<a>>", "active": false}],
                 "auth": {"authType": "basic", "authActive": true, "username": "u", "password": "<<pw>>"},
                 "body": {"contentType": "multipart/form-data", "body": [
                     {"key": "f", "value": "", "active": true, "isFile": true},
                     {"key": "t", "value": "<<v>>", "active": true, "isFile": false}]},
                 "preRequestScript": "pw.env.set('a', '1')", "testScript": ""}
            ]}],
            "requests": [{"name": "Ping", "method": "OPTIONS", "endpoint": "https://h.test/p?x=1",
                          "params": [{"key": "y", "value": "2", "active": true}], "headers": [],
                          "auth": {"authType": "inherit", "authActive": true},
                          "body": {"contentType": null, "body": null}}]
        })
    };
    let single = import_hoppscotch(&collection("Hop").to_string(), "ws_1").unwrap();
    assert_eq!(single.collection.name, "Hop");
    let form = named(&single, "Form");
    assert_eq!(form.url, "{{base}}/form");
    assert_eq!(form.headers[0].value, "{{a}}");
    assert!(!form.headers[0].enabled);
    assert_eq!(
        form.auth,
        AuthConfig::Basic {
            username: "u".into(),
            password_ref: "{{pw}}".into()
        }
    );
    let RequestBody::Multipart { parts } = &form.body else {
        panic!("multipart")
    };
    assert_eq!(
        (parts[0].file_path.as_deref(), parts[1].value.as_str()),
        (Some(""), "{{v}}")
    );
    let ping = named(&single, "Ping");
    assert_eq!(
        (ping.method, ping.query_params.len()),
        (HttpMethod::Options, 2)
    );

    let several = import_hoppscotch(
        &json!([collection("A"), collection("B")]).to_string(),
        "ws_1",
    )
    .unwrap();
    assert_eq!(several.collection.name, "Hoppscotch import");
    // A and B, each with Inner.
    assert_eq!(several.folders.len(), 4);
    assert!(matches!(
        import_hoppscotch("{\"name\": \"x\"}", "ws_1"),
        Err(ImportError::UnsupportedDocument { .. })
    ));
}

#[test]
fn openapi_operations_keep_their_explicit_response_examples() {
    let spec = json!({
        "openapi": "3.0.0", "info": {"title": "Pets", "version": "1"},
        "paths": {"/pets": {"get": {"responses": {
            "200": {"description": "A list", "content": {"application/json": {"example": [{"id": 1}]}}},
            "404": {"description": "Missing", "content": {"application/json": {"schema": {"type": "object"}}}}
        }}}}
    })
    .to_string();
    let imported = import_openapi(&spec, "ws_1").unwrap();
    let examples = &imported.requests[0].examples;
    // The 404 has only a schema: a generated body is not an example.
    assert_eq!(examples.len(), 1);
    assert_eq!(
        (examples[0].name.as_str(), examples[0].status),
        ("A list", 200)
    );
    assert!(examples[0].body.contains("\"id\": 1"));
    assert_eq!(examples[0].headers[0].value, "application/json");
}
