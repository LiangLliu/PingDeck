use serde::{Deserialize, Serialize};

use crate::phase::PhaseValue;
use crate::validation::{WorkspaceError, validate_name, validate_variable_key};

use super::{EnvironmentId, HttpMethod, KeyValue, WorkspaceId};

/// A named set of variables used to interpolate requests in a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub id: EnvironmentId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub variables: Vec<EnvironmentVariable>,
    /// Production guard: sending into this environment needs an explicit confirmation.
    #[serde(default)]
    pub confirm_before_send: bool,
    /// Widens the guard to GET, HEAD and OPTIONS (off: safe methods never need confirmation).
    #[serde(default)]
    pub confirm_safe_methods: bool,
    /// Hosts the user added by hand to the ones the environment derives; lowercase
    /// `host[:port]`, at most [`MAX_DECLARED_HOSTS`].
    #[serde(default)]
    pub declared_hosts: Vec<String>,
    /// When either guard flag last changed, in Unix milliseconds; `None` before the first
    /// change. Kept by the store, never in sync or export files.
    #[serde(skip)]
    pub guard_changed_at: Option<i64>,
}

/// Most explicit hosts one environment declares.
pub const MAX_DECLARED_HOSTS: usize = 64;

impl Environment {
    /// Creates an empty environment with a validated id, workspace id, and name.
    pub fn new(
        id: impl AsRef<str>,
        workspace_id: impl AsRef<str>,
        name: impl AsRef<str>,
    ) -> Result<Self, WorkspaceError> {
        Ok(Self {
            id: EnvironmentId::new(id)?,
            workspace_id: WorkspaceId::new(workspace_id)?,
            name: validate_name("environment", name.as_ref())?,
            variables: Vec::new(),
            confirm_before_send: false,
            confirm_safe_methods: false,
            declared_hosts: Vec::new(),
            guard_changed_at: None,
        })
    }

    /// Whether sending a `method` request into this environment needs a confirmation: the guard
    /// is on and the method is not a safe one (or safe methods are guarded too).
    #[must_use]
    pub fn requires_confirmation(&self, method: HttpMethod) -> bool {
        self.confirm_before_send
            && (self.confirm_safe_methods
                || !matches!(
                    method,
                    HttpMethod::Get | HttpMethod::Head | HttpMethod::Options
                ))
    }
}

/// A variable of an environment, of globals or of a collection. A secret variable is a masked
/// one: its value is kept in the secret store and never leaves the core in a shell-facing DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentVariable {
    /// Stable identity of the variable (`var_…`), so a rename keeps its secret. Empty
    /// for a variable no save has named yet; sync files without it load.
    #[serde(default)]
    pub id: String,
    pub key: String,
    pub value: Option<String>,
    pub is_secret: bool,
    pub enabled: bool,
    /// Free-text note shown next to the variable; sync files without it load.
    #[serde(default)]
    pub note: String,
}

impl EnvironmentVariable {
    /// Creates an enabled, non-secret variable with a validated key and value.
    pub fn plain(key: impl AsRef<str>, value: impl Into<String>) -> Result<Self, WorkspaceError> {
        Ok(Self {
            id: String::new(),
            key: validate_variable_key(key.as_ref())?,
            value: Some(value.into()),
            is_secret: false,
            enabled: true,
            note: String::new(),
        })
    }

    /// Creates a masked variable whose value is stored locally in the workspace.
    pub fn masked(key: impl AsRef<str>, value: impl Into<String>) -> Result<Self, WorkspaceError> {
        Ok(Self {
            id: String::new(),
            key: validate_variable_key(key.as_ref())?,
            value: Some(value.into()),
            is_secret: true,
            enabled: true,
            note: String::new(),
        })
    }

    /// Whether the variable defines its key without a value to send: a secret whose value is
    /// empty (cleared, or a sync or import that carried no value) or a row with no value at all.
    /// When enabled it still shadows every broader layer, so a send that needs it fails as
    /// unresolved instead of falling through to a same-named value from another layer. The
    /// resolver, the unresolved scan and the Resolved view all apply this one rule.
    #[must_use]
    pub fn needs_value(&self) -> bool {
        match self.value.as_deref() {
            None => true,
            Some(value) => self.is_secret && value.is_empty(),
        }
    }
}

// ---------------------------------------------------------------------------
// Call-chain debugging
// ---------------------------------------------------------------------------

/// Where a `{{variable}}` token in an executed request took its value from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VariableResolutionSource {
    Runtime,
    Environment,
    Collection,
    Global,
    BuiltIn,
    #[default]
    Unresolved,
}

/// A resolved `{{variable}}` of an executed request with its source and value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedVariableTrace {
    pub name: String,
    pub source: VariableResolutionSource,
    /// The interpolated value, masked when `secret` is true.
    pub resolved_value: String,
    pub secret: bool,
}

/// Summary of the auth applied to an executed request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthTraceSummary {
    /// `none`, `bearer`, `basic`, `api_key`, `oauth2`, `digest` or `aws_sigv4`.
    pub auth_type: String,
    /// `request`, `folder` or `collection`. The executor only sees the request
    /// after inheritance was resolved, so it reports `request`.
    pub source: String,
    /// Where the credential landed, for example `Header: Authorization (Bearer)`.
    pub applied_to: String,
    /// Non-secret metadata such as the username, header name or signing region.
    pub details: Vec<KeyValue>,
}

/// One redirect hop followed while executing a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedirectHopTrace {
    pub hop: usize,
    pub method: String,
    pub url: String,
    pub status_code: u16,
    pub location: Option<String>,
    pub duration_ms: u128,
}

/// Transport-level details of an executed request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportTraceSummary {
    /// `HTTP/1.1`, `HTTP/2.0`, ... as answered by the server.
    pub protocol: String,
    /// Address of the server that answered, when known.
    pub remote_addr: Option<String>,
    /// The configured proxy with any password replaced by `****`; `None` when the request went
    /// direct because no proxy is configured.
    pub proxy_url: Option<String>,
    /// True only when a proxy is configured but this request skipped it (a no-proxy rule).
    /// Always false today: no such rule exists, so a configured proxy applies to every host.
    pub proxy_bypassed: bool,
    pub custom_ca_count: usize,
    /// Host pattern of the client certificate that matched, if any.
    pub client_cert_host: Option<String>,
    pub tls_disabled_versions: Vec<String>,
    pub redirect_hops: Vec<RedirectHopTrace>,
}

/// Everything that shaped the bytes of one executed request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallChainTrace {
    pub variables: Vec<ResolvedVariableTrace>,
    pub auth: AuthTraceSummary,
    pub transport: TransportTraceSummary,
}

/// Whether a phase carries nothing to report; such a phase is left out of the wire form.
fn phase_unmeasured(value: &PhaseValue) -> bool {
    *value == PhaseValue::NotMeasured
}

/// What a collection run row or a flow step shows of an AI request it ran through the AI run
/// service: the model, why the reply ended, the cost and why the run failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiStepSummary {
    /// The model that answered (the requested one when the answer did not name it).
    pub model: String,
    /// The finish reason the provider reported, as it spelled it.
    #[serde(default)]
    pub finish_reason: Option<String>,
    /// Millionths of `cost_currency`; `None` when no price is known.
    #[serde(default)]
    pub cost_micro: Option<i64>,
    /// ISO 4217 code of the cost.
    #[serde(default)]
    pub cost_currency: Option<String>,
    /// The cost is an estimate (catalogue price or estimated token counts).
    #[serde(default)]
    pub cost_estimated: bool,
    /// Why the run did not end with an answer (rejected, in-stream error, invalid answer, timed
    /// out); the row or step fails with it.
    #[serde(default)]
    pub error: Option<String>,
}

/// How long the request and the response body each took, and the connection phases of the
/// request when the HTTP stack could measure them.
///
/// `reqwest` reports no per-phase times, so the executor measures the resolver and the connector
/// itself (`executor/probe.rs`). A phase it could not measure stays [`PhaseValue::NotMeasured`]
/// and is absent from the wire form, never 0: DNS and connect describe the last hop's new
/// connection, `Reused` stands for a pooled connection, and TLS is part of `connect_ms`
/// (`connect_includes_tls`) because the handshake is not separable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseTiming {
    /// Time until the response headers arrived (connection setup included).
    pub request_ms: u128,
    /// Time spent reading the response body.
    pub response_ms: u128,
    pub total_ms: u128,
    /// Resolver time of the connection this request opened.
    #[serde(default, skip_serializing_if = "phase_unmeasured")]
    pub dns_ms: PhaseValue,
    /// TCP connect, plus the TLS handshake when `connect_includes_tls`.
    #[serde(default, skip_serializing_if = "phase_unmeasured")]
    pub connect_ms: PhaseValue,
    /// TLS handshake on its own: `NotNeeded` for plain HTTP, absent for HTTPS (see
    /// `connect_includes_tls`).
    #[serde(default, skip_serializing_if = "phase_unmeasured")]
    pub tls_ms: PhaseValue,
    /// `connect_ms` also covers the TLS handshake.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub connect_includes_tls: bool,
    /// Connection setup end to response headers; present only when the setup was measured or
    /// the connection was reused (`request_ms` minus the measured setup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    /// Milliseconds from sending to the first answer content of a streamed AI call (TTFT);
    /// absent for an HTTP request and for a stream that sent no content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    /// 1 when the executor's connection probe produced the phases (absent phases then mean not
    /// measurable, e.g. through a proxy); absent on rows recorded before the probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases_version: Option<u8>,
}
