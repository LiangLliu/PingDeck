//! Credential header variants of the sniffer: the trial order per family, the winner of a scan
//! and the session memory.
//!
//! The next variant is tried only after a `401` (never after `403` or `404`), at most two per
//! family, and a `?key=` query is never used. [`AuthMemory`] keeps the winner per origin and
//! protocol for the app session (never persisted) so the next scan starts with it.
//!
//! The wire has no style for OpenAI's second convention (`api-key`) or LiteLLM's
//! `x-litellm-api-key`, so the second OpenAI-style variant is `x-api-key`, which both accept.

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

use crate::llm::wire::{AuthStyle, Protocol};

/// Protocols that share one credential convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum AuthFamily {
    /// Chat, Responses, Embeddings and the OpenAI model list.
    OpenAi,
    /// Messages and the Anthropic model list.
    Anthropic,
    /// Gemini native.
    Gemini,
}

impl AuthFamily {
    /// Every family, in the order the scan remembers their winners.
    pub(super) const ALL: [Self; 3] = [Self::OpenAi, Self::Anthropic, Self::Gemini];

    /// The family of `protocol`.
    pub(super) const fn of(protocol: Protocol) -> Self {
        match protocol {
            Protocol::AnthropicMessages => Self::Anthropic,
            Protocol::Gemini => Self::Gemini,
            Protocol::OpenaiChat
            | Protocol::OpenaiResponses
            | Protocol::Embeddings
            | Protocol::DecisionSystemone
            | Protocol::DecisionOpenai => Self::OpenAi,
        }
    }

    /// The protocol whose remembered style stands for the family (also what the headers are
    /// built for: Messages adds `anthropic-version`).
    pub(super) const fn representative(self) -> Protocol {
        match self {
            Self::OpenAi => Protocol::OpenaiChat,
            Self::Anthropic => Protocol::AnthropicMessages,
            Self::Gemini => Protocol::Gemini,
        }
    }

    /// The trial order without a remembered winner.
    const fn variants(self) -> [AuthStyle; 2] {
        match self {
            Self::OpenAi => [AuthStyle::Bearer, AuthStyle::XApiKey],
            Self::Anthropic => [AuthStyle::XApiKey, AuthStyle::Bearer],
            Self::Gemini => [AuthStyle::XGoogApiKey, AuthStyle::Bearer],
        }
    }
}

/// Auth styles that worked earlier in this app session, per origin and protocol.
#[derive(Debug, Default)]
pub struct AuthMemory {
    styles: Mutex<HashMap<(String, Protocol), AuthStyle>>,
}

impl AuthMemory {
    /// An empty memory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The style that worked for `protocol` on `origin`.
    #[must_use]
    pub fn get(&self, origin: &str, protocol: Protocol) -> Option<AuthStyle> {
        self.styles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(origin.to_owned(), protocol))
            .copied()
    }

    /// Remembers the style that worked.
    pub fn remember(&self, origin: &str, protocol: Protocol, style: AuthStyle) {
        self.styles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((origin.to_owned(), protocol), style);
    }
}

#[derive(Debug, Default)]
struct TrialState {
    winner: Option<AuthStyle>,
    rejected: Vec<AuthStyle>,
}

/// What a scan has learned about one family's credential header.
#[derive(Debug)]
pub(super) struct AuthTrial {
    /// The two variants in trial order.
    order: [AuthStyle; 2],
    state: Mutex<TrialState>,
    /// Held by the one request that is still finding out which variant works, so parallel
    /// requests of the family do not each spend a retry.
    pub(super) gate: tokio::sync::Mutex<()>,
}

impl AuthTrial {
    /// A trial for `family` that starts with `remembered` when there is one.
    pub(super) fn new(family: AuthFamily, remembered: Option<AuthStyle>) -> Self {
        let [first, second] = family.variants();
        let order = match remembered {
            Some(style) if style == second => [second, first],
            _ => [first, second],
        };
        Self {
            order,
            state: Mutex::default(),
            gate: tokio::sync::Mutex::new(()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, TrialState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The style the next request uses: the winner, else the first one not rejected yet, else
    /// the first one (every variant was rejected).
    pub(super) fn current(&self) -> AuthStyle {
        let state = self.state();
        state
            .winner
            .or_else(|| {
                self.order
                    .iter()
                    .find(|style| !state.rejected.contains(style))
                    .copied()
            })
            .unwrap_or_else(|| {
                let [first, _] = self.order;
                first
            })
    }

    /// Whether more requests would learn nothing: a winner exists or every variant failed.
    pub(super) fn is_settled(&self) -> bool {
        let state = self.state();
        state.winner.is_some() || state.rejected.len() >= self.order.len()
    }

    /// The winner so far.
    pub(super) fn winner(&self) -> Option<AuthStyle> {
        self.state().winner
    }

    /// Records a style that passed authentication.
    pub(super) fn accept(&self, style: AuthStyle) {
        self.state().winner.get_or_insert(style);
    }

    /// Records a `401` for `style`; returns whether another variant is left to try.
    pub(super) fn reject(&self, style: AuthStyle) -> bool {
        let mut state = self.state();
        if state.winner.is_none() && !state.rejected.contains(&style) {
            state.rejected.push(style);
        }
        state.winner.is_none() && state.rejected.len() < self.order.len()
    }
}

/// Whether a status shows that authentication passed (the request failed later, on its body or
/// the account, or succeeded).
pub(super) const fn status_passed_auth(status: u16) -> bool {
    matches!(status, 200..=299 | 400 | 402 | 422 | 429)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trial_order_and_memory_per_family() {
        let anthropic = AuthTrial::new(AuthFamily::Anthropic, None);
        assert_eq!(anthropic.current(), AuthStyle::XApiKey);
        assert!(anthropic.reject(AuthStyle::XApiKey));
        assert_eq!(anthropic.current(), AuthStyle::Bearer);
        anthropic.accept(AuthStyle::Bearer);
        assert!(anthropic.is_settled());
        assert_eq!(anthropic.winner(), Some(AuthStyle::Bearer));

        let remembered = AuthTrial::new(AuthFamily::Anthropic, Some(AuthStyle::Bearer));
        assert_eq!(remembered.current(), AuthStyle::Bearer);

        let all_rejected = AuthTrial::new(AuthFamily::Gemini, None);
        assert_eq!(all_rejected.current(), AuthStyle::XGoogApiKey);
        assert!(all_rejected.reject(AuthStyle::XGoogApiKey));
        assert!(!all_rejected.reject(AuthStyle::Bearer));
        assert!(all_rejected.is_settled());
        assert_eq!(all_rejected.winner(), None);

        // A remembered style outside the family is ignored.
        let foreign = AuthTrial::new(AuthFamily::OpenAi, Some(AuthStyle::XGoogApiKey));
        assert_eq!(foreign.current(), AuthStyle::Bearer);
    }

    #[test]
    fn memory_is_per_origin_and_protocol() {
        let memory = AuthMemory::new();
        memory.remember(
            "https://a.test",
            Protocol::AnthropicMessages,
            AuthStyle::Bearer,
        );
        assert_eq!(
            memory.get("https://a.test", Protocol::AnthropicMessages),
            Some(AuthStyle::Bearer)
        );
        assert_eq!(memory.get("https://a.test", Protocol::OpenaiChat), None);
        assert_eq!(
            memory.get("https://b.test", Protocol::AnthropicMessages),
            None
        );
    }

    #[test]
    fn only_post_auth_statuses_count_as_passed() {
        for status in [200, 204, 400, 402, 422, 429] {
            assert!(status_passed_auth(status), "{status}");
        }
        for status in [401, 403, 404, 405, 500, 503] {
            assert!(!status_passed_auth(status), "{status}");
        }
    }
}
