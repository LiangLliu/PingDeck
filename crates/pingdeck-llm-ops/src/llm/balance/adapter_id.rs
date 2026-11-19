//! The stable ids of the balance adapters: one string per adapter that the known-host table, the
//! provider row and the report provenance store, and its parse back into the enum.

use serde::{Deserialize, Serialize};

pub(super) use pingdeck_llm_domain::llm::balance_state::Evidence;

/// One balance adapter. Eight are code, four are declarative entries of
/// `fixtures/llm-balance-probes.json`; [`AdapterId::Custom`] is the user's own query and is never
/// named by the known-host table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AdapterId {
    /// DeepSeek `/user/balance` (code).
    #[serde(rename = "deepseek")]
    DeepSeek,
    /// OpenRouter `/api/v1/credits` with the `/api/v1/key` fallback (code).
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// new-api, Veloera and RixAPI (code).
    #[serde(rename = "new_api")]
    NewApi,
    /// one-api billing facade (code).
    #[serde(rename = "one_api")]
    OneApi,
    /// done-hub: new-api structure with "quota is remaining" semantics (code).
    #[serde(rename = "done_hub")]
    DoneHub,
    /// Sub2API `/v1/usage` (code).
    #[serde(rename = "sub2api")]
    Sub2Api,
    /// LiteLLM proxy `/key/info` and `/v2/user/info` (code).
    #[serde(rename = "litellm")]
    LiteLlm,
    /// VoAPI v2 console `/api/user/info` (code, needs a console token).
    #[serde(rename = "voapi_v2")]
    VoApiV2,
    /// Moonshot (declarative).
    #[serde(rename = "moonshot")]
    Moonshot,
    /// SiliconFlow (declarative).
    #[serde(rename = "siliconflow")]
    SiliconFlow,
    /// StepFun (declarative).
    #[serde(rename = "stepfun")]
    StepFun,
    /// Zhipu and Z.ai (declarative).
    #[serde(rename = "zhipu")]
    Zhipu,
    /// The saved custom query of the provider; wins over every other adapter.
    #[serde(rename = "custom")]
    Custom,
}

impl AdapterId {
    /// The twelve adapters the known-host table may name (everything except `Custom`).
    pub const ALL: [Self; 12] = [
        Self::DeepSeek,
        Self::OpenRouter,
        Self::NewApi,
        Self::OneApi,
        Self::DoneHub,
        Self::Sub2Api,
        Self::LiteLlm,
        Self::VoApiV2,
        Self::Moonshot,
        Self::SiliconFlow,
        Self::StepFun,
        Self::Zhipu,
    ];

    /// The stable id string (`deepseek`, `new_api`, `voapi_v2`, …).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
            Self::NewApi => "new_api",
            Self::OneApi => "one_api",
            Self::DoneHub => "done_hub",
            Self::Sub2Api => "sub2api",
            Self::LiteLlm => "litellm",
            Self::VoApiV2 => "voapi_v2",
            Self::Moonshot => "moonshot",
            Self::SiliconFlow => "siliconflow",
            Self::StepFun => "stepfun",
            Self::Zhipu => "zhipu",
            Self::Custom => "custom",
        }
    }

    /// The adapter named by `id`; `None` for an unknown id and for `custom` (a custom query is
    /// attached to a provider, never named by a table).
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|adapter| adapter.as_str() == id)
    }

    /// The evidence grade of the adapter's endpoint; `None` for a custom query (the user's own).
    /// Declarative adapters read the grade from their fixture entry.
    #[must_use]
    pub fn evidence(self) -> Option<Evidence> {
        match self {
            Self::DeepSeek => Some(Evidence::Documented),
            Self::OpenRouter
            | Self::NewApi
            | Self::OneApi
            | Self::DoneHub
            | Self::Sub2Api
            | Self::LiteLlm => Some(Evidence::UpstreamSource),
            Self::VoApiV2 => Some(Evidence::ThirdPartySource),
            Self::Moonshot | Self::SiliconFlow | Self::StepFun | Self::Zhipu => {
                super::declarative::entry(self).map(|entry| entry.evidence)
            }
            Self::Custom => None,
        }
    }

    /// The date (`YYYY-MM-DD`) the adapter's endpoint was last checked against its source;
    /// `None` is `unverified` (and for a custom query).
    #[must_use]
    pub fn verified_on(self) -> Option<&'static str> {
        match self {
            Self::DeepSeek
            | Self::OpenRouter
            | Self::NewApi
            | Self::OneApi
            | Self::DoneHub
            | Self::Sub2Api
            | Self::LiteLlm => Some("2026-11-07"),
            Self::VoApiV2 | Self::Custom => None,
            Self::Moonshot | Self::SiliconFlow | Self::StepFun | Self::Zhipu => {
                super::declarative::entry(self).and_then(|entry| entry.verified_date())
            }
        }
    }

    /// Whether the adapter is a declarative entry of the probe fixture.
    #[must_use]
    pub const fn is_declarative(self) -> bool {
        matches!(
            self,
            Self::Moonshot | Self::SiliconFlow | Self::StepFun | Self::Zhipu
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::balance::tests::is_iso_date;

    #[test]
    fn every_id_round_trips_and_custom_is_not_parseable() {
        for adapter in AdapterId::ALL {
            assert_eq!(AdapterId::parse(adapter.as_str()), Some(adapter));
            let json = serde_json::to_string(&adapter).unwrap_or_default();
            assert_eq!(json, format!("\"{}\"", adapter.as_str()));
        }
        assert_eq!(AdapterId::parse("custom"), None);
        assert_eq!(AdapterId::parse("deepseek_balance"), None);
        assert_eq!(AdapterId::Custom.as_str(), "custom");
        assert_eq!(AdapterId::Custom.evidence(), None);
        assert_eq!(
            AdapterId::ALL
                .iter()
                .filter(|id| id.is_declarative())
                .count(),
            4
        );
    }

    #[test]
    fn every_adapter_carries_an_evidence_grade_and_a_verified_on_date_or_unverified() {
        for adapter in AdapterId::ALL {
            assert!(adapter.evidence().is_some(), "{adapter:?}");
            if let Some(date) = adapter.verified_on() {
                assert!(is_iso_date(date), "{adapter:?}: {date}");
            }
        }
        assert_eq!(AdapterId::VoApiV2.verified_on(), None);
        assert_eq!(
            AdapterId::Zhipu.evidence(),
            Some(Evidence::ThirdPartySource)
        );
        assert_eq!(AdapterId::StepFun.verified_on(), None);
    }
}
