//! The request budget of a scan: requests sent, slots reserved in priority order, and the
//! opt-in verification requests that are counted outside the budget.
//!
//! The engine reserves a slot per planned request before anything is sent, in tier order
//! (lower is more important). When the budget is full, a request may take the slot of the
//! least important reservation whose tier is higher than its own; a request that finds nothing
//! to take is dropped with the reason `budget`.

use super::{Exec, lock};

/// Tier of the model list (never dropped).
pub(in crate::llm::sniff) const TIER_MODELS: u8 = 1;
/// Tier of the primary route probe, which doubles as the key check.
pub(in crate::llm::sniff) const TIER_KEY_CHECK: u8 = 2;
/// Tier of the identify requests.
pub(in crate::llm::sniff) const TIER_IDENTIFY: u8 = 3;
/// Tier of a retry of anything less important than identify.
const TIER_RETRY: u8 = 8;
/// Tier of the first balance request.
pub(in crate::llm::sniff) const TIER_BALANCE: u8 = 9;
/// Tier of every further balance request.
pub(in crate::llm::sniff) const TIER_BALANCE_EXTRA: u8 = 10;

/// How a request counts toward `requests_used` and which budget slot it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::llm::sniff) enum Spend {
    /// Limited by the budget at priority `tier`; `hold` is a slot reserved for this request by
    /// [`Exec::hold`].
    Budgeted {
        /// Priority tier (lower is more important; [`TIER_MODELS`] is never dropped).
        tier: u8,
        /// The reserved slot, taken by the first send.
        hold: Option<u32>,
    },
    /// Not counted: local-runtime fingerprints.
    Free,
    /// Counted but outside the budget: the opt-in real requests.
    Verify,
}

impl Spend {
    /// An unreserved request of `tier`.
    pub(in crate::llm::sniff) const fn tier(tier: u8) -> Self {
        Self::Budgeted { tier, hold: None }
    }

    /// The same request with a reserved slot.
    pub(in crate::llm::sniff) const fn held(tier: u8, hold: Option<u32>) -> Self {
        Self::Budgeted { tier, hold }
    }

    /// The spend of an auth or rate-limit retry: it never owns the slot of the first send.
    /// Tiers 1 to 3 (model list, key check, identify) keep their tier; every other retry is
    /// tier 8.
    pub(in crate::llm::sniff) const fn retry(self) -> Self {
        match self {
            Self::Budgeted { tier, .. } => Self::Budgeted {
                tier: if tier <= TIER_IDENTIFY {
                    tier
                } else {
                    TIER_RETRY
                },
                hold: None,
            },
            other => other,
        }
    }
}

#[derive(Debug)]
struct Hold {
    id: u32,
    tier: u8,
}

/// The budget ledger: requests sent within the budget, verification requests sent outside
/// it, and the slots still reserved.
#[derive(Debug, Default)]
pub(super) struct Ledger {
    used: u32,
    verified: u32,
    next: u32,
    holds: Vec<Hold>,
    /// The slot reserved for the first balance request.
    balance_hold: Option<u32>,
}

/// Makes room for a request of `tier`: nothing to do while the budget has room, else the
/// reservation of the highest tier above `tier` is dropped (of equal tiers, the last reserved).
/// `false` when the budget is full and nothing may be taken.
fn make_room(ledger: &mut Ledger, budget: u32, tier: u8) -> bool {
    let live = u32::try_from(ledger.holds.len()).unwrap_or(u32::MAX);
    if ledger.used.saturating_add(live) < budget {
        return true;
    }
    let victim = ledger
        .holds
        .iter()
        .enumerate()
        .filter(|(_, hold)| hold.tier > tier)
        .max_by_key(|(_, hold)| hold.tier)
        .map(|(index, _)| index);
    match victim {
        Some(index) => {
            ledger.holds.remove(index);
            true
        }
        None => false,
    }
}

impl Exec<'_> {
    /// Reserves a slot for a request of `tier`; `None` when the budget is full and no less
    /// important reservation can be taken (the request is then dropped with `budget`).
    pub(in crate::llm::sniff) fn hold(&self, tier: u8) -> Option<u32> {
        let mut ledger = lock(&self.ledger);
        if !make_room(&mut ledger, self.budget, tier) {
            return None;
        }
        ledger.next += 1;
        let id = ledger.next;
        ledger.holds.push(Hold { id, tier });
        Some(id)
    }

    /// Gives back every reservation except `keep` (before the balance request).
    pub(in crate::llm::sniff) fn release_all_except(&self, keep: Option<u32>) {
        lock(&self.ledger)
            .holds
            .retain(|hold| Some(hold.id) == keep);
    }

    /// Stores the slot of the first balance request.
    pub(in crate::llm::sniff) fn set_balance_hold(&self, hold: Option<u32>) {
        lock(&self.ledger).balance_hold = hold;
    }

    /// The slot of the first balance request, once.
    pub(in crate::llm::sniff) fn take_balance_hold(&self) -> Option<u32> {
        lock(&self.ledger).balance_hold.take()
    }

    /// Gives the slot of the first balance request back (the balance step sends nothing).
    pub(in crate::llm::sniff) fn release_balance_hold(&self) {
        let mut ledger = lock(&self.ledger);
        if let Some(id) = ledger.balance_hold.take() {
            ledger.holds.retain(|hold| hold.id != id);
        }
    }

    /// Takes the request's share of the budget. A free request needs none; a verification
    /// request is counted without being limited by it (it is opt-in and costed separately).
    pub(super) fn reserve(&self, spend: Spend) -> bool {
        let mut ledger = lock(&self.ledger);
        match spend {
            Spend::Free => true,
            Spend::Verify => {
                ledger.verified += 1;
                true
            }
            Spend::Budgeted { tier, hold } => {
                let owned = hold.is_some_and(|id| {
                    let before = ledger.holds.len();
                    ledger.holds.retain(|held| held.id != id);
                    ledger.holds.len() != before
                });
                let granted = owned || make_room(&mut ledger, self.budget, tier);
                if granted {
                    ledger.used += 1;
                }
                granted
            }
        }
    }

    /// Requests sent so far (retries, balance and verification requests included).
    pub(in crate::llm::sniff) fn used(&self) -> u32 {
        let ledger = lock(&self.ledger);
        ledger.used.saturating_add(ledger.verified)
    }

    /// Opt-in verification requests sent so far: counted in [`Self::used`] but never against
    /// the budget, so they cannot crowd out the balance step.
    pub(in crate::llm::sniff) fn verified(&self) -> u32 {
        lock(&self.ledger).verified
    }

    /// Requests the budget still allows.
    pub(in crate::llm::sniff) fn remaining(&self) -> u32 {
        self.budget.saturating_sub(lock(&self.ledger).used)
    }

    /// `(id, tier)` of every live reservation, oldest first (tests).
    #[cfg(test)]
    pub(super) fn holds(&self) -> Vec<(u32, u8)> {
        lock(&self.ledger)
            .holds
            .iter()
            .map(|hold| (hold.id, hold.tier))
            .collect()
    }
}
