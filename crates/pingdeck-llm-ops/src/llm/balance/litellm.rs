//! LiteLLM proxy `/key/info` and `/v2/user/info`: spend and budget in USD. `max_budget - spend`
//! of the key is a per-key cap (`key_limit_remaining_usd`, label "Budget"), never an account
//! balance; `max_budget = null` reads as "No budget set". The calling key needs no `key` parameter.

use serde_json::Value;

use super::error::{BalanceError, BalanceErrorKind};
use super::extract::{dig, dig_decimal, dig_str, parse_rfc3339_ms};
use super::fetch::{BalanceAuth, Fetched, Session, bearer};
use super::report::{raw_of, seal};
use super::target::require_key;
use super::{BalanceReport, BalanceScope, BalanceTarget, BalanceUnit, BalanceWindow};

const KEY_PATH: &str = "/key/info";
const USER_PATH: &str = "/v2/user/info";

/// The fields of a `/key/info` (nested under `info`) or `/v2/user/info` (flat) answer.
struct Budget {
    spend: Option<f64>,
    max_budget: Option<f64>,
    duration: Option<String>,
    reset_at_ms: Option<i64>,
}

fn budget(body: &Value) -> Budget {
    let fields = dig(body, &["info"])
        .filter(|info| info.is_object())
        .unwrap_or(body);
    Budget {
        spend: dig_decimal(fields, &["spend"]),
        max_budget: dig_decimal(fields, &["max_budget"]),
        duration: dig_str(fields, &["budget_duration"]).map(str::to_owned),
        reset_at_ms: dig_str(fields, &["budget_reset_at"]).and_then(parse_rfc3339_ms),
    }
}

/// The budget the report shows: the key's when it answered, else the user's. Returns the budget,
/// its label, its scope and the path it came from.
fn pick_cap(
    key_ok: Option<&Fetched>,
    user_ok: Option<&Fetched>,
) -> Option<(Budget, &'static str, BalanceScope, &'static str)> {
    match (key_ok, user_ok) {
        (Some(key), _) => Some((budget(&key.json), "Budget", BalanceScope::Key, KEY_PATH)),
        // The key's answer is a per-key cap; without it the user budget is shown, scoped to the
        // account.
        (None, Some(user)) => Some((
            budget(&user.json),
            "User budget",
            BalanceScope::Account,
            USER_PATH,
        )),
        (None, None) => None,
    }
}

/// Writes `cap` into the report: spend, `max_budget - spend` as the key cap with `label` and the
/// budget window, or "No budget set".
fn apply_budget(report: &mut BalanceReport, cap: Budget, label: &str) {
    report.used = cap.spend;
    match cap.max_budget {
        Some(max) => {
            let left = max - cap.spend.unwrap_or(0.0);
            report.key_limit_remaining_usd = Some(left);
            report.labels.push(label.to_owned());
            if let Some(duration) = cap.duration {
                report.rate_windows.push(BalanceWindow {
                    window: duration,
                    limit: Some(max),
                    used: cap.spend,
                    remaining: Some(left),
                    resets_at_ms: cap.reset_at_ms,
                });
            }
        }
        None => report.labels.push("No budget set".to_owned()),
    }
}

/// "User budget left …" of a user answer that has a budget.
fn user_budget_label(user: &Fetched) -> Option<String> {
    let user = budget(&user.json);
    user.max_budget
        .map(|max| format!("User budget left {}", max - user.spend.unwrap_or(0.0)))
}

/// The raw body of the answers that arrived: the single one, or both keyed by path.
fn raw_body(
    key_answer: Result<Fetched, BalanceError>,
    user_answer: Option<Result<Fetched, BalanceError>>,
) -> Value {
    let mut parts = Vec::new();
    if let Ok(key) = key_answer {
        parts.push((KEY_PATH, key.json));
    }
    if let Some(Ok(user)) = user_answer {
        parts.push((USER_PATH, user.json));
    }
    raw_of(parts)
}

/// Reads spend and budget of the calling key and its user.
///
/// # Errors
/// The failure classes of [`Session::get`] for `/key/info`; when it is not available the error
/// of `/v2/user/info`, and the `/key/info` error when neither answers.
pub(super) async fn query(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    now_ms: i64,
) -> Result<BalanceReport, BalanceError> {
    let key = require_key(target, BalanceAuth::Bearer)?;
    let key_url = session.url(KEY_PATH);
    let user_url = session.url(USER_PATH);
    let key_answer = match session.get(&key_url, vec![bearer(key)]).await {
        Err(error)
            if matches!(
                error.kind,
                BalanceErrorKind::Auth | BalanceErrorKind::Cancelled
            ) =>
        {
            return Err(error);
        }
        other => other,
    };
    let user_answer = if session.has_budget() {
        match session.get(&user_url, vec![bearer(key)]).await {
            Err(error) if error.is_cancelled() => return Err(error),
            other => Some(other),
        }
    } else {
        None
    };
    let key_ok = key_answer.as_ref().ok();
    let user_ok = user_answer.as_ref().and_then(|answer| answer.as_ref().ok());
    let Some((cap, label, scope, path)) = pick_cap(key_ok, user_ok) else {
        return Err(key_answer.err().unwrap_or_else(|| {
            BalanceError::new(BalanceErrorKind::NotFound, "No budget endpoint answered")
        }));
    };
    let mut report = BalanceReport {
        unit: BalanceUnit::Usd,
        source_endpoint: Some(session.endpoint_label(&session.url(path))),
        scope,
        ..BalanceReport::default()
    };
    apply_budget(&mut report, cap, label);
    // The user's own budget, when the key's answer is the one shown.
    if key_ok.is_some()
        && let Some(text) = user_ok.and_then(user_budget_label)
    {
        report.labels.push(text);
    }
    let raw = raw_body(key_answer, user_answer);
    Ok(seal(report, &raw, session.secrets(), now_ms))
}
