//! Measured capability states derived from a finished check: which [`Capability`] each result
//! measured and with which [`Measurement`]. Measurements are results, not configuration (D-43).
//! A measurement never replaces a `UserOverride`
//! ([`crate::llm::capabilities::ModelCapabilities::apply_measurement`] enforces it), and only
//! Apply writes measurements into the matrix routing filters on (`apply::plan_capabilities`).

use super::status::answered;
use super::{ProbeResult, ProbeStatus, ProbeTest};
use crate::llm::capabilities::{Capability, Measurement};

/// One state a check measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityUpdate {
    /// The capability measured.
    pub capability: Capability,
    /// What the check found.
    pub measurement: Measurement,
    /// Vector dimension of a passing Embeddings check.
    pub dimension: Option<u64>,
}

/// What `result` measured, or `None` when the check says nothing about the model.
///
/// The measurement follows the cell's status ([`ProbeStatus::from_result`]), so the bench and the
/// matrix never disagree: Ready is `Yes` (Streaming and Embeddings included, the latter with its
/// dimension); Unsupported (HTTP 415, 501, a protocol mismatch, or a model that answered but did
/// not read the image) is `No`; Not observed (Tools, Reasoning, Cache, Search answered without
/// showing it) is `NotObserved`; an Invalid response is `No` only when the model answered with a
/// wrong, checkable result (JSON schema, PDF). A refused request (HTTP 400 or 422 on any check)
/// writes nothing, and so do a failed Stream, Rejected, Rate limited, Unreachable, Timed out,
/// cancelled and Skipped results.
#[must_use]
pub fn derive_capability(result: &ProbeResult) -> Option<CapabilityUpdate> {
    let test = ProbeTest::parse(&result.test)?;
    let capability = match test {
        ProbeTest::Vision => Capability::Vision,
        ProbeTest::Tools => Capability::Tools,
        ProbeTest::JsonSchema => Capability::JsonSchema,
        ProbeTest::Reasoning => Capability::Reasoning,
        ProbeTest::Cache => Capability::PromptCaching,
        ProbeTest::Search => Capability::WebSearch,
        ProbeTest::Pdf => Capability::Document,
        ProbeTest::Embeddings => Capability::Embeddings,
        ProbeTest::Stream => Capability::Streaming,
        ProbeTest::Connectivity
        | ProbeTest::Auth
        | ProbeTest::Models
        | ProbeTest::Chat
        | ProbeTest::Decision
        | ProbeTest::Context => return None,
    };
    let status = ProbeStatus::from_result(result)?;
    let answered = answered(result.status);
    let measurement = match status {
        ProbeStatus::Ready => Measurement::Yes,
        // A stream that failed says nothing about the model's other abilities.
        _ if test == ProbeTest::Stream => return None,
        ProbeStatus::NotObserved => Measurement::NotObserved,
        ProbeStatus::Unsupported => Measurement::No,
        ProbeStatus::InvalidResponse if answered && test != ProbeTest::Embeddings => {
            Measurement::No
        }
        _ => return None,
    };
    let dimension = (test == ProbeTest::Embeddings && measurement == Measurement::Yes)
        .then(|| result.detail["dimension"].as_u64())
        .flatten();
    Some(CapabilityUpdate {
        capability,
        measurement,
        dimension,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::llm::capabilities::{CapabilitySource, ModelCapabilities};
    use crate::llm::failure::FailureKind;
    use crate::llm::probe::test_support::probe_result;

    fn result(
        test: ProbeTest,
        ok: bool,
        status: Option<u16>,
        kind: Option<FailureKind>,
    ) -> ProbeResult {
        ProbeResult {
            ok,
            status,
            failure_kind: kind,
            ..probe_result(test)
        }
    }

    /// Records `update` at `now` the way Apply does.
    fn record(capabilities: &mut ModelCapabilities, update: &CapabilityUpdate, now: i64) -> bool {
        capabilities.apply_measurement(update.capability, update.measurement, now)
    }

    fn measured(
        test: ProbeTest,
        ok: bool,
        status: Option<u16>,
        kind: Option<FailureKind>,
    ) -> Option<Measurement> {
        derive_capability(&result(test, ok, status, kind)).map(|update| update.measurement)
    }

    /// A 2xx answer that failed its content test.
    fn failed_answer(test: ProbeTest) -> Option<Measurement> {
        measured(test, false, Some(200), Some(FailureKind::Unknown))
    }

    #[test]
    fn ready_checks_measure_yes() {
        for (test, capability) in [
            (ProbeTest::Vision, Capability::Vision),
            (ProbeTest::Tools, Capability::Tools),
            (ProbeTest::JsonSchema, Capability::JsonSchema),
            (ProbeTest::Reasoning, Capability::Reasoning),
            (ProbeTest::Cache, Capability::PromptCaching),
            (ProbeTest::Search, Capability::WebSearch),
            (ProbeTest::Pdf, Capability::Document),
            (ProbeTest::Embeddings, Capability::Embeddings),
            (ProbeTest::Stream, Capability::Streaming),
        ] {
            let update = derive_capability(&result(test, true, Some(200), None)).unwrap();
            assert_eq!(update.capability, capability);
            assert_eq!(update.measurement, Measurement::Yes, "{test:?}");
        }
    }

    #[test]
    fn other_checks_measure_no_capability() {
        for test in [
            ProbeTest::Connectivity,
            ProbeTest::Auth,
            ProbeTest::Models,
            ProbeTest::Chat,
            ProbeTest::Decision,
            ProbeTest::Context,
        ] {
            assert_eq!(measured(test, true, Some(200), None), None, "{test:?}");
        }
    }

    #[test]
    fn checkable_wrong_answers_are_measured_no() {
        // The model answered "blue" to the red image.
        for test in [ProbeTest::Vision, ProbeTest::Pdf] {
            assert_eq!(failed_answer(test), Some(Measurement::No), "{test:?}");
        }
        assert_eq!(
            measured(
                ProbeTest::JsonSchema,
                false,
                Some(200),
                Some(FailureKind::ProtocolMismatch)
            ),
            Some(Measurement::No)
        );
        // The provider said the feature does not exist.
        for (test, status) in [(ProbeTest::Vision, 415), (ProbeTest::JsonSchema, 501)] {
            assert_eq!(
                measured(test, false, Some(status), Some(FailureKind::ProviderError)),
                Some(Measurement::No)
            );
        }
    }

    #[test]
    fn failed_content_tests_are_only_not_observed() {
        for test in [
            ProbeTest::Tools,
            ProbeTest::Reasoning,
            ProbeTest::Cache,
            ProbeTest::Search,
        ] {
            assert_eq!(
                failed_answer(test),
                Some(Measurement::NotObserved),
                "{test:?}"
            );
        }
    }

    #[test]
    fn only_an_explicit_refusal_of_the_feature_is_measured_no() {
        assert_eq!(
            measured(
                ProbeTest::Embeddings,
                false,
                Some(501),
                Some(FailureKind::ProviderError)
            ),
            Some(Measurement::No)
        );
        // A 400 or 422 refused the request (often a parameter), not the feature: no verdict, so
        // routing never drops a model that does call tools or reason.
        for test in [
            ProbeTest::Tools,
            ProbeTest::Reasoning,
            ProbeTest::Vision,
            ProbeTest::JsonSchema,
            ProbeTest::Cache,
        ] {
            for status in [400, 422] {
                assert_eq!(
                    measured(test, false, Some(status), Some(FailureKind::ProviderError)),
                    None,
                    "{test:?} {status}"
                );
            }
        }
    }

    #[test]
    fn no_http_answer_and_no_verdict_learns_nothing() {
        assert_eq!(
            measured(ProbeTest::Tools, false, None, Some(FailureKind::Unknown)),
            None
        );
    }

    #[test]
    fn rejected_rate_limited_unreachable_and_timed_out_write_nothing() {
        for (status, kind) in [
            (Some(403), FailureKind::Auth),
            (Some(429), FailureKind::RateLimit),
            (Some(429), FailureKind::Unknown),
            (Some(503), FailureKind::ProviderError),
            (None, FailureKind::Network),
            (None, FailureKind::Timeout),
            (None, FailureKind::Cancelled),
        ] {
            for test in [ProbeTest::Vision, ProbeTest::Tools, ProbeTest::Stream] {
                assert_eq!(
                    measured(test, false, status, Some(kind)),
                    None,
                    "{test:?} {kind:?} {status:?}"
                );
            }
        }
    }

    #[test]
    fn a_failed_stream_never_marks_streaming() {
        assert_eq!(
            measured(
                ProbeTest::Stream,
                false,
                Some(200),
                Some(FailureKind::EmptyOutput)
            ),
            None
        );
    }

    #[test]
    fn embeddings_store_the_dimension_and_ignore_a_bad_body() {
        let mut ok = result(ProbeTest::Embeddings, true, Some(200), None);
        ok.detail = json!({"dimension": 3072});
        assert_eq!(derive_capability(&ok).unwrap().dimension, Some(3072));
        assert_eq!(
            measured(
                ProbeTest::Embeddings,
                false,
                Some(200),
                Some(FailureKind::ProtocolMismatch)
            ),
            None
        );
    }

    #[test]
    fn measurements_never_replace_a_user_override() {
        let mut capabilities = ModelCapabilities::default();
        capabilities.apply(Capability::Vision, true, CapabilitySource::UserOverride);
        let update = derive_capability(&result(
            ProbeTest::Vision,
            false,
            Some(200),
            Some(FailureKind::Unknown),
        ))
        .unwrap();
        assert!(!record(&mut capabilities, &update, 10));
        assert_eq!(capabilities.known(Capability::Vision), Some(true));
        assert_eq!(
            capabilities.get(Capability::Vision).source,
            CapabilitySource::UserOverride
        );
    }

    #[test]
    fn a_blue_vision_answer_is_measured_no_and_a_later_timeout_leaves_it() {
        let mut capabilities = ModelCapabilities::default();
        capabilities.apply(Capability::Vision, true, CapabilitySource::Declared);
        let blue = derive_capability(&result(
            ProbeTest::Vision,
            false,
            Some(200),
            Some(FailureKind::Unknown),
        ))
        .unwrap();
        assert!(record(&mut capabilities, &blue, 100));
        assert!(capabilities.known_absent(Capability::Vision));
        let state = capabilities.get(Capability::Vision);
        assert_eq!((state.supported, state.measured_at_ms), (false, Some(100)));
        let timed_out = result(ProbeTest::Vision, false, None, Some(FailureKind::Timeout));
        assert_eq!(derive_capability(&timed_out), None);
        assert!(capabilities.known_absent(Capability::Vision));
    }

    #[test]
    fn tools_without_a_call_are_not_observed_and_the_samples_accumulate() {
        let mut capabilities = ModelCapabilities::default();
        let update = derive_capability(&result(
            ProbeTest::Tools,
            false,
            Some(200),
            Some(FailureKind::Unknown),
        ))
        .unwrap();
        assert!(record(&mut capabilities, &update, 1));
        let state = capabilities.get(Capability::Tools);
        assert!(state.not_observed && state.samples == 1 && !state.supported);
        // Never filters routing: it is not a measured absence.
        assert!(!capabilities.known_absent(Capability::Tools));
        record(&mut capabilities, &update, 2);
        assert_eq!(capabilities.get(Capability::Tools).samples, 2);
        // A later pass turns it into a measured yes.
        let pass = derive_capability(&result(ProbeTest::Tools, true, Some(200), None)).unwrap();
        record(&mut capabilities, &pass, 3);
        let state = capabilities.get(Capability::Tools);
        assert!(state.supported && !state.not_observed);
    }
}
