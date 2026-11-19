//! "Changes since the last scan" of a re-sniffed provider: a fresh [`SniffReport`] against the
//! saved provider's protocols, models, identity and balance adapter. Pure; the use-case layer
//! applies the result (new models saved disabled, vanished ones flagged, never deleted).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::types::{RouteVerdict, SniffReport};
use crate::llm::wire::{GatewayFamily, Protocol};

/// What the saved provider holds, as far as a scan can differ from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProviderView {
    /// Stored gateway family.
    pub family: GatewayFamily,
    /// Stored brand name.
    pub brand_name: Option<String>,
    /// Stored balance adapter id.
    pub balance_adapter: Option<String>,
    /// Protocols of the stored endpoints.
    pub protocols: Vec<Protocol>,
    /// Ids of the stored models.
    pub model_ids: Vec<String>,
}

/// A change of family or brand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityChange {
    /// Stored family.
    pub from_family: GatewayFamily,
    /// Scanned family.
    pub to_family: GatewayFamily,
    /// Stored brand.
    pub from_brand: Option<String>,
    /// Scanned brand.
    pub to_brand: Option<String>,
}

/// A change of the balance adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterChange {
    /// Stored adapter id.
    pub from: Option<String>,
    /// Scanned adapter id.
    pub to: Option<String>,
}

/// The changes since the last scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SniffDiff {
    /// Protocols whose route exists now and is not stored.
    pub protocols_added: Vec<Protocol>,
    /// Stored protocols whose route was probed and is gone.
    pub protocols_removed: Vec<Protocol>,
    /// Listed models that are not stored (to save disabled).
    pub models_added: Vec<String>,
    /// Stored models the provider no longer lists (to flag, never to delete).
    pub models_removed: Vec<String>,
    /// Family or brand change.
    pub identity: Option<IdentityChange>,
    /// Balance adapter change.
    pub balance_adapter: Option<AdapterChange>,
}

impl SniffDiff {
    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Compares `report` with `stored`. A model list that was not read (no models) never reports
/// removals, and a protocol that was not probed or only unclear is never reported removed.
#[must_use]
pub(super) fn diff_against_provider(
    report: &SniffReport,
    stored: &StoredProviderView,
) -> SniffDiff {
    let stored_protocols: HashSet<Protocol> = stored.protocols.iter().copied().collect();
    let protocols_added = report
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.supported && !stored_protocols.contains(&endpoint.protocol))
        .map(|endpoint| endpoint.protocol)
        .collect();
    let protocols_removed = stored
        .protocols
        .iter()
        .copied()
        .filter(|protocol| {
            report.endpoints.iter().any(|endpoint| {
                endpoint.protocol == *protocol && endpoint.route == RouteVerdict::Absent
            })
        })
        .collect();
    let listed: HashSet<&str> = report
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    let stored_models: HashSet<&str> = stored.model_ids.iter().map(String::as_str).collect();
    let models_added = report
        .models
        .iter()
        .filter(|model| !stored_models.contains(model.id.as_str()))
        .map(|model| model.id.clone())
        .collect();
    let models_removed = if report.models.is_empty() {
        Vec::new()
    } else {
        stored
            .model_ids
            .iter()
            .filter(|id| !listed.contains(id.as_str()))
            .cloned()
            .collect()
    };
    // A scan report always has a brand (assembly falls back to the host); the `is_some` guard is
    // for reports without one, such as test fixtures.
    let identity_changed = report.family != stored.family
        || (report.brand_name.is_some() && report.brand_name != stored.brand_name);
    SniffDiff {
        protocols_added,
        protocols_removed,
        models_added,
        models_removed,
        identity: identity_changed.then(|| IdentityChange {
            from_family: stored.family,
            to_family: report.family,
            from_brand: stored.brand_name.clone(),
            to_brand: report.brand_name.clone(),
        }),
        balance_adapter: (report.balance_adapter != stored.balance_adapter).then(|| {
            AdapterChange {
                from: stored.balance_adapter.clone(),
                to: report.balance_adapter.clone(),
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::sniff::test_support::{blank_report, endpoint, model};

    fn stored() -> StoredProviderView {
        StoredProviderView {
            family: GatewayFamily::NewApi,
            brand_name: Some("Acme Relay".into()),
            balance_adapter: Some("new_api".into()),
            protocols: vec![Protocol::OpenaiChat, Protocol::AnthropicMessages],
            model_ids: vec!["a".into(), "b".into(), "c".into()],
        }
    }

    fn fresh() -> SniffReport {
        let mut report = blank_report("r");
        report.family = GatewayFamily::NewApi;
        report.brand_name = Some("Acme Relay".into());
        report.balance_adapter = Some("new_api".into());
        report.endpoints = vec![
            endpoint(Protocol::OpenaiChat, RouteVerdict::Exists),
            endpoint(Protocol::AnthropicMessages, RouteVerdict::Absent),
            endpoint(Protocol::Gemini, RouteVerdict::Exists),
            endpoint(Protocol::Embeddings, RouteVerdict::Unclear),
        ];
        report.models = ["a", "c", "d", "e", "f"].map(model).to_vec();
        report
    }

    #[test]
    fn three_added_one_removed_and_nothing_else() {
        let diff = diff_against_provider(&fresh(), &stored());
        assert_eq!(diff.models_added, ["d", "e", "f"]);
        assert_eq!(diff.models_removed, ["b"]);
        assert_eq!(diff.protocols_added, [Protocol::Gemini]);
        assert_eq!(diff.protocols_removed, [Protocol::AnthropicMessages]);
        assert!(diff.identity.is_none() && diff.balance_adapter.is_none());
        assert!(!diff.is_empty());
    }

    #[test]
    fn identity_and_adapter_changes_and_the_unread_list() {
        let mut report = fresh();
        report.family = GatewayFamily::DoneHub;
        report.balance_adapter = Some("done_hub".into());
        report.models.clear();
        let diff = diff_against_provider(&report, &stored());
        assert!(
            diff.models_removed.is_empty(),
            "an unread list removes nothing"
        );
        assert_eq!(
            diff.identity.map(|c| c.to_family),
            Some(GatewayFamily::DoneHub)
        );
        assert_eq!(
            diff.balance_adapter.and_then(|c| c.to),
            Some("done_hub".to_owned())
        );
        let same = {
            let mut report = fresh();
            report.endpoints.truncate(1);
            report.models = ["a", "b", "c"].map(model).to_vec();
            diff_against_provider(&report, &stored())
        };
        assert!(same.is_empty());
    }

    #[test]
    fn a_stored_protocol_with_an_unclear_route_is_not_removed() {
        let mut report = fresh();
        report.endpoints[1] = endpoint(Protocol::AnthropicMessages, RouteVerdict::Unclear);
        let diff = diff_against_provider(&report, &stored());
        assert!(diff.protocols_removed.is_empty());
    }
}
