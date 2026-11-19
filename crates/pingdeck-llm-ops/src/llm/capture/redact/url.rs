//! Redaction of a stored URL: user info, R2 query values, R10 path segments and the text rules.

use pingdeck_base::secrets::{REDACTED, redact_url_path};

use super::{is_secret_query_pair, mask_value, redact_text};

/// A stored URL: userinfo, every R2 query value, R10 path segments and the text rules (the
/// fragment included). Returns the URL and how many values were removed.
#[must_use]
pub(in crate::llm::capture) fn redact_url(url: &str, needles: &[String]) -> (String, u32) {
    let mut removed = 0;
    let (head, query) = match url.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (url, None),
    };
    let head = mask_userinfo(head, &mut removed);
    let head = mask_path(&head, &mut removed);
    let (head, text_removed) = redact_text(&head, needles);
    removed += text_removed;
    let Some(query) = query else {
        return (head, removed);
    };
    let (query, fragment) = match query.split_once('#') {
        Some((query, fragment)) => (query, Some(fragment)),
        None => (query, None),
    };
    let pairs: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) if is_secret_query_pair(key, value) => {
                removed += 1;
                format!("{key}={}", mask_value(value))
            }
            _ => {
                let (text, count) = redact_text(pair, needles);
                removed += count;
                text
            }
        })
        .collect();
    let mut out = format!("{head}?{}", pairs.join("&"));
    if let Some(fragment) = fragment {
        // Without a query the fragment is part of `head` and went through the text rules there.
        let (fragment, count) = redact_text(fragment, needles);
        removed += count;
        out.push('#');
        out.push_str(&fragment);
    }
    (out, removed)
}

/// R10 on the path of `head` (`scheme://authority/path`); the rest is kept.
fn mask_path(head: &str, removed: &mut u32) -> String {
    let (prefix, path) = match head.split_once("://") {
        Some((scheme, rest)) => match rest.find('/') {
            Some(at) => (format!("{scheme}://{}", &rest[..at]), &rest[at..]),
            None => return head.to_owned(),
        },
        None => (String::new(), head),
    };
    let masked = redact_url_path(path);
    if masked != path {
        *removed += 1;
    }
    format!("{prefix}{masked}")
}

/// The index of the `@` that ends the user info in `rest`, the part of a URL after `://`: the
/// last `@` before the first `/`, `?` or `#`.
pub(in crate::llm::capture) fn userinfo_at(rest: &str) -> Option<usize> {
    rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())].rfind('@')
}

/// Replaces `user:password@` in `scheme://user:password@host/…`.
fn mask_userinfo(head: &str, removed: &mut u32) -> String {
    let Some((scheme, rest)) = head.split_once("://") else {
        return head.to_owned();
    };
    let Some(at) = userinfo_at(rest) else {
        return head.to_owned();
    };
    if &rest[..at] != REDACTED {
        *removed += 1;
    }
    format!("{scheme}://{REDACTED}{}", &rest[at..])
}
