//! HTTP REST and GraphQL request execution: building, sending, retrying and capturing responses.

mod auth;
mod body;
mod client;
mod cookies;
mod oauth_flow;
mod probe;
mod request;
mod response;
mod retry;
mod send;
mod tls;
mod trace;
mod types;
mod view;

#[cfg(test)]
mod tests;

pub use auth::{OAuthClient, TokenGrant, auth_header_pairs, token_request};
pub use body::MAX_REQUEST_BODY_BYTES;
pub use client::{blocking_http_client, ensure_script_target_allowed, matching_client_certificate};
pub(crate) use cookies::{cookie_is_expired, cookie_matches_url, parse_set_cookie_header};
pub use request::{request_url, substitute_path_variables, with_default_scheme};
pub use response::{MAX_RESPONSE_BODY_BYTES, decode_body_text};
pub use send::{execute_request, execute_request_with_scopes};
pub use tls::{CertificatePassphrases, TlsMaterial, is_pkcs12_path};
pub use trace::{auth_templates, body_templates, template_tokens};
pub use types::{
    AuxiliaryNetwork, BodyIncomplete, CookieJarOutcome, CookieRejectReason, CookieReport,
    ExecutionCookie, ExecutionError, ExecutionHeader, ExecutionResult, ExecutorLimits,
    ExecutorOptions, OAuthTokenSource, RejectedCookie, ResolveGuard, SentRequest,
    StoredOAuthTokens,
};
pub use view::{SuggestedView, suggest_view};
