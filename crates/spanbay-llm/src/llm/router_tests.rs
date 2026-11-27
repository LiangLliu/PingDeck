//! Tests of name resolution: orders, skips, protocol choice, capability filter, errors.

use std::time::Duration;

use super::*;
use crate::llm::capabilities::{Capability, CapabilitySource, ModelCapabilities};
use crate::llm::failover::FailureObservation;
use crate::llm::health::{CircuitConfig, HealthBook, Outcome};
use crate::llm::hub::{HubImage, HubMessage, HubPart, HubRequest, HubTool};
use crate::llm::store::{KeyModel, KeyStrategy, ModelRef, ModelSource, ProviderKey};
use serde_json::json;

fn provider(id: &str, enabled: bool, protocols: &[Protocol]) -> Provider {
    Provider {
        id: id.into(),
        name: id.to_uppercase(),
        category: "custom".into(),
        base_url: format!("https://{id}.test"),
        auth: AuthStyle::Bearer,
        endpoints: protocols
            .iter()
            .map(|&protocol| ProviderEndpoint {
                protocol,
                path_prefix: "/v1".into(),
                auth: None,
                url_mode: Default::default(),
                enabled: true,
            })
            .collect(),
        enabled,
        ..Default::default()
    }
}

fn key(id: &str, provider: &str, enabled: bool) -> ResolvedKey {
    ResolvedKey {
        key: ProviderKey {
            id: id.into(),
            provider_id: provider.into(),
            name: id.into(),
            enabled,
            ..Default::default()
        },
        secret: format!("secret-{id}"),
        ..Default::default()
    }
}

fn model_ref((provider, model): (&str, &str)) -> ModelRef {
    ModelRef {
        provider_id: provider.into(),
        model: model.into(),
    }
}

fn set_chain(snapshot: &mut RoutingSnapshot, primary: (&str, &str), chain: &[(&str, &str)]) {
    let primary = model_ref(primary);
    snapshot.gateway_fallbacks.clear();
    snapshot.gateway_fallbacks.insert(
        primary.clone(),
        chain.iter().copied().map(model_ref).collect(),
    );
    snapshot.legacy_names.clear();
    snapshot.legacy_names.insert("smart".into(), primary);
}

fn model(provider: &str, id: &str, enabled: bool) -> ProviderModel {
    ProviderModel {
        provider_id: provider.into(),
        model_id: id.into(),
        source: ModelSource::Manual,
        enabled,
        ..Default::default()
    }
}

fn without(capability: Capability, mut model: ProviderModel) -> ProviderModel {
    model
        .capabilities
        .apply(capability, false, CapabilitySource::UserOverride);
    model
}

fn snapshot() -> RoutingSnapshot {
    let mut s = RoutingSnapshot::default();
    for p in [
        provider("a", true, &[Protocol::OpenaiChat]),
        provider("b", true, &[Protocol::AnthropicMessages]),
        provider("off", false, &[Protocol::OpenaiChat]),
    ] {
        s.providers.insert(p.id.clone(), p);
    }
    s.keys.insert(
        "a".into(),
        vec![
            key("a1", "a", true),
            key("a2", "a", true),
            key("a3", "a", false),
        ],
    );
    s.keys.insert("b".into(), vec![key("b1", "b", true)]);
    s.keys.insert("off".into(), vec![key("o1", "off", true)]);
    set_chain(
        &mut s,
        ("b", "claude"),
        &[("off", "x"), ("a", "gpt"), ("a", "skip")],
    );
    s.models = vec![model("a", "skip", false)];
    s
}

fn none() -> RequestNeeds {
    RequestNeeds::default()
}

fn plan(snapshot: &RoutingSnapshot, name: &str) -> Result<Plan, PlanError> {
    snapshot.plan(name, &none(), None, &HealthBook::new(), &KeyBook::new())
}

fn shape(plan: &Plan) -> Vec<(String, Vec<String>, String)> {
    plan.groups
        .iter()
        .map(|g| {
            (
                g.provider_id.clone(),
                g.targets.iter().map(|t| t.key_id.clone()).collect(),
                g.model.clone(),
            )
        })
        .collect()
}

fn text(parts: &[(&str, &str, &str)]) -> Vec<(String, Vec<String>, String)> {
    parts
        .iter()
        .map(|(p, keys, m)| {
            (
                (*p).to_owned(),
                keys.split(',')
                    .filter(|k| !k.is_empty())
                    .map(str::to_owned)
                    .collect(),
                (*m).to_owned(),
            )
        })
        .collect()
}

fn reasons(plan: &Plan) -> Vec<(String, String)> {
    plan.skipped
        .iter()
        .map(|s| (s.label.clone(), s.reason.clone()))
        .collect()
}

fn targets(snapshot: &RoutingSnapshot, name: &str, book: &HealthBook) -> Vec<Target> {
    snapshot
        .plan(name, &none(), None, book, &KeyBook::new())
        .unwrap()
        .groups
        .into_iter()
        .flat_map(|g| g.targets)
        .collect()
}

fn three_keys(strategy: KeyStrategy, cooldown: bool) -> RoutingSnapshot {
    let mut s = snapshot();
    let p = s.providers.get_mut("a").unwrap();
    p.key_strategy = Some(strategy);
    p.key_cooldown = Some(cooldown);
    s.keys.insert(
        "a".into(),
        vec![
            key("a1", "a", true),
            key("a2", "a", true),
            key("a3", "a", true),
        ],
    );
    s
}

fn order(snapshot: &RoutingSnapshot, name: &str, books: &KeyBook) -> Vec<String> {
    snapshot
        .plan(name, &none(), None, &HealthBook::new(), books)
        .unwrap()
        .groups
        .into_iter()
        .flat_map(|g| g.targets)
        .map(|t| t.key_id)
        .collect()
}

fn switch_off(snapshot: &mut RoutingSnapshot, id: &str, protocol: Protocol) {
    snapshot
        .providers
        .get_mut(id)
        .unwrap()
        .endpoints
        .iter_mut()
        .filter(|ep| ep.protocol == protocol)
        .for_each(|ep| ep.enabled = false);
}

#[test]
fn chain_planning_skips_non_recursion_and_caller_chains() {
    let mut s = snapshot();
    let planned = plan(&s, "smart").unwrap();
    assert_eq!(planned.requested, "smart");
    assert_eq!(
        shape(&planned),
        text(&[("b", "b1", "claude"), ("a", "a1,a2", "gpt")])
    );
    assert_eq!(planned.key_counts(), vec![1, 2]);
    assert_eq!(
        planned.pairs(),
        vec![("b1", "claude"), ("a1", "gpt"), ("a2", "gpt")]
    );
    assert_eq!(
        reasons(&planned),
        vec![
            ("OFF/x".to_owned(), "disabled".to_owned()),
            ("A/skip".to_owned(), "disabled".to_owned())
        ]
    );
    assert_eq!(
        (
            planned.skipped[0].provider_id.as_str(),
            planned.skipped[0].model.as_str(),
            &planned.skipped[0].key_id
        ),
        ("off", "x", &None)
    );
    assert_eq!(shape(&planned), shape(&plan(&s, "B/claude").unwrap()));

    s.gateway_fallbacks
        .insert(model_ref(("a", "gpt")), vec![model_ref(("b", "deep"))]);
    let models: Vec<_> = plan(&s, "smart")
        .unwrap()
        .groups
        .into_iter()
        .map(|g| g.model)
        .collect();
    assert_eq!(models, ["claude", "gpt"]);

    let caller_models = |chain: &[&str]| -> Vec<String> {
        let chain: Vec<String> = chain.iter().map(|c| (*c).to_owned()).collect();
        s.plan_with_fallbacks(
            "b/claude",
            &chain,
            &none(),
            &HealthBook::new(),
            &KeyBook::new(),
        )
        .unwrap()
        .groups
        .into_iter()
        .map(|g| format!("{}/{}", g.provider_id, g.model))
        .collect()
    };
    assert_eq!(caller_models(&[]), ["b/claude"]);
    assert_eq!(caller_models(&["a/deep"]), ["b/claude", "a/deep"]);
    assert_eq!(
        caller_models(&[
            "ghost/x", "b/claude", "A/deep", "a/deep", "a/m1", "a/m2", "a/m3", "a/m4", "a/m5"
        ]),
        ["b/claude", "a/deep", "a/m1", "a/m2", "a/m3", "a/m4"]
    );
}

#[test]
fn reference_resolution_display_scope_and_unknown_errors() {
    let mut s = snapshot();
    for (id, name) in [("p1", "My Proxy 中转"), ("p2", "p1"), ("p3", "Open Router")] {
        let mut p = provider(id, true, &[Protocol::OpenaiChat]);
        p.name = name.into();
        s.providers.insert(id.into(), p);
        s.keys.insert(id.into(), vec![key(id, id, true)]);
    }
    let resolve = |t: &str| s.resolve_ref(t).map(|f| (f.provider_id, f.model));
    let found = |p: &str, m: &str| Some((p.to_owned(), m.to_owned()));

    assert_eq!(resolve("  My Proxy 中转/gpt-4o "), found("p1", "gpt-4o"));
    assert_eq!(resolve("open router/GPT-4o"), found("p3", "GPT-4o"));
    assert_eq!(
        resolve("Open Router/meta-llama/llama-3"),
        found("p3", "meta-llama/llama-3")
    );
    assert_eq!(resolve("p3/m"), found("p3", "m"));
    assert_eq!(resolve("p1/m"), found("p2", "m"));
    assert_eq!(resolve("smart"), found("b", "claude"));
    for nothing in ["ghost/m", "Open Router/", "nope", ""] {
        assert_eq!(resolve(nothing), None, "{nothing}");
    }
    assert_eq!(
        targets(&s, "a/accounts/x/models/y", &HealthBook::new())[0].model,
        "accounts/x/models/y"
    );

    assert_eq!(
        (s.display_ref("p1", "m/x"), s.display_ref("gone", "m")),
        ("My Proxy 中转/m/x".to_owned(), "gone/m".to_owned())
    );
    let allows = |name: &str| name == "p3/m" || name == "b/claude";
    assert!(
        s.allowed_by("Open Router/m", &allows)
            && s.allowed_by("p3/m", &allows)
            && s.allowed_by("smart", &allows)
    );
    assert!(!s.allowed_by("Open Router/other", &allows));

    for name in ["off/m", "nope", "nope/x", "Smart", ""] {
        assert_eq!(
            plan(&s, name).unwrap_err(),
            PlanError::UnknownModel(name.trim().into())
        );
    }
    assert!(plan(&s, "  smart ").is_ok());
    s.legacy_names.clear();
    assert!(matches!(plan(&s, "smart"), Err(PlanError::UnknownModel(_))));
}

#[test]
fn fallback_compatibility_and_call_kind() {
    let a = model_ref(("a", "text-embedding-3-small"));
    let b = model_ref(("b", "openai/text-embedding-3-small"));
    let c = model_ref(("b", "bge-m3"));
    let chat = model_ref(("b", "claude"));
    let dec = CallKind::Decision;
    let ok = |p: &ModelRef, pk, f: &ModelRef, fk| check_fallback((p, pk), (f, fk));

    assert_eq!(ok(&chat, CallKind::Chat, &a, CallKind::Chat), Ok(()));
    assert_eq!(
        ok(&chat, CallKind::Chat, &a, CallKind::Embedding),
        Err(Incompatible::Kind)
    );
    assert_eq!(
        ok(&a, CallKind::Embedding, &chat, CallKind::Chat),
        Err(Incompatible::Kind)
    );
    assert_eq!(ok(&a, CallKind::Embedding, &b, CallKind::Embedding), Ok(()));
    assert_eq!(
        ok(&a, CallKind::Embedding, &c, CallKind::Embedding),
        Err(Incompatible::EmbeddingModel)
    );
    assert_eq!(
        ok(
            &a,
            dec(Protocol::DecisionOpenai),
            &c,
            dec(Protocol::DecisionOpenai)
        ),
        Ok(())
    );
    assert_eq!(
        ok(
            &a,
            dec(Protocol::DecisionOpenai),
            &c,
            dec(Protocol::DecisionSystemone)
        ),
        Err(Incompatible::DecisionProtocol)
    );
    assert_eq!(
        ok(&chat, CallKind::Chat, &c, dec(Protocol::DecisionOpenai)),
        Err(Incompatible::Kind)
    );
    assert_eq!(
        ok(&a, CallKind::Embedding, &a, CallKind::Embedding),
        Err(Incompatible::SelfRef)
    );
    assert_eq!(
        (
            Incompatible::SelfRef.code(),
            Incompatible::EmbeddingModel.code()
        ),
        ("self", "embedding_model")
    );

    let mut s = snapshot();
    s.models = vec![ProviderModel {
        role: Some(ModelRole::Embedding),
        ..model("a", "vectors", true)
    }];
    assert_eq!(s.call_kind("a", "gpt", None), Some(CallKind::Chat));
    assert_eq!(s.call_kind("a", "vectors", None), Some(CallKind::Embedding));
    assert_eq!(
        s.call_kind("a", "text-embedding-3-small", None),
        Some(CallKind::Embedding)
    );
    assert_eq!(s.call_kind("ghost", "m", None), None);
}

#[test]
fn key_rotation_strategies_and_cooldown() {
    let book = HealthBook::new();
    book.record(
        "a1",
        "m",
        Outcome::Failure,
        Duration::ZERO,
        CircuitConfig::default(),
    );
    let primary_keys: Vec<_> = targets(&snapshot(), "a/m", &book)
        .into_iter()
        .map(|t| t.key_id)
        .collect();
    assert_eq!(primary_keys, ["a1", "a2"]);

    let rr = three_keys(KeyStrategy::RoundRobin, true);
    let kb = KeyBook::new();
    let runs: Vec<_> = (0..4).map(|_| order(&rr, "a/m", &kb)).collect();
    assert_eq!(
        runs,
        [
            vec!["a1", "a2", "a3"],
            vec!["a2", "a3", "a1"],
            vec!["a3", "a1", "a2"],
            vec!["a1", "a2", "a3"]
        ]
    );

    let rnd = three_keys(KeyStrategy::Random, true);
    let cycle = ["a1", "a2", "a3", "a1", "a2"];
    for _ in 0..30 {
        let keys = order(&rnd, "a/m", &kb);
        let start = cycle.iter().position(|k| *k == keys[0]).unwrap();
        assert_eq!(keys, cycle[start..start + 3]);
    }

    let pri = three_keys(KeyStrategy::Primary, true);
    let kb = KeyBook::new();
    let rate_limit = |ms| FailureObservation {
        status: Some(429),
        retry_after_ms: Some(ms),
        ..Default::default()
    };
    kb.failed("a1", "m", &rate_limit(20_000), "429".into(), true);
    let p = pri
        .plan("a/m", &none(), None, &HealthBook::new(), &kb)
        .unwrap();
    assert_eq!(shape(&p), text(&[("a", "a2,a3", "m")]));
    assert_eq!(
        reasons(&p),
        vec![("A/m (key a1)".to_owned(), "cooling".to_owned())]
    );
    kb.advance(Duration::from_secs(21));
    assert_eq!(order(&pri, "a/m", &kb), ["a1", "a2", "a3"]);

    for (k, s) in [("a1", 300), ("a2", 30), ("a3", 120)] {
        kb.failed(k, "m", &rate_limit(s * 1000), "429".into(), true);
    }
    let all_cooling = pri
        .plan("a/m", &none(), None, &HealthBook::new(), &kb)
        .unwrap();
    assert_eq!(shape(&all_cooling), text(&[("a", "a2,a3,a1", "m")]));
    assert!(all_cooling.skipped.is_empty());

    let no_cool = three_keys(KeyStrategy::Primary, false);
    let kb = KeyBook::new();
    let rejected = FailureObservation {
        status: Some(401),
        ..Default::default()
    };
    kb.failed("a1", "m", &rejected, "401".into(), false);
    kb.failed("a2", "m", &rejected, "401".into(), true);
    assert_eq!(order(&no_cool, "a/m", &kb), ["a1", "a2", "a3"]);
}

#[test]
fn skip_reasons_disabled_models_scope_and_plan_errors() {
    let mut s = snapshot();
    s.keys.insert("a".into(), vec![key("a3", "a", false)]);
    set_chain(&mut s, ("a", "m"), &[("b", "n")]);
    let p = plan(&s, "smart").unwrap();
    assert_eq!(
        (shape(&p), reasons(&p)),
        (
            text(&[("b", "b1", "n")]),
            vec![("A/m".to_owned(), "no key".to_owned())]
        )
    );
    set_chain(&mut s, ("a", "m"), &[("off", "x")]);
    assert_eq!(
        plan(&s, "smart").unwrap_err(),
        PlanError::NoUsableTarget("smart".into())
    );

    let mut s = snapshot();
    s.providers
        .insert("emb".into(), provider("emb", true, &[Protocol::Embeddings]));
    s.keys.insert("emb".into(), vec![key("e1", "emb", true)]);
    set_chain(&mut s, ("emb", "e"), &[("b", "n")]);
    let p = plan(&s, "smart").unwrap();
    assert_eq!(
        (shape(&p), reasons(&p)),
        (
            text(&[("b", "b1", "n")]),
            vec![("EMB/e".to_owned(), "no chat protocol".to_owned())]
        )
    );

    let mut s = snapshot();
    s.models = vec![model("a", "glm-5", false), model("a", "glm-4", true)];
    assert_eq!(
        plan(&s, "a/glm-5").unwrap_err(),
        PlanError::UnknownModel("a/glm-5".into())
    );
    assert!(plan(&s, "a/glm-4").is_ok() && plan(&s, "a/new-model").is_ok());
    set_chain(&mut s, ("a", "glm-4"), &[("a", "glm-5"), ("b", "n")]);
    let p = plan(&s, "smart").unwrap();
    assert_eq!(
        (shape(&p), reasons(&p)),
        (
            text(&[("a", "a1,a2", "glm-4"), ("b", "b1", "n")]),
            vec![("A/glm-5".to_owned(), "disabled".to_owned())]
        )
    );

    let allows = |name: &str| name == "smart";
    let scope: &dyn Fn(&str) -> bool = &allows;
    let book = HealthBook::new();
    assert!(
        s.plan("smart", &none(), Some(scope), &book, &KeyBook::new())
            .is_ok()
    );
    let err = s
        .plan("a/m", &none(), Some(scope), &book, &KeyBook::new())
        .unwrap_err();
    assert_eq!(err, PlanError::NotAllowed("a/m".into()));
    assert_eq!(
        (err.answer().status, err.answer().kind),
        (403, "permission_error")
    );
    assert!(matches!(
        AppError::from(PlanError::UnknownModel("x".into())),
        AppError::NotFound { .. }
    ));
    assert!(matches!(
        AppError::from(PlanError::NoUsableTarget("x".into())),
        AppError::Validation { .. }
    ));
    assert!(matches!(
        AppError::from(PlanError::no_target("x", 3)),
        AppError::Network { .. }
    ));
}

#[test]
fn circuit_open_skip_earliest_retry_and_late_unavailable() {
    let mut s = snapshot();
    set_chain(&mut s, ("a", "m"), &[("b", "n"), ("off", "x")]);
    let book = HealthBook::new();
    let cfg = |open_s| CircuitConfig {
        failure_threshold: 1,
        open_for: Duration::from_secs(open_s),
        ..CircuitConfig::default()
    };

    book.record("a1", "m", Outcome::Failure, Duration::ZERO, cfg(30));
    let p = s
        .plan("smart", &none(), None, &book, &KeyBook::new())
        .unwrap();
    assert_eq!(shape(&p), text(&[("a", "a2", "m"), ("b", "b1", "n")]));
    assert_eq!(
        (reasons(&p), p.skipped[0].key_id.as_deref()),
        (
            vec![
                ("A/m (key a1)".to_owned(), "circuit open".to_owned()),
                ("OFF/x".to_owned(), "disabled".to_owned()),
            ],
            Some("a1")
        )
    );

    book.record("a2", "m", Outcome::Failure, Duration::ZERO, cfg(30));
    assert_eq!(
        shape(
            &s.plan("smart", &none(), None, &book, &KeyBook::new())
                .unwrap()
        ),
        text(&[("b", "b1", "n")])
    );

    book.record("b1", "n", Outcome::Failure, Duration::ZERO, cfg(9));
    book.advance(Duration::from_millis(500));
    let err = s
        .plan("smart", &none(), None, &book, &KeyBook::new())
        .unwrap_err();
    let PlanError::NoTarget {
        requested,
        retry_after_s,
        message,
    } = &err
    else {
        panic!("{err:?}")
    };
    assert_eq!(
        (requested.as_str(), *retry_after_s, message.as_str()),
        (
            "smart",
            9,
            "No target available for 'smart'. The earliest retry is in 9 s."
        )
    );
    assert_eq!(
        (
            err.answer().status,
            err.answer().kind,
            err.answer().retry_after_s
        ),
        (503, "overloaded_error", Some(9))
    );
    assert_eq!(PlanError::no_target("x", 0).answer().retry_after_s, Some(1));
    assert!(
        AppError::from(err)
            .message()
            .ends_with("The earliest retry is in 9 s.")
    );

    let explained = s
        .plan_explained("a/m", &none(), None, &book, &KeyBook::new())
        .unwrap_err();
    assert_eq!(
        explained
            .skipped
            .iter()
            .map(|sk| sk.reason.as_str())
            .collect::<Vec<_>>(),
        ["circuit open", "circuit open", "circuit open", "disabled"]
    );
    assert!(
        s.plan_explained("off/m", &none(), None, &HealthBook::new(), &KeyBook::new())
            .unwrap_err()
            .skipped
            .is_empty()
    );

    book.advance(Duration::from_secs(30));
    assert_eq!(
        s.plan("smart", &none(), None, &book, &KeyBook::new())
            .unwrap()
            .groups
            .len(),
        2
    );

    set_chain(&mut s, ("b", "n"), &[]);
    let fresh_book = HealthBook::new();
    let p = s
        .plan("smart", &none(), None, &fresh_book, &KeyBook::new())
        .unwrap();
    fresh_book.record("b1", "n", Outcome::Failure, Duration::ZERO, cfg(30));
    assert!(matches!(
        p.unavailable(&fresh_book),
        PlanError::NoTarget {
            retry_after_s: 30,
            ..
        }
    ));
    assert!(matches!(
        p.unavailable(&HealthBook::new()),
        PlanError::NoTarget {
            retry_after_s: 1,
            ..
        }
    ));
}

fn image_request() -> HubRequest {
    let mut request = HubRequest::user_prompt("m", "look");
    request.messages[0].content.push(HubPart::Image {
        image: HubImage::Url {
            url: "https://example.test/a.png".into(),
        },
    });
    request
}

#[test]
fn capability_needs_and_prefilter() {
    let mut req = HubRequest::user_prompt("m", "hi");
    let plain = RequestNeeds::from_hub(&req);
    assert!(!plain.vision && !plain.tools && !plain.document && plain.endpoints.is_empty());
    // One conversation, one affinity: a later turn of it hashes the same.
    let mut later = req.clone();
    later
        .messages
        .push(crate::llm::hub::HubMessage::user("again"));
    assert_eq!(RequestNeeds::from_hub(&later).affinity, plain.affinity);
    assert!(plain.affinity.is_some());
    assert_ne!(
        RequestNeeds::from_hub(&HubRequest::user_prompt("m", "other")).affinity,
        plain.affinity
    );
    let img_needs = RequestNeeds::from_hub(&image_request());
    assert!(img_needs.vision && !img_needs.tools && !img_needs.document);
    req.tools.push(HubTool {
        name: "t".into(),
        description: None,
        parameters: json!({}),
        strict: None,
    });
    assert!(RequestNeeds::from_hub(&req).tools);
    let nested = HubRequest {
        messages: vec![HubMessage {
            role: crate::llm::hub::HubRole::Tool,
            content: vec![HubPart::ToolResult {
                call_id: "c".into(),
                name: None,
                content: image_request().messages.remove(0).content,
                is_error: false,
            }],
        }],
        ..HubRequest::default()
    };
    assert!(RequestNeeds::from_hub(&nested).vision);

    let mut s = snapshot();
    set_chain(&mut s, ("a", "blind"), &[("b", "sees")]);
    s.models = vec![
        without(Capability::Vision, model("a", "blind", true)),
        model("b", "sees", true),
    ];
    let p = s
        .plan(
            "smart",
            &img_needs,
            None,
            &HealthBook::new(),
            &KeyBook::new(),
        )
        .unwrap();
    assert_eq!(
        (shape(&p), reasons(&p)),
        (
            text(&[("b", "b1", "sees")]),
            vec![("A/blind".to_owned(), "no vision".to_owned())]
        )
    );
    assert_eq!(plan(&s, "smart").unwrap().groups.len(), 2);

    let book = HealthBook::new();
    book.record(
        "b1",
        "sees",
        Outcome::Failure,
        Duration::ZERO,
        CircuitConfig {
            failure_threshold: 1,
            ..CircuitConfig::default()
        },
    );
    assert!(matches!(
        s.plan("smart", &img_needs, None, &book, &KeyBook::new()),
        Err(PlanError::NoTarget { .. })
    ));

    s.models[1] = without(Capability::Vision, model("b", "sees", true));
    let err = s
        .plan(
            "smart",
            &img_needs,
            None,
            &HealthBook::new(),
            &KeyBook::new(),
        )
        .unwrap_err();
    assert!(matches!(
        err,
        PlanError::MissingCapability {
            capability: Capability::Vision,
            ..
        }
    ));
    assert_eq!(
        (err.answer().status, err.answer().kind),
        (400, "invalid_request_error")
    );

    s.models = vec![
        without(Capability::Tools, model("a", "blind", true)),
        without(Capability::Document, model("b", "sees", true)),
    ];
    let p_tools = s
        .plan(
            "smart",
            &RequestNeeds {
                tools: true,
                ..none()
            },
            None,
            &HealthBook::new(),
            &KeyBook::new(),
        )
        .unwrap();
    assert_eq!(
        (shape(&p_tools), p_tools.skipped[0].reason.as_str()),
        (text(&[("b", "b1", "sees")]), "no tools")
    );
    let p_doc = s
        .plan(
            "smart",
            &RequestNeeds {
                document: true,
                ..none()
            },
            None,
            &HealthBook::new(),
            &KeyBook::new(),
        )
        .unwrap();
    assert_eq!(
        (shape(&p_doc), p_doc.skipped[0].reason.as_str()),
        (text(&[("a", "a1,a2", "blind")]), "no document")
    );

    let mut cat = ModelCapabilities::default();
    cat.apply(Capability::Vision, false, CapabilitySource::Catalog);
    s.models[0].capabilities = cat;
    s.models[1] = model("b", "sees", true);
    assert_eq!(
        s.plan(
            "smart",
            &img_needs,
            None,
            &HealthBook::new(),
            &KeyBook::new()
        )
        .unwrap()
        .groups
        .len(),
        2
    );
}

fn target_with(primary: Option<Protocol>, protocols: &[Protocol]) -> Target {
    let mut s = RoutingSnapshot::default();
    let mut p = provider("p", true, protocols);
    p.primary_protocol = primary;
    s.providers.insert("p".into(), p);
    s.keys.insert("p".into(), vec![key("k", "p", true)]);
    s.target_for("p", None, "m", None).unwrap()
}

#[test]
fn protocol_choice_endpoint_switches_and_listed_models() {
    let s = snapshot();
    let book = HealthBook::new();
    let a = &targets(&s, "a/m", &book)[0];
    assert_eq!(
        a.chat_protocol(Some(Protocol::OpenaiChat)),
        Some(Protocol::OpenaiChat)
    );
    assert_eq!(
        a.chat_protocol(Some(Protocol::AnthropicMessages)),
        Some(Protocol::OpenaiChat)
    );
    let b = &targets(&s, "b/m", &book)[0];
    assert_eq!(b.chat_protocol(None), Some(Protocol::AnthropicMessages));
    assert_eq!(
        b.url(Protocol::AnthropicMessages, false).unwrap(),
        "https://b.test/v1/messages"
    );
    assert!(b.url(Protocol::OpenaiChat, false).is_err());

    let both = [Protocol::OpenaiChat, Protocol::AnthropicMessages];
    assert_eq!(
        target_with(Some(Protocol::AnthropicMessages), &both).chat_protocol(None),
        Some(Protocol::AnthropicMessages)
    );
    assert_eq!(
        target_with(Some(Protocol::AnthropicMessages), &both)
            .chat_protocol(Some(Protocol::OpenaiChat)),
        Some(Protocol::OpenaiChat)
    );
    assert_eq!(
        target_with(Some(Protocol::Gemini), &both).chat_protocol(None),
        Some(Protocol::OpenaiChat)
    );
    assert_eq!(
        target_with(
            Some(Protocol::Embeddings),
            &[Protocol::Embeddings, Protocol::AnthropicMessages]
        )
        .chat_protocol(None),
        Some(Protocol::AnthropicMessages)
    );
    assert_eq!(
        target_with(None, &[Protocol::Embeddings]).chat_protocol(Some(Protocol::Embeddings)),
        None
    );

    let all = [
        Protocol::Gemini,
        Protocol::OpenaiResponses,
        Protocol::AnthropicMessages,
        Protocol::OpenaiChat,
    ];
    assert_eq!(
        target_with(None, &all).chat_protocol(None),
        Some(Protocol::OpenaiChat)
    );
    assert_eq!(
        target_with(None, &all[..3]).chat_protocol(None),
        Some(Protocol::AnthropicMessages)
    );
    assert_eq!(
        target_with(None, &all[..2]).chat_protocol(None),
        Some(Protocol::OpenaiResponses)
    );

    let mut s = snapshot();
    s.providers.get_mut("b").unwrap().primary_protocol = Some(Protocol::AnthropicMessages);
    assert_eq!(
        plan(&s, "b/m").unwrap().groups[0].targets[0].primary_protocol,
        Some(Protocol::AnthropicMessages)
    );

    let mut p = provider("p", true, &both);
    p.primary_protocol = Some(Protocol::AnthropicMessages);
    p.endpoints[1].enabled = false;
    p.endpoints[1].auth = Some(AuthStyle::XApiKey);
    let mut single = RoutingSnapshot::default();
    single.providers.insert("p".into(), p);
    single.keys.insert("p".into(), vec![key("k", "p", true)]);
    let t = single.target_for("p", None, "m", None).unwrap();
    assert!(t.endpoint(Protocol::AnthropicMessages).is_none());
    assert_eq!(t.chat_protocol(None), Some(Protocol::OpenaiChat));
    assert_eq!(
        t.chat_protocol(Some(Protocol::AnthropicMessages)),
        Some(Protocol::OpenaiChat)
    );
    assert_eq!(t.auth_for(Protocol::AnthropicMessages), AuthStyle::Bearer);
    assert!(matches!(
        t.url(Protocol::AnthropicMessages, false),
        Err(AppError::Unsupported { .. })
    ));

    let mut s = snapshot();
    s.models = vec![
        model("a", "m1", true),
        model("a", "m2", false),
        model("b", "m2", true),
        model("off", "m3", true),
    ];
    assert_eq!(s.listed_models(), ["A/m1", "B/m2"]);
    switch_off(&mut s, "a", Protocol::OpenaiChat);
    assert!(matches!(
        s.target_for("a", None, "m", Some(Protocol::OpenaiChat)),
        Err(AppError::NotFound { .. })
    ));
    let planned = plan(&s, "smart").unwrap();
    assert_eq!(shape(&planned), text(&[("b", "b1", "claude")]));
    assert_eq!(
        reasons(&planned),
        vec![
            ("OFF/x".to_owned(), "disabled".to_owned()),
            ("A/gpt".to_owned(), "disabled".to_owned()),
            ("A/skip".to_owned(), "disabled".to_owned())
        ]
    );
    switch_off(&mut s, "b", Protocol::AnthropicMessages);
    assert_eq!(
        plan(&s, "smart").unwrap_err(),
        PlanError::UnknownModel("smart".into())
    );
    assert!(matches!(
        plan(&s, "b/claude"),
        Err(PlanError::UnknownModel(_))
    ));
    assert!(s.direct_targets("b", None, "claude").is_none() && s.listed_models().is_empty());
    assert!(
        s.listed_models_via(&[Protocol::OpenaiChat, Protocol::AnthropicMessages])
            .is_empty()
    );

    s.providers
        .get_mut("a")
        .unwrap()
        .endpoints
        .push(ProviderEndpoint {
            protocol: Protocol::Gemini,
            path_prefix: "/v1beta".into(),
            auth: None,
            url_mode: Default::default(),
            enabled: true,
        });
    assert_eq!(
        plan(&s, "a/m").unwrap().groups[0].targets[0].chat_protocol(None),
        Some(Protocol::Gemini)
    );
    let fail = s
        .plan_explained(
            "a/m",
            &RequestNeeds {
                endpoints: &[Protocol::OpenaiChat],
                ..none()
            },
            None,
            &HealthBook::new(),
            &KeyBook::new(),
        )
        .unwrap_err();
    assert_eq!(fail.skipped[0].reason, "no matching endpoint");
    assert!(
        !s.listed_models_via(&[Protocol::OpenaiChat])
            .contains(&"A/m1".to_owned())
    );
    assert!(
        s.listed_models_via(&[Protocol::Gemini])
            .contains(&"A/m1".to_owned())
    );
    assert!(!provider("p", true, &[]).routable());
}

#[test]
fn target_for_pinned_plans_headers_and_debug_redaction() {
    let mut s = snapshot();
    s.models = vec![model("a", "m", false)];
    assert_eq!(s.target_for("a", None, "m", None).unwrap().key_id, "a1");
    assert_eq!(
        s.target_for("a", Some("a2"), "m", None).unwrap().key_id,
        "a2"
    );
    assert!(matches!(
        s.target_for("a", Some("a3"), "m", None),
        Err(AppError::Validation { .. })
    ));
    assert!(matches!(
        s.target_for("off", None, "m", None),
        Err(AppError::NotFound { .. })
    ));
    assert!(matches!(
        s.target_for("ghost", None, "m", None),
        Err(AppError::NotFound { .. })
    ));
    assert!(matches!(
        s.target_for("a", None, "m", Some(Protocol::Gemini)),
        Err(AppError::Unsupported { .. })
    ));
    assert!(
        s.target_for("a", None, "m", Some(Protocol::OpenaiChat))
            .is_ok()
    );

    let book = HealthBook::new();
    let all = s
        .plan_pinned("a", None, "m", &none(), &book, &KeyBook::new())
        .unwrap();
    assert_eq!(
        (all.requested.as_str(), shape(&all)),
        ("a/m", text(&[("a", "a1,a2", "m")]))
    );
    let one = s
        .plan_pinned("a", Some("a2"), "m", &none(), &book, &KeyBook::new())
        .unwrap();
    assert_eq!(shape(&one), text(&[("a", "a2", "m")]));
    for (prov, k) in [
        ("ghost", None),
        ("off", None),
        ("a", Some("a3")),
        ("a", Some("zz")),
    ] {
        assert_eq!(
            s.plan_pinned(prov, k, "m", &none(), &book, &KeyBook::new())
                .unwrap_err(),
            PlanError::NoUsableTarget(format!("{prov}/m"))
        );
    }
    s.models = vec![without(Capability::Vision, model("a", "m", true))];
    assert!(matches!(
        s.plan_pinned(
            "a",
            None,
            "m",
            &RequestNeeds::from_hub(&image_request()),
            &book,
            &KeyBook::new()
        ),
        Err(PlanError::MissingCapability { .. })
    ));
    let one_cfg = CircuitConfig {
        failure_threshold: 1,
        ..CircuitConfig::default()
    };
    book.record("a1", "m", Outcome::Failure, Duration::ZERO, one_cfg);
    book.record("a2", "m", Outcome::Failure, Duration::ZERO, one_cfg);
    assert!(matches!(
        s.plan_pinned("a", None, "m", &none(), &book, &KeyBook::new()),
        Err(PlanError::NoTarget { .. })
    ));

    let mut s = snapshot();
    s.providers.get_mut("a").unwrap().proxy_url = "http://user:proxy-secret@proxy.test:1".into();
    s.providers
        .get_mut("a")
        .unwrap()
        .quirks
        .extra_headers
        .insert("x-org".into(), "{{secret:llm.a.header.x-org}}".into());
    s.headers.insert(
        "a",
        Ok([("x-org".to_owned(), "org-secret-123".to_owned())].into()),
    );
    let t = s.target_for("a", None, "m", None).unwrap();
    assert_eq!(t.quirks.extra_headers["x-org"], "org-secret-123");
    assert_eq!(
        plan(&s, "a/m").unwrap().groups[0].targets[0]
            .quirks
            .extra_headers["x-org"],
        "org-secret-123"
    );
    let dbg = format!("{s:?}{t:?}");
    for secret in ["secret-a1", "proxy-secret", "org-secret-123"] {
        assert!(!dbg.contains(secret), "{dbg}");
    }
    assert!(dbg.contains("x-org"));

    s.headers.insert(
        "a",
        Err(AppError::validation(
            "the secret of the x-org header is missing",
        )),
    );
    assert!(
        s.target_for("a", None, "m", None)
            .unwrap_err()
            .message()
            .contains("x-org")
    );
    set_chain(&mut s, ("a", "m"), &[("b", "n")]);
    let planned = plan(&s, "smart").unwrap();
    assert_eq!(shape(&planned), text(&[("b", "b1", "n")]));
    assert_eq!(
        reasons(&planned),
        vec![(
            "A/m".to_owned(),
            "the secret of the x-org header is missing".to_owned()
        )]
    );
    assert_eq!(shape(&plan(&s, "a/m").unwrap()), text(&[("b", "b1", "n")]));
    assert!(plan(&s, "a/other").is_err());
}

fn relay_groups(strategy: KeyStrategy) -> RoutingSnapshot {
    let mut relay = provider(
        "r",
        true,
        &[
            Protocol::OpenaiChat,
            Protocol::OpenaiResponses,
            Protocol::AnthropicMessages,
        ],
    );
    relay.key_strategy = Some(strategy);
    let mut s = RoutingSnapshot::default();
    s.providers.insert("r".into(), relay);
    let listed = |id: &str| {
        let mut k = key(id, "r", true);
        k.key.models_listed_at_ms = Some(1);
        k
    };
    let mut c1 = listed("c1");
    c1.key.preferred_protocol = Some(Protocol::AnthropicMessages);
    s.keys
        .insert("r".into(), vec![listed("g1"), c1, listed("c2")]);
    s.models = vec![model("r", "gpt-5", true), model("r", "claude", true)];
    let owners: HashMap<String, String> = ["g1", "c1", "c2"]
        .into_iter()
        .map(|id| (id.to_owned(), "r".to_owned()))
        .collect();
    let row = |k: &str, m: &str, protocols: Option<Vec<Protocol>>| KeyModel {
        key_id: k.into(),
        model_id: m.into(),
        protocols,
    };
    s.key_scopes = KeyScopes::new(
        vec![
            row("g1", "gpt-5", Some(vec![Protocol::OpenaiResponses])),
            row("c1", "claude", None),
            row("c2", "claude", None),
        ],
        &owners,
    );
    s
}

#[test]
fn relay_key_groups_routing_rotation_cooldown_and_protocols() {
    let mut s = relay_groups(KeyStrategy::Primary);
    let kb = KeyBook::new();
    assert_eq!(order(&s, "r/claude", &kb), ["c1", "c2"]);
    assert_eq!(order(&s, "r/gpt-5", &kb), ["g1"]);
    assert_eq!(order(&s, "r/manual", &kb), ["g1", "c1", "c2"]);

    let rr = relay_groups(KeyStrategy::RoundRobin);
    let rr_kb = KeyBook::new();
    let claude: Vec<_> = (0..4)
        .map(|_| {
            let c = order(&rr, "r/claude", &rr_kb)[0].clone();
            assert_eq!(order(&rr, "r/gpt-5", &rr_kb), ["g1"]);
            c
        })
        .collect();
    assert_eq!(claude, ["c1", "c2", "c1", "c2"]);

    let limited = FailureObservation {
        status: Some(429),
        retry_after_ms: Some(20_000),
        ..Default::default()
    };
    kb.failed("c1", "claude", &limited, "429".into(), true);
    assert_eq!(order(&s, "r/claude", &kb), ["c2"]);
    kb.failed("c2", "claude", &limited, "429".into(), true);
    let both_resting = order(&s, "r/claude", &kb);
    assert_eq!(both_resting.len(), 2);
    assert!(!both_resting.contains(&"g1".to_owned()));

    let kb = KeyBook::new();
    let gpt = s
        .plan("r/gpt-5", &none(), None, &HealthBook::new(), &kb)
        .unwrap();
    assert_eq!(
        gpt.groups[0].targets[0].chat_protocol(None),
        Some(Protocol::OpenaiResponses)
    );
    assert!(
        gpt.groups[0].targets[0]
            .endpoint(Protocol::OpenaiChat)
            .is_none()
    );

    s.keys.get_mut("r").unwrap()[2].key.protocols = Some(vec![Protocol::OpenaiResponses]);
    let claude_plan = s
        .plan("r/claude", &none(), None, &HealthBook::new(), &kb)
        .unwrap();
    assert_eq!(
        claude_plan.groups[0].targets[0].chat_protocol(None),
        Some(Protocol::AnthropicMessages)
    );
    assert_eq!(
        claude_plan.groups[0].targets[1].chat_protocol(None),
        Some(Protocol::OpenaiResponses)
    );
    assert!(
        claude_plan.groups[0].targets[1]
            .endpoint(Protocol::AnthropicMessages)
            .is_none()
    );

    assert_eq!(
        s.listed_models_via(&[Protocol::AnthropicMessages]),
        ["R/claude"]
    );
    assert_eq!(s.listed_models().len(), 2);
    assert_eq!(
        s.target_for("r", None, "claude", None).unwrap().key_id,
        "c1"
    );
    assert_eq!(
        s.target_for("r", None, "gpt-5", Some(Protocol::OpenaiResponses))
            .unwrap()
            .key_id,
        "g1"
    );
    assert!(
        s.target_for("r", None, "gpt-5", Some(Protocol::OpenaiChat))
            .is_err()
    );
    assert_eq!(
        s.target_for("r", Some("g1"), "claude", None)
            .unwrap()
            .key_id,
        "g1"
    );
}

#[test]
fn a_disabled_primary_falls_through_to_its_chain() {
    // The gateway rule and an in-app caller's chain both reach the fallback.
    let mut s = snapshot();
    s.models.push(model("b", "claude", false));
    let p = plan(&s, "smart").unwrap();
    assert_eq!(shape(&p), text(&[("a", "a1,a2", "gpt")]));
    assert_eq!(
        reasons(&p)[0],
        ("B/claude".to_owned(), "disabled".to_owned())
    );
    let chain = ["a/gpt".to_owned()];
    let caller = |name: &str| {
        s.plan_with_fallbacks(name, &chain, &none(), &HealthBook::new(), &KeyBook::new())
            .map(|p| shape(&p))
    };
    assert_eq!(caller("b/claude"), Ok(text(&[("a", "a1,a2", "gpt")])));
    // A primary whose provider is gone still leaves the caller's chain.
    assert_eq!(caller("ghost/x"), Ok(text(&[("a", "a1,a2", "gpt")])));
    // Nothing usable: still an unknown model, as before.
    s.models.push(model("a", "gpt", false));
    assert_eq!(
        plan(&s, "smart").unwrap_err(),
        PlanError::UnknownModel("smart".into())
    );
}

#[test]
fn round_robin_moves_once_per_request_when_the_chain_repeats_a_provider() {
    // Primary and fallback on the same provider share one cursor step.
    let mut s = three_keys(KeyStrategy::RoundRobin, true);
    set_chain(&mut s, ("a", "m"), &[("a", "n")]);
    let kb = KeyBook::new();
    let firsts: Vec<String> = (0..3)
        .map(|_| {
            let p = plan_with(&s, "smart", &kb);
            assert_eq!(p.groups[0].targets[0].key_id, p.groups[1].targets[0].key_id);
            p.groups[0].targets[0].key_id.clone()
        })
        .collect();
    assert_eq!(firsts, ["a1", "a2", "a3"]);
}

fn plan_with(snapshot: &RoutingSnapshot, name: &str, books: &KeyBook) -> Plan {
    snapshot
        .plan(name, &none(), None, &HealthBook::new(), books)
        .unwrap()
}

#[test]
fn a_former_name_resolves_until_it_expires_and_a_current_name_wins() {
    let far = i64::MAX;
    for (name, expires_at_ms, expected) in [
        ("old-a/gpt", far, Some("a")),
        ("OLD-A/gpt", far, Some("a")),
        ("old-a/gpt", 1, None),
        // the id answers case-insensitively too
        ("A/gpt", far, Some("a")),
    ] {
        let mut s = snapshot();
        s.former_names
            .insert("old-a".into(), ("a".into(), expires_at_ms));
        assert_eq!(
            s.resolve_ref(name).map(|m| m.provider_id),
            expected.map(str::to_owned),
            "{name}"
        );
    }
    // a former name equal to another provider's current name: the current name wins
    let mut s = snapshot();
    s.former_names.insert("b".into(), ("a".into(), far));
    assert_eq!(s.resolve_ref("b/claude").unwrap().provider_id, "b");
    // an expired name gets the close current names in the answer
    assert_eq!(s.close_names("bbb/claude"), vec!["B/claude".to_owned()]);
}

#[test]
fn a_preview_never_moves_the_round_robin_cursor() {
    let s = three_keys(KeyStrategy::RoundRobin, false);
    let books = KeyBook::new();
    let health = HealthBook::new();
    for _ in 0..2 {
        let preview = s
            .plan_preview("A/gpt", &none(), None, &health, &books)
            .unwrap();
        assert_eq!(preview.groups[0].targets[0].key_id, "a1");
    }
    assert_eq!(order(&s, "A/gpt", &books)[0], "a1");
    assert_eq!(order(&s, "A/gpt", &books)[0], "a2");
    let preview = s
        .plan_preview("A/gpt", &none(), None, &health, &books)
        .unwrap();
    assert_eq!(preview.groups[0].targets[0].key_id, "a3");
}

#[test]
fn a_provider_waiting_for_review_is_no_fallback_but_answers_by_name() {
    let mut s = snapshot();
    s.providers.get_mut("a").unwrap().review_pending_since_ms = Some(1);
    let planned = plan(&s, "B/claude").unwrap();
    assert!(planned.groups.iter().all(|g| g.provider_id != "a"));
    assert!(
        planned
            .skipped
            .iter()
            .any(|skip| skip.provider_id == "a" && skip.reason == NOT_REVIEWED)
    );
    assert_eq!(plan(&s, "A/gpt").unwrap().groups[0].provider_id, "a");
}
