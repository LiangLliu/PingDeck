//! Phase 0 of the sniffer, no I/O: whatever the user typed or pasted becomes a
//! [`NormalizedUrl`] (scheme default, punycode host, a pasted operation tail turned into a
//! protocol hint, dashboard paths cut, version segments, host class, known host), and the mounts
//! each protocol is probed at (known hosts use their preset's mounts).
//!
//! A trailing `#` means "use as typed": `typed_mount` keeps the tail, `base_mount` cuts it so
//! the model list can still be found, and `tail_hint` is reported either way.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use url::{Host, ParseError, Url};

use super::types::{SniffError, SniffWarning};
use crate::llm::{
    presets::{self, KnownHost},
    wire::Protocol,
};
use pingdeck_base::net_guard::{self, HostClass};

/// Longest accepted input, in characters.
const MAX_INPUT_CHARS: usize = 2048;

/// First path segments that mean "this is the panel, not the API".
const DASHBOARD_SEGMENTS: [&str; 13] = [
    "console",
    "login",
    "register",
    "dashboard",
    "panel",
    "admin",
    "token",
    "topup",
    "pricing",
    "playground",
    "log",
    "setting",
    "about",
];

/// Gemini method names that end a pasted request URL.
const GEMINI_ACTIONS: [&str; 5] = [
    "generateContent",
    "streamGenerateContent",
    "embedContent",
    "batchEmbedContents",
    "countTokens",
];

/// Anthropic-compatible prefixes tried only by extended scans on unknown hosts.
const MESSAGES_COMPAT_PREFIXES: [&str; 5] = [
    "/anthropic/v1",
    "/apps/anthropic/v1",
    "/api/anthropic/v1",
    "/api/compatible/v1",
    "/api/v1",
];

/// Network class of the typed host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UrlHostClass {
    /// Anything that is not one of the classes below.
    Public,
    /// `localhost`, `127.0.0.0/8`, `::1`.
    Loopback,
    /// RFC 1918, ULA and `*.local`.
    Private,
    /// `100.64.0.0/10` (Tailscale and similar): confirmable like a private network.
    Cgnat,
    /// Link-local, metadata, multicast, unspecified, reserved: always refused.
    LinkLocal,
}

impl UrlHostClass {
    /// Loopback and private hosts: local-runtime fingerprints apply and `allow_private` is set.
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Loopback | Self::Private | Self::Cgnat)
    }
}

/// The classification of a host name or IP literal; a `*.local` name counts as private (the
/// rule of [`net_guard::is_local_host`]).
fn classify_host(host: &str) -> UrlHostClass {
    match net_guard::classify_host_literal(host) {
        Some(HostClass::Loopback) => UrlHostClass::Loopback,
        Some(HostClass::PrivateRfc1918) => UrlHostClass::Private,
        Some(HostClass::Cgnat) => UrlHostClass::Cgnat,
        Some(HostClass::LinkLocalOrMetadata) => UrlHostClass::LinkLocal,
        Some(HostClass::Public) => UrlHostClass::Public,
        None if net_guard::is_local_host(host) => UrlHostClass::Private,
        None => UrlHostClass::Public,
    }
}

/// The normalised address. Everything is derived from the input alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedUrl {
    /// `origin + typed_mount` (+ `#` when typed as is); normalising it again returns it.
    pub typed: String,
    /// `scheme://host[:port]`.
    pub origin: String,
    /// Lower-case punycode host without brackets.
    pub host: String,
    /// The explicit non-default port.
    pub port: Option<u16>,
    /// The path after tail stripping (as typed when `as_typed`), `""` or `/…`.
    pub typed_mount: String,
    /// The typed path with tail and dashboard cut, also when `as_typed`; the base the model
    /// list hangs off.
    pub base_mount: String,
    /// `base_mount` without its last segment when that segment is a version.
    pub root: String,
    /// The protocol of a pasted operation tail.
    pub tail_hint: Option<Protocol>,
    /// The `{id}` of a pasted Gemini `models/{id}:action` tail.
    pub model_hint: Option<String>,
    /// A trailing `#`: probe the typed path only.
    pub as_typed: bool,
    /// Network class of the host.
    pub host_class: UrlHostClass,
    /// The preset region that owns the host.
    pub known_host: Option<KnownHost>,
    /// Userinfo, key-in-URL, dashboard and plain-http warnings.
    pub warnings: Vec<SniffWarning>,
}

fn invalid(reason: &str) -> SniffError {
    SniffError::InvalidInput {
        reason: reason.to_owned(),
    }
}

/// Removes one pair of surrounding quotes or angle brackets.
fn strip_wrapping(text: &str) -> &str {
    for (open, close) in [('"', '"'), ('\'', '\''), ('`', '`'), ('<', '>')] {
        if let Some(inner) = text
            .strip_prefix(open)
            .and_then(|rest| rest.strip_suffix(close))
        {
            return inner.trim();
        }
    }
    text
}

/// The scheme of `text` when it has one: `https://x`, but also `javascript:alert(1)`; a
/// `host:port` is not a scheme.
fn leading_scheme(text: &str) -> Option<&str> {
    let (prefix, rest) = text.split_once(':')?;
    let mut chars = prefix.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid || rest.starts_with("//") {
        return valid.then_some(prefix);
    }
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    let after_port = rest[digits..].chars().next();
    let is_port = digits > 0 && after_port.is_none_or(|c| matches!(c, '/' | '?' | '#'));
    (!is_port).then_some(prefix)
}

/// The host part of a scheme-less input (`user@host:8080/x` → `host`).
fn bare_host(text: &str) -> &str {
    let authority = text.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority.starts_with('[') {
        return authority
            .find(']')
            .map_or(authority, |end| &authority[..=end]);
    }
    authority.split(':').next().unwrap_or_default()
}

/// Whether `segment` is `v<digits>[beta[<digits>]]`, lower-case only.
#[must_use]
pub(super) fn is_version_segment(segment: &str) -> bool {
    let Some(rest) = segment.strip_prefix('v') else {
        return false;
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    match rest[digits..].strip_prefix("beta") {
        Some(tail) => tail.chars().all(|c| c.is_ascii_digit()),
        None => rest.len() == digits,
    }
}

/// Cuts pasted operation tails off the end of `segments` until none is left and returns the
/// protocol and Gemini model of the first (outermost) tail.
fn strip_tails(segments: &mut Vec<&str>) -> (Option<Protocol>, Option<String>) {
    let mut hint = None;
    let mut model_hint = None;
    loop {
        let count = segments.len();
        let last = segments.last().copied();
        let previous = count
            .checked_sub(2)
            .and_then(|index| segments.get(index))
            .copied();
        let (cut, protocol) = match (previous, last) {
            (Some("models"), Some(id)) => {
                match id
                    .rsplit_once(':')
                    .filter(|(name, action)| !name.is_empty() && GEMINI_ACTIONS.contains(action))
                {
                    Some((name, _)) => {
                        model_hint.get_or_insert_with(|| name.to_owned());
                        (2, Some(Protocol::Gemini))
                    }
                    None => (2, None),
                }
            }
            (Some("chat"), Some("completions")) => (2, Some(Protocol::OpenaiChat)),
            (Some("messages"), Some("count_tokens")) => (2, Some(Protocol::AnthropicMessages)),
            (_, Some("responses")) => (1, Some(Protocol::OpenaiResponses)),
            (_, Some("messages")) => (1, Some(Protocol::AnthropicMessages)),
            (_, Some("embeddings")) => (1, Some(Protocol::Embeddings)),
            (_, Some("systemone")) => (1, Some(Protocol::DecisionSystemone)),
            (_, Some("decisions")) => (1, Some(Protocol::DecisionOpenai)),
            (_, Some("models")) => (1, None),
            _ => break,
        };
        if hint.is_none() {
            hint = protocol;
        }
        segments.truncate(count - cut);
    }
    (hint, model_hint)
}

fn join_mount(segments: &[&str]) -> String {
    segments.iter().fold(String::new(), |mut mount, segment| {
        mount.push('/');
        mount.push_str(segment);
        mount
    })
}

fn parse_error(error: ParseError) -> SniffError {
    match error {
        ParseError::InvalidPort => invalid("port"),
        ParseError::EmptyHost => invalid("no_host"),
        _ => invalid("unparsable"),
    }
}

/// The trimmed input without its wrapping and trailing `#`, and whether `#` was there.
fn prepare_input(input: &str) -> Result<(&str, bool), SniffError> {
    let trimmed = strip_wrapping(input.trim());
    if trimmed.is_empty() {
        return Err(invalid("empty"));
    }
    if trimmed.chars().count() > MAX_INPUT_CHARS {
        return Err(invalid("too_long"));
    }
    Ok(match trimmed.strip_suffix('#') {
        Some(rest) => (rest.trim_end(), true),
        None => (trimmed, false),
    })
}

/// `text` with an http(s) scheme: kept when typed, else `http` for a local host and `https`
/// for any other. Any other scheme is refused.
fn with_default_scheme(text: &str) -> Result<Cow<'_, str>, SniffError> {
    match leading_scheme(text) {
        Some(scheme)
            if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") =>
        {
            Ok(Cow::Borrowed(text))
        }
        Some(_) => Err(invalid("scheme")),
        None => {
            let scheme = if classify_host(bare_host(text)).is_local() {
                "http"
            } else {
                "https"
            };
            Ok(Cow::Owned(format!("{scheme}://{text}")))
        }
    }
}

/// The mounts a URL path gives.
struct Mounts {
    typed_mount: String,
    base_mount: String,
    root: String,
    tail_hint: Option<Protocol>,
    model_hint: Option<String>,
    /// A dashboard path was cut off.
    dashboard: bool,
}

/// Splits `path` into mounts: operation tails cut (kept in `typed_mount` when `as_typed`),
/// a dashboard path dropped unless `as_typed`, and the root without a trailing version.
fn mounts_from_path(path: &str, as_typed: bool) -> Mounts {
    let mut segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let typed_mount = as_typed.then(|| join_mount(&segments));
    let (tail_hint, model_hint) = strip_tails(&mut segments);
    let dashboard = !as_typed
        && segments.first().is_some_and(|first| {
            DASHBOARD_SEGMENTS
                .iter()
                .any(|name| first.eq_ignore_ascii_case(name))
        });
    if dashboard {
        segments.clear();
    }
    let base_mount = join_mount(&segments);
    let root = match segments.split_last() {
        Some((last, rest)) if is_version_segment(last) => join_mount(rest),
        _ => base_mount.clone(),
    };
    Mounts {
        typed_mount: typed_mount.unwrap_or_else(|| base_mount.clone()),
        base_mount,
        root,
        tail_hint,
        model_hint,
        dashboard,
    }
}

/// Normalises a typed or pasted base URL without any I/O.
///
/// # Errors
/// [`SniffError::InvalidInput`] for empty, over-long or unparsable input, a scheme other than
/// `http`/`https`, a missing host or a port outside 1 to 65535.
pub fn normalize_base_url(input: &str) -> Result<NormalizedUrl, SniffError> {
    let (text, as_typed) = prepare_input(input)?;
    let candidate = with_default_scheme(text)?;
    let url = Url::parse(&candidate).map_err(parse_error)?;
    if url.port() == Some(0) {
        return Err(invalid("port"));
    }
    let host = match url.host() {
        Some(Host::Domain(name)) if !name.is_empty() => name.to_owned(),
        Some(Host::Ipv4(ip)) => ip.to_string(),
        Some(Host::Ipv6(ip)) => ip.to_string(),
        _ => return Err(invalid("no_host")),
    };
    let authority = url.host_str().unwrap_or(&host);
    let origin = match url.port() {
        Some(port) => format!("{}://{authority}:{port}", url.scheme()),
        None => format!("{}://{authority}", url.scheme()),
    };

    let mut warnings = Vec::new();
    if !url.username().is_empty() || url.password().is_some() {
        warnings.push(SniffWarning::UserinfoIgnored);
    }
    if url
        .query_pairs()
        .any(|(name, _)| name.to_ascii_lowercase().contains("key"))
    {
        warnings.push(SniffWarning::KeyInUrlIgnored);
    }
    let mounts = mounts_from_path(url.path(), as_typed);
    if mounts.dashboard {
        warnings.push(SniffWarning::DashboardPathIgnored);
    }
    let host_class = classify_host(&host);
    if url.scheme() == "http" && host_class == UrlHostClass::Public {
        warnings.push(SniffWarning::PlainHttpPublic);
    }
    let port = url.port();
    Ok(NormalizedUrl {
        typed: format!(
            "{origin}{}{}",
            mounts.typed_mount,
            if as_typed { "#" } else { "" }
        ),
        known_host: presets::known_host(&host, port),
        origin,
        host,
        port,
        typed_mount: mounts.typed_mount,
        base_mount: mounts.base_mount,
        root: mounts.root,
        tail_hint: mounts.tail_hint,
        model_hint: mounts.model_hint,
        as_typed,
        host_class,
        warnings,
    })
}

fn push_unique(list: &mut Vec<String>, mount: String) {
    if !list.contains(&mount) {
        list.push(mount);
    }
}

impl NormalizedUrl {
    /// The preset mount of the first of `protocols` the owning preset lists, for a known host.
    fn preset_mount(&self, protocols: &[Protocol]) -> Option<String> {
        let known = self.known_host.as_ref()?;
        protocols.iter().find_map(|protocol| {
            presets::region_mount(&known.preset_id, &known.region_id, *protocol)
        })
    }

    /// Whether any path segment of the base mount is a version.
    fn has_version_segment(&self) -> bool {
        self.base_mount.split('/').any(is_version_segment)
    }

    /// Mounts for Chat, Responses, Embeddings and the OpenAI model list, in order. Empty for a
    /// known host whose preset lists no OpenAI-style protocol.
    #[must_use]
    pub(super) fn openai_candidates(&self) -> Vec<String> {
        if self.as_typed {
            return vec![self.base_mount.clone()];
        }
        if self.known_host.is_some() {
            return self
                .preset_mount(&[
                    Protocol::OpenaiChat,
                    Protocol::OpenaiResponses,
                    Protocol::Embeddings,
                ])
                .into_iter()
                .collect();
        }
        let mut candidates = Vec::new();
        if self.base_mount.is_empty() {
            candidates.push("/v1".to_owned());
            candidates.push(String::new());
        } else {
            candidates.push(self.base_mount.clone());
            if !self.has_version_segment() {
                push_unique(&mut candidates, format!("{}/v1", self.base_mount));
            }
        }
        candidates
    }

    /// Mounts for Messages: `<root>/v1`, the preset prefix of a known host, and the compat
    /// prefixes of an extended scan on an unknown host. Empty for a known host whose preset
    /// does not list Messages.
    #[must_use]
    pub(super) fn messages_candidates(&self, extended: bool) -> Vec<String> {
        if self.as_typed {
            return vec![self.base_mount.clone()];
        }
        let mut candidates = Vec::new();
        if self.known_host.is_some() {
            let Some(mount) = self.preset_mount(&[Protocol::AnthropicMessages]) else {
                return candidates;
            };
            candidates.push(mount);
            if extended {
                push_unique(&mut candidates, format!("{}/v1", self.root));
            }
            return candidates;
        }
        candidates.push(format!("{}/v1", self.root));
        if extended {
            for prefix in MESSAGES_COMPAT_PREFIXES {
                push_unique(&mut candidates, prefix.to_owned());
            }
        }
        candidates
    }

    /// Mounts for Gemini: `<root>/v1beta`, then `<root>/v1` in an extended scan. Empty for a
    /// known host whose preset does not list Gemini.
    #[must_use]
    pub(super) fn gemini_candidates(&self, extended: bool) -> Vec<String> {
        if self.as_typed {
            return vec![self.base_mount.clone()];
        }
        if self.known_host.is_some() {
            return self.preset_mount(&[Protocol::Gemini]).into_iter().collect();
        }
        let mut candidates = vec![format!("{}/v1beta", self.root)];
        if extended {
            candidates.push(format!("{}/v1", self.root));
        }
        candidates
    }
}

/// The path and query of `candidate` when it is a path or a URL on `origin`; the Models URL of
/// Advanced must stay on the typed origin so the key cannot leave it.
///
/// # Errors
/// [`SniffError::InvalidInput`] with `models_url_origin` for another origin or a bad URL.
pub(super) fn same_origin_path(origin: &str, candidate: &str) -> Result<String, SniffError> {
    let candidate = candidate.trim();
    if candidate.starts_with('/') && !candidate.starts_with("//") {
        return Ok(candidate.split('#').next().unwrap_or_default().to_owned());
    }
    let url = Url::parse(candidate).map_err(|_| invalid("models_url_origin"))?;
    let theirs = normalize_base_url(&format!("{}://{}", url.scheme(), url.authority()))
        .map_err(|_| invalid("models_url_origin"))?;
    if theirs.origin != origin {
        return Err(invalid("models_url_origin"));
    }
    Ok(match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    })
}

#[cfg(test)]
mod tests;
