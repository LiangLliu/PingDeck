use serde_json::Value;

use super::*;
use crate::workspace::ai_request::AiTarget;
use crate::workspace::models::{
    AuthConfig, FolderId, FormPart, HttpMethod, KeyValue, OAuth2GrantType, ProtocolKind,
    RequestBody,
};
use pingdeck_model::wire::Protocol;

const SAMPLE: &str = r#"{
  "info": {"name": "Billing APIs", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
  "item": [
    {
      "name": "Invoices",
      "item": [{
        "name": "List invoices",
        "request": {
          "method": "POST",
          "header": [
            {"key": "Accept", "value": "application/json"},
            {"key": "X-Debug", "value": "1", "disabled": true}
          ],
          "url": {
            "raw": "https://api.example.com/v1/invoices?page=2&size=50",
            "query": [{"key": "page", "value": "2"}, {"key": "size", "value": "50"}]
          },
          "body": {"mode": "raw", "raw": "{\"amount\": 10}", "options": {"raw": {"language": "json"}}},
          "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "{{token}}", "type": "string"}]}
        }
      }]
    },
    {"name": "Health", "request": {"method": "GET", "url": "https://api.example.com/health"}}
  ],
  "variable": [
    {"key": "baseUrl", "value": "https://api.example.com"},
    {"key": "token", "value": "secret://staging/token"},
    {"key": "scope", "value": "read", "disabled": true}
  ]
}"#;

fn kv(key: &str, value: &str, enabled: bool) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: value.into(),
        enabled,
        description: None,
    }
}

fn request_named<'a>(import: &'a PostmanImport, name: &str) -> &'a Request {
    import
        .requests
        .iter()
        .find(|r| r.name == name)
        .expect("request should be imported")
}

fn request_folder_name(import: &PostmanImport, request: &Request) -> Option<String> {
    let folder_id: &FolderId = request.folder_id.as_ref()?;
    import
        .folders
        .iter()
        .find(|f| &f.id == folder_id)
        .map(|f| f.name.clone())
}

#[test]
fn import_and_export_round_trip_nested_collection_and_deterministic_ids() {
    let imported =
        import_collection(SAMPLE, "ws_billing").expect("sample collection should import");
    let second = import_collection(SAMPLE, "ws_billing").expect("deterministic re-import");
    assert_eq!(imported.collection.id.as_str(), "col_billing_apis_1");
    assert_eq!(imported.collection.id, second.collection.id);
    assert_eq!(imported.collection.name, "Billing APIs");
    assert_eq!(imported.collection.workspace_id.as_str(), "ws_billing");
    assert_eq!(imported.collection.auth, AuthConfig::Inherit);
    assert_eq!(imported.folders.len(), 1);
    let folder = &imported.folders[0];
    assert_eq!(
        (folder.id.as_str(), folder.name.as_str(), folder.position),
        ("fold_billing_apis_2", "Invoices", 0)
    );
    assert_eq!(folder.collection_id, imported.collection.id);

    let list = request_named(&imported, "List invoices");
    assert_eq!(
        (list.id.as_str(), list.method, list.url.as_str()),
        (
            "req_billing_apis_3",
            HttpMethod::Post,
            "https://api.example.com/v1/invoices"
        )
    );
    assert_eq!(
        list.query_params,
        vec![kv("page", "2", true), kv("size", "50", true)]
    );
    assert_eq!(
        list.headers,
        vec![
            kv("Accept", "application/json", true),
            kv("X-Debug", "1", false)
        ]
    );
    assert_eq!(
        list.body,
        RequestBody::Json {
            content: "{\"amount\": 10}".into()
        }
    );
    assert_eq!(
        list.auth,
        AuthConfig::Bearer {
            token_ref: "{{token}}".into()
        }
    );
    assert_eq!(
        request_folder_name(&imported, list).as_deref(),
        Some("Invoices")
    );

    let health = request_named(&imported, "Health");
    assert_eq!(
        (
            health.method,
            health.url.as_str(),
            health.folder_id.as_ref(),
            health.position
        ),
        (HttpMethod::Get, "https://api.example.com/health", None, 1)
    );
    assert_eq!(imported.variables.len(), 3);
    assert!(!imported.variables[1].is_secret && !imported.variables[2].enabled);

    let exported = export_collection(
        &imported.collection,
        &imported.folders,
        &imported.requests,
        &imported.variables,
        &PostmanExportContext::default(),
    )
    .expect("collection should export")
    .json;
    let envelope: Value = serde_json::from_str(&exported).expect("export should be JSON");
    assert_eq!(
        (
            envelope["info"]["name"].as_str(),
            envelope["info"]["schema"].as_str()
        ),
        (Some("Billing APIs"), Some(POSTMAN_SCHEMA_URL))
    );
    assert_eq!(envelope["variable"][1]["value"], "secret://staging/token");
    assert!(envelope["variable"][1].get("type").is_none());

    let reimported =
        import_collection(&exported, "ws_billing").expect("exported collection should import");
    assert_eq!(reimported.variables, imported.variables);
    for name in ["List invoices", "Health"] {
        let (before, after) = (
            request_named(&imported, name),
            request_named(&reimported, name),
        );
        assert_eq!(
            (
                &after.method,
                &after.url,
                &after.query_params,
                &after.headers,
                &after.body,
                &after.auth
            ),
            (
                &before.method,
                &before.url,
                &before.query_params,
                &before.headers,
                &before.body,
                &before.auth
            )
        );
        assert_eq!(
            request_folder_name(&reimported, after),
            request_folder_name(&imported, before)
        );
    }
}

#[test]
fn query_sync_fragments_and_blank_urls_behave_consistently() {
    let collection = Collection::new("col_queries", "ws_1", "Queries").expect("collection");
    let mut synced = Request::new(
        "req_synced",
        "ws_1",
        "Synced",
        "{{baseUrl}}/items?tag=one&tag=two&search=old#section?ignored",
    )
    .unwrap()
    .with_collection("col_queries")
    .unwrap();
    synced.query_params = vec![
        kv("tag", "one", true),
        kv("tag", "two", true),
        kv("search", "{{term}} & café", true),
        kv("{{key}}", "a+b", true),
        kv("off", "secret", false),
    ];
    let mut partial = Request::new(
        "req_partial",
        "ws_1",
        "Partial",
        "https://example.com/?keep=a%26b&x=old#frag",
    )
    .unwrap()
    .with_collection("col_queries")
    .unwrap();
    partial.query_params = vec![kv("x", "new=1", true), kv("off", "no", false)];
    let url_only = Request::new(
        "req_url",
        "ws_1",
        "URL only",
        "https://example.com/?a=1&a=2#frag",
    )
    .unwrap()
    .with_collection("col_queries")
    .unwrap();

    let exported = export_collection(
        &collection,
        &[],
        &[synced.clone(), partial, url_only],
        &[],
        &PostmanExportContext::default(),
    )
    .unwrap()
    .json;
    let envelope: Value = serde_json::from_str(&exported).unwrap();
    let items = envelope["item"].as_array().unwrap();
    let url = |name: &str| &items.iter().find(|i| i["name"] == name).unwrap()["request"]["url"];
    assert_eq!(
        url("Synced")["raw"],
        "{{baseUrl}}/items?tag=one&tag=two&search={{term}}+%26+caf%C3%A9&{{key}}=a%2Bb#section?ignored"
    );
    assert_eq!(
        url("Partial")["raw"],
        "https://example.com/?keep=a%26b&x=new%3D1#frag"
    );
    assert_eq!(url("URL only")["raw"], "https://example.com/?a=1&a=2#frag");

    let reimported = import_collection(&exported, "ws_1").unwrap();
    assert_eq!(
        request_named(&reimported, "Synced").url,
        "{{baseUrl}}/items#section?ignored"
    );
    assert_eq!(
        request_named(&reimported, "Synced").query_params,
        synced.query_params
    );
    assert_eq!(
        request_named(&reimported, "Partial").url,
        "https://example.com/#frag"
    );
    assert_eq!(
        request_named(&reimported, "Partial").query_params[0].value,
        "a&b"
    );

    let strings_and_blanks = import_collection(
        r##"{
          "info": {"name": "Urls"},
          "item": [
            {"name": "String request", "request": "https://example.com/a?x=1#one"},
            {"name": "String URL", "request": {"url": "https://example.com/b#two?not=query"}},
            {"name": "Spaces string", "request": {"method": "GET", "url": "   "}},
            {"name": "Spaces raw", "request": {"method": "GET", "url": {"raw": "  "}}},
            {"name": "Spaces bare", "request": "   "},
            {"name": "Fragment only", "request": {"method": "GET", "url": "  #frag"}}
          ]
        }"##,
        "ws_1",
    )
    .unwrap();
    assert_eq!(strings_and_blanks.requests.len(), 2);
    assert_eq!(
        request_named(&strings_and_blanks, "String request").url,
        "https://example.com/a#one"
    );
    assert_eq!(
        request_named(&strings_and_blanks, "String URL").url,
        "https://example.com/b#two?not=query"
    );
    let warnings = strings_and_blanks.warnings.join("\n");
    for skipped in [
        "Spaces string",
        "Spaces raw",
        "Spaces bare",
        "Fragment only",
    ] {
        assert!(warnings.contains(skipped), "{warnings}");
    }
}

#[test]
fn validation_warnings_file_path_stripping_and_pkce_descriptions_work() {
    assert!(matches!(
        import_collection("{\"info\":", "ws_1"),
        Err(PostmanError::InvalidJson { .. })
    ));
    assert!(matches!(
        import_collection("[1, 2, 3]", "ws_1"),
        Err(PostmanError::InvalidJson { .. })
    ));
    assert_eq!(
        import_collection("{\"item\": []}", "ws_1").unwrap_err(),
        PostmanError::MissingCollectionName
    );

    let odd_json = r#"{
      "info": {"name": "Odd collection"},
      "protocolProfileBehavior": {"disableBodyPruning": true, "insecureHTTPParser": true},
      "item": [
        {
          "name": "OAuth call",
          "event": [{"listen": "test", "script": {"exec": ["pm.test('ok', () => {})"]}}],
          "request": {
            "method": "PROPFIND",
            "url": "https://api.example.com/oauth",
            "auth": {"type": "oauth2", "oauth2": []},
            "body": {"mode": "file", "file": {"src": "/tmp/payload.bin"}}
          }
        },
        {
          "name": "Upload",
          "request": {
            "method": "POST",
            "url": "https://api.example.com/upload",
            "body": {"mode": "formdata", "formdata": [{"key": "attachment", "type": "file", "src": "/tmp/report.pdf"}]}
          }
        },
        {
          "name": "Graph query",
          "request": {
            "method": "POST",
            "url": "https://api.example.com/graphql",
            "body": {"mode": "graphql", "graphql": {"query": "query Q { ping }", "variables": "{\"id\": 1}"}}
          }
        },
        {
          "name": "Search",
          "request": {
            "method": "GET",
            "header": [
              {"key": "X-A", "value": "1", "description": "plain note"},
              {"key": "X-B", "value": "2", "description": {"content": "object note", "type": "text/plain"}},
              {"key": "X-C", "value": "3", "description": ""}
            ],
            "url": {"raw": "https://x.test/s?q=1", "query": [{"key": "q", "value": "1", "description": "the query"}]},
            "auth": {"type": "oauth2", "oauth2": [
              {"key": "grant_type", "value": "authorization_code_with_pkce"},
              {"key": "accessTokenUrl", "value": "https://auth.example.com/token"},
              {"key": "authUrl", "value": "https://auth.example.com/authorize"},
              {"key": "clientId", "value": "cid"},
              {"key": "redirect_uri", "value": "http://127.0.0.1:8976/callback"}
            ]}
          }
        }
      ],
      "variable": [{"key": "   ", "value": "ignored"}, {"key": "bad", "value": {"nested": true}}]
    }"#;
    let imported = import_collection(odd_json, "ws_billing").unwrap();
    let warnings = imported.warnings.join("\n");
    for needle in [
        "protocolProfileBehavior insecureHTTPParser ignored",
        "oauth2",
        "PROPFIND",
        "blank key",
        "non-scalar",
    ] {
        assert!(warnings.contains(needle), "{warnings}");
    }
    assert!(
        !warnings.contains("/tmp/")
            && warnings.matches("local file path was not imported").count() == 2
    );

    let oauth = request_named(&imported, "OAuth call");
    assert_eq!(
        (oauth.method, &oauth.auth, oauth.test_script.as_str()),
        (
            HttpMethod::Get,
            &AuthConfig::Inherit,
            "pm.test('ok', () => {})"
        )
    );
    assert_eq!(
        oauth.body,
        RequestBody::Binary {
            file_path: String::new(),
            content_type: None
        }
    );
    assert_eq!(
        request_named(&imported, "Graph query").protocol,
        ProtocolKind::Graphql
    );

    let search = request_named(&imported, "Search");
    let notes: Vec<_> = search
        .headers
        .iter()
        .map(|r| r.description.as_deref())
        .collect();
    assert_eq!(notes, [Some("plain note"), Some("object note"), None]);
    assert!(matches!(
        &search.auth,
        AuthConfig::OAuth2 {
            grant_type: OAuth2GrantType::AuthorizationCodePkce,
            redirect_port: Some(8976),
            ..
        }
    ));

    let collection = Collection::new("col_files", "ws_1", "Files").unwrap();
    let part = |key: &str, path: &str| FormPart {
        key: key.into(),
        value: String::new(),
        file_path: Some(path.into()),
        enabled: true,
    };
    let mut literal = Request::new("req_literal", "ws_1", "Literal", "https://x.test/a")
        .unwrap()
        .with_collection("col_files")
        .unwrap();
    literal.body = RequestBody::Binary {
        file_path: "/etc/passwd".into(),
        content_type: Some("text/plain".into()),
    };
    let mut templated = Request::new("req_tpl", "ws_1", "Templated", "https://x.test/b")
        .unwrap()
        .with_collection("col_files")
        .unwrap();
    templated.body = RequestBody::Binary {
        file_path: "{{filePath}}".into(),
        content_type: None,
    };
    let mut form = Request::new("req_form", "ws_1", "Form", "https://x.test/c")
        .unwrap()
        .with_collection("col_files")
        .unwrap();
    form.body = RequestBody::Multipart {
        parts: vec![
            part("secret", "C:\\secrets\\a.txt"),
            part("tpl", "{{filePath}}"),
            part("two", "{{a}}{{b}}"),
            FormPart {
                key: "note".into(),
                value: "hi".into(),
                file_path: None,
                enabled: true,
            },
        ],
    };
    let exported = export_collection(
        &collection,
        &[],
        &[literal, templated, form],
        &[],
        &PostmanExportContext::default(),
    )
    .unwrap()
    .json;
    assert!(!exported.contains("passwd") && !exported.contains("secrets"));
    let reimported = import_collection(&exported, "ws_1").unwrap();
    assert_eq!(
        request_named(&reimported, "Literal").body,
        RequestBody::Binary {
            file_path: String::new(),
            content_type: Some("text/plain".into())
        }
    );
    assert_eq!(
        request_named(&reimported, "Templated").body,
        RequestBody::Binary {
            file_path: "{{filePath}}".into(),
            content_type: None
        }
    );
    let RequestBody::Multipart { parts } = &request_named(&reimported, "Form").body else {
        panic!("multipart")
    };
    assert_eq!(
        parts
            .iter()
            .map(|p| p.file_path.as_deref())
            .collect::<Vec<_>>(),
        vec![Some(""), Some("{{filePath}}"), Some(""), None]
    );
}

#[test]
fn ai_requests_export_with_protocol_placeholder_or_custom_target() {
    use crate::workspace::ai_request::{AiRequestConfig, RequestMode, non_routable_label};
    let make = |id: &str, protocol: Protocol, target: AiTarget, url: &str| {
        let mut req = Request::new(id, "ws_1", id, url)
            .unwrap()
            .with_collection("col_ai")
            .unwrap();
        req.method = HttpMethod::Post;
        req.mode = RequestMode::AiApi;
        req.ai = Some(AiRequestConfig {
            protocol,
            target,
            builder: None,
            compare_models: Vec::new(),
        });
        req.headers = vec![
            kv("Authorization", "Bearer sk-user-typed", true),
            kv("x-trace", "1", true),
        ];
        req.body = RequestBody::Json {
            content: "{\"model\":\"m\"}".into(),
        };
        req
    };
    let prov = || AiTarget::Provider {
        provider_id: "prov_1".into(),
        provider_name: "DeepSeek".into(),
        base_url_hint: "https://api.deepseek.com".into(),
        model: "m".into(),
    };
    let mut ask = make(
        "ask",
        Protocol::OpenaiChat,
        prov(),
        &non_routable_label("DeepSeek", "/chat/completions"),
    );
    ask.body = RequestBody::Json {
        content: "{\"messages\":[{\"image\":\"{{attachment:aa11}}\"},{\"image\":\"{{attachment:bb22}}\"}]}".into(),
    };
    ask.auth = AuthConfig::Bearer {
        token_ref: "secret://x".into(),
    };
    let anthropic = make("a", Protocol::AnthropicMessages, prov(), "x");
    let gemini = make("g", Protocol::Gemini, prov(), "x");
    let unresolved = make(
        "p",
        Protocol::OpenaiChat,
        prov(),
        "pingdeck-provider://deepseek/chat/completions",
    );
    let custom = make(
        "c",
        Protocol::OpenaiChat,
        AiTarget::Custom { model: "m".into() },
        "https://relay.example/v1/chat/completions",
    );

    let collection = Collection::new("col_ai", "ws_1", "AI").unwrap();
    let context = PostmanExportContext {
        ai_urls: [
            ("ask", "https://api.deepseek.com/chat/completions"),
            ("a", "https://a.test/v1/messages"),
            ("g", "https://g.test/m"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect(),
        ..PostmanExportContext::default()
    };
    let export = export_collection(
        &collection,
        &[],
        &[ask, anthropic, gemini, unresolved, custom],
        &[],
        &context,
    )
    .unwrap();
    let envelope: Value = serde_json::from_str(&export.json).unwrap();
    let items = envelope["item"].as_array().unwrap();
    let item = |name: &str| items.iter().find(|i| i["name"] == name).unwrap();
    let headers = |name: &str| -> Vec<(String, String)> {
        item(name)["request"]["header"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["key"].as_str().unwrap().to_owned(),
                    h["value"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };

    assert_eq!(
        item("ask")["request"]["url"]["raw"],
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(
        item("ask")["request"]["auth"],
        serde_json::json!({"type": "noauth"})
    );
    assert!(headers("ask").contains(&("Authorization".into(), "Bearer {{api_key}}".into())));
    assert!(
        ["ask", "a", "g", "c"]
            .iter()
            .all(|name| { !item(name).to_string().contains("pingdeck-provider") })
    );
    assert_eq!(
        item("p")["request"]["url"]["raw"],
        "pingdeck-provider://deepseek/chat/completions"
    );
    assert!(headers("a").contains(&("x-api-key".into(), "{{api_key}}".into())));
    assert!(!headers("a").iter().any(|(k, _)| k == "Authorization"));
    assert!(headers("g").contains(&("x-goog-api-key".into(), "{{api_key}}".into())));
    assert_eq!(
        item("c")["request"]["url"]["raw"],
        "https://relay.example/v1/chat/completions"
    );
    // A literal credential header is exported empty by default.
    assert!(headers("c").contains(&("Authorization".into(), String::new())));
    assert!(export.warnings.iter().any(|w| w.contains("2 attachments")));
    assert!(
        export
            .warnings
            .iter()
            .any(|w| w.contains("\"p\"") && w.contains("could not be resolved"))
    );
}

#[test]
fn export_leaves_literal_credentials_empty_unless_asked_and_keeps_variable_references() {
    let mut collection = Collection::new("col_auth", "ws_1", "Auth").unwrap();
    collection.auth = AuthConfig::Bearer {
        token_ref: "sk-live-literal".into(),
    };
    let mut request = Request::new(
        "req_auth",
        "ws_1",
        "Me",
        "https://bob:pw-literal@x.test/me?api_key=AIza-literal&page=1",
    )
    .unwrap()
    .with_collection("col_auth")
    .unwrap();
    request.query_params = vec![kv("token", "q-literal", true)];
    request.auth = AuthConfig::Basic {
        username: "ada".into(),
        password_ref: "{{password}}".into(),
    };
    request.headers = vec![
        kv("X-Api-Key", "k-literal", true),
        kv("Authorization", "Bearer {{token}}", true),
        kv("Accept", "application/json", true),
    ];
    let export = |include_literal_credentials| {
        let context = PostmanExportContext {
            include_literal_credentials,
            ..PostmanExportContext::default()
        };
        export_collection(&collection, &[], &[request.clone()], &[], &context).unwrap()
    };

    let stripped = export(false);
    for literal in [
        "sk-live-literal",
        "k-literal",
        "pw-literal",
        "AIza-literal",
        "q-literal",
    ] {
        assert!(!stripped.json.contains(literal), "{literal} leaked");
    }
    let envelope: Value = serde_json::from_str(&stripped.json).unwrap();
    assert_eq!(envelope["auth"]["bearer"][0]["value"], "");
    let item = &envelope["item"][0]["request"];
    // URL userinfo and credential query values follow the same rule as headers and auth.
    assert_eq!(
        item["url"]["raw"],
        "https://x.test/me?api_key=&page=1&token="
    );
    assert_eq!(item["auth"]["basic"][0]["value"], "ada");
    assert_eq!(item["auth"]["basic"][1]["value"], "{{password}}");
    let header = |name: &str| {
        item["header"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["key"] == name)
            .unwrap()["value"]
            .clone()
    };
    assert_eq!(
        (
            header("X-Api-Key"),
            header("Authorization"),
            header("Accept")
        ),
        (
            "".into(),
            "Bearer {{token}}".into(),
            "application/json".into()
        )
    );
    assert!(
        stripped
            .warnings
            .iter()
            .any(|w| w.starts_with("5 literal credentials were left empty"))
    );

    let kept = export(true);
    assert!(kept.json.contains("sk-live-literal") && kept.json.contains("k-literal"));
    assert!(kept.json.contains("bob:pw-literal@") && kept.json.contains("AIza-literal"));
    assert!(!kept.warnings.iter().any(|w| w.contains("literal")));
}

/// OAuth 2.0 extra parameters survive a Postman round trip as `tokenRequestParams` (and
/// `authRequestParams` for the interactive grant); header-sent rows are dropped with a warning.
#[test]
fn oauth2_extra_params_round_trip_through_postman() {
    let collection = Collection::new("col_oauth", "ws_1", "OAuth").unwrap();
    let mut request = Request::new("req_oauth", "ws_1", "Token", "https://x.test/me")
        .unwrap()
        .with_collection("col_oauth")
        .unwrap();
    let extra = vec![
        kv("audience", "https://api.example.com", true),
        kv("resource", "r1", false),
    ];
    request.auth = AuthConfig::OAuth2 {
        grant_type: OAuth2GrantType::AuthorizationCodePkce,
        access_token_url: "https://auth.example.com/token".into(),
        client_id: "cid".into(),
        client_secret_ref: String::new(),
        username: String::new(),
        password_ref: String::new(),
        scope: String::new(),
        authorization_url: "https://auth.example.com/authorize".into(),
        redirect_port: None,
        client_auth: Default::default(),
        extra_params: extra.clone(),
    };
    let exported = export_collection(
        &collection,
        &[],
        &[request],
        &[],
        &PostmanExportContext::default(),
    )
    .unwrap();
    let envelope: Value = serde_json::from_str(&exported.json).unwrap();
    let fields = envelope["item"][0]["request"]["auth"]["oauth2"]
        .as_array()
        .unwrap();
    let list = |name: &str| {
        fields
            .iter()
            .find(|field| field["key"] == name)
            .map(|field| field["value"].clone())
    };
    assert_eq!(
        list("tokenRequestParams").unwrap()[0],
        serde_json::json!({
            "key": "audience",
            "value": "https://api.example.com",
            "enabled": true,
            "send_as": "request_body",
        })
    );
    assert_eq!(
        list("authRequestParams").unwrap()[1]["send_as"],
        "request_url"
    );

    let imported = import_collection(&exported.json, "ws_1").unwrap();
    let AuthConfig::OAuth2 { extra_params, .. } = &imported.requests[0].auth else {
        panic!("not OAuth 2.0: {:?}", imported.requests[0].auth);
    };
    assert_eq!(extra_params, &extra);

    let header_row = r#"{
      "info": {"name": "H", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
      "item": [{"name": "R", "request": {"method": "GET", "url": "https://x.test", "auth": {"type": "oauth2", "oauth2": [
        {"key": "grant_type", "value": "client_credentials"},
        {"key": "accessTokenUrl", "value": "https://auth.example.com/token"},
        {"key": "clientId", "value": "cid"},
        {"key": "tokenRequestParams", "value": [
          {"key": "x-tenant", "value": "t1", "enabled": true, "send_as": "request_header"},
          {"key": "audience", "value": "a1", "enabled": true, "send_as": "request_body"}
        ]}
      ]}}}]
    }"#;
    let imported = import_collection(header_row, "ws_1").unwrap();
    let AuthConfig::OAuth2 { extra_params, .. } = &imported.requests[0].auth else {
        panic!("not OAuth 2.0: {:?}", imported.requests[0].auth);
    };
    assert_eq!(extra_params, &vec![kv("audience", "a1", true)]);
    assert!(
        imported
            .warnings
            .iter()
            .any(|warning| warning.contains("sent as headers")),
        "{:?}",
        imported.warnings
    );
}
