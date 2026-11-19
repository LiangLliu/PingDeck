//! Regex evaluation for mock rules and flow extractions.
//!
//! Patterns run on the `regex` crate: a finite-automaton engine whose matching
//! time is linear in the text, so a hostile pattern such as `(a+)+$` cannot hang
//! a mock worker thread the way a backtracking engine would. Compilation is
//! bounded too (`size_limit`, `nest_limit`).
//!
//! The syntax is Rust's, which is close to but not the same as JavaScript's:
//!
//! - **Not supported** (the pattern fails to compile): look-around (`(?=…)`,
//!   `(?!…)`, `(?<=…)`, `(?<!…)`), backreferences (`\1`, `\k<name>`), and the
//!   JavaScript-only escapes `\cX` and `[^]`. A bare `{` that is not a
//!   quantifier must be escaped.
//! - **Differences**: `\d`, `\w`, `\s` and `\b` are Unicode-aware, and `.`
//!   matches `\r` (it excludes only `\n`).
//!
//! An unsupported, invalid or too-large pattern never panics and never matches
//! (an inverted mock rule therefore matches); nothing is written to stderr. Mock
//! servers therefore reject such rules up front with [`regex_check`], whose
//! message names the unsupported construct. Case folding is Unicode simple
//! folding.

use regex::{Regex, RegexBuilder};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

/// Largest compiled program a pattern may produce (1 MiB). A pattern such as
/// `(a{1000}){1000}` exceeds it and is treated as invalid.
const COMPILED_SIZE_LIMIT: usize = 1 << 20;

/// Deepest group nesting a pattern may have.
const NEST_LIMIT: u32 = 64;

/// The builder matching and checking share, so a pattern that passes
/// [`regex_check`] is exactly one that can match.
fn compile(pattern: &str, ignore_case: bool) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .size_limit(COMPILED_SIZE_LIMIT)
        .nest_limit(NEST_LIMIT)
        .build()
}

/// Most compiled patterns kept for [`first_capture`] and [`is_match`].
const CACHE_CAPACITY: usize = 64;

/// A cached compile result: `None` for a pattern that does not compile, so a bad pattern is not
/// recompiled on every call either.
type CachedRegex = Option<Arc<Regex>>;

/// Compiled patterns by `(pattern, ignore_case)`, least recently used first.
static CACHE: Mutex<VecDeque<(String, bool, CachedRegex)>> = Mutex::new(VecDeque::new());

/// The compiled `pattern`, from the bounded LRU cache when present. Compilation (up to
/// milliseconds) runs outside the lock; two threads that miss together compile twice and the
/// second insert wins, which is harmless.
fn compiled(pattern: &str, ignore_case: bool) -> CachedRegex {
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(index) = cache
        .iter()
        .position(|(cached, folded, _)| *folded == ignore_case && cached == pattern)
        && let Some(entry) = cache.remove(index)
    {
        let found = entry.2.clone();
        cache.push_back(entry);
        return found;
    }
    drop(cache);

    let regex = compile(pattern, ignore_case).ok().map(Arc::new);
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if cache.len() >= CACHE_CAPACITY {
        cache.pop_front();
    }
    cache.push_back((pattern.to_owned(), ignore_case, regex.clone()));
    regex
}

/// `None` when `pattern` compiles under the matching engine; otherwise a one-line
/// message naming the unsupported construct (look-around, backreference, `\cX`,
/// `[^]`), the engine's own syntax error, or that the pattern is too large.
#[must_use]
pub fn regex_check(pattern: &str) -> Option<String> {
    let error = match compile(pattern, false) {
        Ok(_) => return None,
        Err(error) => error,
    };
    if let Some(construct) = unsupported_construct(pattern) {
        return Some(format!(
            "regular expression not supported: {construct} — use a simpler pattern"
        ));
    }
    Some(match error {
        regex::Error::CompiledTooBig(_) => "regular expression is too large".to_owned(),
        other => {
            let text = other.to_string();
            let reason = text
                .lines()
                .rev()
                .find_map(|line| line.strip_prefix("error: "))
                .or_else(|| text.lines().next())
                .unwrap_or("invalid pattern");
            format!("invalid regular expression: {reason}")
        }
    })
}

/// The first JavaScript-only construct in `pattern`, skipping escaped characters and
/// the inside of character classes (where `(?=` and `\1` are literal or other errors).
fn unsupported_construct(pattern: &str) -> Option<&'static str> {
    let mut rest = pattern;
    let mut in_class = false;
    while let Some(current) = rest.chars().next() {
        if current == '\\' {
            let escaped = rest[1..].chars().next();
            if !in_class {
                match escaped {
                    Some('1'..='9') => return Some("backreference \\1…\\9"),
                    Some('k') if rest[2..].starts_with('<') => {
                        return Some("named backreference \\k<name>");
                    }
                    Some('c') => return Some("control escape \\cX"),
                    _ => {}
                }
            }
            rest = &rest[1 + escaped.map_or(0, char::len_utf8)..];
            continue;
        }
        if in_class {
            in_class = current != ']';
        } else if current == '[' {
            if rest.starts_with("[^]") {
                return Some("empty negated class [^] (use [\\s\\S])");
            }
            in_class = true;
            // A leading `]` (or `^]`) is a literal member of the class.
            let after = &rest[1..];
            let after = after.strip_prefix('^').unwrap_or(after);
            rest = &rest[rest.len() - after.strip_prefix(']').unwrap_or(after).len()..];
            continue;
        } else if current == '(' {
            let found = [
                ("(?=", "look-ahead (?=…)"),
                ("(?!", "negative look-ahead (?!…)"),
                ("(?<=", "look-behind (?<=…)"),
                ("(?<!", "negative look-behind (?<!…)"),
            ]
            .into_iter()
            .find(|(prefix, _)| rest.starts_with(prefix));
            if let Some((_, construct)) = found {
                return Some(construct);
            }
        }
        rest = &rest[current.len_utf8()..];
    }
    None
}

/// Returns the first capture group of the first match, the whole match when the
/// pattern defines no group (or group 1 did not take part), or `None` when it
/// does not match, is invalid or exceeds the compile limits.
#[must_use]
pub fn first_capture(pattern: &str, text: &str, ignore_case: bool) -> Option<String> {
    let captures = compiled(pattern, ignore_case)?.captures(text)?;
    captures
        .get(1)
        .or_else(|| captures.get(0))
        .map(|found| found.as_str().to_owned())
}

/// Returns whether `pattern` matches anywhere in `text`.
#[must_use]
pub fn is_match(pattern: &str, text: &str, ignore_case: bool) -> bool {
    first_capture(pattern, text, ignore_case).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_with_and_without_case_folding() {
        assert!(is_match("^ab+c$", "abbbc", false));
        assert!(!is_match("^ab+c$", "abcx", false));
        assert!(is_match("TOKEN", "a token here", true));
        assert!(!is_match("TOKEN", "a token here", false));
    }

    #[test]
    fn returns_capture_group_one_or_the_whole_match() {
        assert_eq!(
            first_capture(r#""token"\s*:\s*"([^"]+)""#, r#"{"token":"abc"}"#, false),
            Some("abc".to_owned())
        );
        assert_eq!(first_capture(r"\d+", "id=42", false), Some("42".to_owned()));
        assert_eq!(first_capture("missing", "abc", false), None);
        assert_eq!(first_capture("(", "abc", false), None, "invalid regex");
        assert_eq!(
            first_capture("a(b)?", "ac", false),
            Some("a".to_owned()),
            "a group that did not take part falls back to the whole match"
        );
        assert_eq!(
            first_capture(r"\/(\w+)", "x/path", false),
            Some("path".to_owned())
        );
    }

    #[test]
    fn catastrophic_patterns_finish_promptly() {
        let text = format!("{}!", "a".repeat(100_000));
        let started = std::time::Instant::now();
        assert!(!is_match("(a+)+$", &text, false));
        assert!(!is_match("(a|aa)+$", &text, true));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn cache_stays_bounded_and_keys_on_case_folding() {
        for index in 0..CACHE_CAPACITY * 2 {
            assert!(is_match(
                &format!("cached{index}"),
                &format!("cached{index}"),
                false
            ));
        }
        assert!(is_match("Folded", "folded", true));
        assert!(
            !is_match("Folded", "folded", false),
            "same pattern, other fold flag"
        );
        assert!(
            !is_match("(", "(", false),
            "a bad pattern stays bad when cached"
        );
        assert!(!is_match("(", "(", false));
        assert!(CACHE.lock().unwrap_or_else(PoisonError::into_inner).len() <= CACHE_CAPACITY);
    }

    #[test]
    fn unsupported_or_oversized_patterns_do_not_match() {
        assert!(!is_match("(?=a)a", "a", false), "look-ahead");
        assert!(!is_match(r"(a)\1", "aa", false), "backreference");
        assert!(
            !is_match("((a{1000}){1000}){1000}", "a", false),
            "too large"
        );
        assert!(
            !is_match(&"(".repeat(1000), "a", false),
            "too deeply nested"
        );
    }
}

#[cfg(test)]
mod check_tests {
    use super::regex_check;

    fn message(pattern: &str) -> String {
        regex_check(pattern).unwrap_or_default()
    }

    #[test]
    fn names_each_unsupported_construct() {
        for (pattern, construct) in [
            ("a(?=b)", "look-ahead (?=…)"),
            ("a(?!b)", "negative look-ahead (?!…)"),
            ("(?<=a)b", "look-behind (?<=…)"),
            ("(?<!a)b", "negative look-behind (?<!…)"),
            (r"(a)\1", "backreference"),
            (r"(?<x>a)\k<x>", "named backreference"),
            (r"\cJ", "control escape"),
            ("[^]", "[^]"),
        ] {
            let text = message(pattern);
            assert!(
                text.starts_with("regular expression not supported: ") && text.contains(construct),
                "{pattern}: {text}"
            );
            assert!(!text.contains('\n'), "{pattern}: one line");
        }
    }

    #[test]
    fn valid_and_escaped_patterns_pass() {
        for pattern in [
            r"^ab+c$",
            r"\(\?=x\)",
            r"\\\(\?=x\)",
            r"[(?=)]+",
            r"(?i)token",
            r"(?<id>\d+)",
            "",
        ] {
            assert_eq!(regex_check(pattern), None, "{pattern}");
        }
    }

    #[test]
    fn other_failures_report_the_engine_reason_or_size() {
        let unclosed = message("(");
        assert!(
            unclosed.starts_with("invalid regular expression: ") && !unclosed.contains('\n'),
            "{unclosed}"
        );
        assert_eq!(
            message("((a{1000}){1000}){1000}"),
            "regular expression is too large"
        );
        assert!(message(&"(".repeat(1000)).starts_with("invalid regular expression: "));
    }
}
