//! Declarative balance adapters from `fixtures/llm-balance-probes.json` (Moonshot, SiliconFlow,
//! StepFun, Zhipu/Z.ai): the embedded catalog (unknown fields fail the parse and the drift test),
//! the match by preset id, host suffix or family, and the run against the provider's own origin.

use std::{collections::BTreeMap, sync::LazyLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::wire::GatewayFamily;

use super::adapter_id::Evidence;
use super::error::{BalanceError, BalanceErrorKind};
use super::fetch::{BalanceAuth, Session};
use super::report::seal;
use super::rules::{ExtractRules, UnitName, host_matches};
use super::target::{host_of, require_key};
use super::{AdapterId, BalanceReport, BalanceTarget, BalanceUnit};

/// The `verified_on` of an entry whose endpoint was never checked.
const UNVERIFIED: &str = "unverified";

const PROBES_JSON: &str = include_str!("../../../../../fixtures/llm-balance-probes.json");

static CATALOG: LazyLock<Result<ProbeCatalog, String>> =
    LazyLock::new(|| serde_json::from_str(PROBES_JSON).map_err(|error| error.to_string()));

/// The whole probe file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProbeCatalog {
    /// Format version of the file.
    pub version: u32,
    /// One entry per declarative adapter.
    pub adapters: Vec<ProbeEntry>,
}

/// Which providers an entry serves.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeMatch {
    /// Preset ids.
    #[serde(default)]
    pub preset_ids: Vec<String>,
    /// Host suffixes of the provider origin.
    #[serde(default)]
    pub host_suffixes: Vec<String>,
    /// Detected gateway families.
    #[serde(default)]
    pub families: Vec<GatewayFamily>,
}

/// The only method of this release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeMethod {
    /// `GET`.
    #[serde(rename = "GET")]
    Get,
}

/// An extra request header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeHeader {
    /// Header name.
    pub name: String,
    /// Header value (never a credential: the key travels through `auth`).
    pub value: String,
}

/// The request of an entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRequest {
    /// Method.
    pub method: ProbeMethod,
    /// Path on the provider's origin.
    pub path: String,
    /// How the key travels.
    pub auth: BalanceAuth,
    /// Extra headers.
    #[serde(default)]
    pub headers: Vec<ProbeHeader>,
}

/// A recorded answer the drift test replays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedBody {
    /// Host the body was recorded from.
    pub host: String,
    /// The body.
    pub body: Value,
    /// Expected `remaining`.
    pub expect_remaining: f64,
    /// Expected unit.
    pub expect_unit: BalanceUnit,
}

/// One declarative adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProbeEntry {
    /// Adapter id.
    pub id: AdapterId,
    /// How well the endpoint is established; drives the evidence tooltip.
    pub evidence: Evidence,
    /// Date (`YYYY-MM-DD`) the endpoint and body shape were last checked, or `unverified`.
    pub verified_on: String,
    /// Providers served.
    #[serde(rename = "match")]
    pub matches: ProbeMatch,
    /// The request.
    pub request: ProbeRequest,
    /// Where the numbers are.
    pub extract: ExtractRules,
    /// Units inferred but not verified on a live key. They are never applied: a host that is
    /// only listed here reads as `Unit unknown` with the raw number. After
    /// the check on a live key a unit moves into `extract.unit.by_host`.
    #[serde(default)]
    pub unverified_units: BTreeMap<String, UnitName>,
    /// Caveats for maintainers.
    pub notes: String,
    /// The recorded body for the drift test.
    pub recorded: RecordedBody,
}

impl ProbeEntry {
    /// The `verified_on` date; `None` when the entry says `unverified`.
    #[must_use]
    pub fn verified_date(&self) -> Option<&str> {
        (self.verified_on != UNVERIFIED).then_some(self.verified_on.as_str())
    }

    /// Whether the entry serves a provider with this preset, origin host or family.
    fn serves(&self, preset_id: Option<&str>, host: Option<&str>, family: GatewayFamily) -> bool {
        preset_id.is_some_and(|id| {
            self.matches
                .preset_ids
                .iter()
                .any(|candidate| candidate == id)
        }) || host.is_some_and(|host| {
            self.matches
                .host_suffixes
                .iter()
                .any(|suffix| host_matches(host, suffix))
        }) || self.matches.families.contains(&family)
    }
}

/// The parsed catalog.
///
/// # Errors
/// The parse error when the embedded file is malformed (caught by the drift test).
pub(super) fn probe_catalog() -> Result<&'static ProbeCatalog, BalanceError> {
    CATALOG.as_ref().map_err(|message| {
        BalanceError::new(
            BalanceErrorKind::SchemaMismatch,
            format!("The bundled balance probe file is invalid: {message}"),
        )
    })
}

/// The entry named `id`.
pub(super) fn entry(id: AdapterId) -> Option<&'static ProbeEntry> {
    probe_catalog()
        .ok()?
        .adapters
        .iter()
        .find(|entry| entry.id == id)
}

/// The adapter of the first entry serving a provider with this preset, host (a known vendor host
/// without a preset) or family.
pub(super) fn select(
    preset_id: Option<&str>,
    host: Option<&str>,
    family: GatewayFamily,
) -> Option<AdapterId> {
    probe_catalog()
        .ok()?
        .adapters
        .iter()
        .find(|entry| entry.serves(preset_id, host, family))
        .map(|entry| entry.id)
}

/// Runs the entry `id` against the target's own origin.
///
/// # Errors
/// The failure classes of [`Session::get`], and a schema mismatch (with the vendor message,
/// redacted) when the envelope or the paths do not match.
pub(super) async fn query(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    id: AdapterId,
    now_ms: i64,
) -> Result<BalanceReport, BalanceError> {
    let entry = entry(id).ok_or_else(|| {
        BalanceError::new(
            BalanceErrorKind::SchemaMismatch,
            format!("No probe entry for {}", id.as_str()),
        )
    })?;
    let request = &entry.request;
    let mut headers = Vec::new();
    if let Some(header) = request.auth.header(require_key(target, request.auth)?) {
        headers.push(header);
    }
    headers.extend(
        request
            .headers
            .iter()
            .map(|header| (header.name.clone(), header.value.clone())),
    );
    let url = session.url(&request.path);
    let fetched = session.get(&url, headers).await?;
    let host = host_of(session.origin()).unwrap_or_default();
    let mut report = entry
        .extract
        .evaluate(&fetched.text, &host, target.quota_per_unit)
        .map_err(|message| BalanceError::schema(session.safe_text(&message), &request.path))?;
    report.source_endpoint = Some(session.endpoint_label(&url));
    Ok(seal(report, &fetched.json, session.secrets(), now_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::balance::BalanceUnit;
    use crate::llm::balance::rules::{UnitName, UnitSpec};
    use crate::llm::balance::tests::is_iso_date;
    use crate::llm::presets::presets;

    #[test]
    fn the_file_parses_and_has_the_four_declarative_adapters() {
        let catalog = probe_catalog().unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(catalog.version, 1);
        let ids: Vec<AdapterId> = catalog.adapters.iter().map(|entry| entry.id).collect();
        assert_eq!(
            ids,
            [
                AdapterId::Moonshot,
                AdapterId::SiliconFlow,
                AdapterId::StepFun,
                AdapterId::Zhipu
            ]
        );
        assert!(ids.iter().all(|id| id.is_declarative()));
    }

    #[test]
    fn entries_are_well_formed_and_agree_with_the_preset_catalog() {
        let catalog = probe_catalog().unwrap_or_else(|error| panic!("{error:?}"));
        let presets = presets().unwrap_or_else(|error| panic!("{error:?}"));
        for entry in &catalog.adapters {
            let label = entry.id.as_str();
            if let Some(day) = entry.verified_date() {
                assert!(
                    is_iso_date(day),
                    "{label}: verified_on {}",
                    entry.verified_on
                );
            } else {
                assert_eq!(entry.verified_on, "unverified", "{label}");
            }
            assert!(entry.request.path.starts_with('/'), "{label}");
            assert!(!entry.notes.is_empty(), "{label}");
            assert!(
                !matches!(entry.extract.unit, UnitSpec::Fixed(UnitName::Custom)),
                "{label}: custom unit is for custom queries"
            );
            assert!(
                entry.request.headers.iter().all(|header| {
                    let name = header.name.to_ascii_lowercase();
                    name != "authorization" && name != "x-api-key"
                }),
                "{label}: credentials travel through auth only"
            );
            for preset_id in &entry.matches.preset_ids {
                let preset = presets
                    .provider(preset_id)
                    .unwrap_or_else(|| panic!("{label}: unknown preset {preset_id}"));
                for region in &preset.regions {
                    let host = host_of(&region.base_url).unwrap_or_default();
                    assert!(
                        entry
                            .matches
                            .host_suffixes
                            .iter()
                            .any(|suffix| host_matches(&host, suffix)),
                        "{label}: region host {host} has no host suffix"
                    );
                }
            }
        }
    }

    #[test]
    fn every_recorded_body_replays_to_the_expected_reading() {
        let catalog = probe_catalog().unwrap_or_else(|error| panic!("{error:?}"));
        for entry in &catalog.adapters {
            let recorded = &entry.recorded;
            let draft = entry
                .extract
                .evaluate(&recorded.body.to_string(), &recorded.host, None)
                .unwrap_or_else(|message| panic!("{}: {message}", entry.id.as_str()));
            assert_eq!(
                draft.remaining,
                Some(recorded.expect_remaining),
                "{}",
                entry.id.as_str()
            );
            assert_eq!(draft.unit, recorded.expect_unit, "{}", entry.id.as_str());
        }
    }

    #[test]
    fn unverified_units_are_listed_but_never_applied() {
        let catalog = probe_catalog().unwrap_or_else(|error| panic!("{error:?}"));
        for entry in &catalog.adapters {
            let UnitSpec::ByHost(applied) = &entry.extract.unit else {
                panic!("{}: host-dependent unit expected", entry.id.as_str());
            };
            for (suffix, unit) in &entry.unverified_units {
                assert!(
                    !applied.by_host.contains_key(suffix),
                    "{}: {suffix} is both verified and unverified",
                    entry.id.as_str()
                );
                assert_eq!(
                    entry.extract.unit.resolve(&format!("api.{suffix}")),
                    BalanceUnit::Unsupported,
                    "{}: {suffix} ({unit:?}) must read Unit unknown",
                    entry.id.as_str()
                );
            }
        }
        let unit = |id, host: &str| entry(id).map(|e| e.extract.unit.resolve(host));
        assert_eq!(
            unit(AdapterId::Moonshot, "api.moonshot.cn"),
            Some(BalanceUnit::Cny)
        );
        assert_eq!(
            unit(AdapterId::Moonshot, "api.moonshot.ai"),
            Some(BalanceUnit::Unsupported)
        );
        assert_eq!(
            unit(AdapterId::SiliconFlow, "api.siliconflow.cn"),
            Some(BalanceUnit::Cny)
        );
        assert_eq!(
            unit(AdapterId::SiliconFlow, "api.siliconflow.com"),
            Some(BalanceUnit::Unsupported)
        );
        assert_eq!(
            unit(AdapterId::Zhipu, "open.bigmodel.cn"),
            Some(BalanceUnit::Cny)
        );
        assert_eq!(
            unit(AdapterId::Zhipu, "api.z.ai"),
            Some(BalanceUnit::Unsupported)
        );
        assert_eq!(
            unit(AdapterId::StepFun, "api.stepfun.com"),
            Some(BalanceUnit::Unsupported)
        );
    }

    #[test]
    fn selection_prefers_preset_then_host_and_ignores_lookalike_hosts() {
        let official = GatewayFamily::Official;
        assert_eq!(
            select(Some("moonshot"), None, official),
            Some(AdapterId::Moonshot)
        );
        assert_eq!(
            select(None, Some("api.moonshot.ai"), official),
            Some(AdapterId::Moonshot)
        );
        assert_eq!(
            select(None, Some("api.z.ai"), official),
            Some(AdapterId::Zhipu)
        );
        assert_eq!(select(None, Some("notmoonshot.cn"), official), None);
        assert_eq!(
            select(Some("openai"), Some("api.openai.com"), official),
            None
        );
        assert_eq!(
            entry(AdapterId::StepFun).map(|e| e.extract.unit.resolve("api.stepfun.com")),
            Some(BalanceUnit::Unsupported)
        );
    }
}
