//! Structured response assertions.

use pingdeck_model::assertion::{AiResultView, AssertionResult};

use crate::executor::{ExecutionHeader, ExecutionResult};
use crate::json_schema;
use pingdeck_model::models::{Assertion, AssertionOp, AssertionTarget, json_number};
use serde_json::Value;

/// Evaluates each assertion against an HTTP execution result, in order. AI kinds fail: they
/// need [`evaluate_assertions_with_ai`].
#[must_use]
pub fn evaluate_assertions(
    assertions: &[Assertion],
    result: &ExecutionResult,
) -> Vec<AssertionResult> {
    evaluate_assertions_with_ai(assertions, result, None)
}

/// Evaluates each assertion in order: HTTP kinds on the raw `result`, AI kinds on `ai`. An AI
/// kind without a view fails with a message that says why; it never passes silently.
#[must_use]
pub fn evaluate_assertions_with_ai(
    assertions: &[Assertion],
    result: &ExecutionResult,
    ai: Option<&AiResultView<'_>>,
) -> Vec<AssertionResult> {
    assertions
        .iter()
        .enumerate()
        .map(|(index, assertion)| {
            let checked = check_assertion(assertion, result, ai);

            AssertionResult {
                index,
                label: assertion_label(assertion),
                passed: checked.passed,
                message: checked.message,
                expected: checked.expected,
                actual: checked.actual,
            }
        })
        .collect()
}

/// One assertion's outcome. `expected` and `actual` are the two values the step drawer shows
/// side by side; both stay `None` for a check without a value pair (an existence,
/// containment or schema check says what it looked for in `message`).
#[derive(Debug, Default)]
struct Checked {
    passed: bool,
    message: String,
    expected: Option<String>,
    actual: Option<String>,
}

impl Checked {
    /// `passed` with `message`.
    fn of(passed: bool, message: impl Into<String>) -> Self {
        Self {
            passed,
            message: message.into(),
            ..Self::default()
        }
    }

    /// `passed` with `message` and the two values compared.
    fn compared(
        passed: bool,
        message: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        Self {
            expected: Some(expected.into()),
            actual: Some(actual.into()),
            ..Self::of(passed, message)
        }
    }
}

fn check_assertion(
    assertion: &Assertion,
    result: &ExecutionResult,
    ai: Option<&AiResultView<'_>>,
) -> Checked {
    match assertion {
        Assertion::StatusEquals { value } => {
            let passed = result.status_code == *value;
            let message = if passed {
                format!("Response status is {value}")
            } else {
                format!("Expected status {value}, got {}", result.status_code)
            };

            Checked::compared(
                passed,
                message,
                value.to_string(),
                result.status_code.to_string(),
            )
        }
        Assertion::StatusIn { values } => {
            let passed = values.contains(&result.status_code);
            let expected = status_list(values);
            let message = if passed {
                format!("Response status {} is in [{expected}]", result.status_code)
            } else {
                format!(
                    "Expected status in [{expected}], got {}",
                    result.status_code
                )
            };

            Checked::compared(
                passed,
                message,
                format!("[{expected}]"),
                result.status_code.to_string(),
            )
        }
        Assertion::HeaderExists { key } => match header_value(&result.headers, key) {
            Some(value) => Checked::of(true, format!("Header {key} is {value}")),
            None => Checked::of(false, format!("Header {key} is missing")),
        },
        Assertion::HeaderEquals { key, value } => match header_value(&result.headers, key) {
            Some(actual) if actual == value.trim() => Checked::compared(
                true,
                format!("Header {key} is {actual}"),
                value.trim(),
                actual,
            ),
            Some(actual) => Checked::compared(
                false,
                format!("Expected header {key} to be {value}, got {actual}"),
                value.trim(),
                actual,
            ),
            None => Checked::of(false, format!("Header {key} is missing")),
        },
        Assertion::JsonPathExists { path } => match lookup_body_json(result, path) {
            Ok(Some(_)) => Checked::of(true, format!("Found JSON path {path}")),
            Ok(None) => Checked::of(false, format!("JSON path {path} was not found")),
            Err(message) => Checked::of(false, message),
        },
        Assertion::JsonPathEquals { path, value } => match lookup_body_json(result, path) {
            Ok(Some(actual)) if &actual == value => Checked::compared(
                true,
                format!("{path} is {}", display_json(&actual)),
                display_json(value),
                display_json(&actual),
            ),
            Ok(Some(actual)) => Checked::compared(
                false,
                format!(
                    "Expected {path} to be {}, got {}",
                    display_json(value),
                    display_json(&actual)
                ),
                display_json(value),
                display_json(&actual),
            ),
            Ok(None) => Checked::of(false, format!("JSON path {path} was not found")),
            Err(message) => Checked::of(false, message),
        },
        Assertion::BodyContains { value } => {
            let passed = result.body_text.contains(value.as_str());
            let message = if passed {
                format!("Body contains {value:?}")
            } else {
                format!("Body does not contain {value:?}")
            };

            Checked::of(passed, message)
        }
        Assertion::ResponseTimeLessThanMs { value } => {
            let passed = result.elapsed_ms < u128::from(*value);
            let message = if passed {
                format!("Response completed in {} ms", result.elapsed_ms)
            } else {
                format!("Expected under {value} ms, took {} ms", result.elapsed_ms)
            };

            Checked::compared(
                passed,
                message,
                format!("< {value} ms"),
                format!("{} ms", result.elapsed_ms),
            )
        }
        Assertion::BodySizeLessThanBytes { value } => {
            let size = body_size(result);
            let passed = size < *value;
            let message = if passed {
                format!("Response body is {size} bytes")
            } else {
                format!("Expected under {value} bytes, got {size} bytes")
            };

            Checked::compared(
                passed,
                message,
                format!("< {value} bytes"),
                format!("{size} bytes"),
            )
        }
        Assertion::Check { target, op, value } => check_target(target, *op, value, result),
        Assertion::JsonSchemaValid { schema, path } => {
            let instance = match path {
                Some(path) => match lookup_body_json(result, path) {
                    Ok(Some(found)) => found,
                    Ok(None) => {
                        return Checked::of(false, format!("JSON path {path} was not found"));
                    }
                    Err(message) => return Checked::of(false, message),
                },
                None => match serde_json::from_str::<Value>(&result.body_text) {
                    Ok(body) => body,
                    Err(error) => {
                        return Checked::of(
                            false,
                            format!("Response body is not valid JSON: {error}"),
                        );
                    }
                },
            };
            match json_schema::validate_document(schema, &instance) {
                Ok(()) => Checked::of(true, "The JSON matches the schema"),
                Err(message) => Checked::of(false, message),
            }
        }
        Assertion::AiReplyContains { value } => with_view(ai, |view| {
            if view.reply.contains(value.as_str()) {
                Checked::of(true, format!("Reply contains {value:?}"))
            } else {
                Checked::of(
                    false,
                    format!(
                        "Reply does not contain {value:?}; reply was {}",
                        preview(view.reply)
                    ),
                )
            }
        }),
        Assertion::AiReplyMatches { pattern } => with_view(ai, |view| {
            if let Some(problem) = pingdeck_base::regex::regex_check(pattern) {
                Checked::of(
                    false,
                    format!("Pattern {pattern:?} cannot be used: {problem}"),
                )
            } else if pingdeck_base::regex::is_match(pattern, view.reply, false) {
                Checked::of(true, format!("Reply matches {pattern:?}"))
            } else {
                Checked::of(
                    false,
                    format!(
                        "Reply does not match {pattern:?}; reply was {}",
                        preview(view.reply)
                    ),
                )
            }
        }),
        Assertion::AiJsonSchemaValid { schema } => {
            with_view(ai, |view| {
                match serde_json::from_str::<serde_json::Value>(view.reply.trim()) {
                    Err(error) => Checked::of(
                        false,
                        format!(
                            "Reply is not valid JSON ({error}); reply was {}",
                            preview(view.reply)
                        ),
                    ),
                    Ok(reply) => match json_schema::validate(schema, &reply) {
                        Ok(()) => Checked::of(true, "Reply is valid JSON and matches the schema"),
                        Err(message) => Checked::of(false, message),
                    },
                }
            })
        }
        Assertion::AiTtftLessThanMs { value } => with_view(ai, |view| match view.ttft_ms {
            Some(ttft) if ttft < *value => Checked::compared(
                true,
                format!("First token after {ttft} ms"),
                format!("< {value} ms"),
                format!("{ttft} ms"),
            ),
            Some(ttft) => Checked::compared(
                false,
                format!("Expected first token under {value} ms, took {ttft} ms"),
                format!("< {value} ms"),
                format!("{ttft} ms"),
            ),
            None => Checked::of(
                false,
                "No first token was received, so TTFT is unknown".to_owned(),
            ),
        }),
        Assertion::AiTokensLessThan { value } => with_view(ai, |view| {
            if view.input_tokens.is_none() && view.output_tokens.is_none() {
                return Checked::of(false, "The response reported no token usage".to_owned());
            }
            let total = view.input_tokens.unwrap_or(0) + view.output_tokens.unwrap_or(0);
            if total < *value {
                Checked::compared(
                    true,
                    format!("Used {total} tokens"),
                    format!("< {value}"),
                    total.to_string(),
                )
            } else {
                Checked::compared(
                    false,
                    format!("Expected under {value} tokens, used {total}"),
                    format!("< {value}"),
                    total.to_string(),
                )
            }
        }),
        Assertion::AiNoError {} => with_view(ai, |view| {
            if view.status >= 400 {
                Checked::of(false, format!("Response status is {}", view.status))
            } else if let Some(error) = view.error {
                Checked::of(false, format!("The stream reported an error: {error}"))
            } else if view.finish_reason == Some("error") {
                Checked::of(false, "The finish reason is \"error\"".to_owned())
            } else {
                Checked::of(true, "No error".to_owned())
            }
        }),
    }
}

/// Runs an AI check, or fails when the request produced no AI result (an HTTP request).
fn with_view(
    ai: Option<&AiResultView<'_>>,
    check: impl FnOnce(&AiResultView<'_>) -> Checked,
) -> Checked {
    ai.map_or_else(
        || {
            Checked::of(
                false,
                "This assertion needs an AI API request; there is no AI response to check"
                    .to_owned(),
            )
        },
        check,
    )
}

/// The first 200 characters of `text`, quoted.
fn preview(text: &str) -> String {
    let mut shown: String = text.chars().take(200).collect();
    if shown.len() < text.len() {
        shown.push('…');
    }
    format!("{shown:?}")
}

fn assertion_label(assertion: &Assertion) -> String {
    match assertion {
        Assertion::StatusEquals { value } => format!("Status is {value}"),
        Assertion::StatusIn { values } => format!("Status in [{}]", status_list(values)),
        Assertion::HeaderExists { key } => format!("Header {key} exists"),
        Assertion::HeaderEquals { key, value } => format!("Header {key} equals {value}"),
        Assertion::JsonPathExists { path } => format!("JSON {path} exists"),
        Assertion::JsonPathEquals { path, value } => {
            format!("JSON {path} equals {}", display_json(value))
        }
        Assertion::BodyContains { value } => format!("Body contains {value:?}"),
        Assertion::ResponseTimeLessThanMs { value } => {
            format!("Response time under {value} ms")
        }
        Assertion::BodySizeLessThanBytes { value } => {
            format!("Body size under {value} bytes")
        }
        Assertion::AiReplyContains { value } => format!("Reply contains {value:?}"),
        Assertion::AiReplyMatches { pattern } => format!("Reply matches {pattern:?}"),
        Assertion::AiJsonSchemaValid { .. } => "Reply is valid JSON for the schema".to_owned(),
        Assertion::AiTtftLessThanMs { value } => format!("TTFT under {value} ms"),
        Assertion::AiTokensLessThan { value } => format!("Tokens under {value}"),
        Assertion::AiNoError {} => "No error".to_owned(),
        Assertion::Check { target, op, value } => match op {
            AssertionOp::Exists | AssertionOp::NotExists => {
                format!("{} {}", target_label(target), op_phrase(*op))
            }
            op => format!(
                "{} {} {}",
                target_label(target),
                op_phrase(*op),
                display_json(value)
            ),
        },
        Assertion::JsonSchemaValid {
            path: Some(path), ..
        } => {
            format!("JSON {path} matches the schema")
        }
        Assertion::JsonSchemaValid { path: None, .. } => "Body matches the JSON schema".to_owned(),
    }
}

/// The body size an assertion reads: the full size when the result knows it (a save-to-file
/// send, a declared length the cap cut), else the captured size.
fn body_size(result: &ExecutionResult) -> u64 {
    result
        .body_total_bytes
        .unwrap_or(result.body_size_bytes as u64)
}

fn target_label(target: &AssertionTarget) -> String {
    match target {
        AssertionTarget::Status => "Status".to_owned(),
        AssertionTarget::Header { name } => format!("Header {name}"),
        AssertionTarget::JsonPath { path } => format!("JSON {path}"),
        AssertionTarget::Body => "Body".to_owned(),
        AssertionTarget::ResponseTimeMs => "Response time (ms)".to_owned(),
        AssertionTarget::BodySizeBytes => "Body size (bytes)".to_owned(),
    }
}

const fn op_phrase(op: AssertionOp) -> &'static str {
    match op {
        AssertionOp::Equals => "equals",
        AssertionOp::NotEquals => "does not equal",
        AssertionOp::Exists => "exists",
        AssertionOp::NotExists => "does not exist",
        AssertionOp::Contains => "contains",
        AssertionOp::NotContains => "does not contain",
        AssertionOp::GreaterThan => "is greater than",
        AssertionOp::GreaterOrEqual => "is at least",
        AssertionOp::LessThan => "is less than",
        AssertionOp::LessOrEqual => "is at most",
        AssertionOp::Matches => "matches",
        AssertionOp::NotMatches => "does not match",
        AssertionOp::IsType => "is of type",
    }
}

/// The value `target` reads from `result`: `None` when it is absent (a missing header or JSON
/// path), `Err` when it cannot be read (the body is not JSON, the path is malformed).
fn target_value(
    target: &AssertionTarget,
    result: &ExecutionResult,
) -> Result<Option<Value>, String> {
    Ok(match target {
        AssertionTarget::Status => Some(Value::from(result.status_code)),
        AssertionTarget::Header { name } => header_value(&result.headers, name).map(Value::String),
        AssertionTarget::JsonPath { path } => lookup_body_json(result, path)?,
        AssertionTarget::Body => Some(Value::String(result.body_text.clone())),
        AssertionTarget::ResponseTimeMs => Some(Value::from(
            u64::try_from(result.elapsed_ms).unwrap_or(u64::MAX),
        )),
        AssertionTarget::BodySizeBytes => Some(Value::from(body_size(result))),
    })
}

/// Evaluates one `Check`: presence on its own, every other operator on a present value.
fn check_target(
    target: &AssertionTarget,
    op: AssertionOp,
    expected: &Value,
    result: &ExecutionResult,
) -> Checked {
    let label = target_label(target);
    let actual = match target_value(target, result) {
        Ok(actual) => actual,
        Err(message) => return Checked::of(false, message),
    };
    match (op, actual) {
        (AssertionOp::Exists, Some(actual)) => {
            Checked::of(true, format!("{label} is {}", display_json(&actual)))
        }
        (AssertionOp::Exists, None) => Checked::of(false, format!("{label} is missing")),
        (AssertionOp::NotExists, None) => Checked::of(true, format!("{label} is absent")),
        (AssertionOp::NotExists, Some(actual)) => Checked::of(
            false,
            format!(
                "Expected {label} to be absent, got {}",
                display_json(&actual)
            ),
        ),
        (_, None) => Checked::of(false, format!("{label} is missing")),
        (op, Some(actual)) => match compare(op, &actual, expected) {
            Ok(passed) => {
                let (expected, actual) = (display_json(expected), display_json(&actual));
                let phrase = op_phrase(op);
                let message = if passed {
                    format!("{label} {phrase} {expected}")
                } else {
                    format!("Expected {label} {phrase} {expected}, got {actual}")
                };
                Checked::compared(passed, message, format!("{phrase} {expected}"), actual)
            }
            Err(message) => Checked::of(false, message),
        },
    }
}

/// Whether `actual` satisfies `op` against `expected`; `Err` when the two cannot be compared
/// that way (a non-number under an ordering, an unusable pattern).
fn compare(op: AssertionOp, actual: &Value, expected: &Value) -> Result<bool, String> {
    Ok(match op {
        AssertionOp::Equals => loose_equals(actual, expected),
        AssertionOp::NotEquals => !loose_equals(actual, expected),
        AssertionOp::Contains => contains(actual, expected),
        AssertionOp::NotContains => !contains(actual, expected),
        AssertionOp::GreaterThan
        | AssertionOp::GreaterOrEqual
        | AssertionOp::LessThan
        | AssertionOp::LessOrEqual => {
            let Some(left) = json_number(actual) else {
                return Err(format!("{} is not a number", display_json(actual)));
            };
            let Some(right) = json_number(expected) else {
                return Err(format!(
                    "The expected value {} is not a number",
                    display_json(expected)
                ));
            };
            match op {
                AssertionOp::GreaterThan => left > right,
                AssertionOp::GreaterOrEqual => left >= right,
                AssertionOp::LessThan => left < right,
                _ => left <= right,
            }
        }
        AssertionOp::Matches | AssertionOp::NotMatches => {
            let Some(pattern) = expected.as_str() else {
                return Err("The pattern must be text".to_owned());
            };
            if let Some(problem) = pingdeck_base::regex::regex_check(pattern) {
                return Err(format!("Pattern {pattern:?} cannot be used: {problem}"));
            }
            let found = pingdeck_base::regex::is_match(pattern, &display_json(actual), false);
            found == (op == AssertionOp::Matches)
        }
        AssertionOp::IsType => match expected.as_str() {
            Some(name) => is_type(actual, name),
            None => return Err("The type name must be text".to_owned()),
        },
        AssertionOp::Exists => true,
        AssertionOp::NotExists => false,
    })
}

/// JSON equality that also reads a number written as text (`"200"` equals `200`, `1` equals
/// `1.0`) and a boolean written as text; header and body values are always text.
fn loose_equals(actual: &Value, expected: &Value) -> bool {
    if actual == expected {
        return true;
    }
    match (actual, expected) {
        (Value::Number(_), _) | (_, Value::Number(_)) => {
            matches!((json_number(actual), json_number(expected)), (Some(a), Some(b)) if a == b)
        }
        (Value::String(text), Value::Bool(flag)) | (Value::Bool(flag), Value::String(text)) => {
            text.trim() == flag.to_string()
        }
        _ => false,
    }
}

/// Text contains the expected text, an array holds an equal element, an object has the key.
fn contains(actual: &Value, expected: &Value) -> bool {
    match actual {
        Value::String(text) => text.contains(display_json(expected).as_str()),
        Value::Array(items) => items.iter().any(|item| loose_equals(item, expected)),
        Value::Object(members) => expected
            .as_str()
            .is_some_and(|key| members.contains_key(key)),
        other => display_json(other).contains(display_json(expected).as_str()),
    }
}

/// Whether `value` is of the JSON type `name` (`integer`: a number without a fraction).
fn is_type(value: &Value, name: &str) -> bool {
    match name {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|n| n.fract() == 0.0)
        }
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn status_list(values: &[u16]) -> String {
    values
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn display_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn header_value(headers: &[ExecutionHeader], key: &str) -> Option<String> {
    let values = headers
        .iter()
        .filter(|header| header.key.eq_ignore_ascii_case(key))
        .map(|header| header.value.trim())
        .collect::<Vec<_>>();

    if values.is_empty() {
        None
    } else {
        Some(values.join(", "))
    }
}

fn lookup_body_json(
    result: &ExecutionResult,
    path: &str,
) -> Result<Option<serde_json::Value>, String> {
    let body = serde_json::from_str::<serde_json::Value>(&result.body_text)
        .map_err(|error| format!("Response body is not valid JSON: {error}"))?;

    pingdeck_response::response_viewer::json_path_value(&body, path)
        .map(|found| found.map(std::borrow::Cow::into_owned))
        .map_err(|message| format!("JSON path {path} is not valid: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response() -> ExecutionResult {
        let body_text =
            r#"{"data":{"items":[{"id":"inv_1","amount":42}],"total":1},"ok":true}"#.to_string();
        ExecutionResult {
            status_code: 200,
            status_text: "OK".to_string(),
            url: "https://api.example.com/v1/invoices".to_string(),
            elapsed_ms: 120,
            attempt_count: 1,
            headers: vec![
                ExecutionHeader {
                    key: "Content-Type".into(),
                    value: "application/json".into(),
                },
                ExecutionHeader {
                    key: "set-cookie".into(),
                    value: "a=1".into(),
                },
                ExecutionHeader {
                    key: "Set-Cookie".into(),
                    value: "b=2".into(),
                },
            ],
            cookies: Vec::new(),
            body_size_bytes: body_text.len(),
            raw_body: None,
            body_handle: None,
            suggested_view: crate::executor::SuggestedView::Raw,
            body_text,
            body_truncated: false,
            body_total_bytes: None,
            body_incomplete: None,
            saved_body_path: None,
            body_base64: None,
            raw_body_available: true,
            sent_request: None,
            assertion_results: Vec::new(),
            script_tests: Vec::new(),
            console: Vec::new(),
            script_error: None,
            next_request: None,
            stop_run: false,
            timing: Default::default(),
            call_chain: Default::default(),
            history_id: None,
            history_saved: false,
            ai: None,
            cookie_report: Default::default(),
        }
    }

    fn view(reply: &str) -> AiResultView<'_> {
        AiResultView {
            status: 200,
            reply,
            finish_reason: Some("stop"),
            error: None,
            ttft_ms: Some(120),
            input_tokens: Some(9),
            output_tokens: Some(2),
        }
    }

    #[test]
    fn http_assertions_check_status_headers_json_paths_body_size_and_timing() {
        assert!(evaluate_assertions(&[], &response()).is_empty());
        let body = response();
        let body_len = body.body_size_bytes as u64;
        let results = evaluate_assertions(
            &[
                Assertion::StatusEquals { value: 200 },
                Assertion::StatusEquals { value: 500 },
                Assertion::StatusIn {
                    values: vec![200, 201, 204],
                },
                Assertion::StatusIn {
                    values: vec![400, 404],
                },
                Assertion::HeaderExists {
                    key: "content-type".into(),
                },
                Assertion::HeaderExists {
                    key: "x-missing".into(),
                },
                Assertion::HeaderEquals {
                    key: "SET-COOKIE".into(),
                    value: "a=1, b=2".into(),
                },
                Assertion::HeaderEquals {
                    key: "content-type".into(),
                    value: "text/plain".into(),
                },
                Assertion::JsonPathExists {
                    path: "data.items[0].id".into(),
                },
                Assertion::JsonPathEquals {
                    path: "data.items[0].id".into(),
                    value: serde_json::json!("inv_1"),
                },
                Assertion::JsonPathEquals {
                    path: "$.data.total".into(),
                    value: serde_json::json!(1),
                },
                Assertion::JsonPathEquals {
                    path: "data.items[0].amount".into(),
                    value: serde_json::json!(7),
                },
                Assertion::JsonPathExists {
                    path: "data.items[9].id".into(),
                },
                Assertion::JsonPathExists {
                    path: "data.missing".into(),
                },
                Assertion::JsonPathExists {
                    path: "data.items[abc]".into(),
                },
                Assertion::BodyContains {
                    value: "inv_1".into(),
                },
                Assertion::BodyContains {
                    value: "inv_2".into(),
                },
                Assertion::BodySizeLessThanBytes { value: body_len },
                Assertion::BodySizeLessThanBytes {
                    value: body_len + 1,
                },
                Assertion::ResponseTimeLessThanMs { value: 120 },
                Assertion::ResponseTimeLessThanMs { value: 121 },
            ],
            &body,
        );
        let passed: Vec<bool> = results.iter().map(|r| r.passed).collect();
        assert_eq!(
            passed,
            [
                true, false, true, false, true, false, true, false, true, true, true, false, false,
                false, false, true, false, false, true, false, true
            ]
        );
        assert_eq!(results[0].label, "Status is 200");
        assert!(results[1].message.contains("Expected status 500, got 200"));
        assert_eq!(results[2].label, "Status in [200, 201, 204]");
        assert_eq!(results[6].label, "Header SET-COOKIE equals a=1, b=2");
        assert_eq!(results[8].label, "JSON data.items[0].id exists");
        assert_eq!(results[9].label, "JSON data.items[0].id equals inv_1");
        assert!(
            results[11]
                .message
                .contains("Expected data.items[0].amount to be 7, got 42")
        );
        assert!(results[12].message.contains("was not found"));
        assert!(results[14].message.contains("Unsupported selector \"abc\""));
        assert_eq!(results[19].label, "Response time under 120 ms");
        assert_eq!(results[20].index, 20);

        let mut invalid_body = response();
        invalid_body.body_text = "not json".into();
        let bad = evaluate_assertions(
            &[Assertion::JsonPathExists {
                path: "data.id".into(),
            }],
            &invalid_body,
        );
        assert!(!bad[0].passed && bad[0].message.contains("not valid JSON"));
    }

    #[test]
    fn ai_assertions_check_view_presence_reply_schema_limits_errors_and_validation() {
        let kinds = [
            Assertion::AiReplyContains {
                value: "pong".into(),
            },
            Assertion::AiReplyMatches {
                pattern: "x".into(),
            },
            Assertion::AiJsonSchemaValid {
                schema: serde_json::json!({}),
            },
            Assertion::AiTtftLessThanMs { value: 500 },
            Assertion::AiTokensLessThan { value: 50 },
            Assertion::AiNoError {},
        ];
        for results in [
            evaluate_assertions(&kinds, &response()),
            evaluate_assertions_with_ai(&kinds, &response(), None),
        ] {
            assert!(
                results
                    .iter()
                    .all(|r| !r.passed && r.message.contains("needs an AI API request"))
            );
        }

        let ai = view("pong");
        let bible = evaluate_assertions_with_ai(
            &[
                kinds[0].clone(),
                kinds[3].clone(),
                kinds[4].clone(),
                kinds[5].clone(),
                Assertion::StatusEquals { value: 200 },
            ],
            &response(),
            Some(&ai),
        );
        assert!(bible.iter().all(|r| r.passed));
        assert_eq!(
            (bible[0].label.as_str(), bible[3].label.as_str()),
            ("Reply contains \"pong\"", "No error")
        );

        let pong = view("Pong!");
        let text_results = evaluate_assertions_with_ai(
            &[
                Assertion::AiReplyContains {
                    value: "pong".into(),
                },
                Assertion::AiReplyMatches {
                    pattern: "^P.ng!$".into(),
                },
                Assertion::AiReplyMatches {
                    pattern: "^p".into(),
                },
                Assertion::AiReplyMatches {
                    pattern: "(?=x)".into(),
                },
            ],
            &response(),
            Some(&pong),
        );
        assert_eq!(
            text_results.iter().map(|r| r.passed).collect::<Vec<_>>(),
            [false, true, false, false]
        );
        assert!(
            text_results[0].message.contains("\"Pong!\"")
                && text_results[3].message.contains("cannot be used")
        );

        let schema = Assertion::AiJsonSchemaValid {
            schema: serde_json::json!({
                "type": "object",
                "required": ["name"],
                "properties": {"name": {"type": "string"}, "age": {"type": "integer"}},
                "additionalProperties": false
            }),
        };
        let run_schema = |reply: &str| {
            evaluate_assertions_with_ai(
                std::slice::from_ref(&schema),
                &response(),
                Some(&view(reply)),
            )
            .remove(0)
        };
        assert!(run_schema(r#" {"name": "Ada", "age": 36} "#).passed);
        assert!(
            run_schema(r#"{"age": 36}"#)
                .message
                .contains("\"name\" is a required property")
        );
        assert!(
            run_schema(r#"{"name": "a", "x": 1}"#)
                .message
                .contains("'x' was unexpected")
        );
        assert!(
            run_schema("Sure! Here you go: {}")
                .message
                .contains("not valid JSON")
        );
        let ref_res = evaluate_assertions_with_ai(
            &[Assertion::AiJsonSchemaValid {
                schema: serde_json::json!({"$ref": "#/definitions/x"}),
            }],
            &response(),
            Some(&view("{}")),
        )
        .remove(0);
        assert!(!ref_res.passed && ref_res.message.starts_with("Invalid schema"));

        let pass = |assertion: Assertion, ai: &AiResultView<'_>| {
            evaluate_assertions_with_ai(&[assertion], &response(), Some(ai))[0].passed
        };
        let mut ai = view("x");
        assert!(pass(Assertion::AiTtftLessThanMs { value: 121 }, &ai));
        assert!(!pass(Assertion::AiTtftLessThanMs { value: 120 }, &ai));
        ai.ttft_ms = None;
        assert!(!pass(Assertion::AiTtftLessThanMs { value: u64::MAX }, &ai));

        let mut ai = view("x");
        assert!(
            pass(Assertion::AiTokensLessThan { value: 12 }, &ai)
                && !pass(Assertion::AiTokensLessThan { value: 11 }, &ai)
        );
        ai.input_tokens = None;
        assert!(
            pass(Assertion::AiTokensLessThan { value: 3 }, &ai)
                && !pass(Assertion::AiTokensLessThan { value: 2 }, &ai)
        );
        ai.output_tokens = None;
        assert!(!pass(Assertion::AiTokensLessThan { value: u64::MAX }, &ai));

        let check_err = |ai: &AiResultView<'_>| {
            evaluate_assertions_with_ai(&[Assertion::AiNoError {}], &response(), Some(ai)).remove(0)
        };
        assert!(check_err(&view("x")).passed);
        let mut ai = view("x");
        ai.status = 429;
        assert!(check_err(&ai).message.contains("429"));
        ai = view("x");
        ai.error = Some("overloaded");
        assert!(check_err(&ai).message.contains("overloaded"));
        ai = view("x");
        ai.finish_reason = Some("error");
        assert!(!check_err(&ai).passed);
        ai.finish_reason = None;
        assert!(check_err(&ai).passed);

        for bad in [
            Assertion::AiReplyContains { value: " ".into() },
            Assertion::AiReplyMatches {
                pattern: String::new(),
            },
            Assertion::AiReplyMatches {
                pattern: "(".into(),
            },
            Assertion::AiReplyMatches {
                pattern: "(?=x)".into(),
            },
            Assertion::AiJsonSchemaValid {
                schema: serde_json::json!([]),
            },
        ] {
            assert!(bad.validate().is_err());
        }
        for ok in [
            Assertion::AiReplyMatches {
                pattern: "^ok$".into(),
            },
            Assertion::AiJsonSchemaValid {
                schema: serde_json::json!({"$ref": "x"}),
            },
            Assertion::AiTtftLessThanMs { value: 0 },
            Assertion::AiNoError {},
        ] {
            assert!(ok.validate().is_ok());
        }
    }

    fn check(target: AssertionTarget, op: AssertionOp, value: Value) -> Assertion {
        Assertion::Check { target, op, value }
    }

    #[test]
    fn check_operators_negate_compare_match_and_type_values() {
        use AssertionOp::*;
        let json = |path: &str| AssertionTarget::JsonPath { path: path.into() };
        let header = |name: &str| AssertionTarget::Header { name: name.into() };
        let cases = [
            (check(AssertionTarget::Status, NotEquals, json!(500)), true),
            (check(AssertionTarget::Status, Equals, json!("200")), true),
            (check(AssertionTarget::Status, LessThan, json!(300)), true),
            (
                check(AssertionTarget::Status, GreaterOrEqual, json!(400)),
                false,
            ),
            (
                check(json("data.items[0].amount"), GreaterThan, json!(41.5)),
                true,
            ),
            (
                check(json("data.items[0].amount"), LessOrEqual, json!(41)),
                false,
            ),
            (
                check(json("data.items[0].amount"), Equals, json!(42.0)),
                true,
            ),
            (
                check(json("data.items[0].id"), Matches, json!("^inv_\\d+$")),
                true,
            ),
            (
                check(json("data.items[0].id"), NotMatches, json!("^inv_")),
                false,
            ),
            (check(json("data.items"), IsType, json!("array")), true),
            (check(json("data.total"), IsType, json!("integer")), true),
            (check(json("ok"), IsType, json!("string")), false),
            (
                check(json("data.items[*].id"), Contains, json!("inv_1")),
                true,
            ),
            (check(json("data"), Contains, json!("total")), true),
            (check(json("data.missing"), NotExists, Value::Null), true),
            (check(json("data.missing"), NotEquals, json!(1)), false),
            (check(json("data.total"), Exists, Value::Null), true),
            (check(header("content-type"), Contains, json!("json")), true),
            (
                check(header("content-type"), NotContains, json!("xml")),
                true,
            ),
            (check(header("x-missing"), NotExists, Value::Null), true),
            (
                check(AssertionTarget::Body, NotContains, json!("inv_2")),
                true,
            ),
            (
                check(AssertionTarget::ResponseTimeMs, LessThan, json!(121)),
                true,
            ),
            (
                check(AssertionTarget::BodySizeBytes, GreaterThan, json!(10)),
                true,
            ),
        ];
        let (assertions, want): (Vec<_>, Vec<_>) = cases.into_iter().unzip();
        let results = evaluate_assertions(&assertions, &response());
        let got: Vec<bool> = results.iter().map(|r| r.passed).collect();
        assert_eq!(got, want, "{results:#?}");
        assert_eq!(results[0].label, "Status does not equal 500");
        assert_eq!(results[14].label, "JSON data.missing does not exist");
        assert!(
            results[3]
                .message
                .contains("Expected Status is at least 400, got 200")
        );
        assert_eq!(results[3].expected.as_deref(), Some("is at least 400"));

        let mut text = response();
        text.body_text = "not json".into();
        let unreadable = evaluate_assertions(&[check(json("a"), NotExists, Value::Null)], &text);
        assert!(!unreadable[0].passed && unreadable[0].message.contains("not valid JSON"));
        let not_number = evaluate_assertions(
            &[check(json("data.items[0].id"), GreaterThan, json!(1))],
            &response(),
        );
        assert!(!not_number[0].passed && not_number[0].message.contains("is not a number"));

        let mut cut = response();
        cut.body_total_bytes = Some(5_000_000);
        let full = evaluate_assertions(
            &[Assertion::BodySizeLessThanBytes { value: 1_000_000 }],
            &cut,
        );
        assert!(
            !full[0].passed,
            "the full size, not the captured one, is checked"
        );
    }

    #[test]
    fn check_validation_rejects_operators_that_do_not_fit() {
        use AssertionOp::*;
        for bad in [
            check(AssertionTarget::Status, Exists, Value::Null),
            check(AssertionTarget::Status, Contains, json!("2")),
            check(AssertionTarget::Body, Contains, Value::Null),
            check(AssertionTarget::Status, LessThan, json!("soon")),
            check(AssertionTarget::Body, Matches, json!("(")),
            check(AssertionTarget::Body, Matches, json!(1)),
            check(AssertionTarget::Body, IsType, json!("date")),
            check(
                AssertionTarget::Header { name: " ".into() },
                Exists,
                Value::Null,
            ),
            Assertion::JsonSchemaValid {
                schema: json!([]),
                path: None,
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        for ok in [
            check(AssertionTarget::Status, LessThan, json!("400")),
            check(
                AssertionTarget::JsonPath { path: "$.a".into() },
                NotExists,
                Value::Null,
            ),
            check(AssertionTarget::Body, NotMatches, json!("^error")),
            Assertion::JsonSchemaValid {
                schema: json!({}),
                path: Some("$.data".into()),
            },
        ] {
            assert!(ok.validate().is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn new_kinds_keep_their_wire_shape_and_old_kinds_still_load() {
        let stored = json!([
            {"StatusEquals": {"value": 200}},
            {"Check": {"target": {"kind": "header", "name": "etag"}, "op": "not_exists"}},
            {"JsonSchemaValid": {"schema": {"type": "object"}}}
        ]);
        let parsed: Vec<Assertion> = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(parsed[0], Assertion::StatusEquals { value: 200 });
        assert_eq!(
            parsed[1],
            check(
                AssertionTarget::Header {
                    name: "etag".into()
                },
                AssertionOp::NotExists,
                Value::Null
            )
        );
        let round: Value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(round[2], stored[2]);
        assert_eq!(
            round[1]["Check"]["target"],
            json!({"kind": "header", "name": "etag"})
        );
    }

    #[test]
    fn json_schema_assertion_validates_the_body_or_a_path_with_openapi_annotations() {
        let schema = json!({
            "type": "object",
            "required": ["id", "amount"],
            "properties": {"id": {"type": "string", "example": "inv_1"}, "amount": {"type": "integer"}},
            "x-origin": "openapi"
        });
        let results = evaluate_assertions(
            &[
                Assertion::JsonSchemaValid {
                    schema: schema.clone(),
                    path: Some("data.items[0]".into()),
                },
                Assertion::JsonSchemaValid {
                    schema: schema.clone(),
                    path: None,
                },
                Assertion::JsonSchemaValid {
                    schema,
                    path: Some("data.nope".into()),
                },
            ],
            &response(),
        );
        assert!(results[0].passed, "{:?}", results[0]);
        assert!(!results[1].passed && results[1].message.contains("\"id\" is a required property"));
        assert!(!results[2].passed && results[2].message.contains("was not found"));
        assert_eq!(results[0].label, "JSON data.items[0] matches the schema");
    }

    /// One dialect in four places. Flow extraction and Mock rules (both through
    /// `items::json_path_lookup`), request assertions (`lookup_body_json`) and the response
    /// filter agree on every path. The filter takes the canonical `$` form, so a legacy dot path
    /// is fed to it with `$.` in front. `-` is no match, `!` an error.
    #[test]
    fn json_path_is_one_dialect_in_four_places() {
        use pingdeck_response::response_viewer::{
            JsonPathFilterOutcomeDto, filter_response_jsonpath,
        };
        let text = r#"{"user":{"name":"Ada"},"items":[{"id":7},{"id":8}],"x-request-id":"r1","a.b":1,"0":"z"}"#;
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        let mut result = response();
        result.body_text = text.to_owned();
        let shown = |found: Option<&serde_json::Value>| match found {
            Some(value) if *value == body => "$root".to_owned(),
            Some(value) => value.to_string(),
            None => "-".to_owned(),
        };
        let mut mismatches = Vec::new();
        for (path, want) in [
            ("$['x-request-id']", r#""r1""#),
            ("x-request-id", r#""r1""#),
            ("items.0.id", "7"),
            ("$.items[0].id", "7"),
            ("$.items.0.id", "7"),
            (" user.name ", r#""Ada""#),
            ("$.user['name']", r#""Ada""#),
            ("$.items[-1].id", "8"),
            ("$.items[*].id", "[7,8]"),
            ("$..id", "[7,8]"),
            ("$['a.b']", "1"),
            ("$.user.missing", "-"),
            ("$.0", r#""z""#),
            ("$", "$root"),
            ("$.user.name.", "!"),
            ("$.items[?(@.id && @.x)]", "!"),
        ] {
            let flow_and_mock = match pingdeck_model::items::json_path_lookup(&body, path) {
                Ok(found) => shown(found.as_deref()),
                Err(_) => "!".to_owned(),
            };
            let assertion = match lookup_body_json(&result, path) {
                Ok(found) => shown(found.as_ref()),
                Err(_) => "!".to_owned(),
            };
            let trimmed = path.trim();
            let canonical = if trimmed.starts_with('$') {
                trimmed.to_owned()
            } else {
                format!("$.{trimmed}")
            };
            let viewer = match filter_response_jsonpath(text, &canonical, usize::MAX) {
                JsonPathFilterOutcomeDto::Filtered { mode, .. } if mode.contains("no matches") => {
                    "-".to_owned()
                }
                JsonPathFilterOutcomeDto::Filtered { text, .. } => shown(
                    serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .as_ref(),
                ),
                JsonPathFilterOutcomeDto::Error { .. } => "!".to_owned(),
            };
            if [&flow_and_mock, &assertion, &viewer]
                .iter()
                .any(|got| got.as_str() != want)
            {
                mismatches.push((path, flow_and_mock, assertion, viewer));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }
}
