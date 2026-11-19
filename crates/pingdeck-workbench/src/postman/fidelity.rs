//! What Postman v2.1 has no field for, written so Postman runs it and a re-import reads it back:
//! declarative assertions as marked `pm.test` lines in the test script, network options as
//! `protocolProfileBehavior`, saved examples as `response` entries. What neither form can hold
//! is counted in [`ExportNotes`] and becomes one export warning per kind.

use serde_json::{Map, Value, json};

use crate::workspace::models::{
    Assertion, HttpVersionPreference, KeyValue, MAX_REDIRECT_HOPS, MAX_REQUEST_EXAMPLES,
    NetworkField, NetworkOptions, RequestExample, TlsVersionPreference,
};

/// First line of the generated assertion block in an exported test script.
pub(super) const ASSERTIONS_BEGIN: &str = "// pingdeck:assertions:begin";
/// Last line of the generated assertion block.
pub(super) const ASSERTIONS_END: &str = "// pingdeck:assertions:end";
/// Separates a generated `pm.test` line from the assertion it was written from (compact JSON).
const ASSERTION_MARK: &str = " // pingdeck:assertion ";

/// Losses of one export, by kind.
#[derive(Debug, Default)]
pub(super) struct ExportNotes {
    /// Assertions written as `pm.test.skip`: Postman cannot evaluate them (AI and operator
    /// checks, JSONPath beyond plain member and index steps). A re-import restores them all the
    /// same.
    pub(super) skipped_assertions: usize,
    /// Requests with their own timeout: Postman has no per-request timeout.
    pub(super) timeouts: usize,
    /// Requests with their own retry count.
    pub(super) retries: usize,
    /// Requests with their own proxy.
    pub(super) proxies: usize,
    /// Requests pinned to HTTP/1.0, exported as `http1`.
    pub(super) http10: usize,
}

impl ExportNotes {
    /// One warning line per kind of loss.
    pub(super) fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut note = |count: usize, one: &str, many: &str| match count {
            0 => {}
            1 => out.push(one.to_owned()),
            count => out.push(many.replace("{count}", &count.to_string())),
        };
        note(
            self.skipped_assertions,
            "1 assertion cannot run in Postman and is written as a skipped test",
            "{count} assertions cannot run in Postman and are written as skipped tests",
        );
        note(
            self.timeouts,
            "1 request has its own timeout, which Postman cannot store; it is not exported",
            "{count} requests have their own timeout, which Postman cannot store; it is not exported",
        );
        note(
            self.retries,
            "1 request has its own retry count, which Postman cannot store; it is not exported",
            "{count} requests have their own retry count, which Postman cannot store; it is not exported",
        );
        note(
            self.proxies,
            "1 request has its own proxy, which Postman cannot store; it is not exported",
            "{count} requests have their own proxy, which Postman cannot store; it is not exported",
        );
        note(
            self.http10,
            "1 request is pinned to HTTP/1.0 and is exported as HTTP/1.x",
            "{count} requests are pinned to HTTP/1.0 and are exported as HTTP/1.x",
        );
        out
    }
}

/// A JavaScript string literal (a JSON string is one).
fn js(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

/// The steps of a plain JSONPath (`$`, `.name`, `[0]`, `['name']`) as a lodash `_.get` path, or
/// `None` for anything else (filters, wildcards, recursive descent, slices).
fn lodash_path(path: &str) -> Option<Value> {
    let mut rest = path.trim().strip_prefix('$')?;
    let mut steps = Vec::new();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
                .unwrap_or(after.len());
            if end == 0 {
                return None;
            }
            steps.push(Value::String(after[..end].to_owned()));
            rest = &after[end..];
            continue;
        }
        let after = rest.strip_prefix('[')?;
        let close = after.find(']')?;
        let inner = after[..close].trim();
        let quoted = inner
            .strip_prefix('\'')
            .and_then(|text| text.strip_suffix('\''))
            .or_else(|| {
                inner
                    .strip_prefix('"')
                    .and_then(|text| text.strip_suffix('"'))
            });
        match quoted {
            Some(name) if !name.contains(['\'', '"', '\\']) => {
                steps.push(Value::String(name.to_owned()));
            }
            Some(_) => return None,
            None => steps.push(Value::from(inner.parse::<u64>().ok()?)),
        }
        rest = &after[close + 1..];
    }
    Some(Value::Array(steps))
}

/// The test name and body of `assertion` as Postman runs it; `None` when Postman cannot.
fn postman_check(assertion: &Assertion) -> Option<(String, String)> {
    let json_get =
        |path: &str| lodash_path(path).map(|steps| format!("_.get(pm.response.json(), {steps})"));
    Some(match assertion {
        Assertion::StatusEquals { value } => (
            format!("Status is {value}"),
            format!("pm.response.to.have.status({value});"),
        ),
        Assertion::StatusIn { values } => (
            format!("Status is one of {values:?}"),
            format!(
                "pm.expect({}).to.include(pm.response.code);",
                serde_json::to_string(values).ok()?
            ),
        ),
        Assertion::HeaderExists { key } => (
            format!("Header {key} is present"),
            format!("pm.response.to.have.header({});", js(key)),
        ),
        Assertion::HeaderEquals { key, value } => (
            format!("Header {key} equals {value}"),
            format!(
                "pm.expect(pm.response.headers.get({})).to.eql({});",
                js(key),
                js(value)
            ),
        ),
        Assertion::JsonPathExists { path } => (
            format!("{path} exists"),
            format!("pm.expect({}).to.not.be.undefined;", json_get(path)?),
        ),
        Assertion::JsonPathEquals { path, value } => (
            format!("{path} equals {value}"),
            format!(
                "pm.expect({}).to.eql({});",
                json_get(path)?,
                serde_json::to_string(value).ok()?
            ),
        ),
        Assertion::BodyContains { value } => (
            format!("Body contains {value}"),
            format!("pm.expect(pm.response.text()).to.include({});", js(value)),
        ),
        Assertion::ResponseTimeLessThanMs { value } => (
            format!("Response time is below {value} ms"),
            format!("pm.expect(pm.response.responseTime).to.be.below({value});"),
        ),
        Assertion::BodySizeLessThanBytes { value } => (
            format!("Body is smaller than {value} bytes"),
            format!("pm.expect(pm.response.size().body).to.be.below({value});"),
        ),
        // AI checks and operator checks have no faithful Postman form; the marker still
        // restores them on a re-import.
        _ => return None,
    })
}

/// A short label of an assertion Postman cannot run.
fn skipped_name(assertion: &Assertion) -> String {
    match assertion {
        Assertion::JsonPathExists { path } | Assertion::JsonPathEquals { path, .. } => {
            format!("PingDeck JSONPath check {path}")
        }
        _ => "PingDeck check".to_owned(),
    }
}

/// `test_script` followed by the marked block of `assertions` (unchanged without assertions).
pub(super) fn test_script_with_assertions(
    test_script: &str,
    assertions: &[Assertion],
    notes: &mut ExportNotes,
) -> String {
    if assertions.is_empty() {
        return test_script.to_owned();
    }
    let mut lines = vec![ASSERTIONS_BEGIN.to_owned()];
    for assertion in assertions {
        let marker = serde_json::to_string(assertion).unwrap_or_default();
        let line = match postman_check(assertion) {
            Some((name, body)) => format!("pm.test({}, function () {{ {body} }});", js(&name)),
            None => {
                notes.skipped_assertions += 1;
                format!(
                    "pm.test.skip({}, function () {{}});",
                    js(&skipped_name(assertion))
                )
            }
        };
        // JavaScript ends a `//` comment at U+2028/U+2029, which serde_json leaves raw: as
        // `\u2028` escapes they stay inside the string literals and the marker JSON alike.
        lines.push(
            format!("{line}{ASSERTION_MARK}{marker}")
                .replace('\u{2028}', "\\u2028")
                .replace('\u{2029}', "\\u2029"),
        );
    }
    lines.push(ASSERTIONS_END.to_owned());
    let block = lines.join("\n");
    if test_script.trim().is_empty() {
        block
    } else {
        format!("{}\n{block}", test_script.trim_end_matches('\n'))
    }
}

/// Splits the marked assertion block off an imported test script: the script without it and the
/// assertions it carries. A line whose assertion cannot be read is reported and dropped; a script
/// without a complete block is returned unchanged.
pub(super) fn take_assertions(
    test_script: &str,
    warnings: &mut Vec<String>,
    label: &str,
) -> (String, Vec<Assertion>) {
    let lines: Vec<&str> = test_script.lines().collect();
    let Some(begin) = lines
        .iter()
        .position(|line| line.trim() == ASSERTIONS_BEGIN)
    else {
        return (test_script.to_owned(), Vec::new());
    };
    let Some(end) = lines[begin..]
        .iter()
        .position(|line| line.trim() == ASSERTIONS_END)
        .map(|offset| begin + offset)
    else {
        return (test_script.to_owned(), Vec::new());
    };
    let mut assertions = Vec::new();
    let mut unreadable = 0_usize;
    for line in &lines[begin + 1..end] {
        // The test name may hold the mark text too: the first place whose rest parses wins.
        let mark = ASSERTION_MARK.trim_start();
        let parsed = line
            .match_indices(mark)
            .find_map(|(at, _)| {
                serde_json::from_str::<Assertion>(line[at + mark.len()..].trim()).ok()
            })
            .filter(|assertion| assertion.validate().is_ok());
        match parsed {
            Some(assertion) => assertions.push(assertion),
            None if line.trim().is_empty() => {}
            None => unreadable += 1,
        }
    }
    if unreadable > 0 {
        warnings.push(format!(
            "{label}: {unreadable} generated assertion line(s) could not be read back; dropped"
        ));
    }
    let script = lines[..begin]
        .iter()
        .chain(&lines[end + 1..])
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    (script.trim_end().to_owned(), assertions)
}

/// The `protocolProfileBehavior` of `options`: only the request's own values that differ from
/// Postman's defaults (which are PingDeck's); fields that follow the global settings are left
/// out. What Postman cannot hold is counted in `notes`.
pub(super) fn protocol_profile_behavior(
    options: &NetworkOptions,
    notes: &mut ExportNotes,
) -> Option<Value> {
    let defaults = NetworkOptions::default();
    let own = |field: NetworkField| !options.inherit.contains(&field);
    let mut behavior = Map::new();
    if own(NetworkField::FollowRedirects) && !options.follow_redirects {
        behavior.insert("followRedirects".into(), Value::Bool(false));
    }
    if own(NetworkField::MaxRedirects) && options.max_redirects != defaults.max_redirects {
        behavior.insert("maxRedirects".into(), Value::from(options.max_redirects));
    }
    if options.follow_original_method {
        behavior.insert("followOriginalHttpMethod".into(), Value::Bool(true));
    }
    if options.follow_auth_header {
        behavior.insert("followAuthorizationHeader".into(), Value::Bool(true));
    }
    if options.remove_referer_on_redirect {
        behavior.insert("removeRefererHeaderOnRedirect".into(), Value::Bool(true));
    }
    if own(NetworkField::VerifyTls) && !options.verify_tls {
        behavior.insert("strictSSL".into(), Value::Bool(false));
    }
    if !options.encode_url_automatically {
        behavior.insert("disableUrlEncoding".into(), Value::Bool(true));
    }
    if !options.send_cookies {
        behavior.insert("disableCookies".into(), Value::Bool(true));
    }
    if !options.disabled_tls_versions.is_empty() {
        let names: Vec<Value> = options
            .disabled_tls_versions
            .iter()
            .map(|version| {
                Value::from(match version {
                    TlsVersionPreference::Tls12 => "TLSv1_2",
                    TlsVersionPreference::Tls13 => "TLSv1_3",
                })
            })
            .collect();
        behavior.insert("tlsDisabledProtocols".into(), Value::Array(names));
    }
    if own(NetworkField::HttpVersion) {
        let version = match options.http_version {
            HttpVersionPreference::Auto => None,
            HttpVersionPreference::Http10 => {
                notes.http10 += 1;
                Some("http1")
            }
            HttpVersionPreference::Http11 => Some("http1"),
            HttpVersionPreference::Http2 => Some("http2"),
        };
        if let Some(version) = version {
            behavior.insert("protocolVersion".into(), Value::from(version));
        }
    }
    if (own(NetworkField::TimeoutMs) && options.timeout_ms != defaults.timeout_ms)
        || options.total_timeout_ms.is_some()
    {
        notes.timeouts += 1;
    }
    if own(NetworkField::RetryCount) && options.retry_count > 0 {
        notes.retries += 1;
    }
    if own(NetworkField::ProxyUrl) && options.proxy_url.is_some() {
        notes.proxies += 1;
    }
    (!behavior.is_empty()).then_some(Value::Object(behavior))
}

/// `inherited` with the keys of `value` (a `protocolProfileBehavior` object) laid over it.
pub(super) fn merge_behavior(
    inherited: &Map<String, Value>,
    value: Option<&Value>,
) -> Map<String, Value> {
    let mut merged = inherited.clone();
    if let Some(object) = value.and_then(Value::as_object) {
        for (key, entry) in object {
            merged.insert(key.clone(), entry.clone());
        }
    }
    merged
}

/// Network options from a (merged) `protocolProfileBehavior`; keys PingDeck has no setting for
/// are reported once each. The file is untrusted input (docs/architecture.md, trust boundaries):
/// a value that would turn off certificate checks or carry `Authorization` across redirects is
/// reported and not applied, so only the user can weaken a request's transport.
pub(super) fn network_options_from_behavior(
    behavior: &Map<String, Value>,
    warnings: &mut Vec<String>,
    label: &str,
) -> NetworkOptions {
    let mut options = NetworkOptions::default();
    let mut ignored = Vec::new();
    let mut refused = Vec::new();
    for (key, value) in behavior {
        match (key.as_str(), value) {
            ("followRedirects", Value::Bool(flag)) => options.follow_redirects = *flag,
            ("maxRedirects", Value::Number(number)) => {
                if let Some(hops) = number.as_u64() {
                    options.max_redirects = u32::try_from(hops)
                        .unwrap_or(MAX_REDIRECT_HOPS)
                        .min(MAX_REDIRECT_HOPS);
                }
            }
            ("followOriginalHttpMethod", Value::Bool(flag)) => {
                options.follow_original_method = *flag;
            }
            ("followAuthorizationHeader", Value::Bool(true)) => {
                refused.push("followAuthorizationHeader (send Authorization to redirect targets)");
            }
            ("followAuthorizationHeader", Value::Bool(false)) => options.follow_auth_header = false,
            ("removeRefererHeaderOnRedirect", Value::Bool(flag)) => {
                options.remove_referer_on_redirect = *flag;
            }
            ("strictSSL", Value::Bool(false)) => {
                refused.push("strictSSL: false (turn off TLS certificate checks)");
            }
            ("strictSSL", Value::Bool(true)) => options.verify_tls = true,
            ("disableUrlEncoding", Value::Bool(flag)) => options.encode_url_automatically = !flag,
            ("disableCookies", Value::Bool(flag)) => options.send_cookies = !flag,
            ("tlsDisabledProtocols", Value::Array(names)) => {
                options.disabled_tls_versions = names
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|name| match name {
                        "TLSv1_2" => Some(TlsVersionPreference::Tls12),
                        "TLSv1_3" => Some(TlsVersionPreference::Tls13),
                        _ => None,
                    })
                    .collect();
            }
            ("protocolVersion", Value::String(version)) => {
                options.http_version = match version.as_str() {
                    "http1" => HttpVersionPreference::Http11,
                    "http2" => HttpVersionPreference::Http2,
                    _ => HttpVersionPreference::Auto,
                };
            }
            // PingDeck sends a GET or HEAD body whenever there is one: nothing to keep.
            ("disableBodyPruning", _) => {}
            _ => ignored.push(key.as_str()),
        }
    }
    if !ignored.is_empty() {
        warnings.push(format!(
            "{label}: protocolProfileBehavior {} ignored",
            ignored.join(", ")
        ));
    }
    if !refused.is_empty() {
        warnings.push(format!(
            "{label}: the collection asks for {}; not applied, change the request's network \
             settings yourself if you trust the source",
            refused.join(" and ")
        ));
    }
    options
}

/// A reason phrase for an example status (Postman's `status`).
fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

/// The Postman `response` entries of a request's saved examples.
pub(super) fn export_examples(examples: &[RequestExample]) -> Vec<Value> {
    examples
        .iter()
        .map(|example| {
            json!({
                "name": example.name,
                "status": reason_phrase(example.status),
                "code": example.status,
                "header": example
                    .headers
                    .iter()
                    .map(|header| json!({ "key": header.key, "value": header.value }))
                    .collect::<Vec<_>>(),
                "body": example.body,
            })
        })
        .collect()
}

/// The saved examples of a Postman item's `response` array: at most [`MAX_REQUEST_EXAMPLES`],
/// bodies cut to the size limit, each loss reported.
pub(super) fn import_examples(
    value: Option<&Value>,
    warnings: &mut Vec<String>,
    label: &str,
) -> Vec<RequestExample> {
    let Some(entries) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut examples = Vec::new();
    let mut cut = 0_usize;
    for (index, entry) in entries.iter().enumerate() {
        let Some(object) = entry.as_object() else {
            continue;
        };
        if examples.len() == MAX_REQUEST_EXAMPLES {
            warnings.push(format!(
                "{label}: only the first {MAX_REQUEST_EXAMPLES} saved examples are imported"
            ));
            break;
        }
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map_or_else(|| format!("Example {}", index + 1), str::to_owned);
        let status = object
            .get("code")
            .and_then(Value::as_u64)
            .and_then(|code| u16::try_from(code).ok())
            .filter(|code| (100..=599).contains(code))
            .unwrap_or(200);
        let headers = object
            .get("header")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let key = row.get("key")?.as_str()?.trim();
                        (!key.is_empty()).then(|| KeyValue {
                            key: key.to_owned(),
                            value: row
                                .get("value")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            enabled: true,
                            description: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let body = object
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let (example, was_cut) = RequestExample {
            name,
            status,
            headers,
            body,
        }
        .capped();
        cut += usize::from(was_cut);
        examples.push(example);
    }
    if cut > 0 {
        warnings.push(format!(
            "{label}: {cut} saved example bod{} cut to 64 KiB",
            if cut == 1 { "y was" } else { "ies were" }
        ));
    }
    examples
}
