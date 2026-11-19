//! Wire types of the provider sniffer: input, context, report, events and errors
//! (`docs/decisions.md` §12 L-9). Every wire type derives `Debug, Clone, PartialEq, Serialize,
//! Deserialize`; [`SniffInput`] carries secrets, so its `Debug` prints names and flags only.

mod input;
mod progress;
mod report;

pub use input::{SniffContext, SniffError, SniffInput};
pub use progress::{PhaseState, PhaseSummary, PlannedRow, SniffEvent, SniffPhase};
pub use report::{
    BalanceFinding, EvidenceRefs, IdentityLabel, KeyCheck, KeyCheckStatus, KeyEvidence,
    RouteVerdict, SkippedItem, SniffReport, SniffWarning, SniffedEndpoint, SniffedModel,
};
pub(crate) use report::{EvidenceItem, RunnerUp};
