//! `ExecutionError` to the shared [`AppError`]: the orphan rule puts this next to the error
//! type it converts.

use pingdeck_base::error::{AppError, ErrorCode, ErrorDetail};

use crate::executor::ExecutionError;

impl From<ExecutionError> for AppError {
    fn from(error: ExecutionError) -> Self {
        let message = error.to_string();
        match error {
            ExecutionError::Cancelled { bytes_received } => Self::Cancelled {
                message,
                bytes_received: Some(bytes_received),
            },
            ExecutionError::UnsupportedProtocol => {
                Self::unsupported(message).coded(ErrorCode::UnsupportedProtocol, None)
            }
            ExecutionError::UnsupportedGraphqlMethod => {
                Self::unsupported(message).coded(ErrorCode::UnsupportedMethod, None)
            }
            ExecutionError::UnsupportedBody => {
                Self::unsupported(message).coded(ErrorCode::UnsupportedBody, None)
            }
            ExecutionError::UnresolvedVariable { keys } => Self::validation(message)
                .coded(ErrorCode::UnresolvedVariable, keys.first().cloned())
                .with_keys(keys),
            ExecutionError::RedirectHostNotDeclared { host } => {
                Self::validation(message).coded(ErrorCode::RedirectRefused, host)
            }
            ExecutionError::GuardRefused { host } => {
                Self::validation(message).coded(ErrorCode::GuardRefused, host)
            }
            ExecutionError::InvalidUrl(url) => {
                Self::validation(message).coded(ErrorCode::InvalidUrl, url)
            }
            // Every producer is local: an invalid TLS/proxy setting or an unreadable or malformed
            // certificate file (the variant carries only text, so these stay one kind).
            ExecutionError::ClientBuild(_) => {
                Self::validation(message).coded(ErrorCode::ClientBuild, None)
            }
            ExecutionError::Network {
                connect_phase,
                attempt_count,
                cause,
                ..
            } => Self::Network {
                message,
                connect_phase: Some(connect_phase),
                attempt_count: Some(attempt_count.max(1)),
                cause,
                detail: ErrorDetail::default(),
            },
            ExecutionError::BodyRead(_) => Self::network(message).coded(ErrorCode::BodyRead, None),
            ExecutionError::BodyWrite { .. } => Self::io(message),
            ExecutionError::OAuth2Token { .. } => {
                Self::network(message).coded(ErrorCode::OauthToken, None)
            }
            ExecutionError::InvalidHeader { key, .. } => {
                Self::validation(message).coded(ErrorCode::InvalidHeader, key)
            }
            ExecutionError::MultipartFileRead {
                path, too_large, ..
            } => {
                let code = if too_large {
                    ErrorCode::RequestTooLarge
                } else {
                    ErrorCode::MultipartRead
                };
                Self::io(message).coded(code, path)
            }
            ExecutionError::InvalidGraphqlVariablesJson(_) => {
                Self::validation(message).coded(ErrorCode::GraphqlVariables, None)
            }
            ExecutionError::GraphqlBodySerialize(_) => {
                Self::internal(message).coded(ErrorCode::GraphqlSerialize, None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pingdeck_base::error::NetworkCause;

    #[test]
    fn send_failures_carry_their_structured_facts_on_the_wire() {
        let network = AppError::from(ExecutionError::Network {
            message: "Connection refused".into(),
            attempt_count: 3,
            connect_phase: true,
            cause: Some(NetworkCause::Refused),
        });
        assert_eq!(
            serde_json::to_string(&network).unwrap(),
            r#"{"kind":"Network","message":"network request failed: Connection refused","connect_phase":true,"attempt_count":3,"cause":"refused"}"#
        );
        let unresolved = AppError::from(ExecutionError::UnresolvedVariable {
            keys: vec!["host".into(), "token".into(), "id".into()],
        });
        assert_eq!(
            serde_json::to_string(&unresolved).unwrap(),
            r#"{"kind":"Validation","message":"unresolved variable host","code":"unresolved_variable","subject":"host","keys":["host","token","id"]}"#
        );
        let cancelled = AppError::from(ExecutionError::Cancelled { bytes_received: 12 });
        assert_eq!(
            serde_json::to_string(&cancelled).unwrap(),
            r#"{"kind":"Cancelled","message":"Request cancelled","bytes_received":12}"#
        );
        // Other network errors and old payloads have neither field.
        assert_eq!(
            serde_json::to_string(&AppError::network("offline")).unwrap(),
            r#"{"kind":"Network","message":"offline"}"#
        );
        assert_eq!(
            serde_json::from_str::<AppError>(
                r#"{"kind":"Cancelled","message":"Request cancelled"}"#
            )
            .unwrap(),
            AppError::cancelled()
        );
    }

    #[test]
    fn local_client_setup_failures_are_validation_not_network() {
        assert!(matches!(
            AppError::from(ExecutionError::ClientBuild("invalid proxy URL: x".into())),
            AppError::Validation { .. }
        ));
        assert!(matches!(
            AppError::from(ExecutionError::BodyRead("reset".into())),
            AppError::Network { .. }
        ));
    }

    #[test]
    fn execution_errors_carry_a_stable_code_and_the_subject_they_name() {
        let cases = [
            (
                ExecutionError::UnresolvedVariable {
                    keys: vec!["token".to_owned()],
                },
                ErrorCode::UnresolvedVariable,
                Some("token"),
                "Validation",
            ),
            (
                ExecutionError::RedirectHostNotDeclared {
                    host: "evil.example".to_owned(),
                },
                ErrorCode::RedirectRefused,
                Some("evil.example"),
                "Validation",
            ),
            (
                ExecutionError::GuardRefused {
                    host: "169.254.169.254".to_owned(),
                },
                ErrorCode::GuardRefused,
                Some("169.254.169.254"),
                "Validation",
            ),
            (
                ExecutionError::InvalidHeader {
                    key: "X-Bad".to_owned(),
                    message: "no".to_owned(),
                },
                ErrorCode::InvalidHeader,
                Some("X-Bad"),
                "Validation",
            ),
            (
                ExecutionError::MultipartFileRead {
                    path: "/tmp/a.bin".to_owned(),
                    message: "file is larger than the 10 MiB request body limit".to_owned(),
                    too_large: true,
                },
                ErrorCode::RequestTooLarge,
                Some("/tmp/a.bin"),
                "Io",
            ),
            (
                ExecutionError::MultipartFileRead {
                    path: "/tmp/a.bin".to_owned(),
                    message: "not a regular file".to_owned(),
                    too_large: false,
                },
                ErrorCode::MultipartRead,
                Some("/tmp/a.bin"),
                "Io",
            ),
            (
                ExecutionError::UnsupportedProtocol,
                ErrorCode::UnsupportedProtocol,
                None,
                "Unsupported",
            ),
            (
                ExecutionError::OAuth2Token {
                    message: "denied".to_owned(),
                },
                ErrorCode::OauthToken,
                None,
                "Network",
            ),
        ];
        for (error, code, subject, kind) in cases {
            let app = AppError::from(error);
            assert_eq!(app.code(), Some(code));
            let json = serde_json::to_value(&app).unwrap();
            assert_eq!(json["kind"], kind);
            assert_eq!(json.get("subject").and_then(|s| s.as_str()), subject);
            assert_eq!(serde_json::from_value::<AppError>(json).unwrap(), app);
        }
    }
}
