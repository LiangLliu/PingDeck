//! Drift check: reads a tool's config back and says whether it still points at this gateway with
//! a token and alias that are still valid. Read-only, local files only.

use serde::{Deserialize, Serialize};

use super::{ClientApp, DetectEnv, check_gateway_url, detect, non_blank};
use pingdeck_base::{crypto::sha256_hex, error::AppError};

/// The drift state of one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Drift {
    /// Neither a config directory nor an executable on `PATH` was found.
    NotInstalled,
    /// The tool's config has no PingDeck address or token.
    NotConfigured,
    /// Address, token and alias all match.
    Connected,
    /// The config points somewhere else than the gateway.
    DifferentAddress {
        /// The address in the config.
        found: String,
        /// The address the gateway answers on (with the tool's path suffix).
        expected: String,
    },
    /// The token in the config belongs to no active client (revoked, rotated or foreign).
    TokenNotRecognised,
    /// The config selects a model that does not exist, or an old alias name (still served
    /// through the legacy map until the file is written again).
    AliasMissing,
}

/// The per-tool status of the client table, derived from [`Drift`] and what the gateway saw. Only
/// a real request of the tool that passed the model check and got a 2xx answer makes it
/// [`ClientStatus::Verified`]; PingDeck's own Verify probe never does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientStatus {
    /// Written and matching; a request of the tool succeeded and its stored mark is on its way.
    Configured,
    /// Written, but the address, token or alias no longer matches (see [`Drift`] for the cause).
    Drifted,
    /// Nothing of PingDeck is in the tool's config (or the tool is not installed).
    NotConfigured,
    /// Written and matching, but no request from the tool has been seen yet.
    WaitingForFirstRequest,
    /// Written, matching, and a real request passed with the file's token since it was written;
    /// survives a restart.
    Verified {
        /// When that request passed (Unix ms).
        at_ms: i64,
    },
    /// A project `.env` or a shell export sets a variable over the written one.
    Overridden {
        /// Where, e.g. `~/proj/.env: GEMINI_API_KEY`.
        source: String,
    },
    /// The model the file names is not in the client's allow-list or not routable.
    ModelUnavailable,
    /// Still no request [`NO_REQUEST_AFTER_MS`] after the write.
    NoRequest,
    /// The tool's requests arrived but were refused or failed.
    Rejected {
        /// The HTTP status of the first one.
        status: u16,
    },
}

/// How long "Waiting for the first request" lasts before it ends as "No request" (10 minutes).
pub const NO_REQUEST_AFTER_MS: i64 = 10 * 60 * 1000;

/// What the gateway and the files say besides the [`Drift`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusFacts {
    /// Status of the first answered request since the write (a later success replaces a refusal).
    pub first_status: Option<u16>,
    /// When a request last passed with the file's token, already checked to be newer than the
    /// write.
    pub verified_at_ms: Option<i64>,
    /// What overrides the written values, if anything.
    pub override_source: Option<String>,
    /// The client may use every model the file names.
    pub model_allowed: bool,
    /// [`NO_REQUEST_AFTER_MS`] passed since the latest write.
    pub waited_out: bool,
}

/// The [`ClientStatus`] of a [`Drift`] and the gateway's [`StatusFacts`].
#[must_use]
pub fn client_status(drift: &Drift, facts: &StatusFacts) -> ClientStatus {
    match drift {
        Drift::NotInstalled | Drift::NotConfigured => ClientStatus::NotConfigured,
        Drift::Connected => {
            if let Some(source) = &facts.override_source {
                return ClientStatus::Overridden {
                    source: source.clone(),
                };
            }
            if !facts.model_allowed {
                return ClientStatus::ModelUnavailable;
            }
            match (facts.verified_at_ms, facts.first_status) {
                (Some(at_ms), _) => ClientStatus::Verified { at_ms },
                (None, Some(status)) if (200..300).contains(&status) => ClientStatus::Configured,
                (None, Some(status)) => ClientStatus::Rejected { status },
                (None, None) if facts.waited_out => ClientStatus::NoRequest,
                (None, None) => ClientStatus::WaitingForFirstRequest,
            }
        }
        Drift::DifferentAddress { .. } | Drift::TokenNotRecognised | Drift::AliasMissing => {
            ClientStatus::Drifted
        }
    }
}

/// The result of [`inspect`]: the state plus the values found. The token is never part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inspection {
    /// The tool.
    pub app: ClientApp,
    /// The state.
    pub drift: Drift,
    /// Address found in the config.
    pub base_url: Option<String>,
    /// Model reference found in the config.
    pub alias: Option<String>,
}

/// What [`inspect`] compares the config with.
pub struct InspectContext<'a> {
    /// The loopback gateway root the writers write, e.g. `http://127.0.0.1:4141`.
    pub gateway_url: &'a str,
    /// Takes the SHA-256 (lower-case hex) of the token found; `true` when an active client has
    /// that hash. The token itself never leaves the module.
    pub token_is_active: &'a dyn Fn(&str) -> bool,
    /// `true` when the model name found resolves to a gateway model directly (a model reference,
    /// not a legacy alias name).
    pub alias_exists: &'a dyn Fn(&str) -> bool,
}

/// Raw values a writer's `inspect` found; the token is reduced to its hash on entry.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Found {
    pub base_url: Option<String>,
    pub token_sha256: Option<String>,
    pub alias: Option<String>,
    /// Claude Code's background-task alias.
    pub fast_alias: Option<String>,
}

impl Found {
    /// The values found, trimmed; blank ones count as absent.
    pub(super) fn new(base_url: Option<&str>, token: Option<&str>, alias: Option<&str>) -> Self {
        Self {
            base_url: non_blank(base_url).map(str::to_owned),
            token_sha256: non_blank(token).map(|token| sha256_hex(token.as_bytes())),
            alias: non_blank(alias).map(str::to_owned),
            fast_alias: None,
        }
    }

    /// Adds Claude Code's background-task alias, trimmed; blank counts as absent.
    pub(super) fn with_fast_alias(mut self, fast_alias: Option<&str>) -> Self {
        self.fast_alias = non_blank(fast_alias).map(str::to_owned);
        self
    }
}

/// Checks the tool's config against the gateway.
///
/// Order: not installed, not configured (no address or no token), different address, token not
/// recognised, alias missing (only when the config names a model), connected.
///
/// # Errors
/// [`AppError::validation`] for a bad gateway URL or a config file that cannot be parsed;
/// [`AppError::io`] when a file cannot be read.
pub fn inspect(
    env: &DetectEnv,
    app: ClientApp,
    ctx: &InspectContext<'_>,
) -> Result<Inspection, AppError> {
    let gateway = check_gateway_url(ctx.gateway_url)?;
    let writer = app.writer();
    // A config that cannot be parsed is an error even when the tool does not look installed.
    let found = (writer.inspect)(&env.home)?;
    let drift = if detect::is_installed(env, app) {
        drift_of(&found, format!("{gateway}{}", writer.address_suffix), ctx)
    } else {
        Drift::NotInstalled
    };
    Ok(Inspection {
        app,
        drift,
        base_url: found.base_url,
        alias: found.alias,
    })
}

/// The drift of an installed tool's config: not configured (no address or no token), different
/// address (`expected` is the gateway root with the tool's suffix), token not recognised, alias
/// missing, connected.
fn drift_of(found: &Found, expected: String, ctx: &InspectContext<'_>) -> Drift {
    let (Some(url), Some(hash)) = (&found.base_url, &found.token_sha256) else {
        return Drift::NotConfigured;
    };
    if url.trim_end_matches('/') != expected {
        Drift::DifferentAddress {
            found: url.clone(),
            expected,
        }
    } else if !(ctx.token_is_active)(hash) {
        Drift::TokenNotRecognised
    } else if [&found.alias, &found.fast_alias]
        .into_iter()
        .flatten()
        .any(|alias| !(ctx.alias_exists)(alias))
    {
        Drift::AliasMissing
    } else {
        Drift::Connected
    }
}

/// The distinct model references a tool's config holds, main model first (Claude Code may hold
/// two: the main and the background-task model), for legacy-name bookkeeping and rename impact.
///
/// # Errors
/// [`AppError::validation`] for a config file that cannot be parsed, [`AppError::io`] for one
/// that cannot be read.
pub fn config_models(env: &DetectEnv, app: ClientApp) -> Result<Vec<String>, AppError> {
    let Found {
        alias, fast_alias, ..
    } = (app.writer().inspect)(&env.home)?;
    // Without a fast alias, Claude Code's background-task key holds the main alias again.
    let fast_alias = fast_alias.filter(|fast| alias.as_ref() != Some(fast));
    Ok(alias.into_iter().chain(fast_alias).collect())
}

#[cfg(test)]
mod tests;
