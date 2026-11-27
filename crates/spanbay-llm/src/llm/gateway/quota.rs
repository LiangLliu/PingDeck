//! Per-client limits of the gateway: expiry, spend and token budgets, requests per minute and
//! requests in flight.
//!
//! Single responsibility: decide, before a spending request is read, whether the calling client's
//! limits ([`ClientLimits`], stored with the client record) let it through. The checks run in a
//! fixed order: an expired token (401), a spend or token budget used up (402), the rolling minute
//! (429 with `Retry-After`), the requests in flight (429). Spend and tokens come from the usage
//! rollup of the client's gateway rows (local day and calendar month), read at most every
//! [`USAGE_TTL`] per client: the log writer stores rows asynchronously, so a budget can be passed
//! by the requests of those few seconds. A client that reaches 80 % of a budget, or is refused for
//! one, is announced once per local day ([`GatewayNoticeCode::ClientLimit`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::llm::events::LlmEvent;
use crate::llm::runtime::{LlmRuntime, now_ms};
use crate::llm::store::{self, ClientLimits, GatewayClient};
use crate::llm::usage::{UsageQuery, UsageSource, local_day, local_midnight_ms};

use super::errors::Refusal;
use super::status::GatewayNoticeCode;

/// How long a client's spend reading is reused.
pub(super) const USAGE_TTL: Duration = Duration::from_secs(10);
/// The rolling window of the requests-per-minute limit.
const MINUTE: Duration = Duration::from_secs(60);
/// The share of a budget that earns the early notice, in percent.
const WARN_PERCENT: i128 = 80;

/// What a client used in the current local day and month.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Spent {
    pub(super) day_cost: i64,
    pub(super) month_cost: i64,
    pub(super) day_tokens: u64,
    pub(super) month_tokens: u64,
}

/// The per-client state of one running gateway.
#[derive(Default)]
pub(super) struct ClientQuotas {
    minute: Mutex<HashMap<String, VecDeque<Instant>>>,
    in_flight: Mutex<HashMap<String, u32>>,
    spent: Mutex<HashMap<String, (Instant, Spent)>>,
    /// `(client id, local day, what)` already announced.
    warned: Mutex<HashSet<(String, String, &'static str)>>,
}

/// A request in flight of a client with a concurrency limit; dropping it frees the place.
pub(super) struct Slot {
    quotas: Arc<ClientQuotas>,
    client_id: String,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut in_flight = self
            .quotas
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = in_flight.get_mut(&self.client_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(&self.client_id);
            }
        }
    }
}

/// Which budget a reading is over, if any: `(what, used, limit)`.
pub(super) fn over_budget(
    limits: &ClientLimits,
    spent: &Spent,
) -> Option<(&'static str, i128, i128)> {
    over(limits, spent, 100)
}

/// The first budget at or above `percent` of its limit.
fn over(limits: &ClientLimits, spent: &Spent, percent: i128) -> Option<(&'static str, i128, i128)> {
    let money = |used: i64| i128::from(used);
    let tokens = |used: u64| i128::from(used);
    [
        (
            "daily budget",
            limits.daily_budget_micro.map(money),
            money(spent.day_cost),
        ),
        (
            "monthly budget",
            limits.monthly_budget_micro.map(money),
            money(spent.month_cost),
        ),
        (
            "daily tokens",
            limits.daily_tokens.map(tokens),
            tokens(spent.day_tokens),
        ),
        (
            "monthly tokens",
            limits.monthly_tokens.map(tokens),
            tokens(spent.month_tokens),
        ),
    ]
    .into_iter()
    .find_map(|(what, limit, used)| {
        let limit = limit?;
        (used * 100 >= limit * percent).then_some((what, used, limit))
    })
}

impl ClientQuotas {
    /// Admits a spending request of `client`, or says why not. `Ok(Some)` holds a place among the
    /// client's requests in flight until it is dropped.
    pub(super) async fn admit(
        self: &Arc<Self>,
        llm: &Arc<LlmRuntime>,
        client: &GatewayClient,
    ) -> Result<Option<Slot>, Refusal> {
        let limits = &client.limits;
        if limits.is_empty() {
            return Ok(None);
        }
        if limits.expired(now_ms()) {
            return Err(Refusal::Expired);
        }
        if limits.counts_usage() {
            let spent = self.spent(llm, &client.id, limits).await;
            if let Some((what, used, limit)) = over_budget(limits, &spent) {
                self.announce(
                    llm,
                    client,
                    what,
                    &format!("{what} reached ({used} of {limit})"),
                );
                return Err(Refusal::Budget { what });
            }
            if let Some((what, used, limit)) = over(limits, &spent, WARN_PERCENT) {
                self.announce(
                    llm,
                    client,
                    what,
                    &format!("{what} at {}%", used * 100 / limit.max(1)),
                );
            }
        }
        if let Some(rpm) = limits.rpm {
            self.count_minute(&client.id, rpm, Instant::now())?;
        }
        let Some(max) = limits.max_concurrent else {
            return Ok(None);
        };
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let count = in_flight.entry(client.id.clone()).or_default();
        if *count >= max {
            return Err(Refusal::ClientBusy);
        }
        *count += 1;
        Ok(Some(Slot {
            quotas: Arc::clone(self),
            client_id: client.id.clone(),
        }))
    }

    /// Counts one request in the client's rolling minute, or refuses it with the wait until the
    /// oldest counted request leaves the window.
    fn count_minute(&self, client_id: &str, rpm: u32, now: Instant) -> Result<(), Refusal> {
        let mut minute = self.minute.lock().unwrap_or_else(PoisonError::into_inner);
        let window = minute.entry(client_id.to_owned()).or_default();
        while window
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= MINUTE)
        {
            window.pop_front();
        }
        if window.len() >= usize::try_from(rpm).unwrap_or(usize::MAX) {
            let wait = window.front().map_or(MINUTE, |oldest| {
                MINUTE.saturating_sub(now.saturating_duration_since(*oldest))
            });
            return Err(Refusal::ClientRate {
                retry_after_s: wait.as_secs().max(1),
            });
        }
        window.push_back(now);
        Ok(())
    }

    /// The client's spend of the current day and month, from the cache while it is fresh.
    async fn spent(&self, llm: &Arc<LlmRuntime>, client_id: &str, limits: &ClientLimits) -> Spent {
        if let Some((at, spent)) = self
            .spent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(client_id)
            .copied()
            && at.elapsed() < USAGE_TTL
        {
            return spent;
        }
        let reader = Arc::clone(llm);
        let id = client_id.to_owned();
        let currency = limits.currency().to_owned();
        let spent = tokio::task::spawn_blocking(move || read_spent(&reader, &id, &currency))
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        self.spent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(client_id.to_owned(), (Instant::now(), spent));
        spent
    }

    /// Pushes the limit notice of `client` for `what`, once per local day.
    fn announce(&self, llm: &LlmRuntime, client: &GatewayClient, what: &'static str, detail: &str) {
        let day = local_day(now_ms(), llm.tz_offset_minutes());
        let fresh = self
            .warned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((client.id.clone(), day, what));
        if fresh {
            llm.emit(LlmEvent::GatewayNotice {
                code: GatewayNoticeCode::ClientLimit,
                notice: format!("{}: {detail}", client.name),
            });
        }
    }
}

/// Sums the client's gateway rollup rows of the current local month (and of today within it):
/// tokens of every row, cost of the rows priced in `currency`. `None` when the store cannot be
/// read (the request then passes: a broken read must not lock every limited client out).
fn read_spent(llm: &LlmRuntime, client_id: &str, currency: &str) -> Option<Spent> {
    let now = now_ms();
    let tz = llm.tz_offset_minutes();
    let today = local_day(now, tz);
    let month_start = format!("{}-01", today.get(..7)?);
    let from_ms = local_midnight_ms(&month_start, tz)?;
    let connection = llm.db().open().ok()?;
    let query = UsageQuery {
        provider_id: None,
        key_id: None,
        client_id: Some(client_id.to_owned()),
        from_ms,
        to_ms: now.saturating_add(1),
        group_by: Vec::new(),
        sources: vec![UsageSource::Gateway],
        tz_offset_minutes: tz,
        bucket_minutes: None,
    };
    let rows = store::usage_rows(&connection, &query).ok()?;
    let mut spent = Spent::default();
    for row in rows.iter().filter(|row| row.client_id == client_id) {
        let tokens = row.input_tokens.saturating_add(row.output_tokens);
        let cost = if row.currency == currency {
            row.cost_micro
        } else {
            0
        };
        spent.month_tokens = spent.month_tokens.saturating_add(tokens);
        spent.month_cost = spent.month_cost.saturating_add(cost);
        if row.day == today {
            spent.day_tokens = spent.day_tokens.saturating_add(tokens);
            spent.day_cost = spent.day_cost.saturating_add(cost);
        }
    }
    Some(spent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rolling_minute_refuses_with_the_wait_of_the_oldest_request() {
        let quotas = ClientQuotas::default();
        let start = Instant::now();
        for second in 0..3 {
            assert!(
                quotas
                    .count_minute("c", 3, start + Duration::from_secs(second))
                    .is_ok()
            );
        }
        match quotas.count_minute("c", 3, start + Duration::from_secs(20)) {
            Err(Refusal::ClientRate { retry_after_s }) => assert_eq!(retry_after_s, 40),
            other => panic!("expected a rate refusal, got {other:?}"),
        }
        assert!(quotas.count_minute("other", 3, start).is_ok(), "per client");
        assert!(
            quotas
                .count_minute("c", 3, start + Duration::from_secs(60))
                .is_ok()
        );
    }

    #[test]
    fn budgets_are_over_at_their_limit_and_warned_at_eighty_percent() {
        let limits = ClientLimits {
            daily_budget_micro: Some(1_000),
            monthly_tokens: Some(100),
            ..ClientLimits::default()
        };
        let mut spent = Spent {
            day_cost: 799,
            ..Spent::default()
        };
        assert_eq!(over(&limits, &spent, WARN_PERCENT), None);
        spent.day_cost = 800;
        assert_eq!(
            over(&limits, &spent, WARN_PERCENT).map(|o| o.0),
            Some("daily budget")
        );
        assert_eq!(over_budget(&limits, &spent), None);
        spent.month_tokens = 100;
        assert_eq!(
            over_budget(&limits, &spent).map(|o| o.0),
            Some("monthly tokens")
        );
    }
}
