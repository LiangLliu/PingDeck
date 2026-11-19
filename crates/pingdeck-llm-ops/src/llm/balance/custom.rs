//! The run of the user's own balance query: the host rule, the request with the provider key in
//! the chosen auth style and the resolved headers, and the one-shot "Test query" (redacted body
//! plus preview). The stored form and its validation live in
//! `pingdeck_llm_domain::llm::balance_state`; resolved header values never become part of a
//! report.

use serde_json::Value;

use crate::llm::safe_http::SafeHttp;
use pingdeck_base::cancel::CancelToken;
use pingdeck_base::secrets::{redact_json, redact_secrets};
pub(super) use pingdeck_llm_domain::llm::balance_state::CustomBalanceQuery;

use super::error::{BalanceError, BalanceErrorKind};
use super::fetch::Session;
use super::report::seal;
use super::rules::{DivisorSpec, ExtractRules, UnitName, UnitSpec};
use super::target::{host_of, normalise_origin, require_key};
use super::{BalanceReport, BalanceTarget, MAX_BODY_BYTES};

/// What "Test query" returns to the form.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomQueryTest {
    /// HTTP status of the answer.
    pub status: u16,
    /// The body, redacted, at most 256 KiB.
    pub body: String,
    /// The reading the saved query would produce.
    pub preview: Option<BalanceReport>,
    /// Why there is no preview.
    pub preview_error: Option<BalanceError>,
}

/// The parts of a saved query's run that need the extraction rules and the session.
trait QueryRun {
    fn rules(&self) -> ExtractRules;

    /// The headers of the request to `self.url`, after every check and before anything is sent.
    fn prepare(
        &self,
        session: &mut Session<'_>,
        target: &BalanceTarget<'_>,
    ) -> Result<Vec<(String, String)>, BalanceError>;
}

impl QueryRun for CustomBalanceQuery {
    fn rules(&self) -> ExtractRules {
        ExtractRules {
            remaining: self.remaining_path.clone(),
            used: self.used_path.clone(),
            total: self.total_path.clone(),
            scope: self.scope.unwrap_or_default(),
            unit: UnitSpec::Fixed(self.unit),
            divisor: self.divisor.map(DivisorSpec::Number),
            unlimited_when: None,
            envelope: None,
            labels: Vec::new(),
        }
    }

    fn prepare(
        &self,
        session: &mut Session<'_>,
        target: &BalanceTarget<'_>,
    ) -> Result<Vec<(String, String)>, BalanceError> {
        let blocked = |message: String| BalanceError::new(BalanceErrorKind::Blocked, message);
        self.validate().map_err(blocked)?;
        if self.is_cross_host(&target.allowed_origins()) {
            if !self.cross_host {
                return Err(blocked(
                    "The query host is not one of the provider's endpoints and was not confirmed"
                        .to_owned(),
                ));
            }
            // The one place a key may leave the stored origins: the user confirmed this host.
            session
                .allow_origin(normalise_origin(&self.url).map_err(|error| blocked(error.message))?);
        }
        let mut headers = Vec::new();
        if let Some(header) = self.auth.header(require_key(target, self.auth)?) {
            headers.push(header);
        }
        for declared in &self.headers {
            let value = self
                .resolved_headers
                .0
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&declared.name))
                .map(|(_, value)| value)
                .ok_or_else(|| {
                    blocked(format!(
                        "Header {} has no value; nothing was sent",
                        declared.name
                    ))
                })?;
            if value.chars().any(char::is_control) {
                return Err(blocked(format!(
                    "Header {} has an invalid value",
                    declared.name
                )));
            }
            session.add_secrets([value.as_str()]);
            headers.push((declared.name.clone(), value.clone()));
        }
        Ok(headers)
    }
}

/// Runs the saved custom query.
///
/// # Errors
/// [`BalanceErrorKind::Blocked`] for an invalid query, an unconfirmed host or an unresolved
/// header (nothing is sent), the failure classes of the request, and a schema mismatch (the reason
/// as [`Session::safe_text`], like "Test query") when the paths do not match the body.
pub(super) async fn query(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    custom: &CustomBalanceQuery,
    now_ms: i64,
) -> Result<BalanceReport, BalanceError> {
    let headers = custom.prepare(session, target)?;
    let url = &custom.url;
    let fetched = session.get(url, headers).await?;
    let endpoint = session.endpoint_label(url);
    reading(session, custom, &fetched.text, &fetched.json, url, now_ms)
        .map_err(|message| BalanceError::schema(session.safe_text(&message), &endpoint))
}

fn reading(
    session: &Session<'_>,
    custom: &CustomBalanceQuery,
    text: &str,
    json: &Value,
    url: &str,
    now_ms: i64,
) -> Result<BalanceReport, String> {
    let host = host_of(url).unwrap_or_default();
    let mut report = custom.rules().evaluate(text, &host, None)?;
    report.source_endpoint = Some(session.endpoint_label(url));
    if custom.unit == UnitName::Custom {
        report.unit_label = custom
            .unit_label
            .as_ref()
            .map(|label| label.trim().to_owned());
    }
    Ok(seal(report, json, session.secrets(), now_ms))
}

/// The body for the form: JSON is redacted by key and by value, anything else (`text`) by the
/// secret redactor; cut at 256 KiB on a character boundary.
fn redacted_body(json: Option<&Value>, text: &str, secrets: &[String]) -> String {
    let mut body = json.map_or_else(
        || redact_secrets(text, secrets),
        |json| redact_json(json, secrets).to_string(),
    );
    if body.len() > MAX_BODY_BYTES {
        body.truncate(body.floor_char_boundary(MAX_BODY_BYTES));
    }
    body
}

/// Performs the query once and returns the redacted body, the status and the preview.
/// `custom` of `target` is ignored; `query` is the unsaved form state.
///
/// # Errors
/// As for the saved query, everything that happens before an answer arrives:
/// [`BalanceErrorKind::Blocked`] for an invalid query, an unconfirmed host or an unresolved header
/// (nothing is sent), and the failure classes of the request. A non-2xx, cut, non-JSON or
/// non-matching answer is the `preview_error` of the result.
pub async fn test_custom_query(
    http: &dyn SafeHttp,
    target: &BalanceTarget<'_>,
    query: &CustomBalanceQuery,
    cancel: &CancelToken,
    now_ms: i64,
) -> Result<CustomQueryTest, BalanceError> {
    let origin = normalise_origin(target.origin)?;
    let mut session = Session::new(http, cancel, origin, target);
    let headers = query.prepare(&mut session, target)?;
    let url = &query.url;
    let endpoint = session.endpoint_label(url);
    let response = session.send(url, headers).await?;
    // Parsed once, shared by the redacted body and the preview (same values as
    // `SafeResponse::json` and `SafeResponse::text`).
    let json = response.json();
    let text = String::from_utf8_lossy(&response.body);
    let body = redacted_body(json.as_ref(), &text, session.secrets());
    let preview = if !response.is_success() {
        Err(BalanceError::from_response(&response, &endpoint))
    } else if response.truncated {
        Err(BalanceError::schema(
            "The answer is larger than 256 KiB",
            &endpoint,
        ))
    } else if let Some(json) = &json {
        reading(&session, query, &text, json, url, now_ms)
            .map_err(|message| BalanceError::schema(session.safe_text(&message), &endpoint))
    } else {
        Err(BalanceError::schema("The answer is not JSON", &endpoint))
    };
    let (preview, preview_error) = match preview {
        Ok(report) => (Some(report), None),
        Err(error) => (None, Some(error)),
    };
    Ok(CustomQueryTest {
        status: response.status,
        body,
        preview,
        preview_error,
    })
}
