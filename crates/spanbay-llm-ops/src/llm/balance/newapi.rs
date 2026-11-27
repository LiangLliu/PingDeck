//! The new-api family (new-api, Veloera, RixAPI, done-hub): the key's quota from
//! `/api/usage/token/`, or the account from `/api/user/self` when a console token and user id are
//! configured, converted with a `quota_per_unit` that was read, never assumed. `quota` of
//! `/api/user/self` is the remaining amount and `used_quota` the spent one; never subtracted.

use serde_json::Value;

use crate::llm::wire::GatewayFamily;

use super::error::{BalanceError, BalanceErrorKind};
use super::extract::{dig, dig_decimal, dig_str};
use super::fetch::{BalanceAuth, Fetched, Session, bearer};
use super::gateway::{GatewayFacts, Wanted, read_facts};
use super::report::{seal, usd_of};
use super::target::require_key;
use super::{AdapterId, BalanceReport, BalanceScope, BalanceTarget, BalanceUnit};

const USAGE_PATH: &str = "/api/usage/token/";
const SELF_PATH: &str = "/api/user/self";

/// A reading in quota units before the divisor is applied.
struct QuotaReading {
    unlimited: bool,
    remaining: Option<f64>,
    used: Option<f64>,
    total: Option<f64>,
    key_limit: Option<f64>,
    scope: BalanceScope,
    labels: Vec<String>,
}

/// The brand shown in labels.
fn brand(adapter: AdapterId, family: GatewayFamily) -> &'static str {
    match (adapter, family) {
        (AdapterId::DoneHub, _) | (_, GatewayFamily::DoneHub) => "done-hub",
        (_, GatewayFamily::Veloera) => "Veloera",
        (_, GatewayFamily::RixApi) => "RixAPI",
        _ => "new-api",
    }
}

/// The header that carries the console user id for `family`, one name per family.
const fn user_id_header(family: GatewayFamily) -> Option<&'static str> {
    match family {
        GatewayFamily::NewApi | GatewayFamily::DoneHub => Some("New-API-User"),
        GatewayFamily::Veloera => Some("Veloera-User"),
        GatewayFamily::RixApi => Some("Rix-Api-User"),
        _ => None,
    }
}

/// Longest console user id accepted (a numeric database id).
const MAX_USER_ID_DIGITS: usize = 20;

/// The console credentials, when all three parts exist: token, numeric user id, header name.
fn console<'a>(target: &BalanceTarget<'a>) -> Option<(&'a str, &'a str, &'static str)> {
    let token = target.management_token.filter(|token| !token.is_empty())?;
    let user_id = target.api_user_id.map(str::trim).filter(|id| {
        !id.is_empty() && id.len() <= MAX_USER_ID_DIGITS && id.bytes().all(|b| b.is_ascii_digit())
    })?;
    Some((token, user_id, user_id_header(target.family)?))
}

/// A body that says it failed (`code: false` or `success: false`), with the vendor message.
fn failure_message(body: &Value) -> Option<String> {
    let failed = ["code", "success"]
        .iter()
        .any(|field| body.get(field) == Some(&Value::Bool(false)));
    failed.then(|| {
        dig_str(body, &["message"])
            .unwrap_or("The service reported a failure")
            .to_owned()
    })
}

/// The key's reading of `/api/usage/token/`: the key quota is a [`BalanceScope::Key`] reading;
/// when the key may query the account (`user_quota`) the reading is the account balance and the
/// key quota stays a key limit; an unlimited key without `user_quota` is unlimited.
fn token_reading(
    fetched: &Fetched,
    brand: &str,
    session: &Session<'_>,
) -> Result<QuotaReading, BalanceError> {
    if let Some(message) = failure_message(&fetched.json) {
        return Err(BalanceError::schema(
            session.safe_text(&message),
            USAGE_PATH,
        ));
    }
    let data = dig(&fetched.json, &["data"])
        .filter(|data| data.is_object())
        .ok_or_else(|| BalanceError::schema("data is missing", USAGE_PATH))?;
    let unlimited_key = dig(data, &["unlimited_quota"]).and_then(Value::as_bool) == Some(true);
    let key = dig_decimal(data, &["total_available"]);
    let account = dig_decimal(data, &["user_quota", "total_available"]);
    if !unlimited_key && key.is_none() {
        return Err(BalanceError::schema(
            "data.total_available is missing",
            USAGE_PATH,
        ));
    }
    let mut reading = QuotaReading {
        scope: BalanceScope::Key,
        unlimited: false,
        remaining: None,
        used: None,
        total: None,
        key_limit: None,
        labels: Vec::new(),
    };
    classify(&mut reading, unlimited_key, key, account, brand);
    Ok(reading)
}

/// Fills `reading` from the key quota, the account balance and the unlimited flag.
fn classify(
    reading: &mut QuotaReading,
    unlimited_key: bool,
    key: Option<f64>,
    account: Option<f64>,
    brand: &str,
) {
    match (unlimited_key, key, account) {
        // The key may query the account (`allow_user_quota_query`): that is the balance, and the
        // key's own quota stays a key cap beside it.
        (true, _, Some(account)) => {
            reading.scope = BalanceScope::Account;
            reading.remaining = Some(account);
            reading
                .labels
                .push(format!("{brand}: unlimited key, account balance shown"));
        }
        (true, _, None) => {
            reading.unlimited = true;
            reading.labels.push(format!("{brand}: unlimited key"));
        }
        (false, Some(key), Some(account)) => {
            reading.scope = BalanceScope::Account;
            reading.remaining = Some(account);
            reading.key_limit = Some(key);
            reading
                .labels
                .push(format!("{brand}: account balance, key quota as key limit"));
        }
        (false, Some(key), None) => {
            reading.remaining = Some(key);
            reading.labels.push("Key remaining".to_owned());
        }
        (false, None, _) => {}
    }
}

/// The account reading of `/api/user/self`: `quota` is remaining, `used_quota` is spent.
fn self_reading(
    fetched: &Fetched,
    brand: &str,
    session: &Session<'_>,
) -> Result<QuotaReading, BalanceError> {
    if let Some(message) = failure_message(&fetched.json) {
        return Err(BalanceError::new(
            BalanceErrorKind::ConsoleTokenExpired,
            session.safe_text(&message),
        )
        .at(SELF_PATH));
    }
    let data = dig(&fetched.json, &["data"])
        .ok_or_else(|| BalanceError::schema("data is missing", SELF_PATH))?;
    let quota = dig_decimal(data, &["quota"])
        .ok_or_else(|| BalanceError::schema("data.quota is missing", SELF_PATH))?;
    let used = dig_decimal(data, &["used_quota"]);
    Ok(QuotaReading {
        scope: BalanceScope::Account,
        unlimited: false,
        remaining: Some(quota),
        used,
        total: used.map(|used| quota + used),
        key_limit: None,
        labels: vec![format!("{brand}: quota is remaining")],
    })
}

/// A reading, the endpoint label it came from and the answer it was read from.
type SourcedReading = (String, QuotaReading, Fetched);

/// Reads the account through the console (`/api/user/self` with the user id header).
async fn console_reading(
    session: &mut Session<'_>,
    (token, user_id, header): (&str, &str, &'static str),
    brand: &str,
) -> Result<SourcedReading, BalanceError> {
    let url = session.url(SELF_PATH);
    let fetched = session
        .get(
            &url,
            vec![bearer(token), (header.to_owned(), user_id.to_owned())],
        )
        .await
        .map_err(BalanceError::into_console_rejected)?;
    let reading = self_reading(&fetched, brand, session)?;
    Ok((session.endpoint_label(&url), reading, fetched))
}

/// Reads the key's quota (`/api/usage/token/`).
async fn key_reading(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    brand: &str,
) -> Result<SourcedReading, BalanceError> {
    let key = require_key(target, BalanceAuth::Bearer)?;
    let url = session.url(USAGE_PATH);
    let fetched = session.get(&url, vec![bearer(key)]).await?;
    let reading = token_reading(&fetched, brand, session)?;
    Ok((session.endpoint_label(&url), reading, fetched))
}

/// The unsealed report of `reading`, converted with the divisor of `facts`.
fn to_report(reading: QuotaReading, path: String, facts: GatewayFacts) -> BalanceReport {
    let mut report = BalanceReport {
        scope: reading.scope,
        source_endpoint: Some(path),
        labels: reading.labels,
        ..BalanceReport::default()
    };
    if reading.unlimited {
        report.unit = BalanceUnit::Unlimited;
    } else {
        report.unit = BalanceUnit::Quota;
        report.remaining = reading.remaining;
        report.used = reading.used;
        report.total = reading.total;
        report.quota_per_unit = facts.quota_per_unit;
        report.remaining_usd = reading
            .remaining
            .and_then(|remaining| usd_of(BalanceUnit::Quota, remaining, facts.quota_per_unit));
        report.key_limit_remaining_usd = reading
            .key_limit
            .and_then(|limit| usd_of(BalanceUnit::Quota, limit, facts.quota_per_unit));
    }
    report
}

/// Reads the balance of a new-api family gateway.
///
/// # Errors
/// The failure classes of [`Session::get`]; a rejected console token is
/// [`BalanceErrorKind::ConsoleTokenExpired`]; a body without the expected fields is a mismatch.
pub(super) async fn query(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    adapter: AdapterId,
    now_ms: i64,
) -> Result<BalanceReport, BalanceError> {
    let brand = brand(adapter, target.family);
    let (path, reading, fetched) = match console(target) {
        Some(console) => console_reading(session, console, brand).await?,
        None => key_reading(session, target, brand).await?,
    };
    let wanted = Wanted {
        quota_per_unit: !reading.unlimited,
        display_in_currency: false,
    };
    let facts = read_facts(session, target, wanted).await?;
    let report = to_report(reading, path, facts);
    Ok(seal(report, &fetched.json, session.secrets(), now_ms))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fetched(body: &Value) -> Fetched {
        Fetched {
            text: body.to_string(),
            json: body.clone(),
        }
    }

    fn token(body: &Value) -> Result<QuotaReading, BalanceError> {
        let http = crate::llm::safe_http::scripted::ScriptedHttp::new();
        let cancel = spanbay_base::cancel::CancelToken::new();
        let target = crate::llm::balance::tests::target(GatewayFamily::NewApi);
        let session = Session::new(&http, &cancel, "https://x.test".to_owned(), &target);
        token_reading(&fetched(body), "new-api", &session)
    }

    #[test]
    fn key_quota_alone_is_the_remaining() {
        let reading = token(
            &json!({"code": true, "data": {"total_available": 6200000, "unlimited_quota": false}}),
        )
        .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            (reading.remaining, reading.unlimited, reading.key_limit),
            (Some(6_200_000.0), false, None)
        );
    }

    #[test]
    fn a_readable_account_is_the_balance_and_the_key_quota_is_a_key_limit() {
        for (key, account) in [(2_000_000.0, 500_000.0), (500_000.0, 2_000_000.0)] {
            let reading = token(&json!({"code": true, "data": {"total_available": key, "user_quota": {"total_available": account}}}))
                .unwrap_or_else(|e| panic!("{e:?}"));
            assert_eq!(reading.scope, BalanceScope::Account);
            assert_eq!(
                (reading.remaining, reading.key_limit),
                (Some(account), Some(key))
            );
        }
        let key_only = token(&json!({"code": true, "data": {"total_available": 7}}))
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(key_only.scope, BalanceScope::Key);
    }

    #[test]
    fn an_unlimited_key_shows_the_account_or_is_unlimited() {
        let account = token(&json!({"code": true, "data": {"total_available": 0, "unlimited_quota": true, "user_quota": {"total_available": 3000000}}}))
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            (account.remaining, account.unlimited),
            (Some(3_000_000.0), false)
        );
        let unlimited =
            token(&json!({"code": true, "data": {"total_available": 0, "unlimited_quota": true}}))
                .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(unlimited.unlimited);
    }

    #[test]
    fn failures_and_missing_fields_are_mismatches() {
        for body in [
            json!({"code": false, "message": "invalid token"}),
            json!({"success": false, "message": "no"}),
            json!({"code": true}),
            json!({"code": true, "data": {"unlimited_quota": false}}),
        ] {
            assert_eq!(
                token(&body).err().map(|e| e.kind),
                Some(BalanceErrorKind::SchemaMismatch),
                "{body}"
            );
        }
    }

    #[test]
    fn console_needs_token_numeric_id_and_a_family_header() {
        let mut target = crate::llm::balance::tests::target(GatewayFamily::DoneHub);
        assert!(console(&target).is_none());
        target.management_token = Some("tok");
        assert!(console(&target).is_none(), "no user id");
        target.api_user_id = Some("12x");
        assert!(console(&target).is_none(), "id must be numeric");
        target.api_user_id = Some(" 12 ");
        assert_eq!(console(&target), Some(("tok", "12", "New-API-User")));
        target.family = GatewayFamily::Veloera;
        assert_eq!(console(&target).map(|c| c.2), Some("Veloera-User"));
        target.family = GatewayFamily::RixApi;
        assert_eq!(console(&target).map(|c| c.2), Some("Rix-Api-User"));
        target.family = GatewayFamily::LiteLlm;
        assert!(console(&target).is_none());
    }
}
