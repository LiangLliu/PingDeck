//! Trim-and-reject-blank validators and the workspace domain error type.

use thiserror::Error;

/// Validation and cookie-jar errors raised by workspace domain operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkspaceError {
    #[error("{entity} name cannot be blank")]
    BlankName { entity: &'static str },
    #[error("{entity} id cannot be blank")]
    BlankId { entity: &'static str },
    #[error("environment variable key cannot be blank")]
    BlankVariableKey,
    #[error("request URL cannot be blank")]
    BlankUrl,
    #[error("invalid cookie source URL: {url}")]
    InvalidCookieSourceUrl { url: String },
    #[error("cookie domain {cookie_domain} does not match response host {source_host}")]
    CookieDomainMismatch {
        source_host: String,
        cookie_domain: String,
    },
    #[error("cookie domain {domain} is a public suffix and cannot set a cookie")]
    CookiePublicSuffixDomain { domain: String },
    #[error("unsupported cookie jar format: {format}")]
    UnsupportedCookieJarFormat { format: String },
    #[error("unsupported cookie jar version: {version}")]
    UnsupportedCookieJarVersion { version: u16 },
    #[error("cookie jar workspace mismatch: expected {expected}, got {actual}")]
    CookieJarWorkspaceMismatch { expected: String, actual: String },
    #[error("{reason}")]
    InvalidAiRequest { reason: String },
    /// A request assertion whose operator, target and expected value do not fit together.
    #[error("{reason}")]
    InvalidAssertion { reason: String },
    #[error("invalid cookie SameSite value: {same_site}")]
    InvalidCookieSameSite { same_site: String },
    /// Saved response examples over the count or body-size limit.
    #[error("{reason}")]
    InvalidExample { reason: String },
}

/// Trims an entity id, rejecting blank values.
pub fn validate_id(entity: &'static str, value: &str) -> Result<String, WorkspaceError> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        Err(WorkspaceError::BlankId { entity })
    } else {
        Ok(trimmed.to_owned())
    }
}

/// Trims an entity name, rejecting blank values.
pub fn validate_name(entity: &'static str, value: &str) -> Result<String, WorkspaceError> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        Err(WorkspaceError::BlankName { entity })
    } else {
        Ok(trimmed.to_owned())
    }
}

/// Trims an environment variable key, rejecting blank values.
pub fn validate_variable_key(value: &str) -> Result<String, WorkspaceError> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        Err(WorkspaceError::BlankVariableKey)
    } else {
        Ok(trimmed.to_owned())
    }
}

/// Trims a request URL, rejecting blank values.
pub fn validate_url(value: &str) -> Result<String, WorkspaceError> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        Err(WorkspaceError::BlankUrl)
    } else {
        Ok(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_variable_key_rejects_blank_keys() {
        let error = validate_variable_key("   ").expect_err("blank variable keys are invalid");

        assert_eq!(error, WorkspaceError::BlankVariableKey);
    }
}
