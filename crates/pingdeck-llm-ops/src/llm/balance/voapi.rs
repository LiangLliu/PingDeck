//! VoAPI v2 console `GET /api/user/info`: `basicBalance + bindBalance` (decimal strings in the
//! account currency) read with the console token. A bare model key cannot read it, so a missing
//! token is [`BalanceErrorKind::NeedsConsoleToken`] before any request, and envelope code `2` (or a
//! 401) is [`BalanceErrorKind::ConsoleTokenExpired`].

use serde_json::Value;

use super::error::{BalanceError, BalanceErrorKind};
use super::extract::{currency_unit, dig, dig_decimal, dig_str};
use super::fetch::{Session, bearer};
use super::report::{seal, usd_of};
use super::{BalanceReport, BalanceTarget, BalanceUnit};

const PATH: &str = "/api/user/info";

/// Envelope code of an expired console session.
const CODE_EXPIRED: f64 = 2.0;

/// The sum of the present parts of `fields`, `None` when none is a number.
fn sum(data: &Value, fields: [&str; 2]) -> Option<f64> {
    fields
        .iter()
        .filter_map(|field| dig_decimal(data, &[field]))
        .reduce(|left, right| left + right)
}

/// The configured console token.
///
/// # Errors
/// [`BalanceErrorKind::NeedsConsoleToken`] when there is none.
fn console_token<'a>(target: &BalanceTarget<'a>) -> Result<&'a str, BalanceError> {
    target
        .management_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            BalanceError::new(
                BalanceErrorKind::NeedsConsoleToken,
                "VoAPI balances need a console token",
            )
        })
}

/// Whether the envelope `code` is `0`.
///
/// # Errors
/// [`BalanceErrorKind::ConsoleTokenExpired`] for code `2`, a mismatch with the vendor `msg`
/// (redacted) otherwise.
fn check_envelope(body: &Value, endpoint: &str, session: &Session<'_>) -> Result<(), BalanceError> {
    match dig_decimal(body, &["code"]) {
        Some(0.0) => Ok(()),
        Some(CODE_EXPIRED) => Err(BalanceError::new(
            BalanceErrorKind::ConsoleTokenExpired,
            "The console session has expired",
        )
        .at(endpoint)),
        _ => {
            let message = dig_str(body, &["msg"]).unwrap_or("code is not 0");
            Err(BalanceError::schema(session.safe_text(message), endpoint))
        }
    }
}

/// Reads the console balance.
///
/// # Errors
/// [`BalanceErrorKind::NeedsConsoleToken`] without a token (nothing is sent),
/// [`BalanceErrorKind::ConsoleTokenExpired`] for a rejected or expired session, the failure
/// classes of [`Session::get`] otherwise, and a mismatch for a body without a balance.
pub(super) async fn query(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    now_ms: i64,
) -> Result<BalanceReport, BalanceError> {
    let token = console_token(target)?;
    let url = session.url(PATH);
    let fetched = session
        .get(&url, vec![bearer(token)])
        .await
        .map_err(BalanceError::into_console_rejected)?;
    let endpoint = session.endpoint_label(&url);
    let body = &fetched.json;
    check_envelope(body, &endpoint, session)?;
    let data =
        dig(body, &["data"]).ok_or_else(|| BalanceError::schema("data is missing", &endpoint))?;
    let remaining = sum(data, ["basicBalance", "bindBalance"]).ok_or_else(|| {
        BalanceError::schema(
            "data.basicBalance and data.bindBalance are missing",
            &endpoint,
        )
    })?;
    let unit = dig_str(data, &["currency"]).map_or(BalanceUnit::Unsupported, currency_unit);
    let report = BalanceReport {
        source_endpoint: Some(endpoint),
        unit,
        remaining: Some(remaining),
        used: sum(data, ["usedBasicBalance", "usedBindBalance"]),
        remaining_usd: usd_of(unit, remaining, None),
        labels: vec!["VoAPI: unconfirmed".to_owned()],
        ..BalanceReport::default()
    };
    Ok(seal(report, body, session.secrets(), now_ms))
}
