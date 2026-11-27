//! Route lookup, rule evaluation, path matching and request value helpers.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde_json::Value;

use crate::workspace::items::{
    MockResponse, MockResponseMode, MockRoute, MockRouteType, MockRule, MockRuleOperator,
    MockRuleTarget, MockRulesOperator, MockSelection, MockSelectionKind, MockServer,
    json_path_lookup, json_value_to_string,
};
use crate::workspace::variables::next_random;

use super::http::ParsedRequest;
use super::server::lock;

/// Normalizes an endpoint prefix to `/segment` form, or empty.
pub fn normalized_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim().trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}

/// Strips `prefix` from `path`, returning `None` when the path is outside it.
pub fn strip_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return Some(path);
    }
    let rest = path.strip_prefix(prefix)?;
    match rest {
        "" => Some("/"),
        _ if rest.starts_with('/') => Some(rest),
        _ => None,
    }
}

struct RuleContext<'a> {
    method: &'a str,
    query: &'a str,
    headers: &'a [(String, String)],
    cookies: &'a HashMap<String, String>,
    params: &'a HashMap<String, String>,
    body: &'a str,
    body_json: Option<&'a Value>,
    /// 1-based count of the requests that reached this route.
    request_number: u64,
}

/// Finds the first enabled route matching the request and selects its response.
pub(super) fn find_route(
    config: &MockServer,
    counters: &Mutex<HashMap<String, u64>>,
    route_hits: &Mutex<HashMap<String, u64>>,
    request: &ParsedRequest,
    path: &str,
    cookies: &HashMap<String, String>,
    body_json: Option<&Value>,
) -> Option<(
    MockRoute,
    HashMap<String, String>,
    MockResponse,
    MockSelection,
)> {
    let mut routes: Vec<&MockRoute> = config
        .routes
        .iter()
        .filter(|route| route.enabled && route.route_type == MockRouteType::Http)
        .collect();
    routes.sort_by_key(|route| route.position);

    for route in routes {
        if !method_matches(&route.method, &request.method) {
            continue;
        }
        let Some(params) = match_path_pattern(&route.path, path) else {
            continue;
        };
        let request_number = {
            let mut hits = lock(route_hits);
            let counter = hits.entry(route.id.clone()).or_insert(0);
            *counter = counter.saturating_add(1);
            *counter
        };
        let context = RuleContext {
            method: &request.method,
            query: &request.query,
            headers: &request.headers,
            cookies,
            params: &params,
            body: &request.body,
            body_json,
            request_number,
        };

        let preferred =
            preferred_status(&request.headers).and_then(|status| preferred_response(route, status));
        if let Some(response) = preferred {
            return Some((
                route.clone(),
                params,
                response.clone(),
                selection(MockSelectionKind::Prefer),
            ));
        }
        if let Some((response, selection)) = select_response(route, &context, counters) {
            return Some((route.clone(), params, response, selection));
        }
        if route.response_mode != MockResponseMode::Fallback {
            return None;
        }
    }

    None
}

/// The status a `Prefer: code=<status>` request header asks for (the Prism convention), from
/// the first `Prefer` header that names one; other preferences are ignored.
pub(super) fn preferred_status(headers: &[(String, String)]) -> Option<u16> {
    headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("prefer"))
        .flat_map(|(_, value)| value.split([',', ';']))
        .find_map(|preference| {
            let (key, value) = preference.split_once('=')?;
            if !key.trim().eq_ignore_ascii_case("code") {
                return None;
            }
            value
                .trim()
                .trim_matches('"')
                .parse::<u16>()
                .ok()
                .filter(|status| (100..=599).contains(status))
        })
}

/// The first response (by position) of `route` with `status`; rules and the response mode are
/// not consulted, so a documented error response can be forced.
fn preferred_response(route: &MockRoute, status: u16) -> Option<&MockResponse> {
    route
        .responses
        .iter()
        .filter(|response| response.status_code == status)
        .min_by_key(|response| response.position)
}

/// Whether an enabled HTTP route of `server` matches `method` and `path` (the path after the
/// endpoint prefix), whatever its responses would select.
#[must_use]
pub fn route_answers(server: &MockServer, method: &str, path: &str) -> bool {
    server.routes.iter().any(|route| {
        route.enabled
            && route.route_type == MockRouteType::Http
            && method_matches(&route.method, method)
            && match_path_pattern(&route.path, path).is_some()
    })
}

/// `ANY`, `ALL` and `*` match every method; anything else matches exactly.
pub fn method_matches(route_method: &str, request_method: &str) -> bool {
    let route_method = route_method.trim();
    route_method == "*"
        || route_method.eq_ignore_ascii_case("ANY")
        || route_method.eq_ignore_ascii_case("ALL")
        || route_method.eq_ignore_ascii_case(request_method)
}

/// Mockoon's `chooseResponse`: in FirstMatch and Fallback a response without rules never
/// matches and the default's rules take part in order; FirstMatch then answers with the default
/// (else the first response), Fallback yields `None`. DisableRules answers with the default.
fn select_response(
    route: &MockRoute,
    context: &RuleContext<'_>,
    counters: &Mutex<HashMap<String, u64>>,
) -> Option<(MockResponse, MockSelection)> {
    let mut responses: Vec<&MockResponse> = route.responses.iter().collect();
    if responses.is_empty() {
        return None;
    }
    responses.sort_by_key(|response| response.position);
    let plain = |response: &MockResponse, kind| Some((response.clone(), selection(kind)));
    let default = |kind_without_default| match responses.iter().find(|r| r.is_default) {
        Some(response) => plain(response, kind_without_default),
        None => responses
            .first()
            .and_then(|response| plain(response, MockSelectionKind::First)),
    };

    match route.response_mode {
        MockResponseMode::Sequential => {
            let mut counters = lock(counters);
            let counter = counters.entry(route.id.clone()).or_insert(0);
            let index = usize::try_from(*counter).unwrap_or(0) % responses.len();
            *counter = counter.wrapping_add(1);
            plain(responses[index], MockSelectionKind::Sequence)
        }
        MockResponseMode::Random => {
            let count = u64::try_from(responses.len()).unwrap_or(1);
            let index = usize::try_from(next_random() % count).unwrap_or(0);
            plain(responses[index], MockSelectionKind::Random)
        }
        MockResponseMode::DisableRules => default(MockSelectionKind::DisabledRules),
        MockResponseMode::FirstMatch | MockResponseMode::Fallback => {
            let matched = responses.iter().find_map(|response| {
                deciding_rule(response, context).map(|index| (*response, index))
            });
            match matched {
                Some((response, index)) => {
                    let rule = &response.rules[index];
                    let received = rule_target_value(rule, context).map(|mut value| {
                        value.truncate(value.floor_char_boundary(RECEIVED_LIMIT_BYTES));
                        value
                    });
                    Some((
                        response.clone(),
                        MockSelection {
                            kind: MockSelectionKind::Rule,
                            rule_number: u32::try_from(index + 1).ok(),
                            rule: Some(rule.clone()),
                            received,
                        },
                    ))
                }
                // With no match Fallback yields to the next route, the proxy, or a 404.
                None if route.response_mode == MockResponseMode::Fallback => None,
                None => default(MockSelectionKind::Default),
            }
        }
    }
}

/// Bytes of the request value a recorded rule selection keeps.
const RECEIVED_LIMIT_BYTES: usize = 200;

fn selection(kind: MockSelectionKind) -> MockSelection {
    MockSelection {
        kind,
        rule_number: None,
        rule: None,
        received: None,
    }
}

/// The index of the rule that makes `response` match (the first rule for "all", the first
/// matching one for "any"), or `None` when it does not match. A response without rules never
/// matches.
fn deciding_rule(response: &MockResponse, context: &RuleContext<'_>) -> Option<usize> {
    if response.rules.is_empty() {
        return None;
    }

    match response.rules_operator {
        MockRulesOperator::And => response
            .rules
            .iter()
            .all(|rule| rule_matches(rule, context))
            .then_some(0),
        MockRulesOperator::Or => response
            .rules
            .iter()
            .position(|rule| rule_matches(rule, context)),
    }
}

fn rule_matches(rule: &MockRule, context: &RuleContext<'_>) -> bool {
    let matched = evaluate_rule(rule, context);
    if rule.invert { !matched } else { matched }
}

fn evaluate_rule(rule: &MockRule, context: &RuleContext<'_>) -> bool {
    let target = rule_target_value(rule, context);

    match rule.operator {
        MockRuleOperator::Equals => target.is_some_and(|value| value.trim() == rule.value.trim()),
        MockRuleOperator::Contains => target.is_some_and(|value| value.contains(&rule.value)),
        MockRuleOperator::StartsWith => target.is_some_and(|value| value.starts_with(&rule.value)),
        MockRuleOperator::Regex => {
            target.is_some_and(|value| crate::regex::is_match(&rule.value, &value, false))
        }
        MockRuleOperator::RegexIgnoreCase => {
            target.is_some_and(|value| crate::regex::is_match(&rule.value, &value, true))
        }
        MockRuleOperator::Exists => target.is_some(),
        MockRuleOperator::Null => target.is_none_or(|value| value.trim().is_empty()),
        MockRuleOperator::EmptyArray => context
            .body_json
            .and_then(|body| json_path_lookup(body, &rule.modifier).ok().flatten())
            .is_some_and(|value| value.as_array().is_some_and(Vec::is_empty)),
    }
}

fn rule_target_value(rule: &MockRule, context: &RuleContext<'_>) -> Option<String> {
    match rule.target {
        MockRuleTarget::Method => Some(context.method.to_ascii_uppercase()),
        MockRuleTarget::PathParam => context.params.get(&rule.modifier).cloned(),
        MockRuleTarget::Header => header_value(context.headers, &rule.modifier).map(str::to_owned),
        MockRuleTarget::Cookie => context.cookies.get(&rule.modifier).cloned(),
        MockRuleTarget::RequestNumber => Some(context.request_number.to_string()),
        MockRuleTarget::Query => {
            if rule.modifier.is_empty() {
                Some(context.query.to_owned())
            } else {
                query_value(context.query, &rule.modifier)
            }
        }
        MockRuleTarget::Body => {
            if rule.modifier.is_empty() {
                Some(context.body.to_owned())
            } else {
                context
                    .body_json
                    .and_then(|body| json_path_lookup(body, &rule.modifier).ok().flatten())
                    .map(|value| json_value_to_string(&value))
            }
        }
        MockRuleTarget::BodyRaw => Some(context.body.to_owned()),
    }
}

/// Matches `path` against a route `pattern`: literals, `:param` captures (`{param}` is an
/// alias), `:param?` optional captures and a `*` that is valid only as the last segment (it
/// matches any remainder, even none). A `*` elsewhere is an ordinary literal segment. Literal
/// segments compare percent-decoded on both sides, like captured values.
pub(super) fn match_path_pattern(pattern: &str, path: &str) -> Option<HashMap<String, String>> {
    let pattern_segments = split_path(pattern);
    let path_segments = split_path(path);
    let mut params = HashMap::new();
    let mut dead_ends = HashSet::new();

    if match_segments(
        &pattern_segments,
        &path_segments,
        &mut params,
        &mut dead_ends,
    ) {
        Some(params)
    } else {
        None
    }
}

fn split_path(path: &str) -> Vec<&str> {
    path.trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// `dead_ends` holds the (pattern, segments) remainders of optional captures already proven
/// not to match, so a pattern of many `:p?` segments is tried in polynomial rather than
/// exponential time.
fn match_segments(
    pattern: &[&str],
    segments: &[&str],
    params: &mut HashMap<String, String>,
    dead_ends: &mut HashSet<(usize, usize)>,
) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return segments.is_empty();
    };
    if *first == "*" && rest.is_empty() {
        return true;
    }

    if let Some(capture) = capture_name(first) {
        let optional = capture.ends_with('?');
        let name = capture.trim_end_matches('?');
        let state = (pattern.len(), segments.len());

        if optional {
            if dead_ends.contains(&state) {
                return false;
            }
            if match_segments(rest, segments, params, dead_ends) {
                return true;
            }
        }

        if let Some((segment, remaining)) = segments.split_first() {
            params.insert(name.to_owned(), decode_percent(segment));
            if match_segments(rest, remaining, params, dead_ends) {
                return true;
            }
            params.remove(name);
        }
        if optional {
            dead_ends.insert(state);
        }
        return false;
    }

    let Some((segment, remaining)) = segments.split_first() else {
        return false;
    };
    if *first != *segment && decode_percent_cow(first) != decode_percent_cow(segment) {
        return false;
    }

    match_segments(rest, remaining, params, dead_ends)
}

/// The capture name of a `:name` / `:name?` segment or its `{name}` alias.
fn capture_name(segment: &str) -> Option<&str> {
    segment.strip_prefix(':').or_else(|| {
        segment
            .strip_prefix('{')?
            .strip_suffix('}')
            .filter(|name| !name.is_empty())
    })
}

/// Percent-decodes `input`, leaving malformed escapes untouched.
pub(super) fn decode_percent(input: &str) -> String {
    decode_percent_cow(input).into_owned()
}

fn decode_percent_cow(input: &str) -> std::borrow::Cow<'_, str> {
    percent_encoding::percent_decode_str(input).decode_utf8_lossy()
}

/// First value of `key` in a raw query string, percent-decoded.
pub(super) fn query_value(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
}

/// Case-insensitive header lookup.
pub(super) fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Parses the `Cookie` headers into a name/value map.
pub(super) fn parse_cookies(headers: &[(String, String)]) -> HashMap<String, String> {
    let mut cookies = HashMap::new();
    for (key, value) in headers {
        if !key.eq_ignore_ascii_case("cookie") {
            continue;
        }
        for pair in value.split(';') {
            if let Some((name, cookie_value)) = pair.split_once('=') {
                cookies.insert(name.trim().to_owned(), cookie_value.trim().to_owned());
            }
        }
    }
    cookies
}
