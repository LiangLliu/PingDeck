//! Which keys of a provider may call which model, over which protocols
//! (D-258).
//!
//! Single responsibility: answer "does this key serve this model" and "which protocols may this
//! key use for it" from what each key's own model list returned. Relays such as sub2api and
//! new-api bind a key to a group; the group decides the models, so a model only some keys list
//! is routed only to those keys, and the keys of one group form the candidates the provider's key
//! strategy rotates over.

use std::collections::{HashMap, HashSet};

use crate::llm::store::{KeyModel, ProviderKey, ProviderModel};
use crate::llm::wire::{Protocol, ProviderEndpoint};

/// What the keys' own lists returned, as routing needs it.
#[derive(Debug, Clone, Default)]
pub struct KeyScopes {
    /// Per key id: each listed model with the protocols the relay declared (`None`: none).
    by_key: HashMap<String, HashMap<String, Option<Vec<Protocol>>>>,
    /// Per provider id: every model some key's list returned (enabled keys or not), now or
    /// before (`listed_confirmed_at_ms`).
    listed: HashMap<String, HashSet<String>>,
    /// Per provider id: every model some key's current list returns.
    listed_now: HashMap<String, HashSet<String>>,
}

impl KeyScopes {
    /// The scopes of `rows` (every listed model of every key), `key_provider` naming each key's
    /// provider.
    #[must_use]
    pub fn new(rows: Vec<KeyModel>, key_provider: &HashMap<String, String>) -> Self {
        let mut scopes = Self::default();
        for row in rows {
            if let Some(provider_id) = key_provider.get(&row.key_id) {
                scopes
                    .listed
                    .entry(provider_id.clone())
                    .or_default()
                    .insert(row.model_id.clone());
                scopes
                    .listed_now
                    .entry(provider_id.clone())
                    .or_default()
                    .insert(row.model_id.clone());
            }
            scopes
                .by_key
                .entry(row.key_id)
                .or_default()
                .insert(row.model_id, row.protocols);
        }
        scopes
    }

    /// These scopes plus the stored models some key's list once named: a key whose list was
    /// replaced (and is not read again yet) leaves them listed, so the keys of other groups do
    /// not start calling them.
    #[must_use]
    pub fn with_confirmed(mut self, models: &[ProviderModel]) -> Self {
        for model in models
            .iter()
            .filter(|model| model.listed_confirmed_at_ms.is_some())
        {
            self.listed
                .entry(model.provider_id.clone())
                .or_default()
                .insert(model.model_id.clone());
        }
        self
    }

    /// Whether `key` may call `model`: it lists the model, or no key of the provider ever
    /// listed it (a hand-added or preset model). A key whose own list was not read yet serves
    /// only the models no key's current list returns: a model another key lists belongs to that
    /// key's group until the new key's read says otherwise (the read is queued when the key is
    /// added, and the key row says it is not read).
    #[must_use]
    pub fn serves(&self, key: &ProviderKey, model: &str) -> bool {
        if key.models_listed_at_ms.is_none() {
            return !self
                .listed_now
                .get(&key.provider_id)
                .is_some_and(|models| models.contains(model));
        }
        self.by_key
            .get(&key.id)
            .is_some_and(|models| models.contains_key(model))
            || !self
                .listed
                .get(&key.provider_id)
                .is_some_and(|models| models.contains(model))
    }

    /// The ids of the keys among `keys` that list `model`, or `None` when every key may call it
    /// (the `key_ids` of a model row).
    #[must_use]
    pub fn listing_keys<'a>(
        &self,
        keys: impl IntoIterator<Item = &'a ProviderKey>,
        model: &str,
    ) -> Option<Vec<String>> {
        let keys: Vec<&ProviderKey> = keys.into_iter().collect();
        let serving: Vec<String> = keys
            .iter()
            .filter(|key| self.serves(key, model))
            .map(|key| key.id.clone())
            .collect();
        (serving.len() < keys.len()).then_some(serving)
    }

    /// The protocols the relay declared for `key` calling `model`, when it declared some.
    fn declared(&self, key_id: &str, model: &str) -> Option<&[Protocol]> {
        self.by_key
            .get(key_id)
            .and_then(|models| models.get(model))
            .and_then(Option::as_deref)
    }

    /// Whether `key` may use `protocol` for `model`: the user's limit and the relay's
    /// declaration both allow it (each only when set).
    #[must_use]
    pub fn allows(&self, key: &ProviderKey, model: &str, protocol: Protocol) -> bool {
        key.protocols
            .as_deref()
            .is_none_or(|limit| limit.is_empty() || limit.contains(&protocol))
            && self
                .declared(&key.id, model)
                .is_none_or(|declared| declared.contains(&protocol))
    }

    /// `endpoints` without the ones `key` may not use for `model`.
    #[must_use]
    pub fn endpoints_for(
        &self,
        endpoints: &[ProviderEndpoint],
        key: &ProviderKey,
        model: &str,
    ) -> Vec<ProviderEndpoint> {
        endpoints
            .iter()
            .filter(|endpoint| self.allows(key, model, endpoint.protocol))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str, listed: bool) -> ProviderKey {
        ProviderKey {
            id: id.into(),
            provider_id: "p".into(),
            models_listed_at_ms: listed.then_some(1),
            ..ProviderKey::default()
        }
    }

    fn row(key_id: &str, model: &str, protocols: Option<Vec<Protocol>>) -> KeyModel {
        KeyModel {
            key_id: key_id.into(),
            model_id: model.into(),
            protocols,
        }
    }

    fn scopes() -> KeyScopes {
        let owners: HashMap<String, String> = ["a", "b"]
            .into_iter()
            .map(|id| (id.to_owned(), "p".to_owned()))
            .collect();
        KeyScopes::new(
            vec![
                row("a", "gpt-5", Some(vec![Protocol::OpenaiResponses])),
                row("b", "claude", None),
            ],
            &owners,
        )
    }

    #[test]
    fn a_model_only_one_key_lists_is_served_by_that_key_alone() {
        let s = scopes();
        let (a, b, fresh) = (key("a", true), key("b", true), key("c", false));
        assert!(s.serves(&b, "claude"));
        assert!(!s.serves(&a, "claude"));
        assert!(
            !s.serves(&fresh, "claude"),
            "a key not read yet does not take another group's model"
        );
        assert!(
            s.serves(&fresh, "manual-model"),
            "a model no key lists stays open to it"
        );
        assert!(s.serves(&a, "manual-model") && s.serves(&b, "manual-model"));
        assert_eq!(
            s.listing_keys([&a, &b, &fresh], "claude"),
            Some(vec!["b".to_owned()])
        );
        assert_eq!(s.listing_keys([&a, &b], "manual-model"), None);
    }

    #[test]
    fn a_once_listed_model_stays_with_its_group_after_the_listing_key_is_replaced() {
        // Key `a` listed `gpt-5` and then had its secret replaced (its rows are gone).
        let owners: HashMap<String, String> = [("a".to_owned(), "p".to_owned())].into();
        let confirmed = ProviderModel {
            provider_id: "p".into(),
            model_id: "gpt-5".into(),
            listed_confirmed_at_ms: Some(1),
            ..ProviderModel::default()
        };
        let s =
            KeyScopes::new(vec![row("b", "claude", None)], &owners).with_confirmed(&[confirmed]);
        let (a, b) = (key("a", false), key("b", true));
        assert!(
            s.serves(&a, "gpt-5"),
            "the replaced key is read again first"
        );
        assert!(
            !s.serves(&b, "gpt-5"),
            "another group's key does not take it over"
        );
        assert!(s.serves(&b, "manual-model"));
    }

    #[test]
    fn declared_and_user_limits_both_narrow_the_protocols() {
        let s = scopes();
        let a = key("a", true);
        assert!(s.allows(&a, "gpt-5", Protocol::OpenaiResponses));
        assert!(!s.allows(&a, "gpt-5", Protocol::OpenaiChat));
        let mut b = key("b", true);
        assert!(s.allows(&b, "claude", Protocol::OpenaiChat));
        b.protocols = Some(vec![Protocol::AnthropicMessages]);
        assert!(!s.allows(&b, "claude", Protocol::OpenaiChat));
        assert!(s.allows(&b, "claude", Protocol::AnthropicMessages));
    }
}
