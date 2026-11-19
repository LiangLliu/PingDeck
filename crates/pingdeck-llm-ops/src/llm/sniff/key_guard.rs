//! The check before the first keyed request: whether the typed host looks like a typo of a
//! known provider host (edit distance 1) or the key's shape belongs to another vendor than the
//! host. Either stops the scan before any request that carries the key, until the user confirms
//! "Send key anyway". Pure: no I/O.

use super::url::NormalizedUrl;
use crate::llm::presets::known_hostnames;

/// Key prefixes of well-known vendors and the host suffix their keys belong to.
const VENDOR_KEYS: [(&str, &str); 6] = [
    ("sk-ant-", "anthropic.com"),
    ("sk-or-", "openrouter.ai"),
    ("sk-proj-", "openai.com"),
    ("gsk_", "groq.com"),
    ("xai-", "x.ai"),
    ("AIza", "googleapis.com"),
];

/// Hosts shorter than this are not compared (every short name is one edit from another).
const MIN_LOOKALIKE_CHARS: usize = 8;

/// Why the key must not be sent yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyConcern {
    /// The host is one edit away from a known provider host.
    LookalikeHost,
    /// The key shape belongs to a known vendor and the host is another.
    VendorKeyOnOtherHost,
}

fn host_under(host: &str, suffix: &str) -> bool {
    host == suffix
        || host
            .strip_suffix(suffix)
            .is_some_and(|rest| rest.ends_with('.'))
}

/// Whether `left` and `right` differ by exactly one insertion, deletion or substitution.
fn one_edit_apart(left: &str, right: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (left.chars().collect(), right.chars().collect());
    if a == b || a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (rest_a, rest_b) = (&a[prefix..], &b[prefix..]);
    match rest_a.len().cmp(&rest_b.len()) {
        std::cmp::Ordering::Equal => rest_a[1..] == rest_b[1..],
        std::cmp::Ordering::Greater => rest_a[1..] == *rest_b,
        std::cmp::Ordering::Less => *rest_a == rest_b[1..],
    }
}

/// The concern about sending `key` to `url`, if any. Local hosts never raise one: a key typed
/// for a local proxy stays on the user's network. A fully qualified host's trailing dot is
/// ignored (`api.anthropic.com.` is `api.anthropic.com`).
pub(super) fn concern(url: &NormalizedUrl, key: &str) -> Option<KeyConcern> {
    if url.host_class.is_local() {
        return None;
    }
    let host = url.host.trim_end_matches('.');
    let vendor = VENDOR_KEYS
        .iter()
        .find(|(prefix, _)| key.starts_with(prefix))
        .map(|(_, suffix)| *suffix);
    if vendor.is_some_and(|suffix| !host_under(host, suffix)) {
        return Some(KeyConcern::VendorKeyOnOtherHost);
    }
    if url.known_host.is_none()
        && host.chars().count() >= MIN_LOOKALIKE_CHARS
        && known_hostnames().any(|known| one_edit_apart(host, known))
    {
        return Some(KeyConcern::LookalikeHost);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::sniff::url::normalize_base_url;

    fn concern_of(url: &str, key: &str) -> Option<KeyConcern> {
        concern(&normalize_base_url(url).unwrap(), key)
    }

    #[test]
    fn lookalike_hosts_and_foreign_key_shapes_raise_a_concern() {
        for host in [
            "api.deepseek.co",
            "api.anthropic.cm",
            "api.deepsek.com",
            "api.openai.con",
        ] {
            assert_eq!(
                concern_of(host, "sk-neutral-123456"),
                Some(KeyConcern::LookalikeHost),
                "{host}"
            );
        }
        assert_eq!(
            concern_of("relay.example.com", "sk-ant-api03-abcdefghijkl"),
            Some(KeyConcern::VendorKeyOnOtherHost)
        );
        assert_eq!(
            concern_of("api.deepseek.com", "sk-ant-api03-abcdefghijkl"),
            Some(KeyConcern::VendorKeyOnOtherHost)
        );
    }

    #[test]
    fn real_hosts_neutral_keys_and_local_hosts_pass() {
        assert_eq!(concern_of("api.deepseek.com", "sk-neutral-123456"), None);
        assert_eq!(
            concern_of("api.anthropic.com", "sk-ant-api03-abcdefghijkl"),
            None
        );
        assert_eq!(
            concern_of("generativelanguage.googleapis.com", "AIzaSyA1234567890"),
            None
        );
        assert_eq!(concern_of("relay.example.com", "sk-neutral-123456"), None);
        assert_eq!(
            concern_of("localhost:8080", "sk-ant-api03-abcdefghijkl"),
            None
        );
        assert_eq!(
            concern_of("api.x.test", "sk-neutral-123456"),
            None,
            "short names are not compared"
        );
    }

    #[test]
    fn a_trailing_dot_names_the_same_host() {
        // Regression guard (both used to raise a concern).
        assert_eq!(
            concern_of("https://api.anthropic.com.", "sk-ant-api03-abcdefghijkl"),
            None
        );
        assert_eq!(
            concern_of("https://api.deepseek.com.", "sk-neutral-123456"),
            None
        );
        assert_eq!(
            concern_of("https://relay.example.com.", "sk-ant-api03-abcdefghijkl"),
            Some(KeyConcern::VendorKeyOnOtherHost)
        );
        assert_eq!(
            concern_of("https://api.deepseek.co.", "sk-neutral-123456"),
            Some(KeyConcern::LookalikeHost)
        );
    }

    #[test]
    fn edit_distance_of_exactly_one() {
        assert!(one_edit_apart("api.deepseek.co", "api.deepseek.com"));
        assert!(one_edit_apart("abc", "abd"));
        assert!(one_edit_apart("abc", "ac"));
        assert!(!one_edit_apart("abc", "abc"));
        assert!(!one_edit_apart("abc", "xyc1"));
        assert!(!one_edit_apart("abcd", "abdc"));
    }
}
