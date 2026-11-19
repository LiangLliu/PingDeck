//! Base-URL normaliser and mount candidate tests.

use super::*;

fn n(input: &str) -> NormalizedUrl {
    normalize_base_url(input).unwrap_or_else(|error| panic!("{input}: {error:?}"))
}

#[test]
fn typed_inputs_normalise_to_origin_mount_and_hint() {
    let rows: [(&str, &str, &str, Option<Protocol>); 15] = [
        ("api.deepseek.com", "https://api.deepseek.com", "", None),
        (
            "https://api.openai.com/v1/chat/completions",
            "https://api.openai.com",
            "/v1",
            Some(Protocol::OpenaiChat),
        ),
        (
            "https://relay.acme.dev/v1/messages",
            "https://relay.acme.dev",
            "/v1",
            Some(Protocol::AnthropicMessages),
        ),
        (
            "https://x.test/v1beta/models/gemini-3.2-flash:generateContent?key=AIzaX",
            "https://x.test",
            "/v1beta",
            Some(Protocol::Gemini),
        ),
        (
            "https://relay.acme.dev/console/token",
            "https://relay.acme.dev",
            "",
            None,
        ),
        (
            "https://gw.example.com/openai/v1#",
            "https://gw.example.com",
            "/openai/v1",
            None,
        ),
        (
            "https://ark.cn-beijing.volces.com/api/v3",
            "https://ark.cn-beijing.volces.com",
            "/api/v3",
            None,
        ),
        (
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
            "https://dashscope.aliyuncs.com",
            "/compatible-mode/v1",
            Some(Protocol::OpenaiChat),
        ),
        (
            "https://api.moonshot.cn/v1/",
            "https://api.moonshot.cn",
            "/v1",
            None,
        ),
        ("localhost:11434", "http://localhost:11434", "", None),
        (
            "http://192.168.1.20:8080/v1",
            "http://192.168.1.20:8080",
            "/v1",
            None,
        ),
        (
            "https://relay.acme.dev:8443/v1/models",
            "https://relay.acme.dev:8443",
            "/v1",
            None,
        ),
        (
            "https://u:p@relay.acme.dev/v1",
            "https://relay.acme.dev",
            "/v1",
            None,
        ),
        ("https://a.test//v1//", "https://a.test", "/v1", None),
        (
            "https://api.anthropic.com/v1/messages",
            "https://api.anthropic.com",
            "/v1",
            Some(Protocol::AnthropicMessages),
        ),
    ];
    for (input, origin, mount, hint) in rows {
        let got = n(input);
        assert_eq!(got.origin, origin, "{input}");
        assert_eq!(got.typed_mount, mount, "{input}");
        assert_eq!(got.tail_hint, hint, "{input}");
    }
    let gemini = n("https://x.test/v1beta/models/gemini-3.2-flash:generateContent?key=AIzaX");
    assert_eq!(gemini.model_hint.as_deref(), Some("gemini-3.2-flash"));
    assert_eq!(gemini.warnings, [SniffWarning::KeyInUrlIgnored]);
    assert_eq!(
        n("https://relay.acme.dev/console/token").warnings,
        [SniffWarning::DashboardPathIgnored]
    );
    assert_eq!(
        n("https://u:p@relay.acme.dev/v1").warnings,
        [SniffWarning::UserinfoIgnored]
    );
    let both = n("https://u:p@relay.acme.dev/v1?key=AIzaX");
    assert_eq!(both.typed, "https://relay.acme.dev/v1");
    assert_eq!(
        both.warnings,
        [SniffWarning::UserinfoIgnored, SniffWarning::KeyInUrlIgnored]
    );
    assert_eq!(
        n("https://api.moonshot.cn/v1/")
            .known_host
            .map(|k| k.region_id),
        Some("china".to_owned())
    );
    assert!(
        n("https://api.anthropic.com/v1/messages")
            .known_host
            .is_some()
    );
    assert!(n("api.deepseek.com").known_host.is_some());
    assert_eq!(n("localhost:11434").host_class, UrlHostClass::Loopback);
    assert_eq!(
        n("http://192.168.1.20:8080/v1").host_class,
        UrlHostClass::Private
    );
    let metadata = n("http://169.254.169.254/latest");
    assert_eq!(metadata.host_class, UrlHostClass::LinkLocal);
    assert_eq!(metadata.typed_mount, "/latest");
    assert_eq!(metadata.origin, "http://169.254.169.254");
}

#[test]
fn invalid_inputs_are_rejected_before_anything_else() {
    for (input, reason) in [
        ("", "empty"),
        ("   ", "empty"),
        ("ftp://x", "scheme"),
        ("javascript:alert(1)", "scheme"),
        ("file:///etc/passwd", "scheme"),
        ("data:text/html,<b>", "scheme"),
        ("https://a.test:0", "port"),
        ("https://a.test:70000", "port"),
        ("https://", "no_host"),
    ] {
        assert_eq!(
            normalize_base_url(input),
            Err(SniffError::InvalidInput {
                reason: reason.to_owned()
            }),
            "{input}"
        );
    }
    let long = format!("https://a.test/{}", "a".repeat(2040));
    assert_eq!(long.chars().count(), 2055);
    assert!(matches!(
        normalize_base_url(&long),
        Err(SniffError::InvalidInput { reason }) if reason == "too_long"
    ));
    let exact = format!("https://a.test/{}", "a".repeat(2048 - 15));
    assert!(normalize_base_url(&exact).is_ok());
}

#[test]
fn trimming_scheme_defaults_host_case_and_ports() {
    assert_eq!(n("  https://a.test/v1 ").typed, "https://a.test/v1");
    assert_eq!(n("\"https://a.test/v1\"").typed, "https://a.test/v1");
    assert_eq!(n("<https://a.test/v1>").typed, "https://a.test/v1");
    assert_eq!(n("api.deepseek.com").origin, "https://api.deepseek.com");
    assert_eq!(n("192.168.1.20:8080").origin, "http://192.168.1.20:8080");
    assert_eq!(n("printer.local/v1").origin, "http://printer.local");
    assert_eq!(n("printer.local").host_class, UrlHostClass::Private);
    assert_eq!(n("[::1]:8080/v1").origin, "http://[::1]:8080");
    assert_eq!(
        n("https://API.Example.COM").origin,
        "https://api.example.com"
    );
    assert_eq!(n("https://münchen.test").host, "xn--mnchen-3ya.test");
    assert_eq!(n("https://a.test:443").origin, "https://a.test");
    assert_eq!(n("https://a.test:8443").port, Some(8443));
    assert_eq!(n("https://a.test/Proxy/V1/").typed_mount, "/Proxy/V1");
    assert!(
        n("http://relay.acme.dev")
            .warnings
            .contains(&SniffWarning::PlainHttpPublic)
    );
    assert!(
        !n("http://localhost:1234")
            .warnings
            .contains(&SniffWarning::PlainHttpPublic)
    );
}

#[test]
fn every_operation_tail_is_stripped_with_its_hint() {
    let cases: [(&str, Option<Protocol>); 16] = [
        ("/v1/chat/completions", Some(Protocol::OpenaiChat)),
        ("/v1/responses", Some(Protocol::OpenaiResponses)),
        ("/v1/messages", Some(Protocol::AnthropicMessages)),
        (
            "/v1/messages/count_tokens",
            Some(Protocol::AnthropicMessages),
        ),
        ("/v1/embeddings", Some(Protocol::Embeddings)),
        ("/v1/systemone", Some(Protocol::DecisionSystemone)),
        ("/v1/decisions", Some(Protocol::DecisionOpenai)),
        ("/v1/models", None),
        ("/v1/models/gpt-4o", None),
        ("/v1beta/models/g-1:generateContent", Some(Protocol::Gemini)),
        (
            "/v1beta/models/g-1:streamGenerateContent",
            Some(Protocol::Gemini),
        ),
        ("/v1beta/models/g-1:embedContent", Some(Protocol::Gemini)),
        (
            "/v1beta/models/g-1:batchEmbedContents",
            Some(Protocol::Gemini),
        ),
        ("/v1beta/models/g-1:countTokens", Some(Protocol::Gemini)),
        ("/v1/models/models", None),
        ("/v1/chat/completions/", Some(Protocol::OpenaiChat)),
    ];
    for (path, hint) in cases {
        let got = n(&format!("https://x.test{path}"));
        assert!(
            got.typed_mount == "/v1" || got.typed_mount == "/v1beta",
            "{path} -> {}",
            got.typed_mount
        );
        assert_eq!(got.tail_hint, hint, "{path}");
        assert_eq!(
            got.model_hint.is_some(),
            hint == Some(Protocol::Gemini),
            "{path}"
        );
    }
    // `#` keeps the tail but still reports it.
    let verbatim = n("https://x.test/v1/chat/completions#");
    assert!(verbatim.as_typed);
    assert_eq!(verbatim.typed_mount, "/v1/chat/completions");
    assert_eq!(verbatim.base_mount, "/v1");
    assert_eq!(verbatim.tail_hint, Some(Protocol::OpenaiChat));
    assert_eq!(verbatim.typed, "https://x.test/v1/chat/completions#");
}

#[test]
fn dashboard_segments_cut_the_rest_unless_typed_as_is() {
    for segment in DASHBOARD_SEGMENTS {
        let got = n(&format!("https://x.test/{segment}/a/b"));
        assert_eq!(got.typed_mount, "");
        assert_eq!(got.warnings, [SniffWarning::DashboardPathIgnored]);
    }
    assert_eq!(n("https://x.test/v1/console").typed_mount, "/v1/console");
    assert_eq!(n("https://x.test/console/x#").typed_mount, "/console/x");
}

#[test]
fn version_segments_and_roots() {
    for (segment, version) in [
        ("v1", true),
        ("v3", true),
        ("v1beta", true),
        ("v2beta3", true),
        ("V1", false),
        ("v", false),
        ("v1alpha", false),
        ("version", false),
        ("v1beta2x", false),
    ] {
        assert_eq!(is_version_segment(segment), version, "{segment}");
    }
    let versioned = n("https://x.test/api/v3");
    assert_eq!(
        (versioned.typed_mount.as_str(), versioned.root.as_str()),
        ("/api/v3", "/api")
    );
    let plain = n("https://x.test/v1/");
    assert_eq!(
        (plain.typed_mount.as_str(), plain.root.as_str()),
        ("/v1", "")
    );
    let upper = n("https://x.test/V1");
    assert_eq!(
        (upper.typed_mount.as_str(), upper.root.as_str()),
        ("/V1", "/V1")
    );
}

#[test]
fn mount_candidates_per_protocol() {
    assert_eq!(n("https://relay.acme.dev").openai_candidates(), ["/v1", ""]);
    assert_eq!(
        n("https://ark.cn-beijing.volces.com/api/v3").openai_candidates(),
        ["/api/v3"]
    );
    assert_eq!(n("https://api.deepseek.com").openai_candidates(), [""]);
    assert_eq!(
        n("https://api.deepseek.com/v1/chat/completions").openai_candidates(),
        [""]
    );
    assert_eq!(n("https://api.moonshot.cn").openai_candidates(), ["/v1"]);
    assert_eq!(
        n("https://qianfan.baidubce.com/v2").openai_candidates(),
        ["/v2"]
    );
    assert_eq!(
        n("https://x.test/openai").openai_candidates(),
        ["/openai", "/openai/v1"]
    );
    assert_eq!(n("https://relay.acme.dev/v1").openai_candidates(), ["/v1"]);
    assert_eq!(
        n("https://gw.example.com/openai/v1#").openai_candidates(),
        ["/openai/v1"]
    );
    // Anthropic and Gemini have no OpenAI-style mount.
    assert!(
        n("https://api.anthropic.com")
            .openai_candidates()
            .is_empty()
    );
    assert!(
        n("https://generativelanguage.googleapis.com")
            .openai_candidates()
            .is_empty()
    );

    let relay = n("https://relay.acme.dev");
    assert_eq!(relay.messages_candidates(false), ["/v1"]);
    let extended = relay.messages_candidates(true);
    assert_eq!(extended[0], "/v1");
    assert!(extended.contains(&"/api/compatible/v1".to_owned()) && extended.len() == 6);
    assert_eq!(
        n("https://relay.acme.dev/api/v3").messages_candidates(false),
        ["/api/v1"]
    );
    assert_eq!(
        n("https://api.deepseek.com").messages_candidates(false),
        ["/anthropic/v1"]
    );
    assert!(
        n("https://api.groq.com")
            .messages_candidates(true)
            .is_empty()
    );
    assert_eq!(relay.gemini_candidates(false), ["/v1beta"]);
    assert_eq!(relay.gemini_candidates(true), ["/v1beta", "/v1"]);
    assert_eq!(
        n("https://generativelanguage.googleapis.com").gemini_candidates(true),
        ["/v1beta"]
    );
    assert!(
        n("https://api.deepseek.com")
            .gemini_candidates(false)
            .is_empty()
    );
}

#[test]
fn the_models_url_stays_on_the_typed_origin() {
    assert_eq!(
        same_origin_path("https://a.test", "/x/models").unwrap(),
        "/x/models"
    );
    assert_eq!(
        same_origin_path("https://a.test", "https://A.test/x/models?all=1").unwrap(),
        "/x/models?all=1"
    );
    for bad in [
        "https://evil.test/models",
        "//evil.test/models",
        "http://a.test/m",
        "nonsense",
    ] {
        assert!(same_origin_path("https://a.test", bad).is_err(), "{bad}");
    }
    // A typed origin smuggled in as userinfo does not make another host the same origin.
    assert!(same_origin_path("https://a.test", "https://a.test@evil.test/m").is_err());
}
