//! The OS sandbox tiers and what the startup self-test found.

use serde::{Deserialize, Serialize};

/// The sandbox tier a shell command runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxKind {
    /// macOS `sandbox-exec` with a generated Seatbelt profile.
    Seatbelt,
    /// Linux `bwrap` (bubblewrap).
    Bubblewrap,
    /// No OS sandbox (Windows, or Linux without `bwrap`): path confinement and approvals only.
    None,
}

/// What the startup self-test found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    /// A minimal profile was applied to `/usr/bin/true`.
    Available,
    /// The app itself runs inside a sandbox and cannot apply another.
    Nested,
    /// The tool is absent or unusable, or the platform has none.
    Missing,
}

/// The result of the sandbox self-test: shown in app info and Settings, and the input of the
/// permission decision (`nested` and `missing` make `shell` a forced Ask).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxStatus {
    /// Whether the sandbox applies.
    pub state: SandboxState,
    /// The tier of the tool found; [`SandboxKind::None`] when none is installed.
    pub tier: SandboxKind,
}

impl SandboxStatus {
    /// The tier a command can run under: the detected one when the sandbox applies, else none.
    #[must_use]
    pub fn usable(self) -> SandboxKind {
        match self.state {
            SandboxState::Available => self.tier,
            SandboxState::Nested | SandboxState::Missing => SandboxKind::None,
        }
    }

    /// The folder-info word: `seatbelt`, `bubblewrap`, `none` or `nested`.
    #[must_use]
    pub fn folder_label(self) -> &'static str {
        match (self.state, self.tier) {
            (SandboxState::Nested, _) => "nested",
            (_, SandboxKind::Seatbelt) => "seatbelt",
            (_, SandboxKind::Bubblewrap) => "bubblewrap",
            _ => "none",
        }
    }
}

impl SandboxState {
    /// The word recorded with a call: `available`, or `unavailable` for `nested` and `missing`.
    #[must_use]
    pub fn recorded(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Nested | Self::Missing => "unavailable",
        }
    }
}

/// Whether a shell call ran under the sandbox, as recorded with the call (step row, audit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTag {
    /// The command ran inside the OS sandbox.
    Sandboxed,
    /// The chat acknowledged "Run without a sandbox": the command ran outside it.
    SkippedByChoice,
}

impl SandboxTag {
    /// The tag of a shell call of a chat that has (`acknowledged`) or has not acknowledged
    /// "Run without a sandbox".
    #[must_use]
    pub fn of_call(acknowledged: bool) -> Self {
        if acknowledged {
            Self::SkippedByChoice
        } else {
            Self::Sandboxed
        }
    }
}
