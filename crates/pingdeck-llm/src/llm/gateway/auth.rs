//! Who is calling: token extraction, client lookup and the identity a request runs under.
//!
//! Single responsibility: client token authentication. The token is read from `Authorization: Bearer`,
//! `x-api-key`, `x-goog-api-key` or `?key=` (a repeated header presents nothing), hashed once and
//! compared with every cached client hash in constant time ([`crate::llm::store::match_token`]).
//! The outcome is an [`Identity`] or a refusal; a missing, unknown and revoked token are told
//! apart only in the log (`errors::Refusal`). Presented tokens that no active client owns are
//! rate limited (`limiter`), and a client's `last_used` is written at most once per 30 s.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use hyper::Response;

use crate::llm::runtime::{LlmRuntime, now_ms};
use crate::llm::store::{self, GatewayClient, TokenLookup};
use crate::llm::wire::Protocol;
use pingdeck_base::error::AppError;

use super::errors::{Refusal, error_response};
use super::limiter::Verdict;
use super::server::{Context, GatewayBody};

/// Name the log shows for a request without a (known) token when `require_token` is off.
pub(super) const ANONYMOUS: &str = "Anonymous";

/// Tokens kept for redaction: the plaintext of active client tokens seen on the request
/// path. Bounded; the oldest is forgotten first.
const MAX_SEEN_TOKENS: usize = 64;

/// Why a token was refused (the log row says it; the client cannot tell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reason {
    /// No token was presented.
    Missing,
    /// No client has this token.
    Unknown,
    /// The client was revoked.
    Revoked,
}

impl Reason {
    /// The `error` text of the log row.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "token_missing",
            Self::Unknown => "token_unknown",
            Self::Revoked => "token_revoked",
        }
    }
}

/// The caller of a request.
#[derive(Debug, Clone)]
pub(super) enum Identity {
    /// Not authenticated (yet): the guards and `/health`.
    Pending,
    /// `require_token` is off and no known token was presented: every alias is allowed.
    Anonymous,
    /// An active client.
    Client(Arc<GatewayClient>),
    /// A replay of log row `of`: in-process, no token, every model allowed.
    Replay {
        /// The row that is re-sent.
        of: i64,
    },
}

impl Identity {
    /// The client record, when there is one.
    pub(super) fn client(&self) -> Option<&GatewayClient> {
        match self {
            Self::Client(client) => Some(client),
            Self::Pending | Self::Anonymous | Self::Replay { .. } => None,
        }
    }

    /// Whether the caller may use `model` (an alias or a `provider/model` id).
    pub(super) fn allows(&self, model: &str) -> bool {
        self.client().is_none_or(|client| client.allows(model))
    }
}

/// When each client's `last_used` was last written.
#[derive(Default)]
pub(super) struct TouchBook(Mutex<HashMap<String, Instant>>);

impl TouchBook {
    /// Whether `client_id` is due for a write now (and records that it is being written).
    fn due(&self, client_id: &str, now: Instant) -> bool {
        const EVERY: Duration = Duration::from_millis(store::TOUCH_INTERVAL_MS.unsigned_abs());
        let mut book = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match book.get(client_id) {
            Some(at) if now.saturating_duration_since(*at) < EVERY => false,
            _ => {
                book.insert(client_id.to_owned(), now);
                true
            }
        }
    }
}

/// The plaintext tokens of recently seen active clients, handed to the log writer as redaction
/// needles.
#[derive(Default)]
pub(crate) struct SeenTokens(Mutex<Vec<String>>);

impl SeenTokens {
    pub(super) fn note(&self, token: &str) {
        let mut seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if seen.iter().any(|known| known == token) {
            return;
        }
        if seen.len() >= MAX_SEEN_TOKENS {
            seen.remove(0);
        }
        seen.push(token.to_owned());
    }

    /// The tokens seen so far.
    pub(super) fn snapshot(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The access token a request carries: Bearer, `x-api-key`, `x-goog-api-key`, or `?key=`.
/// A request that repeats one of the three headers is ambiguous and presents nothing.
pub(super) fn presented_token(request: &hyper::http::request::Parts) -> Option<String> {
    let header = |name: &str| {
        request
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    let repeated = ["authorization", "x-api-key", "x-goog-api-key"]
        .iter()
        .any(|name| request.headers.get_all(*name).iter().nth(1).is_some());
    if repeated {
        return None;
    }
    if let Some(auth) = header("authorization") {
        let auth = auth.trim();
        if auth.len() > 7 && auth[..7].eq_ignore_ascii_case("bearer ") {
            return Some(auth[7..].trim().to_owned());
        }
    }
    if let Some(key) = header("x-api-key").or_else(|| header("x-goog-api-key")) {
        return Some(key.trim().to_owned());
    }
    request
        .uri
        .query()
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes()).find(|(name, _)| name == "key")
        })
        .map(|(_, value)| value.into_owned())
}

/// Writes `last_used` of `client_id` on the blocking pool without waiting for it: the request
/// never waits on the database (a locked one would stall it up to the busy timeout). Best
/// effort: a failed write is dropped, the in-memory throttle already allows the next try 30 s on.
fn touch(llm: &Arc<LlmRuntime>, client_id: String) {
    let llm = Arc::clone(llm);
    drop(tokio::task::spawn_blocking(move || {
        let connection = llm.db().open()?;
        store::touch_client(&connection, &client_id, now_ms())
    }));
}

/// Whether the request is the Verify test request: it comes from loopback and carries a secret
/// the core minted (one shot, see [`crate::llm::client_watch`]).
fn is_verify_request(context: &Context, parts: &hyper::http::request::Parts) -> bool {
    context.peer.ip().is_loopback()
        && parts
            .headers
            .get(crate::llm::client_watch::VERIFY_HEADER)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|secret| context.llm.services.client_watch.verify.consume(secret))
}

/// Authenticates the request of `context`: sets its identity, or returns the refusal response.
///
/// A token that matches no active client (unknown, rotated away, revoked) is a 401 whatever
/// `require_token` says; only a request that presents no token at all (an empty bearer value
/// counts as none) is the anonymous caller when `require_token` is off. Refusals of a presented
/// token are counted per (peer, token hash) on loopback and per peer in LAN mode; a request
/// without a token is refused without counting, and a valid token is never throttled.
pub(super) async fn authorize(
    context: &Context,
    parts: &hyper::http::request::Parts,
    route: &super::handlers::Route,
) -> Option<Response<GatewayBody>> {
    let family = route.family();
    // Extra allowed hosts require tokens whatever `require_token` says.
    let require = context.llm.settings().ai.gateway.token_required();
    let token = presented_token(parts).filter(|token| !token.is_empty());
    let outcome = match &token {
        None => Ok(Err(Reason::Missing)),
        Some(token) => context.llm.client_hashes().await.map(|hashes| {
            match store::match_token(&hashes, token) {
                TokenLookup::Active(client) => Ok(client),
                TokenLookup::Revoked => Err(Reason::Revoked),
                TokenLookup::Unknown => Err(Reason::Unknown),
            }
        }),
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => return Some(unavailable(&error, family)),
    };
    let reason = match outcome {
        Ok(client) => {
            if let Some(token) = &token {
                context.shared.seen_tokens.note(token);
            }
            if is_verify_request(context, parts) {
                // The Verify test request of the client use case: logged
                // as a probe, never a "first request" and never a `last_used` write.
                context.mark_probe();
            } else if context.shared.touches.due(&client.id, Instant::now()) {
                // The first-request mark waits for the logged outcome (see `logwriter`).
                touch(&context.llm, client.id.clone());
            }
            context.set_identity(Identity::Client(Arc::new(client)));
            return None;
        }
        Err(Reason::Missing) if !require => {
            context.set_identity(Identity::Anonymous);
            return None;
        }
        Err(reason) => reason,
    };
    context
        .shared
        .auth_failures
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let peer = context.peer.ip();
    // The key never holds the token itself, only its hash.
    let key = token.as_deref().map(|token| {
        if context.shared.lan {
            peer.to_string()
        } else {
            format!(
                "{peer}#{}",
                pingdeck_base::crypto::sha256_hex(token.as_bytes())
            )
        }
    });
    let verdict = context
        .shared
        .throttle
        .failed(peer, key.as_deref(), Instant::now());
    let (refusal, log) = match verdict {
        Verdict::Unauthorized { log } => (Refusal::Token(reason), log),
        Verdict::Throttled { retry_after_s, log } => (Refusal::Throttled { retry_after_s }, log),
    };
    Some(context.refuse(&refusal, route, None, log))
}

/// The answer when the client list cannot be read: a server error, not a 401 (the caller did
/// nothing wrong).
fn unavailable(error: &AppError, family: Option<Protocol>) -> Response<GatewayBody> {
    error_response(
        family,
        500,
        &format!(
            "The gateway cannot read its client list: {}",
            error.message()
        ),
        "api_error",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(request: hyper::Request<()>) -> hyper::http::request::Parts {
        request.into_parts().0
    }

    #[test]
    fn the_token_comes_from_each_carrier_and_repeats_present_nothing() {
        let bearer = parts(
            hyper::Request::get("/v1/models")
                .header("authorization", "Bearer  pd_x ")
                .body(())
                .unwrap(),
        );
        assert_eq!(presented_token(&bearer).as_deref(), Some("pd_x"));
        let api_key = parts(
            hyper::Request::get("/v1/models")
                .header("x-api-key", "pd_y")
                .body(())
                .unwrap(),
        );
        assert_eq!(presented_token(&api_key).as_deref(), Some("pd_y"));
        let goog = parts(
            hyper::Request::get("/v1beta/models")
                .header("x-goog-api-key", "pd_z")
                .body(())
                .unwrap(),
        );
        assert_eq!(presented_token(&goog).as_deref(), Some("pd_z"));
        let query = parts(
            hyper::Request::get("/v1beta/models?a=1&key=pd_q%2B")
                .body(())
                .unwrap(),
        );
        assert_eq!(presented_token(&query).as_deref(), Some("pd_q+"));
        let repeated = parts(
            hyper::Request::get("/v1/models")
                .header("x-api-key", "a")
                .header("x-api-key", "a")
                .body(())
                .unwrap(),
        );
        assert_eq!(presented_token(&repeated), None);
        let none = parts(hyper::Request::get("/v1/models").body(()).unwrap());
        assert_eq!(presented_token(&none), None);
    }

    #[test]
    fn last_used_is_due_once_per_thirty_seconds_per_client() {
        let book = TouchBook::default();
        let start = Instant::now();
        assert!(book.due("a", start));
        for second in 1..30 {
            assert!(!book.due("a", start + Duration::from_secs(second)));
        }
        assert!(book.due("a", start + Duration::from_secs(30)));
        assert!(book.due("b", start), "per client");
    }

    #[test]
    fn seen_tokens_are_bounded_and_deduplicated() {
        let seen = SeenTokens::default();
        for index in 0..MAX_SEEN_TOKENS + 5 {
            seen.note(&format!("token-{index}"));
            seen.note("token-0");
        }
        let tokens = seen.snapshot();
        assert_eq!(tokens.len(), MAX_SEEN_TOKENS);
        assert!(tokens.contains(&format!("token-{}", MAX_SEEN_TOKENS + 4)));
    }
}
