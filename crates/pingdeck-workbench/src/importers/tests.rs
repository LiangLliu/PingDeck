use serde_json::Value;

use super::curl::tokenize_curl;
use super::helpers::header_value;
use super::*;
use crate::workspace::models::{ApiKeyLocation, AuthConfig, HttpMethod, Request, RequestBody};

fn request_named<'a>(imported: &'a ImportedCollection, name: &str) -> &'a Request {
    imported
        .requests
        .iter()
        .find(|r| r.name == name)
        .expect("request should exist")
}

fn body_json(body: &RequestBody) -> Value {
    match body {
        RequestBody::Json { content } => {
            serde_json::from_str(content).expect("body should be valid JSON")
        }
        other => panic!("expected JSON body, got {other:?}"),
    }
}

#[test]
fn curl_imports_lexer_auth_bodies_flags_and_deterministic_ids() {
    assert_eq!(
        tokenize_curl("curl\u{a0}-d 'a^\nb' ^\r\nhttps://x.test #literal"),
        ["-d", "a^\nb", "https://x.test", "#literal"]
    );
    assert_eq!(tokenize_curl("curl -d 'unfinished"), ["-d", "unfinished"]);
    assert_eq!(tokenize_curl("curl -d a^\nb"), ["-d", "ab"]);

    let post_cmd = "curl -X POST 'https://api.example.com/v1/users?page=2' \\\n      -H 'Content-Type: application/json' \\\n      -H 'X-Trace: first' \\\n      -H 'X-Trace: second' \\\n      -H 'Authorization: Bearer tok_123' \\\n      -d '{\"name\":\"Ada\"}'";
    let imported = import_curl(post_cmd, "ws_1").unwrap();
    let second = import_curl(post_cmd, "ws_1").unwrap();
    assert_eq!(
        (&imported.collection.id, &imported.requests[0].id),
        (&second.collection.id, &second.requests[0].id)
    );
    assert_eq!(imported.collection.name, "api.example.com/v1/users");
    let req = &imported.requests[0];
    assert_eq!(
        (
            req.method,
            req.url.as_str(),
            req.query_params[0].value.as_str()
        ),
        (HttpMethod::Post, "https://api.example.com/v1/users", "2")
    );
    assert_eq!(
        req.auth,
        AuthConfig::Bearer {
            token_ref: "tok_123".into()
        }
    );
    assert!(
        req.headers
            .iter()
            .all(|h| !h.key.eq_ignore_ascii_case("authorization"))
    );
    assert_eq!(
        req.body,
        RequestBody::Json {
            content: "{\"name\":\"Ada\"}".into()
        }
    );

    let basic = import_curl(
        "curl -H 'Authorization: Basic YWxpY2U6czNjcmV0' https://api.example.com/me",
        "ws_1",
    )
    .unwrap();
    assert_eq!(
        basic.requests[0].auth,
        AuthConfig::Basic {
            username: "alice".into(),
            password_ref: "s3cret".into()
        }
    );
    assert!(basic.requests[0].headers.is_empty());

    let get_q = import_curl(
        "curl --compressed --bogus -d 'term=ada&lang=en' -G https://api.example.com/search",
        "ws_1",
    )
    .unwrap();
    assert_eq!(
        (
            get_q.requests[0].method,
            &get_q.requests[0].body,
            get_q.requests[0].query_params.len()
        ),
        (HttpMethod::Get, &RequestBody::None, 2)
    );
    assert!(get_q.warnings.iter().any(|w| w.contains("--bogus")));

    let raw = import_curl(
        "curl -XPOST -H\"X-Test: 1\" -d\"a=1&b=2\" ^\n https://api.example.com/echo",
        "ws_1",
    )
    .unwrap();
    assert_eq!(
        raw.requests[0].body,
        RequestBody::RawText {
            language: None,
            content: "a=1&b=2".into()
        }
    );
    assert_eq!(header_value(&raw.requests[0].headers, "X-Test"), Some("1"));

    let json_flag = import_curl(
        "curl --json '{\"ok\":true}' https://api.example.com/echo",
        "ws_1",
    )
    .unwrap();
    assert_eq!(
        json_flag.requests[0].body,
        RequestBody::Json {
            content: "{\"ok\":true}".into()
        }
    );
    assert_eq!(
        header_value(&json_flag.requests[0].headers, "content-type"),
        Some("application/json")
    );

    assert_eq!(
        import_curl("curl -X GET -H 'Accept: */*'", "ws_1").unwrap_err(),
        ImportError::MissingUrl
    );
}

/// Chromium's `escapeStringPosix` ("Copy as cURL (bash)").
fn chrome_bash(value: &str) -> String {
    let special = |c: char| c == '!' || c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c);
    if !value.chars().any(|c| c == '\'' || special(c)) {
        return format!("'{value}'");
    }
    let mut out = String::from("$'");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if special(c) => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Chromium's `escapeStringWin` ("Copy as cURL (cmd)").
fn chrome_cmd(value: &str) -> String {
    let safe =
        |c: char| c.is_ascii_alphanumeric() || c.is_whitespace() || "_-:=+~'/.,?;()*`&".contains(c);
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    let mut out = String::from("^\"");
    let mut chars = escaped.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            out.push_str("^\n\n");
            continue;
        }
        if !safe(c) {
            out.push('^');
        }
        out.push(c);
        if c == '%'
            && chars
                .peek()
                .is_some_and(|n| n.is_ascii_alphanumeric() || *n == '_')
        {
            out.push('^');
        }
    }
    out.push_str("^\"");
    out
}

#[test]
fn curl_reads_chrome_bash_and_cmd_copies_byte_for_byte() {
    let url = "https://api.example.com/v1/users?page=2&q=50%off";
    let body = "{\"name\":\"Ada \\\"A\\\" it's!\",\"path\":\"C:\\\\tmp\",\"note\":\"line1\nline2\u{1}\",\"city\":\"Zürich\"}";
    let bash: fn(&str) -> String = chrome_bash;
    for (copy, next) in [(bash, " \\\n  "), (chrome_cmd, " ^\n  ")] {
        let command = format!(
            "curl {}{next}-H {}{next}--data-raw {}{next}--compressed",
            copy(url),
            copy("content-type: application/json"),
            copy(body)
        );
        let req = import_curl(&command, "ws_1").unwrap().requests.remove(0);
        assert_eq!(
            req.body,
            RequestBody::Json {
                content: body.into()
            },
            "{command}"
        );
        assert_eq!(req.query_params[1].value, "50%off", "{command}");
    }
    assert_eq!(
        tokenize_curl("curl --data-raw ^\"^{^\\^\"a^\\^\":1^}^\" ^\n  --compressed"),
        ["--data-raw", "{\"a\":1}", "--compressed"]
    );
}

#[test]
fn curl_decodes_the_ansi_c_escape_table() {
    assert_eq!(
        tokenize_curl(r#"curl -d $'\x41\101\u00e9\U0001F600\e\a\t\v\f\b\cA\?\"\'\\\z' -H x"#),
        [
            "-d",
            "AAé😀\u{1b}\u{7}\t\u{b}\u{c}\u{8}\u{1}?\"'\\\\z",
            "-H",
            "x"
        ]
    );
    assert_eq!(tokenize_curl(r"curl -d $'\xe4\xb8\xad'x"), ["-d", "中x"]);
}

#[test]
fn curl_data_flags_follow_curl_defaults() {
    let form = import_curl("curl -d 'a=1&b=2' https://x.example.com", "ws_1").unwrap();
    let req = &form.requests[0];
    assert_eq!(
        header_value(&req.headers, "content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(
        req.body,
        RequestBody::RawText {
            language: None,
            content: "a=1&b=2".into()
        }
    );

    let encoded = import_curl(
        "curl --data-urlencode 'q=a b&c/é' --data-urlencode '=x*y' --data-urlencode plain -d raw=1 https://x.example.com",
        "ws_1",
    )
    .unwrap();
    assert_eq!(
        encoded.requests[0].body,
        RequestBody::RawText {
            language: None,
            content: "q=a%20b%26c%2F%C3%A9&x%2Ay&plain&raw=1".into()
        }
    );

    let digest = import_curl(
        "curl --digest -u 'alice:s3cret' --form-string 'k=@not-a-file;type=x' https://x.example.com",
        "ws_1",
    )
    .unwrap();
    assert_eq!(
        digest.requests[0].auth,
        AuthConfig::Digest {
            username: "alice".into(),
            password_ref: Some("s3cret".into())
        }
    );
    let RequestBody::Multipart { parts } = &digest.requests[0].body else {
        panic!("expected multipart")
    };
    assert_eq!(
        (parts[0].key.as_str(), parts[0].value.as_str()),
        ("k", "@not-a-file;type=x")
    );
    assert!(digest.warnings.is_empty(), "{:?}", digest.warnings);
}

#[test]
fn curl_never_keeps_local_file_references() {
    let form = import_curl(
        "curl -u 'alice:s3cret' -F 'file=@/etc/passwd' -F 'other=@/etc/hosts;type=text/plain' -F 'in=</etc/shadow' -F 'name=Ada' https://x.example.com/up",
        "ws_1",
    )
    .unwrap();
    assert_eq!(
        form.requests[0].auth,
        AuthConfig::Basic {
            username: "alice".into(),
            password_ref: "s3cret".into()
        }
    );
    let RequestBody::Multipart { parts } = &form.requests[0].body else {
        panic!("expected multipart")
    };
    assert_eq!(parts.len(), 4);
    assert!(
        parts[..3]
            .iter()
            .all(|p| p.file_path.is_none() && p.value.is_empty())
    );
    assert_eq!(
        (parts[3].key.as_str(), parts[3].value.as_str()),
        ("name", "Ada")
    );
    assert_eq!(
        form.warnings
            .join("\n")
            .matches("local file path was not imported")
            .count(),
        1
    );

    for flags in [
        "--data-binary @/etc/passwd",
        "-T /etc/passwd",
        "--json @/etc/passwd",
    ] {
        let imported =
            import_curl(&format!("curl {flags} https://x.example.com/up"), "ws_1").unwrap();
        assert_eq!(imported.requests[0].body, RequestBody::None, "{flags}");
        assert!(
            imported.warnings.iter().any(|w| w.contains("not imported")),
            "{flags}"
        );
    }
}

#[test]
fn har_imports_entries_into_host_folders_and_names_single_host() {
    let har = r#"{
        "log": {
            "entries": [
                {
                    "request": {
                        "method": "GET",
                        "url": "https://api.example.com/users?page=1",
                        "queryString": [{"name": "page", "value": "1"}],
                        "headers": [{"name": "Accept", "value": "application/json"}, {"name": "Authorization", "value": "Bearer har_tok"}]
                    }
                },
                {
                    "request": {
                        "method": "POST",
                        "url": "https://api.example.com/users",
                        "headers": [{"name": ":authority", "value": "api.example.com"}, {"name": "Host", "value": "api.example.com"}, {"name": "Content-Type", "value": "application/json"}, {"name": "content-length", "value": "14"}],
                        "postData": {"mimeType": "application/json", "text": "{\"name\":\"Ada\"}"}
                    }
                },
                {"request": {"method": "GET", "headers": []}},
                {"request": {"method": "GET", "url": "https://files.example.com/blob", "headers": []}}
            ]
        }
    }"#;
    let imported = import_har(har, "ws_1").unwrap();
    assert_eq!(
        (
            imported.collection.name.as_str(),
            imported.requests.len(),
            imported.warnings.len()
        ),
        ("HAR import", 3, 1)
    );
    assert_eq!(
        imported
            .folders
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        vec!["api.example.com", "files.example.com"]
    );
    let list = request_named(&imported, "GET /users");
    assert_eq!(
        (list.url.as_str(), &list.auth, list.folder_id.as_ref()),
        (
            "https://api.example.com/users",
            &AuthConfig::Bearer {
                token_ref: "har_tok".into()
            },
            Some(&imported.folders[0].id)
        )
    );
    let create = request_named(&imported, "POST /users");
    assert_eq!(body_json(&create.body)["name"], Value::String("Ada".into()));
    // Pseudo-headers, Host and Content-Length are the client's to compute.
    assert_eq!(
        create
            .headers
            .iter()
            .map(|h| h.key.as_str())
            .collect::<Vec<_>>(),
        vec!["Content-Type"]
    );

    let single = import_har(r#"{"log":{"entries":[{"request":{"method":"GET","url":"https://only.example.com/ping","headers":[]}}]}}"#, "ws_1").unwrap();
    assert_eq!(
        (
            single.collection.name.as_str(),
            single.requests[0].name.as_str()
        ),
        ("only.example.com", "GET /ping")
    );
}

#[test]
fn openapi_imports_v3_v2_yaml_refs_and_enforces_schema_budget() {
    let v3 = r##"{
        "openapi": "3.0.3",
        "info": {"title": "Swagger Petstore", "description": "Pet APIs"},
        "servers": [{"url": "https://petstore.example.com/v1"}],
        "security": [{"bearerAuth": []}],
        "paths": {
            "/pets": {
                "parameters": [
                    {"$ref": "#/components/parameters/Page"},
                    {"name": "X-Trace", "in": "header", "schema": {"type": "string", "default": "root"}}
                ],
                "get": {
                    "tags": ["pets"],
                    "summary": "List pets",
                    "parameters": [{"name": "page", "in": "query", "schema": {"type": "integer", "default": 7}}],
                    "responses": {}
                },
                "post": {
                    "tags": ["pets"],
                    "summary": "Create pet",
                    "requestBody": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/NewPet"}}}},
                    "responses": {}
                }
            },
            "/health": {"get": {"summary": "Health", "security": [{"oauth": ["read"]}]}}
        },
        "components": {
            "parameters": {"Page": {"name": "page", "in": "query", "schema": {"type": "integer", "default": 1}}},
            "securitySchemes": {
                "bearerAuth": {"type": "http", "scheme": "bearer"},
                "oauth": {"type": "oauth2", "flows": {}}
            },
            "schemas": {
                "NewPet": {"type": "object", "properties": {"name": {"type": "string", "example": "Rex"}, "tag": {"type": "string"}}}
            }
        }
    }"##;
    let imported = import_openapi(v3, "ws_1").unwrap();
    assert_eq!(
        (
            imported.collection.name.as_str(),
            imported.collection.variables[0].value.as_deref()
        ),
        ("Swagger Petstore", Some("https://petstore.example.com/v1"))
    );
    assert_eq!(
        imported.collection.auth,
        AuthConfig::Bearer {
            token_ref: String::new()
        }
    );
    assert!(imported.warnings.iter().any(|w| w.contains("OAuth2")));
    let list = request_named(&imported, "List pets");
    assert_eq!(
        (
            list.url.as_str(),
            list.query_params[0].value.as_str(),
            header_value(&list.headers, "X-Trace")
        ),
        ("{{baseUrl}}/pets", "7", Some("root"))
    );
    let create_body = body_json(&request_named(&imported, "Create pet").body);
    assert_eq!(
        (&create_body["name"], &create_body["tag"]),
        (
            &Value::String("Rex".into()),
            &Value::String("string".into())
        )
    );

    let yaml = "openapi: 3.0.0\ninfo:\n  title: YAML API\nservers:\n  - url: https://yaml.example.com\npaths:\n  /health:\n    get:\n      summary: Health\n";
    let yaml_imported = import_openapi(yaml, "ws_yaml").unwrap();
    assert_eq!(
        (
            yaml_imported.collection.name.as_str(),
            yaml_imported.requests[0].url.as_str()
        ),
        ("YAML API", "{{baseUrl}}/health")
    );

    let v2 = r##"{
        "swagger": "2.0",
        "info": {"title": "Pet Store v2"},
        "host": "petstore.example.com",
        "basePath": "/v2",
        "schemes": ["https"],
        "securityDefinitions": {"api_key": {"type": "apiKey", "name": "api_key", "in": "header"}},
        "security": [{"api_key": []}],
        "paths": {
            "/pets/{petId}": {
                "put": {
                    "operationId": "updatePet",
                    "parameters": [{"name": "body", "in": "body", "schema": {"$ref": "#/definitions/Pet"}}],
                    "responses": {}
                }
            },
            "/upload": {
                "post": {"parameters": [{"name": "file", "in": "formData", "type": "file"}], "responses": {}}
            }
        },
        "definitions": {"Pet": {"type": "object", "properties": {"id": {"type": "integer"}}}}
    }"##;
    let v2_imported = import_openapi(v2, "ws_1").unwrap();
    assert_eq!(
        v2_imported.collection.auth,
        AuthConfig::ApiKey {
            key: "api_key".into(),
            value_ref: String::new(),
            location: ApiKeyLocation::Header
        }
    );
    assert_eq!(
        body_json(&request_named(&v2_imported, "updatePet").body)["id"],
        Value::from(0)
    );
    let RequestBody::Multipart { parts } = &request_named(&v2_imported, "POST /upload").body else {
        panic!("multipart")
    };
    assert_eq!(
        (parts[0].key.as_str(), parts[0].file_path.as_ref()),
        ("file", None)
    );

    assert!(matches!(
        import_openapi("{not json", "ws_1"),
        Err(ImportError::InvalidJson { .. })
    ));
    assert!(matches!(
        import_har("{not json", "ws_1"),
        Err(ImportError::InvalidJson { .. })
    ));
    assert!(matches!(
        import_openapi(r#"{"openapi": "4.0.0"}"#, "ws_1"),
        Err(ImportError::UnsupportedDocument { .. })
    ));

    let properties: String = (0..20)
        .map(|n| format!(r##""p{n}": {{"$ref": "#/components/schemas/A"}}"##))
        .collect::<Vec<_>>()
        .join(",");
    let loop_doc = format!(
        r##"{{"openapi":"3.0.0","info":{{"title":"Loop","version":"1"}},"paths":{{"/a":{{"post":{{"summary":"A","requestBody":{{"content":{{"application/json":{{"schema":{{"$ref":"#/components/schemas/A"}}}}}}}}}}}}}},"components":{{"schemas":{{"A":{{"type":"object","properties":{{{properties}}}}}}}}}}}"##
    );
    let loop_imported = import_openapi(&loop_doc, "ws_1").unwrap();
    let RequestBody::Json { content } = &request_named(&loop_imported, "A").body else {
        panic!("JSON body")
    };
    assert!(
        content.len() < 2 * 1024 * 1024
            && loop_imported
                .warnings
                .iter()
                .any(|w| w.contains("too large"))
    );
}

#[test]
fn openapi_mock_routes_take_examples_and_schemas_and_reject_other_documents() {
    let doc = r##"{
      "openapi": "3.0.0",
      "info": {"title": "Billing", "version": "1"},
      "servers": [{"url": "https://api.example.com/v1/"}],
      "paths": {
        "/invoices/{id}": {
          "get": {
            "summary": "Get invoice",
            "responses": {
              "404": {"description": "Missing", "content": {"application/json": {"example": {"error": "missing"}}}},
              "200": {"description": "Found", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Invoice"}}}},
              "default": {"description": "Error"}
            }
          },
          "delete": {"responses": {"2XX": {"description": "Gone"}}}
        },
        "/health": {"get": {"responses": {"200": {"content": {"text/plain": {"example": "ok"}}}}}}
      },
      "components": {"schemas": {"Invoice": {"type": "object", "properties": {
        "id": {"type": "string", "example": "inv_1"}, "total": {"type": "integer"}}}}}
    }"##;
    let generated = openapi_mock_routes(doc).unwrap();
    assert_eq!(generated.name, "Billing");
    assert_eq!(generated.endpoint_prefix, "/v1");
    assert_eq!(generated.routes.len(), 3);

    // Paths come in document-key order (serde_json sorts keys), methods in GET..OPTIONS order.
    let get = &generated.routes[1];
    assert_eq!(
        (get.method.as_str(), get.path.as_str()),
        ("GET", "/invoices/{id}")
    );
    let statuses: Vec<u16> = get.responses.iter().map(|r| r.status_code).collect();
    assert_eq!(
        statuses,
        [200, 404],
        "the 2xx comes first and `default` is skipped"
    );
    assert!(get.responses[0].is_default && !get.responses[1].is_default);
    let body: Value = serde_json::from_str(&get.responses[0].body).unwrap();
    assert_eq!(body, serde_json::json!({"id": "inv_1", "total": 0}));
    assert_eq!(
        get.responses[1].body.replace([' ', '\n'], ""),
        r#"{"error":"missing"}"#
    );

    assert_eq!(generated.routes[2].responses[0].status_code, 200);
    let health = &generated.routes[0].responses[0];
    assert_eq!(
        (health.body.as_str(), health.headers[0].value.as_str()),
        ("ok", "text/plain")
    );

    assert!(matches!(
        openapi_mock_routes(r#"{"openapi": "4.0.0"}"#),
        Err(ImportError::UnsupportedDocument { .. })
    ));
}
