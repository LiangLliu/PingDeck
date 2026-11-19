//! The first-request watcher and the one-shot Verify secrets.
//!
//! Single responsibility: the two small in-memory facts the gateway's request path shares with
//! the client use cases. After Connect the use case *arms* a client; the gateway's log writer then records
//! the first finished request of the client with its status ([`FirstRequests::mark`]), which is
//! how "Waiting for the first request" turns into "Seen" or "Rejected" without a token. The Verify test request
//! carries a secret the core minted ([`VerifyGate::mint`]); the gateway accepts it once
//! ([`VerifyGate::consume`]) and then treats that request as a probe, so it never counts as a
//! first request, never touches `last_used` and is logged with `source = probe`.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use pingdeck_base::crypto::{constant_time_eq, hex_lower};
use pingdeck_base::error::AppError;

/// The request header that carries a Verify secret.
pub const VERIFY_HEADER: &str = "x-pingdeck-verify";

/// Verify secrets that may wait for their request at once; the oldest is forgotten first.
const MAX_PENDING_SECRETS: usize = 8;

/// The first request of an armed client the gateway answered: its id and the status it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenRequest {
    /// `rq_<16 hex>` of the request.
    pub request_id: String,
    /// The HTTP status the gateway answered with.
    pub status: u16,
}

impl SeenRequest {
    /// The gateway answered with a success status (model allowed, upstream 2xx).
    #[must_use]
    pub fn succeeded(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The first answered request of every armed client (recorded once the request is
/// finished and logged, so a refused model or an upstream error never counts as success).
#[derive(Debug, Default)]
pub struct FirstRequests(Mutex<HashMap<String, Option<SeenRequest>>>);

impl FirstRequests {
    /// Starts (or restarts) watching `client_id`: a request seen before does not count.
    pub fn arm(&self, client_id: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(client_id.to_owned(), None);
    }

    /// Stops watching `client_id` (the client was deleted).
    pub fn disarm(&self, client_id: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(client_id);
    }

    /// Records the finished request `request_id` of `client_id` with its `status` when the client
    /// is armed: the first one, and later the first success after a refusal. Called by the log
    /// writer for every logged gateway request (never for PingDeck's own probes).
    pub fn mark(&self, client_id: &str, request_id: &str, status: u16) {
        let mut slots = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(slot) = slots.get_mut(client_id) else {
            return;
        };
        let replace = slot
            .as_ref()
            .is_none_or(|seen| !seen.succeeded() && (200..300).contains(&status));
        if replace {
            *slot = Some(SeenRequest {
                request_id: request_id.to_owned(),
                status,
            });
        }
    }

    /// The first answered request of `client_id` since it was armed (a later success replaces a
    /// refusal).
    #[must_use]
    pub fn seen(&self, client_id: &str) -> Option<SeenRequest> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(client_id)
            .cloned()
            .flatten()
    }
}

/// The Verify secrets the core minted and the gateway has not yet seen.
#[derive(Debug, Default)]
pub struct VerifyGate(Mutex<Vec<String>>);

impl VerifyGate {
    /// A new secret for one Verify request: 128 random bits as hex.
    ///
    /// # Errors
    /// [`AppError::Internal`] without OS randomness.
    pub fn mint(&self) -> Result<String, AppError> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes)
            .map_err(|error| AppError::internal(format!("no OS randomness: {error}")))?;
        let secret = hex_lower(&bytes);
        let mut pending = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.len() >= MAX_PENDING_SECRETS {
            pending.remove(0);
        }
        pending.push(secret.clone());
        Ok(secret)
    }

    /// Whether `presented` is a pending secret; a pending secret is spent by the first match.
    pub fn consume(&self, presented: &str) -> bool {
        let mut pending = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mut found = None;
        for (index, secret) in pending.iter().enumerate() {
            if constant_time_eq(secret.as_bytes(), presented.as_bytes()) {
                found = Some(index);
            }
        }
        found.map(|index| pending.remove(index)).is_some()
    }

    /// Forgets `secret` (its request could not be sent).
    pub fn revoke(&self, secret: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|pending| pending != secret);
    }
}

/// Both pieces, as one field of [`crate::llm::services::LlmServices`].
#[derive(Debug, Default)]
pub struct ClientWatch {
    /// The first request of every armed client.
    pub first_requests: FirstRequests,
    /// The pending Verify secrets.
    pub verify: VerifyGate,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_first_request_is_kept_until_a_success_replaces_it() {
        let watch = FirstRequests::default();
        watch.mark("a", "rq_1", 200);
        assert_eq!(watch.seen("a"), None, "not armed: nothing recorded");
        watch.arm("a");
        watch.mark("a", "rq_2", 403);
        watch.mark("a", "rq_3", 502);
        let seen = watch.seen("a").unwrap();
        assert_eq!(
            (seen.request_id.as_str(), seen.succeeded()),
            ("rq_2", false)
        );
        watch.mark("a", "rq_4", 200);
        watch.mark("a", "rq_5", 200);
        assert_eq!(watch.seen("a").unwrap().request_id, "rq_4");
        watch.arm("a");
        assert_eq!(watch.seen("a"), None, "arming again restarts the watch");
    }

    #[test]
    fn a_verify_secret_is_spent_by_its_first_match_and_the_pending_list_is_bounded() {
        let gate = VerifyGate::default();
        let secret = gate.mint().unwrap();
        assert_eq!(secret.len(), 32);
        assert!(!gate.consume("not-a-secret"));
        assert!(gate.consume(&secret));
        assert!(!gate.consume(&secret), "one shot");
        let first = gate.mint().unwrap();
        for _ in 0..MAX_PENDING_SECRETS {
            gate.mint().unwrap();
        }
        assert!(!gate.consume(&first), "the oldest secret was forgotten");
        let last = gate.mint().unwrap();
        gate.revoke(&last);
        assert!(!gate.consume(&last));
    }
}
