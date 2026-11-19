//! How well a balance adapter's endpoint is established: the provenance grade a report carries.

use serde::{Deserialize, Serialize};

/// How well an endpoint is established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// Vendor documentation or specification.
    Documented,
    /// Read from the gateway's open-source code.
    UpstreamSource,
    /// Read from another project's code (or reverse-engineered): the endpoint may be
    /// undocumented and may change.
    ThirdPartySource,
    /// Reasoning only.
    Inferred,
}

impl Evidence {
    /// Whether the provenance tooltip says "From a community-documented endpoint. May change."
    #[must_use]
    pub const fn is_community_documented(self) -> bool {
        matches!(self, Self::ThirdPartySource | Self::Inferred)
    }
}
