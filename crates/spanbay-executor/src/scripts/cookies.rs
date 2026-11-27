//! Scripts and the workspace cookie jar: `pm.cookies` (the cookies the request's URL sends, with
//! a response's own `Set-Cookie` cookies on top after it) and `pm.cookies.jar()`, which reads and
//! writes jar cookies. The jar is reachable only for the request's host, its parent domains and
//! its subdomains (that host is fixed before the phase's scripts run), so an imported script
//! cannot read the session cookies of an unrelated site and ship them elsewhere. Writes change
//! this run's jar at once (the main request sends them) and are reported for the caller to
//! persist.

use serde_json::{Value, json};
use std::time::SystemTime;

use spanbay_base::time::now_ms;
use spanbay_model::models::{CookieJarEntry, effective_cookie_domain};

use super::runtime::HostState;
use crate::executor::{cookie_is_expired, cookie_matches_url};

/// The URL `pm.cookies` reads for and whose host bounds `pm.cookies.jar()`, worked out once
/// before the phase's first script runs: after the response, the URL the request was sent to;
/// before it, the request URL resolved against the variables as they stand then. Later variable
/// writes by a script never move it, so `pm.variables.set("baseUrl", …)` cannot point the jar at
/// another site.
pub(super) fn frozen_cookie_url(host: &HostState) -> Option<url::Url> {
    let sent = host
        .response
        .as_ref()
        .and_then(|response| url::Url::parse(&response.url).ok())
        .filter(|url| url.host_str().is_some());
    sent.or_else(|| {
        let scopes = super::bridge::host_scopes(host);
        let url = spanbay_model::resolve_template_lenient(&host.request.url, &scopes);
        url::Url::parse(&crate::executor::with_default_scheme(&url)).ok()
    })
}

fn request_url(host: &HostState) -> Option<&url::Url> {
    host.cookie_url.as_ref()
}

/// The jar URL a `pm.cookies.jar()` call names (a bare host means `https://host/`), refused when
/// its host is not the request's host, a parent domain or a subdomain of it.
fn jar_url(host: &HostState, payload: &Value) -> Result<url::Url, String> {
    let text = payload
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| "pm.cookies.jar() needs a URL".to_owned())?;
    let url = url::Url::parse(text)
        .or_else(|_| url::Url::parse(&format!("https://{text}")))
        .map_err(|_| format!("pm.cookies.jar(): {text} is not a URL"))?;
    let target = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let own = request_url(host)
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    let related = !own.is_empty()
        && (target == own
            || own.ends_with(&format!(".{target}"))
            || target.ends_with(&format!(".{own}")));
    if related {
        Ok(url)
    } else {
        Err(format!(
            "pm.cookies.jar() reaches only the cookies of {} and its parent or sub domains",
            if own.is_empty() {
                "the request host"
            } else {
                own.as_str()
            }
        ))
    }
}

fn cookie_json(cookie: &CookieJarEntry) -> Value {
    json!({
        "name": cookie.name,
        "value": cookie.value,
        "domain": cookie.domain,
        "path": cookie.path,
        "hostOnly": cookie.host_only,
        "secure": cookie.secure,
        "httpOnly": cookie.http_only,
        "expires": cookie.expires,
        "maxAge": cookie.max_age,
        "sameSite": cookie.same_site,
    })
}

/// The unexpired jar cookies `url` sends, longest path first (the `Cookie` header order).
fn matching<'a>(host: &'a HostState, url: &url::Url) -> Vec<&'a CookieJarEntry> {
    let now = SystemTime::now();
    let mut cookies = host
        .cookie_jar
        .iter()
        // A script only reads the jar; nothing goes on the wire here, so the route does not matter.
        .filter(|cookie| !cookie_is_expired(cookie, now) && cookie_matches_url(cookie, url, true))
        .collect::<Vec<_>>();
    cookies.sort_by_key(|cookie| std::cmp::Reverse(cookie.path.len()));
    cookies
}

/// `pm.cookies`: the jar cookies of the request URL, then the response's own cookies replacing
/// those of the same name.
pub(super) fn request_cookies(host: &HostState) -> Value {
    let mut list = request_url(host)
        .map(|url| {
            matching(host, url)
                .into_iter()
                .map(cookie_json)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(response) = host.response.as_ref() {
        for cookie in &response.cookies {
            list.retain(|existing| existing["name"] != cookie.name.as_str());
            list.push(json!({
                "name": cookie.name,
                "value": cookie.value,
                "domain": cookie.domain,
                "path": cookie.path,
                "hostOnly": cookie.host_only,
                "secure": cookie.secure,
                "httpOnly": cookie.http_only,
                "expires": cookie.expires,
                "maxAge": cookie.max_age,
                "sameSite": cookie.same_site,
            }));
        }
    }
    Value::Array(list)
}

/// `jar.getAll(url)`.
pub(super) fn jar_list(host: &HostState, payload: &Value) -> Result<Value, String> {
    let url = jar_url(host, payload)?;
    Ok(json!({ "cookies": matching(host, &url).into_iter().map(cookie_json).collect::<Vec<_>>() }))
}

/// `jar.get(url, name)`: the value of the first matching cookie of that name.
pub(super) fn jar_get(host: &HostState, payload: &Value) -> Result<Value, String> {
    let url = jar_url(host, payload)?;
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let value = matching(host, &url)
        .into_iter()
        .find(|cookie| cookie.name == name)
        .map(|cookie| cookie.value.clone());
    Ok(json!({ "value": value }))
}

/// `jar.set(url, cookie)`: stores the cookie for the URL's host (or the domain it names, which
/// must cover that host) in this run's jar and records it for the caller to persist.
pub(super) fn jar_set(host: &mut HostState, payload: &Value) -> Result<Value, String> {
    let url = jar_url(host, payload)?;
    let cookie = payload.get("cookie").cloned().unwrap_or(Value::Null);
    let text = |key: &str| cookie.get(key).and_then(Value::as_str).map(str::to_owned);
    let flag = |key: &str| cookie.get(key).and_then(Value::as_bool).unwrap_or(false);
    let name = text("name")
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "pm.cookies.jar().set needs a cookie name".to_owned())?;
    let value = match cookie.get("value") {
        Some(Value::String(value)) => value.clone(),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    };
    let source_host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let (domain, host_only) = effective_cookie_domain(text("domain").as_deref(), &source_host)
        .map_err(|error| format!("pm.cookies.jar().set: {error}"))?;
    let max_age = cookie.get("maxAge").map(|value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    });
    let entry = CookieJarEntry::from_resolved_cookie(
        host.request.workspace_id.as_str(),
        None,
        &name,
        value,
        &domain,
        text("path").as_deref().unwrap_or("/"),
        host_only,
        text("expires").as_deref(),
        max_age.as_deref(),
        flag("secure"),
        flag("httpOnly"),
        text("sameSite").as_deref(),
        now_ms().to_string(),
    )
    .map_err(|error| format!("pm.cookies.jar().set: {error}"))?;
    host.cookie_jar.retain(|existing| existing.id != entry.id);
    host.cookie_writes
        .retain(|existing| existing.id != entry.id);
    host.cookie_deletes.retain(|id| *id != entry.id);
    host.cookie_jar.push(entry.clone());
    host.cookie_writes.push(entry);
    Ok(Value::Null)
}

/// `jar.unset(url, name)` (one name) and `jar.clear(url)` (every cookie the URL sends).
pub(super) fn jar_remove(host: &mut HostState, payload: &Value) -> Result<Value, String> {
    let url = jar_url(host, payload)?;
    let name = payload.get("name").and_then(Value::as_str);
    let removed = matching(host, &url)
        .into_iter()
        .filter(|cookie| name.is_none_or(|name| cookie.name == name))
        .map(|cookie| cookie.id.clone())
        .collect::<Vec<_>>();
    host.cookie_jar
        .retain(|cookie| !removed.contains(&cookie.id));
    host.cookie_writes
        .retain(|cookie| !removed.contains(&cookie.id));
    for id in removed {
        if !host.cookie_deletes.contains(&id) {
            host.cookie_deletes.push(id);
        }
    }
    Ok(Value::Null)
}
