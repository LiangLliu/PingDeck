use super::json_ast::{Json, decode_string, parse_document};
use super::json_path::{parse_json_path, select_json_path};
use super::{
    FormattedBodyDto, JsonPathFilterOutcomeDto, MAX_JSON_DEPTH, ResponseBodyFormat, TOO_LARGE_MODE,
    filter_response_jsonpath as filter_capped, format_response_body,
};

fn select<'a>(root: &'a Json<'a>, expression: &str) -> Result<Vec<&'a Json<'a>>, String> {
    select_json_path(root, &parse_json_path(expression)?)
}

/// The default response body cap, as the settings ship it.
const TEST_CAP: usize = 10 * 1024 * 1024;

fn format(body: &str, format: ResponseBodyFormat) -> FormattedBodyDto {
    format_response_body(body, format, TEST_CAP)
}

fn filter_response_jsonpath(body: &str, expression: &str) -> JsonPathFilterOutcomeDto {
    filter_capped(body, expression, TEST_CAP)
}
fn compact(value: &Json<'_>) -> String {
    match value {
        Json::Null => "null".to_owned(),
        Json::Bool(flag) => flag.to_string(),
        Json::Number(lexeme) | Json::String(lexeme) => (*lexeme).to_owned(),
        Json::Array(items) => {
            let parts: Vec<String> = items.iter().map(compact).collect();
            format!("[{}]", parts.join(","))
        }
        Json::Object(members) => {
            let parts: Vec<String> = members
                .iter()
                .map(|(key, child)| format!("{key}:{}", compact(child)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

fn to_value(value: &Json<'_>) -> serde_json::Value {
    serde_json::from_str(&compact(value)).expect("compact output is valid JSON")
}

fn field<'a, 'b>(case: &'b Json<'a>, name: &str) -> &'b Json<'a> {
    case.get(name).expect("fixture field is present")
}

fn text(value: &Json<'_>) -> String {
    match value {
        Json::String(lexeme) => decode_string(lexeme).into_owned(),
        other => panic!("expected a string, got {other:?}"),
    }
}

fn fixture_cases(source: &str) -> Json<'_> {
    let parsed = parse_document(source).expect("fixture is valid JSON");
    assert!(matches!(parsed, Json::Array(_)), "fixture is an array");
    parsed
}

#[test]
fn nesting_depth_is_limited_without_overflow() {
    let nested = |depth: usize| format!("{}{}", "[".repeat(depth), "]".repeat(depth));
    assert!(parse_document(&nested(MAX_JSON_DEPTH)).is_some());
    assert!(parse_document(&nested(MAX_JSON_DEPTH + 1)).is_none());
    assert!(parse_document(&"[".repeat(1_000_000)).is_none());
    let objects = format!(
        "{}1{}",
        r#"{"a":"#.repeat(MAX_JSON_DEPTH + 1),
        "}".repeat(MAX_JSON_DEPTH + 1)
    );
    assert!(parse_document(&objects).is_none());
}

#[test]
fn oversized_bodies_are_returned_exact() {
    let body = "{\"a\":1}".repeat(TEST_CAP / 7 + 1);
    assert!(body.len() > TEST_CAP);
    for format_kind in [
        ResponseBodyFormat::Json,
        ResponseBodyFormat::Xml,
        ResponseBodyFormat::Html,
        ResponseBodyFormat::Yaml,
    ] {
        let big = format(&body, format_kind);
        assert_eq!(big.mode, TOO_LARGE_MODE);
        assert!(!big.is_valid_json);
        assert!(big.too_large);
        assert_eq!(big.text, body);
    }
    let normal = format("{\"a\":1}", ResponseBodyFormat::Json);
    assert!(!normal.too_large);
}

#[test]
fn format_names_are_lowercase_and_closed() {
    let yaml: ResponseBodyFormat = serde_json::from_str("\"yaml\"").expect("known format");
    assert_eq!(yaml, ResponseBodyFormat::Yaml);
    let text: ResponseBodyFormat = serde_json::from_str("\"text\"").expect("known format");
    assert_eq!(text, ResponseBodyFormat::Text);
    for hex_and_base64 in ["hex", "base64"] {
        assert!(
            serde_json::from_str::<ResponseBodyFormat>(&format!("\"{hex_and_base64}\"")).is_ok()
        );
    }
    for removed in ["raw", "javascript", "markdown"] {
        assert!(serde_json::from_str::<ResponseBodyFormat>(&format!("\"{removed}\"")).is_err());
    }
}

#[test]
fn detect_response_format_handles_headers_sniffing_and_fallbacks() {
    use super::detect_response_format;
    assert_eq!(
        detect_response_format(Some("application/json; charset=utf-8"), r#"{"ok":true}"#),
        ResponseBodyFormat::Json
    );
    assert_eq!(
        detect_response_format(Some("text/plain"), r#"{"id":42,"name":"alice"}"#),
        ResponseBodyFormat::Json
    );
    assert_eq!(
        detect_response_format(None, "[1, 2, 3]"),
        ResponseBodyFormat::Json
    );
    assert_eq!(
        detect_response_format(Some("text/plain"), "hello world"),
        ResponseBodyFormat::Text
    );
    assert_eq!(
        detect_response_format(None, "{not json at all"),
        ResponseBodyFormat::Text
    );
    assert_eq!(
        detect_response_format(
            Some("application/json"),
            "<!DOCTYPE html><html><body>502 Bad Gateway</body></html>"
        ),
        ResponseBodyFormat::Html
    );
    assert_eq!(
        detect_response_format(None, "<?xml version=\"1.0\"?><root/>"),
        ResponseBodyFormat::Xml
    );
    assert_eq!(
        detect_response_format(Some("application/yaml"), "a: 1\nb: 2\n"),
        ResponseBodyFormat::Yaml
    );
    assert_eq!(
        format("plain output", ResponseBodyFormat::Text).text,
        "plain output"
    );
}

#[test]
fn json_path_filter_reports_matches_and_invalid_bodies() {
    assert_eq!(
        filter_response_jsonpath(r#"{"a":{"id":1}}"#, "$.a.id"),
        JsonPathFilterOutcomeDto::Filtered {
            text: "1".to_owned(),
            mode: "1 JSONPath match".to_owned(),
        }
    );
    assert_eq!(
        filter_response_jsonpath(r#"{"a":[1,2]}"#, "$.a[*]"),
        JsonPathFilterOutcomeDto::Filtered {
            text: "[\n  1,\n  2\n]".to_owned(),
            mode: "2 JSONPath matches".to_owned(),
        }
    );
    assert_eq!(
        filter_response_jsonpath(r#"{"a":1}"#, "$.missing"),
        JsonPathFilterOutcomeDto::Filtered {
            text: "[]".to_owned(),
            mode: "JSONPath · no matches".to_owned(),
        }
    );
    assert_eq!(
        filter_response_jsonpath("nope", "$.a"),
        JsonPathFilterOutcomeDto::Error {
            message: "Response body is not valid JSON.".to_owned(),
        }
    );
}

fn error_message(outcome: JsonPathFilterOutcomeDto) -> String {
    match outcome {
        JsonPathFilterOutcomeDto::Error { message } => message,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn json_path_filter_rejects_oversized_bodies_before_parsing() {
    // Whitespace is not JSON: only the size check can produce this message.
    let body = " ".repeat(TEST_CAP + 1);
    assert_eq!(
        error_message(filter_response_jsonpath(&body, "$.a")),
        "Response body is too large to filter (limit 10485760 bytes)."
    );
}

#[test]
fn json_path_expression_is_validated_before_the_body() {
    assert!(error_message(filter_response_jsonpath("nope", "$.a[")).contains("Unclosed"));
    assert_eq!(
        error_message(filter_response_jsonpath("nope", "$.a")),
        "Response body is not valid JSON."
    );
}

#[test]
fn json_path_node_ceiling_stops_union_duplicate_blowup() {
    let union = format!("[{}]", ["0"; 50].join(","));
    assert_eq!(
        error_message(filter_response_jsonpath(
            "[[[1]]]",
            &format!("${union}{union}{union}")
        )),
        "JSONPath matched too many nodes (limit 100000)."
    );
    match filter_response_jsonpath("[[1]]", &format!("${union}{union}")) {
        JsonPathFilterOutcomeDto::Filtered { mode, .. } => {
            assert_eq!(mode, "2500 JSONPath matches");
        }
        other => panic!("two unions stay under the ceiling, got {other:?}"),
    }
}

/// The raw body of a fixture case: its `body` string, else its `document` written compactly.
fn fixture_body(case: &Json<'_>) -> String {
    match case.get("body") {
        Some(body) => text(body),
        None => compact(field(case, "document")),
    }
}

/// Cases with a `document` are also checked node by node against `expected`.
#[test]
fn jsonpath_shared_fixture() {
    let source = include_str!("../../../../fixtures/jsonpath-cases.json");
    let cases = fixture_cases(source);
    let Json::Array(cases) = &cases else {
        panic!("fixture is an array");
    };
    assert!(cases.len() >= 28, "fixture needs at least 28 cases");
    for case in cases {
        let name = text(field(case, "name"));
        let expression = text(field(case, "expression"));
        let is_error = matches!(field(case, "error"), Json::Bool(true));
        // Exact output of the native filter: `text` for a match, `message` for an error.
        match (
            filter_response_jsonpath(&fixture_body(case), &expression),
            is_error,
        ) {
            (JsonPathFilterOutcomeDto::Filtered { text: actual, .. }, false) => {
                assert_eq!(actual, text(field(case, "text")), "{name}");
            }
            (JsonPathFilterOutcomeDto::Error { message }, true) => {
                assert_eq!(message, text(field(case, "message")), "{name}");
            }
            (outcome, _) => panic!("{name}: unexpected outcome {outcome:?}"),
        }
        let Some(document) = case.get("document") else {
            continue;
        };
        let selected = select(document, &expression);
        if is_error {
            assert!(selected.is_err(), "{name}: expected an error");
            continue;
        }
        let selected = selected.unwrap_or_else(|message| panic!("{name}: {message}"));
        let Json::Array(expected) = field(case, "expected") else {
            panic!("{name}: expected must be an array");
        };
        let got: Vec<_> = selected.into_iter().map(to_value).collect();
        let want: Vec<_> = expected.iter().map(to_value).collect();
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn json_format_shared_fixture() {
    let source = include_str!("../../../../fixtures/json-format-cases.json");
    let cases = fixture_cases(source);
    let Json::Array(cases) = &cases else {
        panic!("fixture is an array");
    };
    assert!(cases.len() >= 15, "fixture needs at least 15 cases");
    for case in cases {
        let name = text(field(case, "name"));
        let input = text(field(case, "input"));
        for (kind, key) in [
            (ResponseBodyFormat::Json, "json"),
            (ResponseBodyFormat::Yaml, "yaml"),
        ] {
            let actual = format(&input, kind);
            match field(case, key) {
                Json::Null => {
                    assert!(!actual.is_valid_json, "{name} ({key})");
                    assert_eq!(actual.text, input, "{name} ({key})");
                }
                expected => {
                    assert!(actual.is_valid_json, "{name} ({key})");
                    assert_eq!(actual.text, text(expected), "{name} ({key})");
                }
            }
        }
    }
}

#[test]
fn markup_format_shared_fixture() {
    let source = include_str!("../../../../fixtures/markup-format-cases.json");
    let cases = fixture_cases(source);
    let Json::Array(cases) = &cases else {
        panic!("fixture is an array");
    };
    assert!(cases.len() >= 12, "fixture needs at least 12 cases");
    for case in cases {
        let name = text(field(case, "name"));
        let kind = match text(field(case, "format")).as_str() {
            "xml" => ResponseBodyFormat::Xml,
            "html" => ResponseBodyFormat::Html,
            other => panic!("{name}: unknown format {other}"),
        };
        let actual = format(&text(field(case, "input")), kind);
        assert_eq!(actual.text, text(field(case, "output")), "{name}");
    }
}

/// The native half of the 10 MiB first-screen target (`docs/development.md` §3): formatting a
/// 10 MiB JSON body. Ignored: the nightly job runs it for the printed time; only an optimised
/// build asserts the budget (`cargo test -p spanbay-response --release --locked -- --ignored
/// ten_mib`), a debug build only reports.
#[test]
#[ignore = "perf: prints the native formatting time of a 10 MiB JSON body"]
fn ten_mib_json_formats_within_the_first_screen_budget() {
    const BUDGET_MS: u128 = 1_500;
    let row = r#"{"id":123456,"name":"Billing account","tags":["a","b","c"],"active":true,"score":12.5,"owner":{"id":"u_1","email":"ops@example.com"}}"#;
    let mut body = String::from("[");
    while body.len() < TEST_CAP - row.len() - 2 {
        if body.len() > 1 {
            body.push(',');
        }
        body.push_str(row);
    }
    body.push(']');
    let started = std::time::Instant::now();
    let formatted = format(&body, ResponseBodyFormat::Json);
    let elapsed = started.elapsed().as_millis();
    println!(
        "format_response_body: {} bytes of JSON in {elapsed} ms",
        body.len()
    );
    assert!(
        formatted.is_valid_json && !formatted.too_large,
        "{}",
        formatted.mode
    );
    if !cfg!(debug_assertions) {
        assert!(
            elapsed <= BUDGET_MS,
            "{elapsed} ms is over the {BUDGET_MS} ms budget"
        );
    }
}
