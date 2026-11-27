//! Transport retry policy: attempt limits, exponential backoff and which failures are retryable.

use crate::executor::send::await_or_cancel;
use crate::executor::types::{ExecutionError, ExecutionResult, ExecutorOptions};
use spanbay_model::{HttpMethod, Request};
use std::future::Future;
use std::time::Duration;
use tokio::time::sleep;

/// Most retries honoured whatever `retry_count` asks for.
const MAX_RETRIES: u32 = 10;

/// Wait before the first retry; each further retry doubles it.
const RETRY_BACKOFF_BASE: Duration = Duration::from_millis(200);

const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(5);

/// Runs `attempt` until it succeeds or the retry budget is spent, backing off between tries.
///
/// A failure is retried only when the method is idempotent or the connection was never
/// established; a cancelled run and a guard refusal (a policy decision, not a transport fault)
/// are never retried.
pub(super) async fn execute_with_retries<F, Fut>(
    request: &Request,
    options: &ExecutorOptions,
    mut attempt: F,
) -> Result<ExecutionResult, ExecutionError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<ExecutionResult, ExecutionError>>,
{
    let max_retries = u32::from(request.network_options.retry_count).min(MAX_RETRIES);
    let idempotent = is_idempotent(request.method);
    let mut attempt_count = 0_u32;

    loop {
        options.check_cancelled()?;
        attempt_count += 1;
        match attempt().await {
            Ok(mut response) => {
                response.attempt_count = attempt_count;
                return Ok(response);
            }
            Err(error)
                if attempt_count <= max_retries
                    && is_retryable_execution_error(&error, idempotent) =>
            {
                await_or_cancel(options, sleep(retry_backoff(attempt_count))).await?;
            }
            Err(error) => return Err(error.with_attempt_count(attempt_count)),
        }
    }
}

/// Delay before retry number `retry_number` (1-based): 200 ms, 400 ms, 800 ms ... capped at 5 s.
pub(super) fn retry_backoff(retry_number: u32) -> Duration {
    let exponent = retry_number.saturating_sub(1).min(5);
    (RETRY_BACKOFF_BASE * 2_u32.pow(exponent)).min(MAX_RETRY_BACKOFF)
}

fn is_idempotent(method: HttpMethod) -> bool {
    matches!(
        method,
        HttpMethod::Get
            | HttpMethod::Head
            | HttpMethod::Options
            | HttpMethod::Put
            | HttpMethod::Delete
    )
}

fn is_retryable_execution_error(error: &ExecutionError, idempotent: bool) -> bool {
    matches!(
        error,
        ExecutionError::Network { connect_phase, .. } if idempotent || *connect_phase
    )
}
