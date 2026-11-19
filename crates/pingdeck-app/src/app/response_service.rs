//! Response-body viewing use-cases: formatting and JSONPath over inline text, inline Base64 or a
//! stored body handle.
//!
//! Single responsibility: the one implementation of every body view a shell shows. A large body
//! is referenced by its `body_handle`, so its bytes never travel to the shell just to be
//! formatted; only the (usually smaller) formatted text comes back.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};

use crate::{
    error::AppError,
    executor::decode_body_text,
    response_viewer::{
        FormattedBodyDto, JsonPathFilterOutcomeDto, ResponseBodyFormat, filter_response_jsonpath,
        format_response_body, format_response_bytes,
    },
};

use super::AppState;

/// Where the bytes of a body view come from. Serializes as `{"text": …}`, `{"base64": …}` or
/// `{"handle": …}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodySource {
    /// The body as text (a text view of a UTF-8 body, or any preview).
    Text(String),
    /// The exact bytes, Base64-encoded (`body_base64` of an inline result).
    Base64(String),
    /// A `body_handle` of a stored large body.
    Handle(String),
}

/// Formats a body for one view format (JSON, XML, HTML, YAML, Hex, Base64).
///
/// Fails with `NotFound` for an unknown or evicted handle and `Validation` for malformed Base64.
pub fn response_format_body_impl(
    state: &AppState,
    source: BodySource,
    format: ResponseBodyFormat,
) -> Result<FormattedBodyDto, AppError> {
    let cap = state.settings().data.response_body_cap();
    Ok(match source {
        BodySource::Text(text) => format_response_body(&text, format, cap),
        BodySource::Base64(encoded) => format_response_bytes(&decode(&encoded)?, format, cap),
        BodySource::Handle(handle) => {
            let (bytes, content_type) = state.bodies.get_with_content_type(&handle)?;
            match format {
                ResponseBodyFormat::Hex | ResponseBodyFormat::Base64 => {
                    format_response_bytes(&bytes, format, cap)
                }
                _ => format_response_body(
                    &decode_body_text(&bytes, content_type.as_deref()),
                    format,
                    cap,
                ),
            }
        }
    })
}

/// Evaluates a JSONPath expression over a JSON body.
///
/// Fails like [`response_format_body_impl`]; an invalid expression or non-JSON body is a normal
/// [`JsonPathFilterOutcomeDto::Error`] outcome, not an `Err`.
pub fn response_filter_jsonpath_impl(
    state: &AppState,
    source: BodySource,
    expression: &str,
) -> Result<JsonPathFilterOutcomeDto, AppError> {
    let cap = state.settings().data.response_body_cap();
    Ok(match source {
        BodySource::Text(text) => filter_response_jsonpath(&text, expression, cap),
        BodySource::Base64(encoded) => filter_response_jsonpath(
            &String::from_utf8_lossy(&decode(&encoded)?),
            expression,
            cap,
        ),
        BodySource::Handle(handle) => {
            let (bytes, content_type) = state.bodies.get_with_content_type(&handle)?;
            filter_response_jsonpath(
                &decode_body_text(&bytes, content_type.as_deref()),
                expression,
                cap,
            )
        }
    })
}

fn decode(encoded: &str) -> Result<Vec<u8>, AppError> {
    BASE64_STANDARD
        .decode(encoded)
        .map_err(|error| AppError::validation(format!("invalid response body encoding: {error}")))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    #[test]
    fn every_source_yields_the_same_view_and_a_missing_handle_is_not_found() {
        let state = AppState::open_in_memory().unwrap();
        let handle = state.bodies.put(Bytes::from_static(br#"{"a":[1,2]}"#));
        let views: Vec<_> = [
            BodySource::Text(r#"{"a":[1,2]}"#.into()),
            BodySource::Base64(BASE64_STANDARD.encode(r#"{"a":[1,2]}"#)),
            BodySource::Handle(handle),
        ]
        .into_iter()
        .map(|source| response_format_body_impl(&state, source, ResponseBodyFormat::Json).unwrap())
        .collect();
        assert!(views.windows(2).all(|pair| pair[0] == pair[1]));
        assert!(views[0].is_valid_json && views[0].text.contains("\n  \"a\""));

        let filtered = response_filter_jsonpath_impl(
            &state,
            BodySource::Text(r#"{"a":[1,2]}"#.into()),
            "$.a[1]",
        )
        .unwrap();
        assert_eq!(
            filtered,
            JsonPathFilterOutcomeDto::Filtered {
                text: "2".into(),
                mode: "1 JSONPath match".into()
            }
        );
        assert!(matches!(
            response_format_body_impl(
                &state,
                BodySource::Handle("body_x".into()),
                ResponseBodyFormat::Hex
            ),
            Err(AppError::NotFound { .. })
        ));
        assert!(matches!(
            response_format_body_impl(
                &state,
                BodySource::Base64("***".into()),
                ResponseBodyFormat::Hex
            ),
            Err(AppError::Validation { .. })
        ));
    }

    #[test]
    fn a_stored_body_is_read_with_its_declared_charset() {
        let state = AppState::open_in_memory().unwrap();
        // `{"k":"中文"}` in GBK.
        let gbk = [&br#"{"k":""#[..], &[0xD6, 0xD0, 0xCE, 0xC4], br#""}"#].concat();
        let handle = state
            .bodies
            .put_with_content_type(gbk.into(), Some("application/json; charset=GBK".into()));

        let view = response_format_body_impl(
            &state,
            BodySource::Handle(handle.clone()),
            ResponseBodyFormat::Json,
        )
        .unwrap();
        assert!(view.text.contains("中文"), "{}", view.text);
        assert_eq!(
            response_filter_jsonpath_impl(&state, BodySource::Handle(handle), "$.k").unwrap(),
            JsonPathFilterOutcomeDto::Filtered {
                text: "\"中文\"".into(),
                mode: "1 JSONPath match".into()
            }
        );
    }

    #[test]
    fn a_ten_mebibyte_json_body_formats_natively_through_its_handle() {
        let state = AppState::open_in_memory().unwrap();
        let item = r#"{"id":1,"tags":["a","b"]}"#;
        let body = format!(
            "[{}]",
            vec![item; 10 * 1024 * 1024 / (item.len() + 1) - 1].join(",")
        );
        let handle = state.bodies.put(Bytes::from(body.clone().into_bytes()));

        let view =
            response_format_body_impl(&state, BodySource::Handle(handle), ResponseBodyFormat::Json)
                .unwrap();

        assert_eq!(view.mode, "Pretty printed");
        assert!(view.text.len() > body.len());
    }

    #[test]
    fn the_response_body_cap_setting_sets_what_the_views_format_and_refuse() {
        use serde_json::json;

        use crate::app::settings_update_impl;
        use crate::settings::SettingsSection::Data;

        let state = AppState::open_in_memory().unwrap();
        let item = r#"{"id":1}"#;
        let big = format!(
            "[{}]",
            vec![item; 12 * 1024 * 1024 / (item.len() + 1)].join(",")
        );
        assert!(big.len() > 10 * 1024 * 1024);
        let text = || BodySource::Text(big.clone());

        let refused = response_format_body_impl(&state, text(), ResponseBodyFormat::Json).unwrap();
        assert_eq!(refused.text, big);
        assert!(!refused.is_valid_json);

        settings_update_impl(
            &state,
            Data,
            &json!({"response_body_cap_bytes": 32 * 1024 * 1024}),
        )
        .unwrap();
        let view = response_format_body_impl(&state, text(), ResponseBodyFormat::Json).unwrap();
        assert_eq!(view.mode, "Pretty printed");
        assert!(matches!(
            response_filter_jsonpath_impl(&state, text(), "$[0].id").unwrap(),
            JsonPathFilterOutcomeDto::Filtered { .. }
        ));

        settings_update_impl(&state, Data, &json!({"response_body_cap_bytes": 1024})).unwrap();
        let small = BodySource::Text(format!("[{}]", vec![item; 200].join(",")));
        let view =
            response_format_body_impl(&state, small.clone(), ResponseBodyFormat::Json).unwrap();
        assert_ne!(view.mode, "Pretty printed");
        assert!(matches!(
            response_filter_jsonpath_impl(&state, small, "$[0].id").unwrap(),
            JsonPathFilterOutcomeDto::Error { .. }
        ));
    }
}
