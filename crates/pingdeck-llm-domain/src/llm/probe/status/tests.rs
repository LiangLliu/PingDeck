use super::*;
use crate::llm::probe::test_support::probe_result;

/// A failed result; `test` is free text so an unknown name can be tried too.
fn result(test: &str, status: Option<u16>, kind: Option<FailureKind>) -> ProbeResult {
    ProbeResult {
        test: test.to_owned(),
        status,
        failure_kind: kind,
        ..probe_result(ProbeTest::Chat)
    }
}

fn word(test: &str, status: Option<u16>, kind: FailureKind) -> Option<ProbeStatus> {
    ProbeStatus::from_result(&result(test, status, Some(kind)))
}

#[test]
fn labels_are_the_nine_words_and_identifiers_round_trip() {
    let labels: Vec<_> = ProbeStatus::ALL.iter().map(|s| s.label()).collect();
    assert_eq!(
        labels,
        [
            "Ready",
            "Rejected",
            "Rate limited",
            "Unreachable",
            "Invalid response",
            "Unsupported",
            "Not observed",
            "Skipped",
            "Timed out"
        ]
    );
    for status in ProbeStatus::ALL {
        assert_eq!(serde_json::to_value(status).unwrap(), status.as_str());
        assert_eq!(ProbeStatus::parse(status.as_str()), Some(status));
    }
    assert_eq!(ProbeStatus::parse("unknown"), None);
    assert!(ProbeStatus::TimedOut.is_failure() && !ProbeStatus::Unsupported.is_failure());
    assert!(ProbeStatus::RateLimited.is_failure());
    // Not observed is no failure, yet a retry runs it again; Ready and Unsupported are final.
    assert!(!ProbeStatus::NotObserved.is_failure() && ProbeStatus::NotObserved.is_retryable());
    assert!(!ProbeStatus::Ready.is_retryable() && !ProbeStatus::Unsupported.is_retryable());
}

#[test]
fn a_passing_result_is_ready_whatever_its_status() {
    let mut ok = result("auth", Some(400), None);
    ok.ok = true;
    assert_eq!(ProbeStatus::from_result(&ok), Some(ProbeStatus::Ready));
}

/// 13 failure kinds x 12 status codes on a chat check. Each row is the expected word per status:
/// R Rejected, L Rate limited, T Timed out, U Unreachable, I Invalid response, `-` nothing stored.
#[test]
fn chat_table_over_kinds_and_statuses() {
    let statuses = [
        None,
        Some(200),
        Some(400),
        Some(401),
        Some(402),
        Some(403),
        Some(404),
        Some(408),
        Some(429),
        Some(500),
        Some(503),
        Some(504),
    ];
    let table = [
        (FailureKind::Auth, "RRRRRRRTLRRT"),
        (FailureKind::Region, "RRRRRRRTLRRT"),
        (FailureKind::Quota, "RRRRRRRTLRRT"),
        (FailureKind::RateLimit, "LLLLLLLLLLLL"),
        (FailureKind::ModelUnavailable, "RRRRRRRTLRRT"),
        (FailureKind::PolicyBlocked, "RRRRRRRTLRRT"),
        (FailureKind::Timeout, "TTTTTTTTLTTT"),
        (FailureKind::EmptyOutput, "IIIRRRRTLUUT"),
        (FailureKind::ProtocolMismatch, "IIIRRRRTLUUT"),
        (FailureKind::ProviderError, "IIIRRRRTLUUT"),
        (FailureKind::Unknown, "IIIRRRRTLUUT"),
        (FailureKind::Network, "UUUUUUUTLUUT"),
        (FailureKind::Cancelled, "------------"),
    ];
    assert_eq!(table.len(), 13);
    for (kind, row) in table {
        for (status, code) in statuses.iter().zip(row.chars()) {
            let want = match code {
                'R' => Some(ProbeStatus::Rejected),
                'L' => Some(ProbeStatus::RateLimited),
                'T' => Some(ProbeStatus::TimedOut),
                'U' => Some(ProbeStatus::Unreachable),
                'I' => Some(ProbeStatus::InvalidResponse),
                _ => None,
            };
            assert_eq!(
                word("chat", *status, kind),
                want,
                "chat, kind {kind:?}, status {status:?}"
            );
        }
    }
}

#[test]
fn a_404_is_rejected_on_chat_routes_only() {
    assert_eq!(
        word("chat", Some(404), FailureKind::ProviderError),
        Some(ProbeStatus::Rejected)
    );
    // Elsewhere a bare 404 is still a refused request.
    assert_eq!(
        word("tools", Some(404), FailureKind::ProviderError),
        Some(ProbeStatus::Rejected)
    );
}

#[test]
fn only_explicit_capability_refusals_are_unsupported() {
    let tests = [
        "tools",
        "json_schema",
        "vision",
        "embeddings",
        "search",
        "pdf",
        "reasoning",
    ];
    for status in [415, 501] {
        for test in tests {
            assert_eq!(
                word(test, Some(status), FailureKind::ProviderError),
                Some(ProbeStatus::Unsupported),
                "{test} {status}"
            );
        }
    }
    // A 400 or 422 is a refused request on every check: often a parameter, never a verdict on
    // the capability.
    for status in [400, 422] {
        for test in tests.iter().chain(&["chat", "stream"]) {
            assert_eq!(
                word(test, Some(status), FailureKind::ProviderError),
                Some(ProbeStatus::InvalidResponse),
                "{test} {status}"
            );
        }
    }
    assert_eq!(
        word("chat", Some(415), FailureKind::ProviderError),
        Some(ProbeStatus::Rejected)
    );
    assert_eq!(
        word("vision", None, FailureKind::ProtocolMismatch),
        Some(ProbeStatus::Unsupported)
    );
}

#[test]
fn failed_content_tests_follow_the_pass_rules() {
    let content = |test: &str| word(test, Some(200), FailureKind::Unknown);
    for test in ["chat", "stream", "embeddings", "pdf"] {
        assert_eq!(content(test), Some(ProbeStatus::InvalidResponse), "{test}");
    }
    assert_eq!(
        word("json_schema", Some(200), FailureKind::ProtocolMismatch),
        Some(ProbeStatus::InvalidResponse)
    );
    assert_eq!(content("vision"), Some(ProbeStatus::Unsupported));
    for test in ["tools", "reasoning", "cache", "search"] {
        assert_eq!(content(test), Some(ProbeStatus::NotObserved), "{test}");
    }
}

#[test]
fn provider_outage_is_unreachable_and_gateway_timeout_timed_out() {
    for status in [500, 502, 503] {
        assert_eq!(
            word("stream", Some(status), FailureKind::ProviderError),
            Some(ProbeStatus::Unreachable)
        );
    }
    assert_eq!(
        word("stream", Some(504), FailureKind::ProviderError),
        Some(ProbeStatus::TimedOut)
    );
}

#[test]
fn reason_lines_reproduce_the_prototype_examples() {
    let relay = ReasonContext {
        host: "the relay",
        ..ReasonContext::default()
    };
    let forbidden = result("chat", Some(403), Some(FailureKind::Auth));
    assert_eq!(
        reason_line(&forbidden, ProbeStatus::Rejected, &relay),
        "Rejected: this key cannot use the model (403). Enable it on the relay, then test again."
    );
    let timed_out = result("chat", None, Some(FailureKind::Timeout));
    assert_eq!(
        reason_line(&timed_out, ProbeStatus::TimedOut, &relay),
        "Timed out: no first token in 30 s. The relay may be queueing it upstream. Test again."
    );
}

#[test]
fn rate_limit_names_the_wait() {
    let limited = result("chat", Some(429), Some(FailureKind::RateLimit));
    let context = ReasonContext {
        retry_after_s: Some(8),
        ..ReasonContext::default()
    };
    assert_eq!(
        reason_line(&limited, ProbeStatus::RateLimited, &context),
        "Rate limited, retry in 8 s."
    );
    assert!(
        reason_line(
            &limited,
            ProbeStatus::RateLimited,
            &ReasonContext::default()
        )
        .starts_with("Rate limited.")
    );
}

#[test]
fn a_429_is_never_rejected_whatever_the_check_or_kind() {
    for test in ProbeTest::ALL {
        for kind in [
            FailureKind::RateLimit,
            FailureKind::Quota,
            FailureKind::ProviderError,
            FailureKind::Unknown,
        ] {
            assert_eq!(
                word(test.as_str(), Some(429), kind),
                Some(ProbeStatus::RateLimited),
                "{test:?} {kind:?}"
            );
        }
    }
}

#[test]
fn a_refused_request_says_so_and_is_marked_on_every_check() {
    for (test, status) in [
        ("chat", 400),
        ("stream", 422),
        ("reasoning", 400),
        ("tools", 400),
        ("vision", 422),
    ] {
        let refused = result(test, Some(status), Some(FailureKind::ProviderError));
        assert!(is_request_refused(&refused), "{test}");
        assert_eq!(
            reason_line(
                &refused,
                ProbeStatus::InvalidResponse,
                &ReasonContext::default()
            ),
            format!(
                "Request refused ({status}); often a parameter this model does not accept. Says nothing about support."
            )
        );
    }
    // Not for other statuses, or a 400 that names a rejection.
    assert!(!is_request_refused(&result(
        "chat",
        Some(500),
        Some(FailureKind::ProviderError)
    )));
    assert!(!is_request_refused(&result(
        "chat",
        Some(400),
        Some(FailureKind::PolicyBlocked)
    )));
    assert!(!is_request_refused(&result(
        "chat",
        Some(200),
        Some(FailureKind::EmptyOutput)
    )));
}

#[test]
fn every_reason_fits_and_carries_no_provider_text() {
    let secret_message = "sk-live-abcdef0123456789 leaked";
    let long_host = "the very long named relay of a provider that keeps going ".repeat(4);
    for test in ProbeTest::ALL {
        for kind in [
            FailureKind::Auth,
            FailureKind::Region,
            FailureKind::Quota,
            FailureKind::RateLimit,
            FailureKind::ModelUnavailable,
            FailureKind::PolicyBlocked,
            FailureKind::Unknown,
            FailureKind::Network,
        ] {
            for code in [None, Some(200), Some(400), Some(403), Some(503)] {
                for status in ProbeStatus::ALL {
                    let mut stored = result(test.as_str(), code, Some(kind));
                    stored.message = secret_message.to_owned();
                    for host in ["the provider", long_host.as_str()] {
                        let line = reason_line(
                            &stored,
                            status,
                            &ReasonContext {
                                host,
                                ..ReasonContext::default()
                            },
                        );
                        assert!(line.chars().count() <= MAX_REASON_CHARS, "{line}");
                        assert!(!line.contains("sk-live"), "{line}");
                        assert_eq!(
                            line.is_empty(),
                            matches!(status, ProbeStatus::Ready | ProbeStatus::Skipped)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn unreachable_and_invalid_lines_name_the_check() {
    let down = result("chat", None, Some(FailureKind::Network));
    assert!(
        reason_line(&down, ProbeStatus::Unreachable, &ReasonContext::default())
            .starts_with("Unreachable: no answer from the provider")
    );
    let tools = result("tools", Some(200), Some(FailureKind::Unknown));
    assert_eq!(
        reason_line(&tools, ProbeStatus::NotObserved, &ReasonContext::default()),
        "Not observed: the model answered without calling the tool. The model may still support tool calls; test again."
    );
    let vision = result("vision", Some(200), Some(FailureKind::Unknown));
    assert_eq!(
        reason_line(&vision, ProbeStatus::Unsupported, &ReasonContext::default()),
        "Unsupported: the model answered but did not read the image."
    );
}
