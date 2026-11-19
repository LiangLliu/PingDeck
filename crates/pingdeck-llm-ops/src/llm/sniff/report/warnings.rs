//! The report's warnings: the address's and the executor's, the scan's own, then the preset's.

use super::super::{
    key_guard::KeyConcern,
    plan::Plan,
    route_probe::RouteResult,
    types::{KeyCheckStatus, RouteVerdict, SniffWarning},
};
use super::ScanEnd;
use crate::llm::presets::ProviderPreset;

/// The report's warnings, each kept once in the order first seen.
pub(super) struct Warnings(Vec<SniffWarning>);

impl Warnings {
    /// The warnings in order.
    pub(super) fn into_vec(self) -> Vec<SniffWarning> {
        self.0
    }

    fn add(&mut self, warning: SniffWarning) {
        if !self.0.contains(&warning) {
            self.0.push(warning);
        }
    }
}

/// Warnings about the preset: routes it lists that are absent, and a rejected key that
/// probably belongs to the only sibling region.
pub(super) fn add_preset_warnings(
    warnings: &mut Warnings,
    plan: &Plan,
    routes: &[RouteResult],
    key_status: KeyCheckStatus,
    preset: Option<&ProviderPreset>,
) {
    let Some(known) = &plan.known else {
        return;
    };
    for route in routes
        .iter()
        .filter(|route| route.verdict == RouteVerdict::Absent)
    {
        warnings.add(SniffWarning::PresetRouteMismatch {
            protocol: route.protocol,
        });
    }
    if key_status != KeyCheckStatus::Rejected {
        return;
    }
    let siblings: Vec<&str> = preset
        .map(|provider| {
            provider
                .regions
                .iter()
                .filter(|region| region.id != known.region_id)
                .map(|region| region.id.as_str())
                .collect()
        })
        .unwrap_or_default();
    if let [only] = siblings.as_slice() {
        warnings.add(SniffWarning::RegionMismatch {
            suggested_region_id: (*only).to_owned(),
        });
    }
}

/// The warnings of the address, the executor and the scan itself, before the preset's.
pub(super) fn scan_warnings(
    url_warnings: &[SniffWarning],
    exec_warnings: Vec<SniffWarning>,
    key_concern: Option<KeyConcern>,
    ends: ScanEnd,
    third_party: bool,
) -> Warnings {
    let mut warnings = Warnings(url_warnings.to_vec());
    for warning in exec_warnings {
        warnings.add(warning);
    }
    match key_concern {
        Some(KeyConcern::LookalikeHost) => warnings.add(SniffWarning::LookalikeHost),
        Some(KeyConcern::VendorKeyOnOtherHost) => warnings.add(SniffWarning::VendorKeyOnOtherHost),
        None => {}
    }
    if ends.timed_out {
        warnings.add(SniffWarning::ScanTimeout);
    }
    if ends.cancelled {
        warnings.add(SniffWarning::ScanCancelled);
    }
    if third_party {
        warnings.add(SniffWarning::BrandThirdParty);
    }
    warnings
}
