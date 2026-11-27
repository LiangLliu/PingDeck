use std::collections::BTreeMap;

use serde_json::Value;

use crate::executor::ExecutionResult;
use crate::workspace::items::{
    FlowExtraction, FlowExtractionSource, json_path_lookup, json_value_to_string,
};
use crate::workspace::{EnvironmentVariable, VariableScopes};

use super::condition::resolve_side;

/// The condition as it was evaluated: every `{{token}}` filled in (unresolved ones empty), secret
/// values masked, so a skipped (or failed) step can show why.
pub(super) fn render_condition(expression: &str, scopes: &VariableScopes) -> String {
    spanbay_base::secrets::mask_secrets(
        &resolve_side(expression.trim(), scopes),
        &scopes.secret_values(),
    )
}

pub(super) fn extract_value(
    extraction: &FlowExtraction,
    result: &ExecutionResult,
) -> Result<String, String> {
    match extraction.source {
        FlowExtractionSource::BodyJsonPath => {
            let body: Value = serde_json::from_str(&result.body_text).map_err(|_| {
                format!(
                    "extraction '{}': response body is not JSON",
                    extraction.variable_name
                )
            })?;
            let found = json_path_lookup(&body, &extraction.expression).map_err(|message| {
                format!(
                    "extraction '{}': path '{}' is not valid: {message}",
                    extraction.variable_name, extraction.expression
                )
            })?;
            found
                .map(|value| json_value_to_string(&value))
                .ok_or_else(|| {
                    format!(
                        "extraction '{}': path '{}' not found",
                        extraction.variable_name, extraction.expression
                    )
                })
        }
        FlowExtractionSource::Header => result
            .headers
            .iter()
            .find(|header| header.key.eq_ignore_ascii_case(&extraction.expression))
            .map(|header| header.value.clone())
            .ok_or_else(|| {
                format!(
                    "extraction '{}': header '{}' not found",
                    extraction.variable_name, extraction.expression
                )
            }),
        FlowExtractionSource::StatusCode => Ok(result.status_code.to_string()),
        FlowExtractionSource::BodyRegex => {
            crate::regex::first_capture(&extraction.expression, &result.body_text, false)
                .ok_or_else(|| {
                    format!(
                        "extraction '{}': regex '{}' did not match",
                        extraction.variable_name, extraction.expression
                    )
                })
        }
        FlowExtractionSource::Cookie => result
            .cookies
            .iter()
            .find(|cookie| cookie.name == extraction.expression)
            .map(|cookie| cookie.value.clone())
            .ok_or_else(|| {
                format!(
                    "extraction '{}': cookie '{}' not set",
                    extraction.variable_name, extraction.expression
                )
            }),
        FlowExtractionSource::ResponseBody => Ok(result.body_text.clone()),
    }
}

/// Sets flow variable `key` to `value`, adding it when it is new. A variable that is a secret
/// stays one; `secret` makes it one.
pub(super) fn set_variable(
    variables: &mut Vec<EnvironmentVariable>,
    key: &str,
    value: &str,
    secret: bool,
) {
    if let Some(existing) = variables.iter_mut().find(|variable| variable.key == key) {
        existing.value = Some(value.to_owned());
        existing.is_secret |= secret;
        return;
    }

    variables.push(EnvironmentVariable {
        id: String::new(),
        key: key.to_owned(),
        value: Some(value.to_owned()),
        is_secret: secret,
        enabled: true,
        note: String::new(),
    });
}

/// Makes the flow variables match the script-local scope a step left behind: what its scripts set
/// (`pm.variables.set`) is added or updated, what they unset is removed. Secret flags are kept.
pub(super) fn apply_local(
    variables: &mut Vec<EnvironmentVariable>,
    local: BTreeMap<String, String>,
) {
    variables.retain(|variable| local.contains_key(&variable.key));
    for (key, value) in local {
        if EnvironmentVariable::plain(&key, value.clone()).is_ok() {
            set_variable(variables, &key, &value, false);
        }
    }
}
