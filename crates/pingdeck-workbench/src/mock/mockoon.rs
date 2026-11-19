//! Imports a Mockoon environment file into a [`MockServer`].

use std::collections::HashMap;

use serde_json::Value;

use crate::workspace::items::{
    MockBodySource, MockCallback, MockDataBucket, MockResponse, MockResponseMode, MockRoute,
    MockRouteType, MockRule, MockRuleOperator, MockRuleTarget, MockRulesOperator, MockServer,
    json_value_to_string,
};
use crate::workspace::models::{HttpMethod, KeyValue, WorkspaceId};

use super::logs::now_ms;
use super::reply::MAX_LATENCY_MS;
use super::template::unknown_helpers;

/// Converts a Mockoon environment document into a mock server draft.
///
/// Unsupported Mockoon features (WebSocket/GraphQL routes, per-route proxying, TLS termination
/// and streaming) are reported as warnings instead of being silently dropped, and so are the
/// template helpers PingDeck does not render. A file body keeps its path (served per request
/// when it is absolute); a data-bucket body becomes `{{data '<bucket>'}}`. A rule PingDeck
/// cannot evaluate is kept as a rule that never matches, so its response is never chosen by a
/// rule it did not satisfy. The imported server
/// never has proxying enabled and its callbacks are not attached to any
/// response, so opening a foreign file cannot make requests on its own.
pub fn import_mockoon_environment(
    workspace_id: &WorkspaceId,
    json_text: &str,
) -> Result<(MockServer, Vec<String>), String> {
    let environment: Value = serde_json::from_str(json_text)
        .map_err(|error| format!("invalid Mockoon environment JSON: {error}"))?;
    if !environment.is_object() {
        return Err("Mockoon environment must be a JSON object".into());
    }

    let mut warnings = Vec::new();
    let name = non_blank(environment.get("name"))
        .unwrap_or("Imported mock")
        .to_owned();
    let port = environment
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port > 0)
        .unwrap_or(3000);
    let endpoint_prefix = environment
        .get("endpointPrefix")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_matches('/')
        .to_owned();
    let latency_ms = clamped_latency(environment.get("latency"), "environment", &mut warnings);
    let cors_enabled = environment
        .get("cors")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let headers = mockoon_headers(environment.get("headers"));

    if environment
        .get("tlsOptions")
        .and_then(|tls| tls.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        warnings.push("TLS termination is not supported; the mock will serve plain HTTP".into());
    }

    // A data-bucket body names its bucket by name, which survives the id re-issuing of a copy.
    let bucket_names: HashMap<&str, &str> = environment
        .get("data")
        .and_then(Value::as_array)
        .map(|buckets| {
            buckets
                .iter()
                .filter_map(|bucket| {
                    Some((
                        non_blank(bucket.get("uuid"))?,
                        non_blank(bucket.get("name"))?,
                    ))
                })
                .filter(|(_, name)| !name.contains('\''))
                .collect()
        })
        .unwrap_or_default();

    let mut routes = Vec::new();
    let route_values = environment
        .get("routes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for (route_index, route) in route_values.iter().enumerate() {
        let endpoint = route
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let label = if endpoint.is_empty() {
            format!("route {route_index}")
        } else {
            endpoint.clone()
        };

        let route_type = route.get("type").and_then(Value::as_str).unwrap_or("http");
        let (route_type, data_bucket_id, crud_id_key) = match route_type {
            "http" => (MockRouteType::Http, String::new(), "id".to_owned()),
            "crud" => {
                let Some(bucket_id) = non_blank(
                    route
                        .get("databucketID")
                        .or_else(|| route.get("databucketId")),
                ) else {
                    warnings.push(format!(
                        "route '{label}': crud route has no data bucket and was skipped"
                    ));
                    continue;
                };
                (
                    MockRouteType::Crud,
                    bucket_id.to_owned(),
                    non_blank(route.get("crudKey")).unwrap_or("id").to_owned(),
                )
            }
            other => {
                warnings.push(format!(
                    "route '{label}': type '{other}' is not supported and was skipped"
                ));
                continue;
            }
        };
        if route
            .get("proxyMode")
            .is_some_and(|proxy| !proxy.is_null() && proxy != &Value::Bool(false))
        {
            warnings.push(format!(
                "route '{label}': per-route proxying is not supported; unmatched requests use the server proxy"
            ));
        }
        if route
            .get("callbacks")
            .and_then(Value::as_array)
            .is_some_and(|callbacks| !callbacks.is_empty())
        {
            warnings.push(format!("route '{label}': callbacks are not supported"));
        }

        let method = route
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("get")
            .to_ascii_uppercase();
        let method = if method == "ALL" {
            "ANY".to_owned()
        } else {
            method
        };
        if !SUPPORTED_METHODS.contains(&method.as_str()) {
            warnings.push(format!(
                "route '{label}': method '{method}' is not supported and the route was skipped"
            ));
            continue;
        }
        let path = if endpoint.starts_with('/') {
            endpoint
        } else {
            format!("/{endpoint}")
        };
        let response_mode = match route
            .get("responseMode")
            .and_then(Value::as_str)
            .unwrap_or("FIRST_MATCH")
            .to_ascii_uppercase()
            .as_str()
        {
            "SEQUENTIAL" => MockResponseMode::Sequential,
            "RANDOM" => MockResponseMode::Random,
            "FALLBACK" => MockResponseMode::Fallback,
            "DISABLE_RULES" => MockResponseMode::DisableRules,
            _ => MockResponseMode::FirstMatch,
        };

        let response_values = route
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if response_values.is_empty() {
            warnings.push(format!("route '{label}': no responses, route skipped"));
            continue;
        }

        let mut default_seen = false;
        let responses = response_values
            .iter()
            .enumerate()
            .map(|(response_index, response)| {
                import_mockoon_response(
                    response,
                    (route_index, response_index),
                    &label,
                    &bucket_names,
                    &mut default_seen,
                    &mut warnings,
                )
            })
            .collect();

        routes.push(MockRoute {
            id: non_blank(route.get("uuid"))
                .map_or_else(|| format!("mock_route_{route_index}"), str::to_owned),
            method,
            path,
            enabled: route
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            description: route
                .get("documentation")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            route_type,
            data_bucket_id,
            crud_id_key,
            response_mode,
            responses,
            position: i64::try_from(route_index).unwrap_or(0),
        });
    }

    let data_buckets = environment
        .get("data")
        .and_then(Value::as_array)
        .map(|buckets| {
            buckets
                .iter()
                .enumerate()
                .map(|(index, bucket)| {
                    let value = bucket.get("value").and_then(Value::as_str).unwrap_or("[]");
                    if serde_json::from_str::<Value>(value).is_err() {
                        warnings.push(format!(
                            "data bucket {index}: value is not valid JSON; served as an empty array"
                        ));
                    }
                    MockDataBucket {
                        id: non_blank(bucket.get("uuid"))
                            .map_or_else(|| format!("mock_bucket_{index}"), str::to_owned),
                        name: bucket
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        documentation: bucket
                            .get("documentation")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        value_json: value.to_owned(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let callbacks: Vec<MockCallback> = environment
        .get("callbacks")
        .and_then(Value::as_array)
        .map(|callbacks| {
            callbacks
                .iter()
                .enumerate()
                .filter_map(|(index, callback)| {
                    import_mockoon_callback(callback, index, &mut warnings)
                })
                .collect()
        })
        .unwrap_or_default();

    if !callbacks.is_empty() {
        warnings.push(format!(
            "{} callback(s) imported but not attached to any response; review the URLs and attach them manually",
            callbacks.len()
        ));
    }

    if environment
        .get("proxyMode")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        warnings.push(
            "proxy mode was disabled on import; review the target URL and enable it manually"
                .into(),
        );
    }

    let proxy_base_url = environment
        .get("proxyHost")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let proxy_remove_prefix = environment
        .get("proxyRemovePrefix")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let server = MockServer {
        id: non_blank(environment.get("uuid"))
            .map_or_else(|| format!("mock_imported_{port}"), str::to_owned),
        workspace_id: workspace_id.clone(),
        name,
        port,
        endpoint_prefix,
        latency_ms,
        cors_enabled,
        headers,
        routes,
        data_buckets,
        callbacks,
        proxy_enabled: false,
        proxy_base_url,
        proxy_remove_prefix,
        proxy_req_headers: mockoon_headers(environment.get("proxyReqHeaders")),
        proxy_res_headers: mockoon_headers(environment.get("proxyResHeaders")),
        updated_at_ms: now_ms(),
        environment_id: None,
    };

    Ok((server, warnings))
}

fn import_mockoon_callback(
    callback: &Value,
    index: usize,
    warnings: &mut Vec<String>,
) -> Option<MockCallback> {
    let method = callback
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("post")
        .to_ascii_uppercase();
    let method = match method.as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        other => {
            warnings.push(format!(
                "callback {index}: method '{other}' is not supported; it was skipped"
            ));
            return None;
        }
    };
    let url = callback
        .get("endpoint")
        .or_else(|| callback.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if url.trim().is_empty() {
        warnings.push(format!("callback {index}: no URL; it was skipped"));
        return None;
    }

    Some(MockCallback {
        id: non_blank(callback.get("uuid"))
            .map_or_else(|| format!("mock_callback_{index}"), str::to_owned),
        name: callback
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        documentation: callback
            .get("documentation")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        method,
        url: url.to_owned(),
        delay_ms: callback
            .get("latency")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(MAX_LATENCY_MS),
        headers: mockoon_headers(callback.get("headers")),
        body: callback
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    })
}

fn import_mockoon_response(
    response: &Value,
    (route_index, response_index): (usize, usize),
    route_label: &str,
    bucket_names: &HashMap<&str, &str>,
    default_seen: &mut bool,
    warnings: &mut Vec<String>,
) -> MockResponse {
    let body_type = response
        .get("bodyType")
        .and_then(Value::as_str)
        .unwrap_or("INLINE")
        .to_ascii_uppercase();
    let response_label = response.get("label").and_then(Value::as_str).unwrap_or("");
    let (body, body_source) = match body_type.as_str() {
        "INLINE" => (
            response
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            None,
        ),
        "FILE" => {
            let path = response
                .get("filePath")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned();
            if path.contains("{{") {
                warnings.push(format!(
                    "route '{route_label}': the file path '{path}' uses templates, which file bodies do not render; set a fixed absolute path"
                ));
            } else if !std::path::Path::new(&path).is_absolute() {
                warnings.push(format!(
                    "route '{route_label}': the file path '{path}' is not absolute and answers 500 until it is set to an absolute path"
                ));
            } else {
                warnings.push(format!(
                    "route '{route_label}': the response body is the local file '{path}', which this mock serves to its clients once started; check the path before starting the server"
                ));
            }
            (path, Some(MockBodySource::File))
        }
        "DATABUCKET" => match non_blank(
            response
                .get("databucketID")
                .or_else(|| response.get("databucketId")),
        ) {
            Some(bucket) => {
                let reference = bucket_names.get(bucket).copied().unwrap_or(bucket);
                (format!("{{{{data '{reference}'}}}}"), None)
            }
            None => {
                warnings.push(format!(
                    "route '{route_label}': the data bucket body names no bucket; the response body was left empty"
                ));
                (String::new(), None)
            }
        },
        other => {
            warnings.push(format!(
                "route '{route_label}': body type '{other}' is not supported; the response body was left empty"
            ));
            (String::new(), None)
        }
    };
    let headers = mockoon_headers(response.get("headers"));
    let mut unknown: Vec<String> = Vec::new();
    for template in std::iter::once(body.as_str())
        .filter(|_| body_source.is_none())
        .chain(headers.iter().map(|header| header.value.as_str()))
    {
        for name in unknown_helpers(template) {
            if !unknown.contains(&name) {
                unknown.push(name);
            }
        }
    }
    if !unknown.is_empty() {
        warnings.push(format!(
            "route '{route_label}', response \"{response_label}\": template helpers PingDeck does not render are sent as written: {}",
            unknown.join(", ")
        ));
    }

    let is_default = response
        .get("default")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_default && *default_seen {
        warnings.push(format!(
            "route '{route_label}': extra default response was imported as a regular response"
        ));
    }
    let is_default = is_default && !*default_seen;
    if is_default {
        *default_seen = true;
    }

    let rules_operator = match response
        .get("rulesOperator")
        .and_then(Value::as_str)
        .unwrap_or("AND")
        .to_ascii_uppercase()
        .as_str()
    {
        "OR" => MockRulesOperator::Or,
        _ => MockRulesOperator::And,
    };

    let rules = response
        .get("rules")
        .and_then(Value::as_array)
        .map(|rules| {
            rules
                .iter()
                .enumerate()
                .map(|(position, rule)| {
                    import_mockoon_rule(rule, position, route_label, response_label, warnings)
                })
                .collect()
        })
        .unwrap_or_default();

    MockResponse {
        id: non_blank(response.get("uuid")).map_or_else(
            || format!("mock_response_{route_index}_{response_index}"),
            str::to_owned,
        ),
        label: response
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        status_code: response
            .get("statusCode")
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok())
            .filter(|status| (100..=599).contains(status))
            .unwrap_or(200),
        headers,
        body,
        body_source,
        fault: None,
        latency_ms: clamped_latency(response.get("latency"), route_label, warnings),
        is_default,
        rules_operator,
        rules,
        callback_ids: Vec::new(),
        position: i64::try_from(response_index).unwrap_or(0),
        stream: None,
    }
}

/// A rule that never matches (`request number equals 0`: the count starts at 1), standing in for
/// a rule PingDeck cannot evaluate.
fn never_matching_rule(rule: &Value, position: usize) -> MockRule {
    MockRule {
        id: non_blank(rule.get("uuid"))
            .map_or_else(|| format!("mock_rule_{position}"), str::to_owned),
        target: MockRuleTarget::RequestNumber,
        modifier: String::new(),
        operator: MockRuleOperator::Equals,
        value: "0".to_owned(),
        invert: false,
        position: i64::try_from(position).unwrap_or(0),
    }
}

/// The imported rule, or [`never_matching_rule`] (with a warning) when its target, operator or
/// regex cannot be evaluated.
fn import_mockoon_rule(
    rule: &Value,
    position: usize,
    route_label: &str,
    response_label: &str,
    warnings: &mut Vec<String>,
) -> MockRule {
    match supported_rule(rule, position, route_label, response_label) {
        Ok(imported) => imported,
        Err(reason) => {
            warnings.push(format!(
                "{reason}; it was kept as a rule that never matches (request number equals 0)"
            ));
            never_matching_rule(rule, position)
        }
    }
}

fn supported_rule(
    rule: &Value,
    position: usize,
    route_label: &str,
    response_label: &str,
) -> Result<MockRule, String> {
    let target_text = rule
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let target = match target_text.as_str() {
        "query" | "queryparam" | "query_param" => MockRuleTarget::Query,
        "header" => MockRuleTarget::Header,
        "params" | "pathparam" | "path_param" => MockRuleTarget::PathParam,
        "cookie" => MockRuleTarget::Cookie,
        "body" => MockRuleTarget::Body,
        "body_raw" | "bodyraw" => MockRuleTarget::BodyRaw,
        "method" => MockRuleTarget::Method,
        "request_number" | "requestnumber" => MockRuleTarget::RequestNumber,
        other => {
            return Err(format!(
                "route '{route_label}': rule target '{other}' is not supported"
            ));
        }
    };

    let operator_text = rule
        .get("operator")
        .and_then(Value::as_str)
        .unwrap_or("equals")
        .to_ascii_lowercase();
    let operator = match operator_text.as_str() {
        "equals" | "equal" => MockRuleOperator::Equals,
        "contains" => MockRuleOperator::Contains,
        "starts_with" | "startswith" => MockRuleOperator::StartsWith,
        "regex" => MockRuleOperator::Regex,
        "regex_i" | "regex_ignore_case" | "regexignorecase" => MockRuleOperator::RegexIgnoreCase,
        "exists" => MockRuleOperator::Exists,
        "null" => MockRuleOperator::Null,
        "empty_array" => MockRuleOperator::EmptyArray,
        other => {
            return Err(format!(
                "route '{route_label}': rule operator '{other}' is not supported"
            ));
        }
    };

    let value = rule
        .get("value")
        .map(json_value_to_string)
        .unwrap_or_default();
    if matches!(
        operator,
        MockRuleOperator::Regex | MockRuleOperator::RegexIgnoreCase
    ) && let Some(reason) = crate::regex::regex_check(&value)
    {
        return Err(format!(
            "route '{route_label}', response \"{response_label}\", rule {}: {reason}",
            position + 1
        ));
    }

    Ok(MockRule {
        id: non_blank(rule.get("uuid"))
            .map_or_else(|| format!("mock_rule_{position}"), str::to_owned),
        target,
        modifier: rule
            .get("modifier")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        operator,
        value,
        invert: rule.get("invert").and_then(Value::as_bool).unwrap_or(false),
        position: i64::try_from(position).unwrap_or(0),
    })
}

/// Methods a route may use (the set the save validation accepts).
const SUPPORTED_METHODS: [&str; 10] = [
    "ANY", "ALL", "*", "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS",
];

/// The string at `value` unless it is missing or blank.
fn non_blank(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
}

/// A Mockoon latency in milliseconds, clamped to [`MAX_LATENCY_MS`] (with a warning).
fn clamped_latency(value: Option<&Value>, what: &str, warnings: &mut Vec<String>) -> u64 {
    let latency = value.and_then(Value::as_u64).unwrap_or(0);
    if latency > MAX_LATENCY_MS {
        warnings.push(format!(
            "{what}: latency {latency} ms was clamped to {MAX_LATENCY_MS} ms"
        ));
    }
    latency.min(MAX_LATENCY_MS)
}

fn mockoon_headers(value: Option<&Value>) -> Vec<KeyValue> {
    value
        .and_then(Value::as_array)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|header| {
                    let key = header.get("key").and_then(Value::as_str)?;
                    let value = header.get("value").and_then(Value::as_str).unwrap_or("");
                    Some(KeyValue {
                        key: key.to_owned(),
                        value: value.to_owned(),
                        enabled: true,
                        description: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
