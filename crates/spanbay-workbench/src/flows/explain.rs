//! Deterministic "why did this step fail" sentences.
//!
//! Single responsibility: map a failed [`FlowStepResult`] to one kind, one English sentence, one
//! action and the facts a localized sentence is built from; no model call, no I/O.

use reqwest::StatusCode;

use crate::error::NetworkCause;
use crate::workspace::Assertion;
use crate::workspace::items::{
    ExplanationAction, FailureExplanation, FailureFacts, FailureKind, FlowStep, FlowStepResult,
};

/// Explains why `result` (the outcome of `step`, judged by `assertions`: the saved request's then
/// the step's, as sent) failed, or `None` for a passed or skipped step and for failures no rule
/// covers. `cause` is the transport cause of a send that got no response.
///
/// The first matching rule wins:
/// 1. an unresolved `{{variable}}` → [`FailureKind::UnresolvedVariable`], add an extraction;
/// 2. a failed status-equals assertion whose actual status is in the same class (202 vs 200)
///    → [`FailureKind::StatusMismatch`], edit the assertion;
/// 3. any failure with a 4xx/5xx status → [`FailureKind::HttpError`], open the response;
/// 4. an extraction path that is not in the response → [`FailureKind::ExtractionNotFound`],
///    open the response;
/// 5. a script that threw → [`FailureKind::Script`], open the response (console);
/// 6. any other failed assertion or script test → [`FailureKind::AssertionFailed`], edit it;
/// 7. no response because the transport failed → [`FailureKind::Timeout`], [`FailureKind::Tls`]
///    or [`FailureKind::Network`], check network settings.
///
/// The request is named by the sent method and path, falling back to the step's own overrides and
/// then to its name.
#[must_use]
pub fn explain_failure(
    step: &FlowStep,
    assertions: &[Assertion],
    result: &FlowStepResult,
    cause: Option<NetworkCause>,
) -> Option<FailureExplanation> {
    if result.passed || result.skipped {
        return None;
    }
    let error = result.error.as_deref();
    let (method, path) = request_parts(step, result);
    let base = FailureFacts {
        method,
        path,
        status: result.status_code,
        ..FailureFacts::default()
    };
    let explain = |kind, message: String, action, detail: Option<String>, facts: FailureFacts| {
        Some(FailureExplanation {
            kind,
            message,
            action,
            detail,
            facts: Some(facts),
        })
    };

    if let Some(key) = error.and_then(|error| error.strip_prefix("unresolved variable ")) {
        return explain(
            FailureKind::UnresolvedVariable,
            format!("{{{{{key}}}}} has no value. Extract it in an earlier step."),
            ExplanationAction::AddExtraction,
            error.map(str::to_owned),
            FailureFacts {
                variable: Some(key.to_owned()),
                ..base
            },
        );
    }

    let failed_assertion = result
        .assertion_results
        .iter()
        .find(|assertion| !assertion.passed);
    if let Some(actual) = result.status_code {
        let label = label(&base, result);
        let answer = status_label(actual);
        let mismatch = result
            .assertion_results
            .iter()
            .filter(|assertion| !assertion.passed)
            .find_map(|assertion| match assertions.get(assertion.index) {
                Some(Assertion::StatusEquals { value })
                    if *value != actual && value / 100 == actual / 100 =>
                {
                    Some((*value, assertion.message.clone()))
                }
                _ => None,
            });
        if let Some((expected, detail)) = mismatch {
            return explain(
                FailureKind::StatusMismatch,
                format!(
                    "{label} answered {answer} instead of {expected}. Change the assertion to {actual}, or poll first."
                ),
                ExplanationAction::EditAssertion,
                Some(detail),
                FailureFacts {
                    expected: Some(expected.to_string()),
                    actual: Some(actual.to_string()),
                    ..base
                },
            );
        }
        if actual >= 400 {
            return explain(
                FailureKind::HttpError,
                format!("{label} answered {answer}."),
                ExplanationAction::OpenResponse,
                first_failure_detail(result),
                base,
            );
        }
    }

    if let Some(error) = error {
        if let Some((variable, path)) = missing_extraction(error) {
            let path = if path.starts_with('$') {
                path.to_owned()
            } else if path.starts_with('[') {
                format!("${path}")
            } else {
                format!("$.{path}")
            };
            return explain(
                FailureKind::ExtractionNotFound,
                format!("{path} was not in the response."),
                ExplanationAction::OpenResponse,
                Some(error.to_owned()),
                FailureFacts {
                    expected: Some(path),
                    variable: Some(variable.to_owned()),
                    ..base
                },
            );
        }
        if error.starts_with("script failed in ") {
            return explain(
                FailureKind::Script,
                error.to_owned(),
                ExplanationAction::OpenResponse,
                Some(error.to_owned()),
                base,
            );
        }
    }

    if result.status_code.is_some() {
        if let Some(assertion) = failed_assertion {
            return explain(
                FailureKind::AssertionFailed,
                format!("{} failed: {}", assertion.label, assertion.message),
                ExplanationAction::EditAssertion,
                Some(assertion.message.clone()),
                FailureFacts {
                    expected: assertion.expected.clone(),
                    actual: assertion.actual.clone(),
                    ..base
                },
            );
        }
        if let Some(test) = result
            .script_tests
            .iter()
            .find(|test| !test.passed && !test.skipped)
        {
            return explain(
                FailureKind::AssertionFailed,
                format!("Test \"{}\" failed.", test.name),
                ExplanationAction::EditAssertion,
                test.error.clone(),
                base,
            );
        }
    }

    if let Some(error) = error
        && result.status_code.is_none()
        && (cause.is_some() || is_transport_failure(error))
    {
        let kind = match cause {
            Some(NetworkCause::Timeout) => FailureKind::Timeout,
            Some(NetworkCause::Tls) => FailureKind::Tls,
            _ => FailureKind::Network,
        };
        return explain(
            kind,
            error.to_owned(),
            ExplanationAction::CheckNetworkSettings,
            None,
            base,
        );
    }

    None
}

/// The sent method and path, else the step's overrides; `None` where neither says.
fn request_parts(step: &FlowStep, result: &FlowStepResult) -> (Option<String>, Option<String>) {
    match &result.executed_request {
        Some(sent) => (Some(sent.method.clone()), Some(url_path(&sent.url))),
        None => (
            step.method_override
                .map(|method| format!("{method:?}").to_ascii_uppercase()),
            step.url_override.as_deref().map(url_path),
        ),
    }
}

/// `METHOD /path`, else the step's name.
fn label(facts: &FailureFacts, result: &FlowStepResult) -> String {
    match (&facts.method, &facts.path) {
        (Some(method), Some(path)) => format!("{method} {path}"),
        _ => result.step_name.clone(),
    }
}

/// The path of `url`; text that is not a URL (a template with variables) keeps everything before
/// its query.
fn url_path(url: &str) -> String {
    url::Url::parse(url).map_or_else(
        |_| url.split('?').next().unwrap_or(url).to_owned(),
        |url| url.path().to_owned(),
    )
}

/// `404 Not Found`, or just the number for a status without a standard reason.
fn status_label(status: u16) -> String {
    match StatusCode::from_u16(status)
        .ok()
        .and_then(|code| code.canonical_reason())
    {
        Some(reason) => format!("{status} {reason}"),
        None => status.to_string(),
    }
}

/// The message of the first failed assertion, else the step's error.
fn first_failure_detail(result: &FlowStepResult) -> Option<String> {
    result
        .assertion_results
        .iter()
        .find(|assertion| !assertion.passed)
        .map(|assertion| assertion.message.clone())
        .or_else(|| result.error.clone())
}

/// The variable and path of an `extraction 'x': path 'EXPR' not found` error.
fn missing_extraction(error: &str) -> Option<(&str, &str)> {
    let rest = error.strip_prefix("extraction '")?;
    let (variable, rest) = rest.split_once("': path '")?;
    Some((variable, rest.strip_suffix("' not found")?))
}

/// Whether `error` is the text of a transport failure (no response arrived or could be read).
fn is_transport_failure(error: &str) -> bool {
    error.starts_with("network request failed") || error.starts_with("failed to read response body")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::workspace::items::ExecutedRequestSummary;
    use spanbay_model::assertion::AssertionResult;

    fn explain(step: &FlowStep, result: &FlowStepResult) -> Option<FailureExplanation> {
        explain_failure(step, &step.assertions, result, None)
    }

    fn step(assertions: Vec<Assertion>) -> FlowStep {
        serde_json::from_value::<FlowStep>(json!({
            "id": "refund", "name": "Refund", "delay_ms": 0,
            "continue_on_error": false, "position": 0,
        }))
        .map(|step| FlowStep { assertions, ..step })
        .unwrap()
    }

    fn failed(status_code: Option<u16>, error: Option<&str>) -> FlowStepResult {
        FlowStepResult {
            step_id: "refund".into(),
            step_name: "Refund".into(),
            request_id: String::new(),
            status_code,
            elapsed_ms: 12,
            passed: false,
            error: error.map(str::to_owned),
            skipped: false,
            attempts: 1,
            extracted_variables: Vec::new(),
            assertion_results: Vec::new(),
            executed_request: Some(ExecutedRequestSummary {
                method: "POST".into(),
                url: "https://billing.acme.dev/refunds?dry=1".into(),
                headers: Vec::new(),
                body_preview: String::new(),
            }),
            response_headers: Vec::new(),
            response_body_preview: String::new(),
            timing: None,
            console_logs: Vec::new(),
            script_tests: Vec::new(),
            call_chain: None,
            explanation: None,
            attempt_list: Vec::new(),
            request_assertion_count: 0,
            history_entry_id: None,
            condition: None,
            ai: None,
            matched_branch_id: None,
            started_offset_ms: None,
        }
    }

    fn failed_assertion(index: usize, message: &str) -> AssertionResult {
        AssertionResult {
            index,
            label: "Status".into(),
            passed: false,
            message: message.into(),
            expected: Some("200".into()),
            actual: Some("500".into()),
        }
    }

    #[test]
    fn a_status_assertion_missed_within_the_same_class_suggests_editing_it() {
        let step = step(vec![Assertion::StatusEquals { value: 200 }]);
        let mut result = failed(Some(202), None);
        result.assertion_results = vec![failed_assertion(0, "Expected status 200, got 202")];

        let explanation = explain(&step, &result).unwrap();

        assert_eq!(explanation.kind, FailureKind::StatusMismatch);
        assert_eq!(
            explanation.message,
            "POST /refunds answered 202 Accepted instead of 200. Change the assertion to 202, or poll first."
        );
        assert_eq!(explanation.action, ExplanationAction::EditAssertion);
        assert_eq!(
            explanation.detail.as_deref(),
            Some("Expected status 200, got 202")
        );
    }

    #[test]
    fn a_failure_with_a_client_or_server_status_reports_the_status() {
        let step = step(vec![Assertion::StatusEquals { value: 200 }]);
        let mut result = failed(Some(404), None);
        result.assertion_results = vec![failed_assertion(0, "Expected status 200, got 404")];

        let explanation = explain(&step, &result).unwrap();

        assert_eq!(explanation.kind, FailureKind::HttpError);
        assert_eq!(explanation.message, "POST /refunds answered 404 Not Found.");
        assert_eq!(explanation.action, ExplanationAction::OpenResponse);

        let server_error = failed(Some(503), Some("script failed in request: boom"));
        let explanation = explain(&step, &server_error).unwrap();
        assert_eq!(
            explanation.message,
            "POST /refunds answered 503 Service Unavailable."
        );
        assert_eq!(
            explanation.detail.as_deref(),
            Some("script failed in request: boom")
        );
    }

    #[test]
    fn an_unresolved_variable_asks_for_an_extraction() {
        let result = failed(None, Some("unresolved variable invoice_id"));

        let explanation = explain(&step(Vec::new()), &result).unwrap();

        assert_eq!(explanation.kind, FailureKind::UnresolvedVariable);
        assert_eq!(
            explanation.message,
            "{{invoice_id}} has no value. Extract it in an earlier step."
        );
        assert_eq!(explanation.action, ExplanationAction::AddExtraction);
    }

    #[test]
    fn a_missing_json_path_names_the_path() {
        let mut result = failed(
            Some(200),
            Some("extraction 'invoice_id': path 'data[0].id' not found"),
        );
        result.executed_request = None;

        let explanation = explain(&step(Vec::new()), &result).unwrap();

        assert_eq!(explanation.kind, FailureKind::ExtractionNotFound);
        assert_eq!(explanation.message, "$.data[0].id was not in the response.");
        assert_eq!(explanation.action, ExplanationAction::OpenResponse);

        let rooted = failed(
            Some(200),
            Some("extraction 'x': path '$.items[0]' not found"),
        );
        assert_eq!(
            explain(&step(Vec::new()), &rooted).unwrap().message,
            "$.items[0] was not in the response."
        );
    }

    #[test]
    fn a_transport_failure_points_at_the_network_settings() {
        let error = "network request failed: error sending request for url (https://billing.acme.dev/refunds): operation timed out";
        let result = failed(None, Some(error));

        let explanation = explain(&step(Vec::new()), &result).unwrap();

        assert_eq!(explanation.kind, FailureKind::Network);
        assert_eq!(explanation.message, error);
        assert_eq!(explanation.action, ExplanationAction::CheckNetworkSettings);
    }

    #[test]
    fn passed_skipped_and_unexplained_steps_have_no_explanation() {
        let step = step(Vec::new());
        let mut passed = failed(Some(200), None);
        passed.passed = true;
        let mut skipped = failed(None, None);
        skipped.skipped = true;

        assert_eq!(explain(&step, &passed), None);
        assert_eq!(explain(&step, &skipped), None);
        assert_eq!(
            explain(&step, &failed(None, Some("request req_x was not found"))),
            None
        );
    }

    #[test]
    fn the_label_falls_back_to_overrides_and_the_step_name() {
        let mut result = failed(Some(500), None);
        result.executed_request = None;
        let mut overridden = step(Vec::new());
        overridden.method_override = Some(crate::workspace::HttpMethod::Delete);
        overridden.url_override = Some("{{base}}/refunds/7?force=1".into());

        assert_eq!(
            explain(&overridden, &result).unwrap().message,
            "DELETE {{base}}/refunds/7 answered 500 Internal Server Error."
        );
        assert_eq!(
            explain(&step(Vec::new()), &result).unwrap().message,
            "Refund answered 500 Internal Server Error."
        );
    }
}
