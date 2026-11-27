//! Which reasoning parameters a model generation takes.
//!
//! Single responsibility: read a model id (any vendor prefix) and say which thinking request its
//! generation accepts. Claude before 4.6 takes only `{"type": "enabled", "budget_tokens"}`; 4.6
//! accepts both forms; 4.7 and later (and Fable, Mythos) reject a budget and take only
//! `adaptive`. Gemini 2.x takes `thinkingBudget`, Gemini 3 and later `thinkingLevel`. An id the
//! table does not know keeps the codec's own behaviour.

/// The thinking request a model generation takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Thinking {
    /// A token budget only (Claude before 4.6, Gemini 2.x).
    Budget,
    /// Adaptive or a level only: a budget is refused (Claude 4.7 and later, Gemini 3 and later).
    Adaptive,
    /// Either works (Claude 4.6), or the id is not in the table.
    Either,
}

/// Whether a lowercase id names a codename Claude (Fable, Mythos), which behaves as the newest
/// generation.
fn is_codename_generation(id: &str) -> bool {
    id.contains("claude-fable") || id.contains("claude-mythos")
}

/// The `(major, minor)` of a lowercase Claude id: `claude-sonnet-4-5`, `claude-3-7-sonnet-…`,
/// `anthropic.claude-opus-4-1-20250805-v1:0`, `claude-opus-4-20250514` (a date is minor 0).
fn claude_version(id: &str) -> Option<(u32, u32)> {
    let rest = &id[id.find("claude-")? + "claude-".len()..];
    let mut parts = rest.split(['-', '.', '@']);
    let first = parts.next()?;
    let (major, minor) = if first.chars().all(|ch| ch.is_ascii_digit()) {
        // Old naming: claude-3-7-sonnet.
        (first, parts.next())
    } else {
        (parts.next()?, parts.next())
    };
    let major = major.parse().ok()?;
    let minor = minor
        .filter(|text| text.len() < 3)
        .and_then(|text| text.parse().ok())
        .unwrap_or(0);
    Some((major, minor))
}

/// The thinking request a Claude model takes.
pub(crate) fn claude(model: &str) -> Thinking {
    let id = model.to_ascii_lowercase();
    if is_codename_generation(&id) {
        return Thinking::Adaptive;
    }
    match claude_version(&id) {
        Some(version) if version < (4, 6) => Thinking::Budget,
        Some(version) if version > (4, 6) => Thinking::Adaptive,
        _ => Thinking::Either,
    }
}

/// Whether a Claude model takes `temperature`, `top_p` and `top_k`: Claude 5 and later (and Fable,
/// Mythos) refuse them. Any other id does.
pub(crate) fn claude_takes_sampling(model: &str) -> bool {
    let id = model.to_ascii_lowercase();
    if is_codename_generation(&id) {
        return false;
    }
    claude_version(&id).is_none_or(|version| version < (5, 0))
}

/// The `(major, minor)` the Anthropic prefill guard reads from a model id; `None` when the id
/// names no Claude version. `(u32::MAX, 0)` is the sentinel for Mythos, the newest generation.
///
/// This is a second, looser parser than [`claude_version`], and the two disagree:
/// 1. `claude-fable-preview` gives `None` here (so the prefill guard is skipped even with
///    thinking on), while [`claude`] calls every `claude-fable` id `Adaptive`.
/// 2. It matches `claude` without the hyphen, so `claudette-7` is `(7, 0)` here but `Either` in
///    [`claude`].
/// 3. It skips non-numeric tokens: `claude-sonnet-beta-4` is `(4, 0)` here, but `claude_version`
///    gives `None` (`Either`).
/// 4. Any `mythos` after `claude` gives the sentinel (`claude-opus-4-5-mythos-tuned`), where
///    [`claude`] only knows `claude-mythos`; a major version of three or more digits is skipped
///    here but accepted by `claude_version`.
///
/// The two parsers are kept apart on purpose: merging them changes which models get the prefill
/// guard, so it needs an owner decision.
pub(crate) fn claude_prefill_generation(model: &str) -> Option<(u32, u32)> {
    let lower = model.to_ascii_lowercase();
    let rest = &lower[lower.find("claude")? + "claude".len()..];
    if rest.contains("mythos") {
        return Some((u32::MAX, 0));
    }
    let tokens: Vec<&str> = rest
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let version = |token: &str| {
        (token.len() <= 2)
            .then(|| token.parse::<u32>().ok())
            .flatten()
    };
    let at = tokens.iter().position(|t| version(t).is_some())?;
    let major = version(tokens[at])?;
    let minor = tokens.get(at + 1).and_then(|t| version(t)).unwrap_or(0);
    Some((major, minor))
}

/// The thinking request a Gemini model takes.
pub(crate) fn gemini(model: &str) -> Thinking {
    let id = model.to_ascii_lowercase();
    let Some(at) = id.find("gemini-") else {
        return Thinking::Either;
    };
    let major: Option<u32> = id[at + "gemini-".len()..]
        .split(['-', '.'])
        .next()
        .and_then(|text| text.parse().ok());
    match major {
        Some(major) if major < 3 => Thinking::Budget,
        Some(_) => Thinking::Adaptive,
        None => Thinking::Either,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generations_by_model_id() {
        for budget in [
            "claude-3-7-sonnet-20250219",
            "claude-sonnet-4-5",
            "claude-opus-4-20250514",
            "anthropic.claude-opus-4-1-20250805-v1:0",
            "claude-haiku-4-5@20251001",
        ] {
            assert_eq!(claude(budget), Thinking::Budget, "{budget}");
        }
        for adaptive in [
            "claude-opus-4-7",
            "claude-sonnet-5-5",
            "claude-fable-5-2",
            "anthropic/claude-opus-5",
        ] {
            assert_eq!(claude(adaptive), Thinking::Adaptive, "{adaptive}");
        }
        for either in ["claude-opus-4-6", "my-relay-alias"] {
            assert_eq!(claude(either), Thinking::Either, "{either}");
        }
        assert_eq!(gemini("gemini-2.5-pro"), Thinking::Budget);
        assert_eq!(gemini("models/gemini-3-pro-preview"), Thinking::Adaptive);
        assert_eq!(gemini("learnlm"), Thinking::Either);
    }

    #[test]
    fn claude_generation_and_claude_version_divergences() {
        // Pins the current divergence between the two parsers; change only with an owner decision.
        for (model, expected) in [
            ("claude-fable-preview", None),
            ("claudette-7", Some((7, 0))),
            ("claude-sonnet-beta-4", Some((4, 0))),
            ("claude-mythos-preview", Some((u32::MAX, 0))),
        ] {
            assert_eq!(claude_prefill_generation(model), expected, "{model}");
        }
        for (model, expected) in [
            ("claude-fable-preview", Thinking::Adaptive),
            ("claudette-7", Thinking::Either),
            ("claude-sonnet-beta-4", Thinking::Either),
        ] {
            assert_eq!(claude(model), expected, "{model}");
        }
        for (model, expected) in [
            ("claude-sonnet-5-5", false),
            ("claude-fable-x", false),
            ("claude-opus-4-7", true),
            ("gpt-6", true),
        ] {
            assert_eq!(claude_takes_sampling(model), expected, "{model}");
        }
    }
}
