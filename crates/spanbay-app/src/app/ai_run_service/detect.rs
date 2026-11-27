//! "Detect" of the Custom URL target: the sniffer, on the typed URL, saving nothing.
//!
//! Single responsibility: run the same scan as the Add provider wizard (without "try more
//! address variants": 8 requests without a key, 12 with one) so the request editor can fill
//! protocol, base path and model list. The scan never creates a provider; the key it was given
//! goes only to the scanned origin, and the network guard of the sniffer applies (redirects
//! manual, link-local and metadata addresses refused, typed loopback and private addresses
//! allowed).

use serde::{Deserialize, Serialize};

use crate::app::sniff_runner::run_unverified_sniff;
use crate::app::{AppState, execution_context_for};
use crate::error::AppError;
use crate::events::EventSink;
use crate::llm::sniff::{SniffEvent, SniffInput, SniffReport};
use crate::workspace::variables::resolve_template;

/// Input of `ai_detect`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiDetectInput {
    /// The typed URL (`{{variables}}` apply when a workspace is given).
    pub url: String,
    /// The typed key, if any (`{{variables}}` apply).
    #[serde(default)]
    pub key: Option<String>,
    /// Workspace whose variables resolve the URL and key.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Environment whose variables apply.
    #[serde(default)]
    pub environment_id: Option<String>,
}

/// The key is a secret: `Debug` shows only whether there is one.
impl std::fmt::Debug for AiDetectInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiDetectInput")
            .field("url", &self.url)
            .field("key", &self.key.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

/// Scans the typed URL and returns the report; nothing is saved and no provider is created.
///
/// # Errors
/// [`AppError::Validation`] for an unusable or always-refused address (`blocked_address` in the
/// message) or an unresolved variable, [`AppError::Network`] when nothing answered,
/// [`AppError::Cancelled`].
pub async fn ai_detect_impl(
    state: &AppState,
    input: AiDetectInput,
    cancel_token: Option<String>,
    sink: &dyn EventSink<SniffEvent>,
) -> Result<SniffReport, AppError> {
    let scopes = match input.workspace_id.as_deref() {
        Some(workspace_id) => {
            let store = state.store();
            Some(
                execution_context_for(
                    &store,
                    workspace_id,
                    None,
                    None,
                    input.environment_id.as_deref(),
                )?
                .scopes(),
            )
        }
        None => None,
    };
    let resolve = |text: &str| -> Result<String, AppError> {
        match &scopes {
            Some(scopes) => resolve_template(text, scopes)
                .map_err(|error| AppError::validation(error.to_string())),
            None => Ok(text.to_owned()),
        }
    };
    let mut scan = SniffInput::new(resolve(input.url.trim())?);
    scan.api_key = input
        .key
        .as_deref()
        .map(|key| resolve(key.trim()))
        .transpose()?
        .filter(|key| !key.is_empty());
    run_unverified_sniff(state, scan, cancel_token, sink).await
}
