//! Conversions from the model's own errors to the shared [`AppError`].

use spanbay_base::error::AppError;

use crate::{
    models::TlsCertificateError, validation::WorkspaceError, variables::VariableResolutionError,
};

impl From<WorkspaceError> for AppError {
    fn from(error: WorkspaceError) -> Self {
        let message = error.to_string();
        match error {
            WorkspaceError::UnsupportedCookieJarFormat { .. }
            | WorkspaceError::UnsupportedCookieJarVersion { .. } => Self::unsupported(message),
            WorkspaceError::CookieJarWorkspaceMismatch { .. } => Self::conflict(message),
            _ => Self::validation(message),
        }
    }
}

impl From<TlsCertificateError> for AppError {
    fn from(error: TlsCertificateError) -> Self {
        Self::validation(error.to_string())
    }
}

impl From<VariableResolutionError> for AppError {
    fn from(error: VariableResolutionError) -> Self {
        Self::validation(error.to_string())
    }
}
