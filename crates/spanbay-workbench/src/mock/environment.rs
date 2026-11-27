//! Environment bridge, step one: the plain variables of a server's environment fill in
//! `{{name}}` in the outbound parts of its configuration before the manager serves it.

use std::collections::HashMap;

use crate::workspace::items::MockServer;

/// Fills `{{name}}` in the proxy target, the proxy request and response header values and the
/// URL and header values of every callback with `variables` (the caller passes plain variables
/// only). A name `variables` lacks (a template helper such as `{{body 'id'}}`, a secret, an
/// unknown name) stays as written, so the per-request templates still run.
#[must_use]
pub fn with_environment(mut server: MockServer, variables: &HashMap<String, String>) -> MockServer {
    if variables.is_empty() {
        return server;
    }
    let fill = |text: &mut String| {
        if let Some(expanded) = expand(text, variables) {
            *text = expanded;
        }
    };
    fill(&mut server.proxy_base_url);
    for header in server
        .proxy_req_headers
        .iter_mut()
        .chain(server.proxy_res_headers.iter_mut())
    {
        fill(&mut header.value);
    }
    for callback in &mut server.callbacks {
        fill(&mut callback.url);
        for header in &mut callback.headers {
            fill(&mut header.value);
        }
    }
    server
}

/// `text` with every `{{name}}` that `variables` holds replaced; `None` when it has no token.
fn expand(text: &str, variables: &HashMap<String, String>) -> Option<String> {
    if !text.contains("{{") {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(length) = rest[start + 2..].find("}}") else {
            break;
        };
        let end = start + length + 4;
        out.push_str(&rest[..start]);
        match variables.get(rest[start + 2..end - 2].trim()) {
            Some(value) => out.push_str(value),
            None => out.push_str(&rest[start..end]),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    Some(out)
}
