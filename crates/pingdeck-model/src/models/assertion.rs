use serde::{Deserialize, Serialize};

use crate::validation::{WorkspaceError, validate_name};

/// A response check evaluated against an executed request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Assertion {
    StatusEquals {
        value: u16,
    },
    StatusIn {
        values: Vec<u16>,
    },
    HeaderExists {
        key: String,
    },
    HeaderEquals {
        key: String,
        value: String,
    },
    JsonPathExists {
        path: String,
    },
    JsonPathEquals {
        path: String,
        value: serde_json::Value,
    },
    BodyContains {
        value: String,
    },
    ResponseTimeLessThanMs {
        value: u64,
    },
    BodySizeLessThanBytes {
        value: u64,
    },
    /// AI API requests only: the merged reply contains the text (case-sensitive).
    AiReplyContains {
        value: String,
    },
    /// AI API requests only: the merged reply matches the pattern (`crate::regex` dialect).
    AiReplyMatches {
        pattern: String,
    },
    /// AI API requests only: the merged reply parses as JSON and validates against the schema.
    AiJsonSchemaValid {
        schema: serde_json::Value,
    },
    /// AI API requests only: time to first token is below the limit.
    AiTtftLessThanMs {
        value: u64,
    },
    /// AI API requests only: input plus output tokens are below the limit.
    AiTokensLessThan {
        value: u64,
    },
    /// AI API requests only: status below 400, no in-stream error, finish reason is not `error`.
    AiNoError {},
    /// One value of the response compared with an operator: equality and its negation,
    /// presence, containment, ordering, a regular expression or a JSON type.
    Check {
        target: AssertionTarget,
        op: AssertionOp,
        /// The expected value; ignored by `exists` and `not_exists`, a pattern string for
        /// `matches`, a type name ([`ASSERTION_TYPE_NAMES`]) for `is_type`.
        #[serde(default)]
        value: serde_json::Value,
    },
    /// The response body parses as JSON and validates against the schema; with `path`, the
    /// value at that JSON path is validated instead of the whole body.
    JsonSchemaValid {
        schema: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
}

/// What a [`Assertion::Check`] reads from the response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssertionTarget {
    /// The status code, a number.
    Status,
    /// A header (case-insensitive name); repeated headers are joined with `, `.
    Header { name: String },
    /// The value at a JSON path of the body (the one dialect of `response_viewer`).
    JsonPath { path: String },
    /// The body as text.
    Body,
    /// The elapsed time in milliseconds.
    ResponseTimeMs,
    /// The body size in bytes: the full size when it is known, else the captured size.
    BodySizeBytes,
}

/// The operator of a [`Assertion::Check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionOp {
    Equals,
    NotEquals,
    Exists,
    NotExists,
    Contains,
    NotContains,
    GreaterThan,
    GreaterOrEqual,
    LessThan,
    LessOrEqual,
    Matches,
    NotMatches,
    IsType,
}

/// The type names `is_type` accepts, as JSON spells its value types (`integer` is a number
/// without a fraction).
pub const ASSERTION_TYPE_NAMES: [&str; 7] = [
    "string", "number", "integer", "boolean", "array", "object", "null",
];

impl AssertionOp {
    /// Whether the operator compares numbers.
    #[must_use]
    pub const fn is_ordering(self) -> bool {
        matches!(
            self,
            Self::GreaterThan | Self::GreaterOrEqual | Self::LessThan | Self::LessOrEqual
        )
    }
}

impl AssertionTarget {
    /// Whether the target may be absent from a response (a header, a JSON path), so
    /// `exists` and `not_exists` mean something.
    #[must_use]
    pub const fn may_be_absent(&self) -> bool {
        matches!(self, Self::Header { .. } | Self::JsonPath { .. })
    }
}

/// Checks that the operator fits the target and the expected value fits the operator.
fn validate_check(
    target: &AssertionTarget,
    op: AssertionOp,
    value: &serde_json::Value,
) -> Result<(), WorkspaceError> {
    let invalid = |reason: String| Err(WorkspaceError::InvalidAssertion { reason });
    match target {
        AssertionTarget::Header { name } => {
            validate_name("assertion header key", name)?;
        }
        AssertionTarget::JsonPath { path } => {
            validate_name("assertion JSON path", path)?;
        }
        AssertionTarget::Status
        | AssertionTarget::Body
        | AssertionTarget::ResponseTimeMs
        | AssertionTarget::BodySizeBytes => {}
    }
    let numeric_target = matches!(
        target,
        AssertionTarget::Status | AssertionTarget::ResponseTimeMs | AssertionTarget::BodySizeBytes
    );
    match op {
        AssertionOp::Exists | AssertionOp::NotExists if !target.may_be_absent() => {
            invalid("only a header or a JSON path can be checked for presence".to_owned())
        }
        AssertionOp::Contains | AssertionOp::NotContains if numeric_target => {
            invalid("containment needs a text, header or JSON value".to_owned())
        }
        AssertionOp::Contains | AssertionOp::NotContains if value.is_null() => {
            invalid("containment needs a value to look for".to_owned())
        }
        op if op.is_ordering() && json_number(value).is_none() => {
            invalid("a comparison needs a number as the expected value".to_owned())
        }
        AssertionOp::Matches | AssertionOp::NotMatches => match value.as_str() {
            Some(pattern) if !pattern.trim().is_empty() => {
                match pingdeck_base::regex::regex_check(pattern) {
                    Some(reason) => invalid(reason),
                    None => Ok(()),
                }
            }
            _ => invalid("a regular expression check needs a pattern".to_owned()),
        },
        AssertionOp::IsType => match value.as_str() {
            Some(name) if ASSERTION_TYPE_NAMES.contains(&name) => Ok(()),
            _ => invalid(format!(
                "a type check needs one of {}",
                ASSERTION_TYPE_NAMES.join(", ")
            )),
        },
        _ => Ok(()),
    }
}

/// A JSON number, or a string that parses as one (`"200"`), as `f64`.
#[must_use]
pub fn json_number(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => {
            text.trim().parse::<f64>().ok().filter(|n| n.is_finite())
        }
        _ => None,
    }
}

impl Assertion {
    /// Checks that the assertion's header key, JSON path, or body value is not blank, and that a
    /// `Check` operator fits its target and expected value.
    pub fn validate(&self) -> Result<(), WorkspaceError> {
        match self {
            Self::Check { target, op, value } => validate_check(target, *op, value)?,
            Self::JsonSchemaValid { schema, path } => {
                if !schema.is_object() {
                    return Err(WorkspaceError::InvalidAssertion {
                        reason: "the JSON schema of an assertion must be an object".to_owned(),
                    });
                }
                if let Some(path) = path {
                    validate_name("assertion JSON path", path)?;
                }
            }
            Self::HeaderExists { key } | Self::HeaderEquals { key, .. } => {
                validate_name("assertion header key", key)?;
            }
            Self::JsonPathExists { path } | Self::JsonPathEquals { path, .. } => {
                validate_name("assertion JSON path", path)?;
            }
            Self::BodyContains { value } => {
                validate_name("assertion body value", value)?;
            }
            Self::AiReplyContains { value } => {
                validate_name("assertion reply value", value)?;
            }
            Self::AiReplyMatches { pattern } => {
                validate_name("assertion reply pattern", pattern)?;
                if let Some(reason) = pingdeck_base::regex::regex_check(pattern) {
                    return Err(WorkspaceError::InvalidAiRequest { reason });
                }
            }
            Self::AiJsonSchemaValid { schema } if !schema.is_object() => {
                return Err(WorkspaceError::InvalidAiRequest {
                    reason: "the JSON schema of an assertion must be an object".to_owned(),
                });
            }
            Self::StatusEquals { .. }
            | Self::StatusIn { .. }
            | Self::ResponseTimeLessThanMs { .. }
            | Self::BodySizeLessThanBytes { .. }
            | Self::AiJsonSchemaValid { .. }
            | Self::AiTtftLessThanMs { .. }
            | Self::AiTokensLessThan { .. }
            | Self::AiNoError {} => {}
        }

        Ok(())
    }
}
