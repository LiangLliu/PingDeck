//! The structured error every `spanbay_core::app` use-case returns.
//!
//! Single responsibility: give a UI shell a stable, machine-readable failure
//! category (`kind`) next to the human-readable `message`, so it can branch on
//! "cancelled" or "not found" without matching text. Serializes as
//! `{"kind": "NotFound", "message": "…"}`; the message is exactly the text the
//! pre-structured API returned as a plain string, so displayed errors are unchanged.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cancel::CANCELLED_MESSAGE;

/// A stable, machine-readable reason of a failed send or run, next to the category (`kind`): a UI
/// picks its failure card from it instead of matching the English message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// A `{{variable}}` has no value; the subject is its key.
    UnresolvedVariable,
    /// The URL does not parse; the subject is the URL text.
    InvalidUrl,
    /// The HTTP client could not be built (an invalid TLS or proxy setting, an unreadable
    /// certificate).
    ClientBuild,
    /// The response body could not be read to its end.
    BodyRead,
    /// A header name or value is not allowed; the subject is the header name.
    InvalidHeader,
    /// A multipart file part could not be read; the subject is its path.
    MultipartRead,
    /// The GraphQL variables are not valid JSON.
    GraphqlVariables,
    /// The GraphQL body could not be serialised.
    GraphqlSerialize,
    /// The OAuth 2.0 token request failed.
    OauthToken,
    /// A redirect was refused because the request carries a secret and the new host is not one
    /// the environment declares; the subject is the host.
    RedirectRefused,
    /// The SSRF guard refused the connection: the host is, or resolves to, a link-local, metadata
    /// or (for this request) private address; the subject is the host. Never retried.
    GuardRefused,
    /// A request body or file is over the 10 MiB limit; the subject is the file path when there is
    /// one.
    RequestTooLarge,
    /// A pre-request, post-response or test script failed; the subject is where (`pre-request`, a
    /// scope name).
    ScriptFailed,
    /// WebSocket and gRPC requests are not sent yet.
    UnsupportedProtocol,
    /// The HTTP method is not supported for this protocol.
    UnsupportedMethod,
    /// The body type is not supported for execution.
    UnsupportedBody,
    /// A save named the version it read (`expected_updated_at_ms`) and the stored row has another
    /// one: someone else wrote it in between. Nothing was written.
    StaleWrite,
    /// A provider save moves it to another outbound host (base URL or balance URL) and did not
    /// confirm it; the subject is the new hosts, comma-separated. Nothing was written.
    HostChangeNeedsConfirm,
    /// A gateway settings change would leave extra allowed hosts reachable without a token:
    /// turn on "Require tokens" or remove the hosts first. Nothing was written.
    GatewayTokenRequired,
    /// A chat needs a model and none is usable: no chat was created and no message
    /// stored.
    NoModel,
    /// The provider rejected the key on its model list (401 or 403); the subject is the status.
    ModelListUnauthorized,
    /// The provider's model list address answered 404.
    ModelListNotFound,
    /// The model list address answered with a web page instead of a list.
    ModelListHtml,
    /// The model list request ran out of time.
    ModelListTimeout,
    /// The model list request failed in transit (DNS, refused, TLS, reset); the subject is the
    /// transport failure's stable name.
    ModelListTransport,
    /// The named provider does not exist (deleted elsewhere); the subject is its id.
    ProviderNotFound,
    /// The secret store (the `vault` next to the database) could not be read or written.
    KeyStoreUnavailable,
    /// A provider name equals (ignoring case) another provider's name or id; the subject is
    /// the holder's id. Nothing was written.
    NameTaken,
    /// A provider name contains `/`, which separates provider and model in `<name>/<model>`.
    NameHasSlash,
    /// Confirmation: the request the agent wrote, not yet sent from the UI, carries a secret to a
    /// host its environment does not declare; the subject is the host.
    AgentTouchedSecret,
    /// [`ErrorCode::AgentTouchedSecret`] in an environment that also guards this method:
    /// one confirmation covers both, so the question names the environment too.
    AgentTouchedSecretGuarded,
    /// The workspace the operation names does not exist (deleted elsewhere); the subject is its
    /// id.
    WorkspaceNotFound,
    /// The database stayed busy or locked by another writer past the wait; nothing was written.
    DatabaseBusy,
    /// The disk holding the data folder (or the target file) is full.
    DiskFull,
    /// The operating system refused access to a file or folder (permissions, a read-only
    /// volume, macOS privacy settings).
    PermissionDenied,
}

/// Why a send's transport failed, read from the client error's type (never its message text): a
/// UI picks the failure card and the order of its actions from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkCause {
    /// The request ran out of its time limit.
    Timeout,
    /// The TLS handshake failed (untrusted or invalid certificate, protocol mismatch).
    Tls,
    /// The host name did not resolve.
    Dns,
    /// The peer refused the connection.
    Refused,
    /// The connection was reset or closed mid-exchange.
    Reset,
    /// The proxy asked for credentials (407 to the `CONNECT`).
    ProxyAuth,
    /// The proxy answered the `CONNECT` (or SOCKS handshake) with a refusal.
    ProxyRefused,
    /// The proxy itself could not be reached (refused, unresolvable): Clash or Surge not running.
    ProxyUnreachable,
}

/// The [`NetworkCause`] of a client error, read from its source chain: typed I/O errors by
/// kind, and the fixed first words of the connector's own nodes (`dns error`, `tunnel error: …`,
/// `SOCKS error: …`), which never carry the URL. `proxied` says the request went through a proxy,
/// where a refused or unresolvable peer is the proxy. A timeout flag the client keeps outside the
/// chain (`reqwest::Error::is_timeout`) is the caller's to check first.
#[must_use]
pub fn transport_cause(
    error: &(dyn std::error::Error + 'static),
    proxied: bool,
) -> Option<NetworkCause> {
    let mut source = Some(error);
    while let Some(current) = source {
        let text = current.to_string();
        if text.starts_with("tunnel error: proxy authorization required") {
            return Some(NetworkCause::ProxyAuth);
        }
        if text.starts_with("tunnel error: failed to create underlying connection")
            || text.starts_with("SOCKS error: failed to create underlying connection")
        {
            return Some(NetworkCause::ProxyUnreachable);
        }
        if text.starts_with("tunnel error: unsuccessful") || text.starts_with("SOCKS error: ") {
            return Some(NetworkCause::ProxyRefused);
        }
        if text.starts_with("error resolving for socks proxy") {
            return Some(NetworkCause::Dns);
        }
        // The resolver's own I/O error sits below this node; its kind says nothing more.
        let found = if text.starts_with("dns error") {
            Some(NetworkCause::Dns)
        } else {
            current.downcast_ref::<std::io::Error>().and_then(io_cause)
        };
        if let Some(cause) = found {
            return Some(match cause {
                NetworkCause::Refused | NetworkCause::Dns if proxied => {
                    NetworkCause::ProxyUnreachable
                }
                other => other,
            });
        }
        source = current.source();
    }
    None
}

/// The cause an I/O error's kind names, looking through I/O errors wrapped in it (the connector
/// wraps the TLS stream's error in one of kind `Other`, whose `source` skips the inner one).
fn io_cause(error: &std::io::Error) -> Option<NetworkCause> {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::TimedOut => Some(NetworkCause::Timeout),
        ErrorKind::ConnectionRefused => Some(NetworkCause::Refused),
        ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::BrokenPipe
        | ErrorKind::UnexpectedEof => Some(NetworkCause::Reset),
        // tokio-rustls reports a failed handshake (bad certificate, alert) as InvalidData.
        ErrorKind::InvalidData => Some(NetworkCause::Tls),
        _ => error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<std::io::Error>())
            .and_then(io_cause),
    }
}

/// The optional detail of a failure: its [`ErrorCode`], what the failure names and whether a
/// failed send stayed out of History. All are absent for errors that need no special card.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// The stable reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ErrorCode>,
    /// What the reason names (see [`ErrorCode`]): a variable, header, path or host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// A failed send whose history row could not be written: the failure is not in History.
    /// Absent when the row was written or the send records none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_unsaved: Option<bool>,
}

/// A failed use-case: a category plus the message to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(tag = "kind")]
pub enum AppError {
    /// The operation was aborted through its cancel token.
    #[error("{message}")]
    Cancelled {
        message: String,
        /// Response body bytes received before the abort, when the abort interrupted a send;
        /// absent when the core cannot tell.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes_received: Option<u64>,
    },
    /// A referenced entity does not exist (or not in this workspace).
    #[error("{message}")]
    NotFound {
        message: String,
        /// The stable reason, when the failure has one (`provider_not_found`).
        #[serde(flatten)]
        detail: ErrorDetail,
    },
    /// The input is malformed, blank, over a limit or otherwise rejected.
    #[error("{message}")]
    Validation {
        message: String,
        /// The stable reason and what it names, when the failure has one.
        #[serde(flatten)]
        detail: ErrorDetail,
        /// Every name the failure is about (all unresolved variables of a send); the
        /// detail's `subject` is the first. Boxed so the error stays small
        /// (`clippy::result_large_err`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[allow(clippy::box_collection)]
        keys: Option<Box<Vec<String>>>,
    },
    /// The request conflicts with current state (port in use, concurrent change, id reuse).
    #[error("{message}")]
    Conflict {
        message: String,
        /// The stable reason, when the failure has one (`stale_write`).
        #[serde(flatten)]
        detail: ErrorDetail,
    },
    /// The target environment has "Confirm before send" on and the caller did not pass
    /// `confirmed: true`: nothing was sent.
    #[error("{message}")]
    ConfirmationRequired {
        message: String,
        /// Why it asks, when not (only) the guarded environment (`agent_touched_secret*`).
        #[serde(flatten)]
        detail: ErrorDetail,
    },
    /// A filesystem or other local I/O operation failed.
    #[error("{message}")]
    Io {
        message: String,
        /// The stable reason and what it names, when the failure has one.
        #[serde(flatten)]
        detail: ErrorDetail,
    },
    /// SQLite or stored data failed.
    #[error("{message}")]
    Database {
        message: String,
        /// The stable reason, when the failure has one (`key_store_unavailable`).
        #[serde(flatten)]
        detail: ErrorDetail,
    },
    /// A network transfer or HTTP client failed.
    #[error("{message}")]
    Network {
        message: String,
        /// Whether the failure happened while establishing the connection (a send's transport
        /// failure); absent for other network errors.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connect_phase: Option<bool>,
        /// Attempts made when a send failed (retries included); absent for other network errors.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt_count: Option<u32>,
        /// Why a send's transport failed, when the client error says; absent otherwise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<NetworkCause>,
        /// The stable reason and what it names, when the failure has one.
        #[serde(flatten)]
        detail: ErrorDetail,
    },
    /// The request is well-formed but not supported by this build.
    #[error("{message}")]
    Unsupported {
        message: String,
        /// The stable reason and what it names, when the failure has one.
        #[serde(flatten)]
        detail: ErrorDetail,
    },
    /// An invariant broke inside SpanBay (script worker crash, ID exhaustion, …).
    #[error("{message}")]
    Internal {
        message: String,
        /// The stable reason and what it names, when the failure has one.
        #[serde(flatten)]
        detail: ErrorDetail,
    },
}

impl AppError {
    /// The cancellation error with the canonical "Request cancelled" message.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::Cancelled {
            message: CANCELLED_MESSAGE.to_owned(),
            bytes_received: None,
        }
    }

    /// A [`AppError::NotFound`] with `message`.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound {
            message: message.into(),
            detail: ErrorDetail::default(),
        }
    }

    /// A [`AppError::ConfirmationRequired`] with `message`.
    #[must_use]
    pub fn confirmation_required(message: impl Into<String>) -> Self {
        Self::ConfirmationRequired {
            message: message.into(),
            detail: ErrorDetail::default(),
        }
    }

    /// A [`AppError::Validation`] with `message`.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
            detail: ErrorDetail::default(),
            keys: None,
        }
    }

    /// A [`AppError::Conflict`] with `message`.
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict {
            message: message.into(),
            detail: ErrorDetail::default(),
        }
    }

    /// An [`AppError::Io`] with `message`.
    #[must_use]
    pub fn io(message: impl Into<String>) -> Self {
        Self::Io {
            message: message.into(),
            detail: ErrorDetail::default(),
        }
    }

    /// An [`AppError::Database`] with `message`.
    #[must_use]
    pub fn database(message: impl Into<String>) -> Self {
        Self::Database {
            message: message.into(),
            detail: ErrorDetail::default(),
        }
    }

    /// An [`AppError::Network`] with `message`.
    #[must_use]
    pub fn network(message: impl Into<String>) -> Self {
        Self::Network {
            message: message.into(),
            connect_phase: None,
            attempt_count: None,
            cause: None,
            detail: ErrorDetail::default(),
        }
    }

    /// An [`AppError::Unsupported`] with `message`.
    #[must_use]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported {
            message: message.into(),
            detail: ErrorDetail::default(),
        }
    }

    /// An [`AppError::Internal`] with `message`.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
            detail: ErrorDetail::default(),
        }
    }

    /// This error with the stable `code` and the `subject` it names (a no-op for the kinds that
    /// carry no detail: cancelled).
    #[must_use]
    pub fn coded(mut self, code: ErrorCode, subject: impl Into<Option<String>>) -> Self {
        if let Some(detail) = self.detail_mut() {
            detail.code = Some(code);
            detail.subject = subject.into();
        }
        self
    }

    /// This error marked as "not saved to History" when `unsaved` (a failed send whose history
    /// row could not be written); unchanged otherwise and for cancelled errors.
    #[must_use]
    pub fn with_history_unsaved(mut self, unsaved: bool) -> Self {
        if unsaved && let Some(detail) = self.detail_mut() {
            detail.history_unsaved = Some(true);
        }
        self
    }

    fn detail_mut(&mut self) -> Option<&mut ErrorDetail> {
        match self {
            Self::NotFound { detail, .. }
            | Self::Validation { detail, .. }
            | Self::Conflict { detail, .. }
            | Self::Io { detail, .. }
            | Self::Database { detail, .. }
            | Self::Internal { detail, .. }
            | Self::Unsupported { detail, .. }
            | Self::ConfirmationRequired { detail, .. }
            | Self::Network { detail, .. } => Some(detail),
            Self::Cancelled { .. } => None,
        }
    }

    /// This validation error naming every one of `keys` (a no-op for the other kinds).
    #[must_use]
    pub fn with_keys(mut self, keys: Vec<String>) -> Self {
        if let Self::Validation { keys: slot, .. } = &mut self {
            *slot = (!keys.is_empty()).then(|| Box::new(keys));
        }
        self
    }

    /// The stable reason of this failure, when it has one.
    #[must_use]
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::NotFound { detail, .. }
            | Self::Validation { detail, .. }
            | Self::Conflict { detail, .. }
            | Self::Io { detail, .. }
            | Self::Database { detail, .. }
            | Self::Internal { detail, .. }
            | Self::Unsupported { detail, .. }
            | Self::ConfirmationRequired { detail, .. }
            | Self::Network { detail, .. } => detail.code,
            _ => None,
        }
    }

    /// The message to show the user.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Cancelled { message, .. }
            | Self::NotFound { message, .. }
            | Self::Validation { message, .. }
            | Self::Conflict { message, .. }
            | Self::ConfirmationRequired { message, .. }
            | Self::Io { message, .. }
            | Self::Database { message, .. }
            | Self::Network { message, .. }
            | Self::Unsupported { message, .. }
            | Self::Internal { message, .. } => message,
        }
    }

    /// Whether the operation was cancelled rather than failed.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        let code = io_code(error.kind());
        let error = Self::io(error.to_string());
        match code {
            Some(code) => error.coded(code, None),
            None => error,
        }
    }
}

/// The [`ErrorCode`] an I/O failure of `kind` carries, for the kinds a UI explains on its own.
#[must_use]
pub fn io_code(kind: std::io::ErrorKind) -> Option<ErrorCode> {
    use std::io::ErrorKind;
    match kind {
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => {
            Some(ErrorCode::PermissionDenied)
        }
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded => Some(ErrorCode::DiskFull),
        _ => None,
    }
}

impl From<serde_json::Error> for AppError {
    fn from(error: serde_json::Error) -> Self {
        Self::validation(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_with_a_kind_tag_and_keeps_the_plain_message() {
        let error = AppError::not_found("request r1 not found");
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"kind":"NotFound","message":"request r1 not found"}"#
        );
        assert_eq!(error.to_string(), "request r1 not found");
        assert!(AppError::cancelled().is_cancelled());
        assert_eq!(AppError::cancelled().message(), "Request cancelled");
    }

    #[test]
    fn an_error_without_a_code_serializes_exactly_as_before() {
        let json = serde_json::to_value(AppError::validation("bad")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "Validation", "message": "bad"})
        );
        let old: AppError =
            serde_json::from_str(r#"{"kind":"Validation","message":"bad"}"#).unwrap();
        assert_eq!(old, AppError::validation("bad"));
        assert_eq!(old.code(), None);
        // Setting a code on a kind that carries none changes nothing.
        let kept = AppError::cancelled().coded(ErrorCode::InvalidUrl, None);
        assert_eq!(kept, AppError::cancelled());
        assert_eq!(
            AppError::cancelled().with_history_unsaved(true),
            AppError::cancelled()
        );
    }

    #[test]
    fn the_history_mark_survives_a_later_code_and_is_absent_unless_set() {
        let error = AppError::network("refused")
            .with_history_unsaved(true)
            .coded(ErrorCode::GuardRefused, Some("10.0.0.1".to_owned()));
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["history_unsaved"], true);
        assert_eq!(json["code"], "guard_refused");
        let kept =
            serde_json::to_value(AppError::network("x").with_history_unsaved(false)).unwrap();
        assert!(kept.get("history_unsaved").is_none(), "{kept}");
    }

    #[test]
    fn io_failures_a_user_can_act_on_carry_a_code() {
        use std::io::{Error, ErrorKind};
        let denied = AppError::from(Error::from(ErrorKind::PermissionDenied));
        assert_eq!(denied.code(), Some(ErrorCode::PermissionDenied));
        assert!(matches!(denied, AppError::Io { .. }));
        let full = AppError::from(Error::from(ErrorKind::StorageFull));
        assert_eq!(full.code(), Some(ErrorCode::DiskFull));
        assert_eq!(
            AppError::from(Error::from(ErrorKind::NotFound)).code(),
            None
        );
    }
}
