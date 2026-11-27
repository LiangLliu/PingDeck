//! Pure catalog grouping rules: which model each synced catalog row is an offer of.
//!
//! [`model_keys`] assigns every row its `model_key` (the rules are on that function);
//! `normalized` is the form model ids are compared by. Nothing here touches storage: the caller
//! reads the rows and writes the keys back.

use std::collections::{HashMap, HashSet};

/// The id a provider's model id is compared by: the part after the last `/`, without an
/// `@region` suffix or a Bedrock-style `-v1:0` / `:0` suffix, without leading region or vendor
/// segments before a `.` (`us.`, `global.`, `anthropic.`: letters and `-` only), lower-cased,
/// with `.` and `_` read as `-`.
#[must_use]
fn normalized(id: &str) -> String {
    let id = id.rsplit_once('/').map_or(id, |(_, tail)| tail);
    let id = id.split_once('@').map_or(id, |(head, _)| head);
    let id = match id.rsplit_once(':') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
            strip_bedrock_version(head)
        }
        _ => id,
    };
    let mut rest = id;
    while let Some((head, tail)) = rest.split_once('.') {
        if head.is_empty() || !head.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-') {
            break;
        }
        rest = tail;
    }
    rest.chars()
        .map(|c| match c {
            '.' | '_' => '-',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

/// `head` without a trailing `-v<digits>`.
fn strip_bedrock_version(head: &str) -> &str {
    match head.rsplit_once("-v") {
        Some((base, digits))
            if !base.is_empty()
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            base
        }
        _ => head,
    }
}

/// What [`model_keys`] reads of a synced catalog row.
#[derive(Debug, Clone, Copy)]
pub struct GroupingRow<'a> {
    /// The catalog family (models.dev provider id) that lists the row.
    pub family: &'a str,
    /// The id the family lists the model under.
    pub model_id: &'a str,
    /// models.dev's `canonical_model_id` (`<publisher>/<id>`), empty or malformed when unknown.
    pub canonical_id: &'a str,
}

/// The one value `keys` yields, if it yields exactly one distinct value.
fn single_distinct<'a>(mut keys: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let first = keys.next()?;
    keys.all(|key| key == first).then_some(first)
}

/// models.dev's canonical id of `offer` when it has the `<publisher>/<id>` shape.
fn canonical<'a>(offer: &GroupingRow<'a>) -> Option<&'a str> {
    Some(offer.canonical_id.trim()).filter(|id| {
        id.split_once('/')
            .is_some_and(|(p, m)| !p.is_empty() && !m.is_empty())
    })
}

/// The `model_key` of each synced row, in order. The rules:
/// 1. A row with models.dev's `canonical_model_id` (`<publisher>/<id>`) is an offer of that key.
/// 2. A row without one whose `normalized` id equals that of exactly one rule-1 key joins it.
/// 3. Otherwise a row of a publisher family (a family some canonical id starts with) is its own
///    publisher's model: key `<family>/<model_id>`.
/// 4. Any other row joins the one rule-1 or rule-3 key of the same normalized id, else it is a
///    model of its own: key `<family>/<model_id>`.
#[must_use]
pub fn model_keys(offers: &[GroupingRow<'_>]) -> Vec<String> {
    let publishers: HashSet<&str> = offers
        .iter()
        .filter_map(|offer| {
            canonical(offer)?
                .split_once('/')
                .map(|(publisher, _)| publisher)
        })
        .collect();
    let mut by_canonical: HashMap<String, HashSet<&str>> = HashMap::new();
    for key in offers.iter().filter_map(canonical) {
        if let Some((_, id)) = key.split_once('/') {
            by_canonical.entry(normalized(id)).or_default().insert(key);
        }
    }
    let mut keys: Vec<Option<String>> = vec![None; offers.len()];
    let mut own: HashMap<String, HashSet<String>> = HashMap::new();
    let mut later = Vec::new();
    for (index, offer) in offers.iter().enumerate() {
        if let Some(key) = canonical(offer) {
            keys[index] = Some(key.to_owned());
            continue;
        }
        let id = normalized(offer.model_id);
        if let Some(key) = single_distinct(by_canonical.get(&id).into_iter().flatten().copied()) {
            keys[index] = Some(key.to_owned());
        } else if publishers.contains(offer.family) {
            let key = format!("{}/{}", offer.family, offer.model_id);
            own.entry(id).or_default().insert(key.clone());
            keys[index] = Some(key);
        } else {
            later.push((index, id));
        }
    }
    for (index, id) in later {
        let found = by_canonical
            .get(&id)
            .into_iter()
            .flatten()
            .copied()
            .chain(own.get(&id).into_iter().flatten().map(String::as_str));
        let offer = &offers[index];
        keys[index] = Some(single_distinct(found).map_or_else(
            || format!("{}/{}", offer.family, offer.model_id),
            str::to_owned,
        ));
    }
    keys.into_iter().map(Option::unwrap_or_default).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_strips_vendor_region_and_version() {
        for (raw, want) in [
            ("claude-opus-4-5", "claude-opus-4-5"),
            ("anthropic/claude-opus-4.5", "claude-opus-4-5"),
            ("us.anthropic.claude-opus-4-5-v1:0", "claude-opus-4-5"),
            ("global.anthropic.claude-opus-4-5", "claude-opus-4-5"),
            ("claude-opus-4-6@eu", "claude-opus-4-6"),
            ("Qwen3.5_Plus", "qwen3-5-plus"),
            ("gpt-4.1", "gpt-4-1"),
            ("~anthropic/claude-opus-latest", "claude-opus-latest"),
            ("Ä.B_c", "Ä-b-c"),
        ] {
            assert_eq!(normalized(raw), want, "{raw}");
        }
    }

    #[test]
    fn offers_join_canonical_and_family_keys() {
        let offer = |family, model_id, canonical_id| GroupingRow {
            family,
            model_id,
            canonical_id,
        };
        assert_eq!(
            model_keys(&[
                offer("anthropic", "claude-opus-4-5", ""),
                offer(
                    "openrouter",
                    "anthropic/claude-opus-4.5",
                    "anthropic/claude-opus-4-5"
                ),
                offer("amazon-bedrock", "us.anthropic.claude-opus-4-5-v1:0", ""),
                offer("azure", "claude-opus-4-5", ""),
                offer("anthropic", "claude-haiku-9", ""),
                offer("azure", "claude-haiku-9", ""),
                offer("nano", "mystery-1", ""),
                offer("r1", "x-dup", "a/dup"),
                offer("r2", "y-dup", "b/dup"),
                offer("relay", "dup", ""),
            ]),
            [
                "anthropic/claude-opus-4-5",
                "anthropic/claude-opus-4-5",
                "anthropic/claude-opus-4-5",
                "anthropic/claude-opus-4-5",
                "anthropic/claude-haiku-9",
                "anthropic/claude-haiku-9",
                "nano/mystery-1",
                "a/dup",
                "b/dup",
                "relay/dup",
            ]
        );
    }
}
