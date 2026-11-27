//! One timing phase of an upstream attempt: a duration, or why there is none.

use serde::{Deserialize, Serialize};

/// One phase of an attempt: a value, or why there is none. Unknown is never stored as 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "PhaseRepr", into = "PhaseRepr")]
pub enum PhaseValue {
    /// Measured milliseconds.
    Ms(u64),
    /// The pooled connection made this phase unnecessary.
    Reused,
    /// The phase does not exist for this attempt (IP literal host: no DNS).
    NotNeeded,
    /// The stack could not measure it.
    #[default]
    NotMeasured,
}

impl PhaseValue {
    /// The milliseconds, when measured.
    #[must_use]
    pub fn ms(self) -> Option<u64> {
        match self {
            Self::Ms(ms) => Some(ms),
            _ => None,
        }
    }
}

/// Wire form of [`PhaseValue`]: a number, or one of three words.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PhaseRepr {
    Ms(u64),
    State(PhaseState),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PhaseState {
    Reused,
    NotNeeded,
    NotMeasured,
}

impl From<PhaseRepr> for PhaseValue {
    fn from(repr: PhaseRepr) -> Self {
        match repr {
            PhaseRepr::Ms(ms) => Self::Ms(ms),
            PhaseRepr::State(PhaseState::Reused) => Self::Reused,
            PhaseRepr::State(PhaseState::NotNeeded) => Self::NotNeeded,
            PhaseRepr::State(PhaseState::NotMeasured) => Self::NotMeasured,
        }
    }
}

impl From<PhaseValue> for PhaseRepr {
    fn from(value: PhaseValue) -> Self {
        match value {
            PhaseValue::Ms(ms) => Self::Ms(ms),
            PhaseValue::Reused => Self::State(PhaseState::Reused),
            PhaseValue::NotNeeded => Self::State(PhaseState::NotNeeded),
            PhaseValue::NotMeasured => Self::State(PhaseState::NotMeasured),
        }
    }
}
