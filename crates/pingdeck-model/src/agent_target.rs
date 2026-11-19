//! Which model an agent session talks to.

use serde::{Deserialize, Serialize};

/// Which model a session talks to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentTarget {
    /// An alias name or `provider_id/model`, resolved with failover.
    Named {
        /// The alias or `provider_id/model`.
        name: String,
    },
    /// One provider model, optionally pinned to one key.
    Pinned {
        /// Provider id.
        provider_id: String,
        /// A specific key, or `None` for the provider's enabled keys in order.
        key_id: Option<String>,
        /// Upstream model id.
        model: String,
    },
}

impl AgentTarget {
    /// A short label for the model line of a message and for logs.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Named { name } => name.clone(),
            Self::Pinned {
                provider_id, model, ..
            } => format!("{provider_id}/{model}"),
        }
    }
}
