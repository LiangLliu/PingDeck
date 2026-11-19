//! The per-phase outcome of a scan: a fixed decision table per phase over what the scan
//! collected.

use super::super::{
    exec::StopReason,
    plan::Plan,
    types::{
        BalanceFinding, EvidenceItem, KeyCheckStatus, KeyEvidence, PhaseState, PhaseSummary,
        SkippedItem, SniffPhase,
    },
};

fn phase(phase: SniffPhase, state: PhaseState, reason: Option<&str>) -> PhaseSummary {
    PhaseSummary {
        phase,
        state,
        reason: reason.map(str::to_owned),
    }
}

/// What the per-phase outcome is decided from.
pub(super) struct PhaseInputs<'a> {
    pub(super) plan: &'a Plan,
    pub(super) local_hit: bool,
    pub(super) evidence: &'a [EvidenceItem],
    pub(super) skipped: &'a [SkippedItem],
    pub(super) any_models: bool,
    pub(super) had_models_attempts: bool,
    pub(super) any_supported: bool,
    pub(super) no_routes: bool,
    pub(super) stop: Option<StopReason>,
    pub(super) key_status: KeyCheckStatus,
    pub(super) key_evidence: &'a KeyEvidence,
    pub(super) balance_finding: &'a BalanceFinding,
    pub(super) allow_billable_probe: bool,
}

impl PhaseInputs<'_> {
    /// Whether a request of `phase` got a response.
    fn answered(&self, phase: &str) -> bool {
        self.evidence
            .iter()
            .any(|item| item.phase == phase && item.status.is_some())
    }

    /// The reason of the first skipped item whose name starts with `what`.
    fn skip_reason(&self, what: &str) -> Option<&str> {
        self.skipped
            .iter()
            .find(|item| item.what.starts_with(what))
            .map(|item| item.reason.as_str())
    }

    fn local(&self) -> Option<PhaseSummary> {
        self.plan.local.then(|| {
            if self.local_hit {
                phase(SniffPhase::Local, PhaseState::Ok, None)
            } else {
                phase(SniffPhase::Local, PhaseState::Skipped, Some("no_runtime"))
            }
        })
    }

    fn identify(&self) -> PhaseSummary {
        if !self.plan.identify {
            let reason = if self.plan.known.is_some() {
                "known_host"
            } else {
                "local_host"
            };
            phase(SniffPhase::Identify, PhaseState::Skipped, Some(reason))
        } else if self.answered("identify") {
            phase(SniffPhase::Identify, PhaseState::Ok, None)
        } else if let Some(reason) = self.skip_reason("identify") {
            phase(SniffPhase::Identify, PhaseState::Skipped, Some(reason))
        } else {
            phase(SniffPhase::Identify, PhaseState::Failed, Some("no_answer"))
        }
    }

    fn models(&self) -> PhaseSummary {
        if self.any_models {
            phase(SniffPhase::Models, PhaseState::Ok, None)
        } else if !self.had_models_attempts {
            phase(
                SniffPhase::Models,
                PhaseState::Skipped,
                Some("not_applicable"),
            )
        } else if let Some(reason) = self.skip_reason("models") {
            phase(SniffPhase::Models, PhaseState::Skipped, Some(reason))
        } else {
            phase(SniffPhase::Models, PhaseState::Failed, Some("no_list"))
        }
    }

    fn routes(&self) -> PhaseSummary {
        if self.any_supported {
            phase(SniffPhase::Routes, PhaseState::Ok, None)
        } else if self.no_routes {
            let reason = if let Some(StopReason::Html | StopReason::Challenge) = self.stop {
                "stopped"
            } else {
                "not_applicable"
            };
            phase(SniffPhase::Routes, PhaseState::Skipped, Some(reason))
        } else {
            phase(SniffPhase::Routes, PhaseState::Failed, Some("no_route"))
        }
    }

    fn key(&self) -> PhaseSummary {
        match (
            self.key_status,
            self.key_evidence.untested_reason.as_deref(),
        ) {
            (KeyCheckStatus::Untested, Some(reason)) => {
                phase(SniffPhase::Key, PhaseState::Skipped, Some(reason))
            }
            (KeyCheckStatus::Rejected, _) => {
                phase(SniffPhase::Key, PhaseState::Failed, Some("rejected"))
            }
            _ => phase(SniffPhase::Key, PhaseState::Ok, None),
        }
    }

    fn balance(&self) -> PhaseSummary {
        match self.balance_finding {
            BalanceFinding::Found => phase(SniffPhase::Balance, PhaseState::Ok, None),
            BalanceFinding::NoEndpoint => {
                phase(SniffPhase::Balance, PhaseState::Failed, Some("no_endpoint"))
            }
            BalanceFinding::Failed { error } => {
                phase(SniffPhase::Balance, PhaseState::Failed, Some(error))
            }
            BalanceFinding::NotAttempted { reason } => {
                phase(SniffPhase::Balance, PhaseState::Skipped, Some(reason))
            }
        }
    }

    /// The outcome of every phase the scan has, in announcement order.
    pub(super) fn summaries(&self) -> Vec<PhaseSummary> {
        let mut phases: Vec<PhaseSummary> = self.local().into_iter().collect();
        phases.extend([
            self.identify(),
            self.models(),
            self.routes(),
            self.key(),
            self.balance(),
        ]);
        if self.allow_billable_probe {
            phases.push(phase(SniffPhase::Verify, PhaseState::Ok, None));
        }
        phases
    }
}
