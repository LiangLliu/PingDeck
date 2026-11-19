//! Response templating and data-helper snapshot tests.

use std::collections::HashMap;

use serde_json::Value;

use crate::workspace::items::{MockCallback, MockDataBucket};
use crate::workspace::models::{HttpMethod, KeyValue};

use super::super::reply::{proxy_uses_data, response_uses_data, template_uses_data};
use super::super::server::MockServerManager;
use super::super::template::{TemplateContext, render};
use super::support::*;

#[test]
fn templating_and_data_helper_snapshots_work() {
    let headers = vec![("x-api-key".to_owned(), "secret".to_owned())];
    let cookies = HashMap::from([("sid".to_owned(), "abc".to_owned())]);
    let params = HashMap::from([("id".to_owned(), "7".to_owned())]);
    let body = r#"{"user":{"name":"Ada"},"items":[]}"#;
    let body_json: Value = serde_json::from_str(body).unwrap();
    let buckets = HashMap::from([(
        "bucket_users".to_owned(),
        serde_json::json!([{"id": 1, "name": "Ada"}]),
    )]);
    let bucket_defs = [MockDataBucket {
        id: "bucket_users".into(),
        name: "Users".into(),
        documentation: String::new(),
        value_json: "[]".into(),
    }];
    let context = TemplateContext {
        method: "POST",
        path: "/users/7",
        query: "page=2",
        headers: &headers,
        cookies: &cookies,
        params,
        body,
        body_json: Some(&body_json),
        peer_ip: "127.0.0.1",
        base_url: "http://127.0.0.1:3000/api",
        buckets: &buckets,
        bucket_defs: &bucket_defs,
    };
    for (tpl, expected) in [
        ("{{urlParam 'id'}}", "7"),
        ("{{params.id}}", "7"),
        ("{{queryParam 'page'}}", "2"),
        ("{{query.page}}", "2"),
        ("{{header 'x-api-key'}}", "secret"),
        ("{{headers.x-api-key}}", "secret"),
        ("{{body 'user.name'}}", "Ada"),
        ("{{body.user.name}}", "Ada"),
        ("{{cookie 'sid'}}", "abc"),
        ("{{method}} {{request.method}}", "POST POST"),
        ("{{path}}", "/users/7"),
        ("{{baseUrl}}", "http://127.0.0.1:3000/api"),
        ("{{bodyRaw}}", body),
        ("{{queryParam 'missing' 'fallback'}}", "fallback"),
        ("{{cookie 'missing' 'fallback'}}", "fallback"),
        ("{{unknown 'x'}}", "{{unknown 'x'}}"),
        ("{{data 'Users'}}", r#"[{"id":1,"name":"Ada"}]"#),
        ("{{data 'bucket_users'}}", r#"[{"id":1,"name":"Ada"}]"#),
    ] {
        assert_eq!(render(tpl, &context), expected, "{tpl}");
    }
    assert!((3..=5).contains(&render("{{int 3 5}}", &context).parse::<i64>().unwrap()));
    assert_eq!(render("{{$guid}}", &context).len(), 36);
    for dynamic in [
        "{{now}}",
        "{{$randomEmail}}",
        "{{$randomUserName}}",
        "{{$timestamp}}",
        "{{$isoTimestamp}}",
    ] {
        assert!(!render(dynamic, &context).is_empty(), "{dynamic}");
    }

    assert!(template_uses_data("{{data 'Pets'}}") && template_uses_data("x {{ data \"Pets\" }}"));
    for no_data in [
        "",
        "{{database 'x'}}",
        "{{data}}",
        "{{body 'data'}} data {{path}}",
    ] {
        assert!(!template_uses_data(no_data), "{no_data}");
    }
    let mut config = server(vec![route(
        "route_pets",
        "GET",
        "/pets",
        vec![response(
            "resp_pets",
            200,
            "{{data 'Pets'}}|{{data 'bucket_pets'}}",
            vec![],
        )],
    )]);
    config.data_buckets = vec![MockDataBucket {
        id: "bucket_pets".into(),
        name: "Pets".into(),
        documentation: String::new(),
        value_json: r#"[{"id":1}]"#.into(),
    }];
    config.callbacks = vec![
        MockCallback {
            id: "uses_data".into(),
            name: String::new(),
            documentation: String::new(),
            method: HttpMethod::Post,
            url: "http://example.test".into(),
            delay_ms: 0,
            headers: Vec::new(),
            body: "{{data 'Pets'}}".into(),
        },
        MockCallback {
            id: "plain".into(),
            name: String::new(),
            documentation: String::new(),
            method: HttpMethod::Post,
            url: "http://example.test".into(),
            delay_ms: 0,
            headers: Vec::new(),
            body: "static".into(),
        },
    ];
    let mut plain = response("plain", 200, "static", vec![]);
    assert!(!response_uses_data(&config, &plain));
    plain.callback_ids = vec!["plain".into()];
    assert!(!response_uses_data(&config, &plain));
    plain.callback_ids = vec!["uses_data".into()];
    assert!(
        response_uses_data(&config, &plain)
            && response_uses_data(&config, &response("b", 200, "{{data 'Pets'}}", vec![]))
    );
    config.proxy_res_headers = vec![KeyValue {
        key: "x-pets".into(),
        value: "{{data 'Pets'}}".into(),
        enabled: true,
        description: None,
    }];
    assert!(proxy_uses_data(&config));

    let manager = MockServerManager::new();
    let bound = manager.start(config).unwrap();
    assert_eq!(get(bound, "/pets").2, r#"[{"id":1}]|[{"id":1}]"#);
}

/// A context with a JSON body and one `Users` bucket.
fn with_context<R>(run: impl FnOnce(&TemplateContext<'_>) -> R) -> R {
    let headers = vec![("x-role".to_owned(), "admin".to_owned())];
    let cookies = HashMap::new();
    let body = r#"{"items":[{"n":"a"},{"n":"b"}],"flag":true}"#;
    let body_json: Value = serde_json::from_str(body).unwrap();
    let buckets = HashMap::from([(
        "bucket_users".to_owned(),
        serde_json::json!([{"id": 1, "name": "Ada"}, {"id": 2, "name": "Lin"}]),
    )]);
    let bucket_defs = [MockDataBucket {
        id: "bucket_users".into(),
        name: "Users".into(),
        documentation: String::new(),
        value_json: "[]".into(),
    }];
    let context = TemplateContext {
        method: "GET",
        path: "/x",
        query: "mode=full",
        headers: &headers,
        cookies: &cookies,
        params: HashMap::new(),
        body,
        body_json: Some(&body_json),
        peer_ip: "127.0.0.1",
        base_url: "http://127.0.0.1:3000",
        buckets: &buckets,
        bucket_defs: &bucket_defs,
    };
    run(&context)
}

#[test]
fn blocks_sub_expressions_and_helpers_render_like_handlebars() {
    with_context(|context| {
        for (template, expected) in [
            ("{{#repeat 3}}x{{/repeat}}", "x,x,x"),
            ("[{{#repeat 2 comma=false}}{{@index}}{{/repeat}}]", "[01]"),
            (
                "{{#each (data 'Users')}}{{name}}{{#unless @last}};{{/unless}}{{/each}}",
                "Ada;Lin",
            ),
            ("{{#each (body 'items')}}{{this.n}}{{/each}}", "ab"),
            ("{{#each (body 'missing')}}x{{else}}none{{/each}}", "none"),
            (
                "{{#if (eq (queryParam 'mode') 'full')}}full{{else}}short{{/if}}",
                "full",
            ),
            ("{{#if (body 'flag')}}on{{/if}}", "on"),
            (
                "{{#unless (header 'x-none')}}no header{{/unless}}",
                "no header",
            ),
            ("{{data 'Users' '0.name'}}", "Ada"),
            ("{{{header 'x-role'}}}", "admin"),
            ("{{uppercase (header 'x-role')}}", "ADMIN"),
            ("{{len (data 'Users')}}", "2"),
            ("{{oneOf (array 'only')}}", "only"),
            ("{{!-- a comment --}}kept", "kept"),
            (
                "{{now 'yyyy'}}",
                &chrono::Utc::now().format("%Y").to_string(),
            ),
            (
                "{{date '2024-03-05' '2024-03-05' \"yyyy-MM-dd'T'HH:mm\"}}",
                "2024-03-05T00:00",
            ),
            // Blocks that do not nest render as written; plain expressions still render.
            ("{{#if x}}{{header 'x-role'}}", "{{#if x}}admin"),
        ] {
            assert_eq!(render(template, context), expected, "{template}");
        }
        assert!(
            render("{{faker 'person.firstName'}}", context)
                .chars()
                .all(char::is_alphabetic)
        );
        let number: i64 = render("{{faker 'number.int' 5 7}}", context)
            .parse()
            .unwrap();
        assert!((5..=7).contains(&number));
        assert_eq!(
            render("{{faker 'music.genre'}}", context),
            "{{faker 'music.genre'}}"
        );
    });
}

#[test]
fn rendering_is_bounded_and_unknown_helpers_are_listed() {
    use super::super::template::{MAX_BLOCK_ITERATIONS, MAX_RENDER_BYTES, unknown_helpers};
    with_context(|context| {
        let repeated = render("{{#repeat 100000 comma=false}}x{{/repeat}}", context);
        assert_eq!(repeated.len(), MAX_BLOCK_ITERATIONS);
        // Nested loops stop at the total iteration budget instead of running 10^9 bodies.
        let nested = render(
            "{{#repeat 1000 comma=false}}{{#repeat 1000 comma=false}}{{#repeat 1000 comma=false}}y{{/repeat}}{{/repeat}}{{/repeat}}",
            context,
        );
        assert!(nested.len() < 10_001, "{}", nested.len());
        let wide = render(
            &format!(
                "{{{{#repeat 1000 comma=false}}}}{}{{{{/repeat}}}}",
                "z".repeat(2048)
            ),
            context,
        );
        assert_eq!(wide.len(), MAX_RENDER_BYTES);
    });
    assert_eq!(
        unknown_helpers(
            "{{faker 'music.genre'}} {{#switch x}}{{/switch}} {{setVar 'a' 1}} {{$nope}} {{#each (data 'Users')}}{{name}}{{/each}} {{body.a}} {{urlParam 'id'}}"
        ),
        ["#switch", "$nope", "faker 'music.genre'", "setVar"]
    );
    assert!(
        unknown_helpers("{{#each (unknownList)}}{{/each}}").contains(&"unknownList".to_owned())
    );
    assert!(template_uses_data("{{#each (data 'Users')}}{{/each}}"));
}

#[test]
fn deep_hash_chains_and_nested_sub_expressions_do_not_recurse_unbounded() {
    use super::super::template::unknown_helpers;
    let chain = format!("{{{{int {}1}}}}", "a=".repeat(100_000));
    let nested = format!(
        "{{{{int {}1{}}}}}",
        "(int ".repeat(100_000),
        ")".repeat(100_000)
    );
    with_context(|context| {
        // A `key=` value never opens another option, so the chain is flat.
        assert!(render(&chain, context).parse::<i64>().is_ok());
        assert!(render(&nested, context).parse::<i64>().is_ok());
    });
    assert!(unknown_helpers(&chain).is_empty());
    // The scan stops at the nesting limit instead of overflowing the stack.
    assert!(unknown_helpers(&nested).is_empty());
}
