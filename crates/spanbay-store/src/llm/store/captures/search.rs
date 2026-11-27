//! Body search over the stored captures: newest first, in bounded steps, stopped by a
//! [`SearchBudget`] of rows, time and a cancel token.

use std::time::{Duration, Instant};

use rusqlite::{Connection, params};

use crate::sqlite::{db, i64_from_usize};
use spanbay_base::cancel::CancelToken;
use spanbay_base::error::AppError;

/// Most log row ids one body search returns.
pub const MAX_SEARCH_RESULTS: usize = 5_000;

/// Newest capture rows one search step reads; the budget is checked between steps.
const SEARCH_BATCH_ROWS: usize = 100;

/// Most capture rows a body search reads (the newest ones); older captures are not searched.
pub const MAX_SEARCH_ROWS: usize = 20_000;

/// Longest a body search runs before it returns what it found.
pub const SEARCH_TIME_BUDGET: Duration = Duration::from_secs(10);

/// What bounds one body search: how many capture rows it may read, how long it may run and the
/// token that stops it. A bounded search returns the matches found so far, newest first.
#[derive(Debug, Clone)]
pub struct SearchBudget {
    max_rows: usize,
    deadline: Instant,
    cancel: CancelToken,
}

impl SearchBudget {
    /// The product's limits ([`MAX_SEARCH_ROWS`], [`SEARCH_TIME_BUDGET`]) from now on.
    #[must_use]
    pub fn new(cancel: CancelToken) -> Self {
        Self::with_limits(MAX_SEARCH_ROWS, SEARCH_TIME_BUDGET, cancel)
    }

    /// A budget of at most `max_rows` capture rows and `time` from now on.
    #[must_use]
    pub fn with_limits(max_rows: usize, time: Duration, cancel: CancelToken) -> Self {
        Self {
            max_rows,
            deadline: Instant::now() + time,
            cancel,
        }
    }
}

/// Ids of the log rows whose captured request or response bodies (both sides) contain `text`,
/// case-insensitively for ASCII, newest first, at most [`MAX_SEARCH_RESULTS`] (headers and stream
/// frames are not searched). `text` is only ever a bound parameter. The captures are read newest
/// first in steps of `SEARCH_BATCH_ROWS` rows; the search stops, with the matches found so far,
/// when `budget` runs out of rows or time or its token fires.
pub fn search_log_bodies(
    connection: &Connection,
    text: &str,
    budget: &SearchBudget,
) -> Result<Vec<i64>, AppError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut oldest_of_step = connection
        .prepare_cached(
            "SELECT MIN(request_log_id) FROM (
                 SELECT request_log_id FROM llm_debug_captures
                 WHERE request_log_id < ?1 ORDER BY request_log_id DESC LIMIT ?2)",
        )
        .map_err(db)?;
    let mut matches = connection
        .prepare_cached(
            "SELECT request_log_id FROM llm_debug_captures
             WHERE request_log_id >= ?2 AND request_log_id < ?3
               AND (instr(lower(coalesce(client_body_text, '')), lower(?1)) > 0
                 OR instr(lower(coalesce(upstream_req_body_text, '')), lower(?1)) > 0
                 OR instr(lower(coalesce(upstream_resp_body_text, '')), lower(?1)) > 0
                 OR instr(lower(coalesce(client_resp_body_text, '')), lower(?1)) > 0)
             ORDER BY request_log_id DESC",
        )
        .map_err(db)?;
    let mut found: Vec<i64> = Vec::new();
    let mut above = i64::MAX;
    let mut read = 0;
    while read < budget.max_rows
        && found.len() < MAX_SEARCH_RESULTS
        && !budget.cancel.is_cancelled()
        && Instant::now() < budget.deadline
    {
        let step = SEARCH_BATCH_ROWS.min(budget.max_rows - read);
        let oldest: Option<i64> = oldest_of_step
            .query_row(params![above, i64_from_usize(step)], |row| row.get(0))
            .map_err(db)?;
        let Some(oldest) = oldest else { break };
        let ids = matches
            .query_map(params![text, oldest, above], |row| row.get::<_, i64>(0))
            .map_err(db)?;
        for id in ids {
            found.push(id.map_err(db)?);
        }
        read += step;
        above = oldest;
    }
    found.truncate(MAX_SEARCH_RESULTS);
    Ok(found)
}
