//! `From<lower error> for AppError`.
//!
//! These live next to the error types they convert, not in [`spanbay_base::error`]: the
//! orphan rule puts them in the crate that defines the source type. Each one moves into its
//! owning crate as that crate is carved out.

use spanbay_base::error::AppError;

use crate::{importers::ImportError, mock::MockStartError, postman::PostmanError};

impl From<ImportError> for AppError {
    fn from(error: ImportError) -> Self {
        let message = error.to_string();
        match error {
            ImportError::UnsupportedDocument { .. } => Self::unsupported(message),
            ImportError::Workspace(inner) => inner.into(),
            _ => Self::validation(message),
        }
    }
}

impl From<PostmanError> for AppError {
    fn from(error: PostmanError) -> Self {
        let message = error.to_string();
        match error {
            PostmanError::Serialization { .. } => Self::internal(message),
            PostmanError::Workspace(inner) => inner.into(),
            _ => Self::validation(message),
        }
    }
}

impl From<MockStartError> for AppError {
    fn from(error: MockStartError) -> Self {
        let message = error.to_string();
        match error {
            MockStartError::PortInUse { .. } | MockStartError::Ipv6PortInUse { .. } => {
                Self::conflict(message)
            }
            _ => Self::io(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_errors_map_to_their_category() {
        assert!(matches!(
            AppError::from(MockStartError::PortInUse { port: 1 }),
            AppError::Conflict { .. }
        ));
    }
}
