//! Validation messages as data: every issue the save use-cases would reject, not just the first.
//!
//! Single responsibility: let a form pre-check its input and show the exact texts the save
//! use-cases return. The save paths themselves call into this module (mock servers and flows) or
//! the same domain validators (names, variables), so the first issue of each list is always the
//! error the matching save returns.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{
    app::{EnvironmentVariableInput, environment_service::duplicate_key_message},
    error::AppError,
    flows::check_condition,
    regex::regex_check,
    workspace::{
        MockRouteType,
        items::{
            FLOW_MAX_RETRIES, FLOW_MAX_RETRY_WAIT_MS, FLOW_MAX_TIMEOUT_MS, Flow,
            FlowExtractionSource, MockRuleOperator, MockServer,
        },
        validation::validate_name,
    },
};

/// Longest latency or flow-step delay, in milliseconds.
const MAX_LATENCY_MS: u64 = 60_000;

/// The stable reason of a variable-list issue, for a UI to word it in its own language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    /// The environment name is blank.
    NameBlank,
    /// A variable key is blank.
    KeyBlank,
    /// A plain variable has no value.
    ValueMissing,
    /// A variable key is already used by an earlier row.
    KeyDuplicate,
}

/// One rejected input: the field it belongs to and the message to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    /// Dotted/indexed path of the offending field, e.g. `name` or `routes[2].path`.
    pub field: String,
    /// The message the save use-case returns for it.
    pub message: String,
    /// The stable reason; set by the variable-list validators only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<IssueCode>,
    /// 0-based index of the variable row the issue names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<usize>,
}

fn issue(field: impl Into<String>, message: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        field: field.into(),
        message: message.into(),
        code: None,
        row: None,
    }
}

/// The first issue as the error a save returns, or `Ok` when there is none.
pub fn first_issue_error(issues: Vec<ValidationIssue>) -> Result<(), AppError> {
    issues
        .into_iter()
        .next()
        .map_or(Ok(()), |first| Err(AppError::validation(first.message)))
}

/// [`first_issue_error`] for a flow save: an issue of a step (`steps[i].field…`) also names the
/// step id and the field (`["step_1", "condition_expression"]`) in the error's keys, so the
/// editor can mark the field.
pub fn first_flow_issue_error(flow: &Flow, issues: Vec<ValidationIssue>) -> Result<(), AppError> {
    let Some(first) = issues.into_iter().next() else {
        return Ok(());
    };
    let step_field = first.field.strip_prefix("steps[").and_then(|rest| {
        let (index, field) = rest.split_once("].")?;
        let step = flow.steps.get(index.parse::<usize>().ok()?)?;
        let field = field.split(['.', '[']).next().unwrap_or(field);
        Some(vec![step.id.clone(), field.to_owned()])
    });
    let error = AppError::validation(first.message);
    Err(match step_field {
        Some(keys) => error.with_keys(keys),
        None => error,
    })
}

fn name_issue(entity: &'static str, name: &str) -> Option<ValidationIssue> {
    validate_name(entity, name)
        .err()
        .map(|error| issue("name", error.to_string()))
}

/// One issue per variable row the save would reject (blank key, missing plain value, and with
/// `unique` a key an earlier row uses), in row order; `prefix[i]` names the row. A secret row
/// without a value keeps the stored one on save, which only the save can check, so it is
/// accepted here.
fn variable_issues(
    prefix: &str,
    rows: &[EnvironmentVariableInput],
    unique: bool,
) -> Vec<ValidationIssue> {
    let mut seen = HashSet::new();
    rows.iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let key = row.key.trim();
            let (code, message) = if key.is_empty() || (!row.is_secret && row.value.is_none()) {
                let code = if key.is_empty() {
                    IssueCode::KeyBlank
                } else {
                    IssueCode::ValueMissing
                };
                let mut row = row.clone();
                if row.is_secret && row.value.is_none() {
                    row.value = Some(String::new());
                }
                let error = row.into_domain("", &[]).err()?;
                (code, error.message().to_owned())
            } else if unique && !seen.insert(key) {
                (IssueCode::KeyDuplicate, duplicate_key_message(key))
            } else {
                return None;
            };
            Some(ValidationIssue {
                code: Some(code),
                row: Some(index),
                ..issue(format!("{prefix}[{index}]"), message)
            })
        })
        .collect()
}

/// Issues of a folder save: a blank name.
#[must_use]
pub fn validate_folder(folder_name: &str) -> Vec<ValidationIssue> {
    name_issue("folder", folder_name).into_iter().collect()
}

/// Issues of a globals save: every invalid variable row.
#[must_use]
pub fn validate_globals(variables: &[EnvironmentVariableInput]) -> Vec<ValidationIssue> {
    variable_issues("variables", variables, true)
}

/// Issues of a collection save: a blank name, then every invalid variable row.
#[must_use]
pub fn validate_collection(
    collection_name: &str,
    variables: &[EnvironmentVariableInput],
) -> Vec<ValidationIssue> {
    let mut issues: Vec<_> = name_issue("collection", collection_name)
        .into_iter()
        .collect();
    issues.extend(variable_issues("variables", variables, false));
    issues
}

/// Issues of an environment save: a blank name, then every invalid variable row.
#[must_use]
pub fn validate_environment(
    environment_name: &str,
    variables: &[EnvironmentVariableInput],
) -> Vec<ValidationIssue> {
    let mut issues: Vec<_> = name_issue("environment", environment_name)
        .map(|issue| ValidationIssue {
            code: Some(IssueCode::NameBlank),
            ..issue
        })
        .into_iter()
        .collect();
    issues.extend(variable_issues("variables", variables, true));
    issues
}

/// Every issue of a mock server: blank id/name, port, latency, routes and their responses.
#[must_use]
pub fn validate_mock_server(server: &MockServer) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if server.id.trim().is_empty() {
        issues.push(issue("id", "mock server id cannot be blank"));
    }
    if server.name.trim().is_empty() {
        issues.push(issue("name", "mock server name cannot be blank"));
    }
    if server.port == 0 {
        issues.push(issue(
            "port",
            "mock server port must be between 1 and 65535",
        ));
    }
    if server.latency_ms > MAX_LATENCY_MS {
        issues.push(issue(
            "latency_ms",
            "mock server latency cannot exceed 60000 ms",
        ));
    }
    for (index, route) in server.routes.iter().enumerate() {
        let at = format!("routes[{index}]");
        if route.id.trim().is_empty() {
            issues.push(issue(format!("{at}.id"), "mock route id cannot be blank"));
        }
        if !route.path.starts_with('/') {
            issues.push(issue(
                format!("{at}.path"),
                format!("mock route {} path must start with '/'", route.id),
            ));
        }
        if !matches!(
            route.method.to_ascii_uppercase().as_str(),
            "ANY" | "ALL" | "*" | "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
        ) {
            issues.push(issue(
                format!("{at}.method"),
                format!(
                    "mock route {} method {} is not supported",
                    route.id, route.method
                ),
            ));
        }
        if route.route_type == MockRouteType::Crud && route.data_bucket_id.trim().is_empty() {
            issues.push(issue(
                format!("{at}.data_bucket_id"),
                format!("crud route {} must reference a data bucket", route.id),
            ));
        }
        for (position, response) in route.responses.iter().enumerate() {
            let at = format!("{at}.responses[{position}]");
            if !(100..=599).contains(&response.status_code) {
                issues.push(issue(
                    format!("{at}.status_code"),
                    format!(
                        "mock response {} status {} must be between 100 and 599",
                        response.id, response.status_code
                    ),
                ));
            }
            if response.latency_ms > MAX_LATENCY_MS {
                issues.push(issue(
                    format!("{at}.latency_ms"),
                    format!(
                        "mock response {} latency cannot exceed 60000 ms",
                        response.id
                    ),
                ));
            }
            let label = if response.label.trim().is_empty() {
                response.id.as_str()
            } else {
                response.label.as_str()
            };
            for (rule_index, rule) in response.rules.iter().enumerate() {
                if matches!(
                    rule.operator,
                    MockRuleOperator::Regex | MockRuleOperator::RegexIgnoreCase
                ) && let Some(reason) = regex_check(&rule.value)
                {
                    issues.push(issue(
                        format!("{at}.rules[{rule_index}].value"),
                        format!(
                            "mock route {}, response \"{label}\", rule {}: {reason}",
                            route.path,
                            rule_index + 1
                        ),
                    ));
                }
            }
        }
    }
    issues
}

/// Every issue of a flow: blank name or id, blank or duplicate step ids, step delays, retries and
/// timeouts, conditions that do not parse, and unsupported extraction patterns.
#[must_use]
pub fn validate_flow(flow: &Flow) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if flow.name.trim().is_empty() {
        issues.push(issue("name", "flow name cannot be blank"));
    }
    if flow.id.trim().is_empty() {
        issues.push(issue("id", "flow id cannot be blank"));
    }
    let mut seen = HashSet::new();
    for (index, step) in flow.steps.iter().enumerate() {
        if step.id.trim().is_empty() {
            issues.push(issue(
                format!("steps[{index}].id"),
                "step id cannot be blank",
            ));
        } else if !seen.insert(step.id.as_str()) {
            issues.push(issue(
                format!("steps[{index}].id"),
                format!("duplicate step id {} in flow", step.id),
            ));
        }
    }
    for (index, step) in flow.steps.iter().enumerate() {
        let label = if step.name.trim().is_empty() {
            step.id.as_str()
        } else {
            step.name.as_str()
        };
        if step.delay_ms > MAX_LATENCY_MS {
            issues.push(issue(
                format!("steps[{index}].delay_ms"),
                format!("flow step {} delay cannot exceed 60000 ms", step.id),
            ));
        }
        if step.retries > FLOW_MAX_RETRIES {
            issues.push(issue(
                format!("steps[{index}].retries"),
                format!(
                    "flow step {} retries cannot exceed {FLOW_MAX_RETRIES}",
                    step.id
                ),
            ));
        }
        if step.retry_wait_ms > FLOW_MAX_RETRY_WAIT_MS {
            issues.push(issue(
                format!("steps[{index}].retry_wait_ms"),
                format!(
                    "flow step {} retry wait cannot exceed {FLOW_MAX_RETRY_WAIT_MS} ms",
                    step.id
                ),
            ));
        }
        if step
            .timeout_ms
            .is_some_and(|timeout| !(1..=FLOW_MAX_TIMEOUT_MS).contains(&timeout))
        {
            issues.push(issue(
                format!("steps[{index}].timeout_ms"),
                format!(
                    "flow step {} timeout must be between 1 and {FLOW_MAX_TIMEOUT_MS} ms",
                    step.id
                ),
            ));
        }
        if let Some(reason) = check_condition(&step.condition_expression) {
            issues.push(issue(
                format!("steps[{index}].condition_expression"),
                format!("flow step \"{label}\" condition: {reason}"),
            ));
        }
        for (position, extraction) in step.extractions.iter().enumerate() {
            if extraction.source == FlowExtractionSource::BodyRegex
                && let Some(reason) = regex_check(&extraction.expression)
            {
                issues.push(issue(
                    format!("steps[{index}].extractions[{position}].expression"),
                    format!(
                        "flow step \"{label}\", extraction {} ({}): {reason}",
                        position + 1,
                        extraction.variable_name
                    ),
                ));
            }
        }
    }
    issues
}
