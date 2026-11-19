//! Provider rules: the `models_failed` denominator, fallback compatibility, bulk switch with
//! Undo and key saving.

use pingdeck_core::app::{
    AppState, SaveProviderKeyInput, llm_key_list_impl, llm_key_save_impl,
    llm_model_fallbacks_set_impl, llm_model_save_impl, llm_models_set_enabled_impl,
    llm_models_undo_impl, llm_provider_save_impl, llm_provider_status_list_impl,
};
use pingdeck_core::llm::failover::FailureObservation;
use pingdeck_core::llm::store::{
    ModelRef, ProbeResult, Provider, ProviderModel, save_probe_result,
};
use serde_json::{Value, json};

use crate::{now_ms, outcome, verify, world};

fn provider(state: &AppState, name: &str) -> String {
    let wire = json!({
        "id": "", "name": name, "preset_id": null, "category": "custom",
        "base_url": "http://127.0.0.1:9", "auth": "bearer",
        "endpoints": [{ "protocol": "openai_chat" }], "models_endpoint": null,
        "quirks": {}, "proxy_url": "", "enabled": true,
    });
    let provider: Provider = serde_json::from_value(wire).unwrap();
    llm_provider_save_impl(state, provider).unwrap().id
}

fn model(state: &AppState, provider_id: &str, model_id: &str, enabled: bool) {
    let wire = json!({ "provider_id": provider_id, "model_id": model_id, "source": "manual", "enabled": enabled });
    let model: ProviderModel = serde_json::from_value(wire).unwrap();
    llm_model_save_impl(state, model).unwrap();
}

fn key_input(provider_id: &str, id: &str) -> SaveProviderKeyInput {
    serde_json::from_value(json!({
        "id": id, "provider_id": provider_id, "name": "Main", "secret": null,
        "enabled": true, "position": 0,
    }))
    .unwrap()
}

fn models_failed(state: &AppState, provider_id: &str) -> Value {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let statuses = runtime
        .block_on(llm_provider_status_list_impl(state))
        .unwrap();
    let status = serde_json::to_value(
        statuses
            .iter()
            .find(|s| s.provider_id == provider_id)
            .unwrap(),
    )
    .unwrap();
    let reason = status["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["code"] == "models_failed")
        .cloned();
    reason.map_or(Value::Null, |r| r["params"].clone())
}

/// The relay of the content bible: 38 stored models, 4 of them off, 2 failing their last test.
#[test]
fn status_denominator() {
    let w = world("status");
    let state = &w.state;
    let id = provider(state, "relay.acme.dev");
    let mut key = key_input(&id, "relay-key");
    key.secret = Some("sk-relay".into());
    llm_key_save_impl(state, key).unwrap();
    let (stored, disabled, failed) = (38, 4, ["m-failed-0", "m-failed-1"]);
    for n in 0..stored - failed.len() {
        model(state, &id, &format!("m-{n:02}"), n >= disabled);
    }
    let connection = pingdeck_core::llm::runtime::DbHandle::path(&w.db)
        .open()
        .unwrap();
    for model_id in failed {
        model(state, &id, model_id, true);
        let result = ProbeResult {
            key_id: "relay-key".into(),
            model_id: model_id.into(),
            endpoint: "openai_chat".into(),
            test: "chat".into(),
            ok: false,
            status: Some(500),
            status_word: "invalid_response".into(),
            checked_at_ms: now_ms(),
            ..ProbeResult::default()
        };
        save_probe_result(&connection, &result).unwrap();
    }
    let mut cases = vec![
        json!({ "name": "seeded relay", "disable_failed": 0, "expected": models_failed(state, &id) }),
    ];
    for (n, model_id) in failed.iter().enumerate() {
        llm_models_set_enabled_impl(state, &id, &[((*model_id).to_owned(), false)]).unwrap();
        cases.push(json!({ "name": format!("{} failing model(s) switched off", n + 1), "disable_failed": n + 1, "expected": models_failed(state, &id) }));
    }
    let note = "Provider with `stored` models, `disabled` of them off and `failed` enabled models whose newest test failed; switch off the first `disable_failed` failing models, then read the `models_failed` reason params (null = no reason).";
    let setup = json!({ "name": "relay.acme.dev", "stored": stored, "disabled": disabled, "failed": failed.len() });
    verify("provider-status", note, setup, cases);
}

/// Fallback chains: the call-kind rule, limits and unknown targets.
#[test]
fn fallbacks() {
    let w = world("fallbacks");
    let state = &w.state;
    let names = ["Alpha", "Beta"];
    let ids: Vec<String> = names.iter().map(|n| provider(state, n)).collect();
    let models = [
        ("Alpha", "chat-a"),
        ("Alpha", "text-embedding-3-small"),
        ("Beta", "chat-b"),
        ("Beta", "text-embedding-3-small"),
        ("Beta", "text-embedding-3-large"),
    ];
    let extra: Vec<String> = (1..=5).map(|n| format!("chat-b{n}")).collect();
    let id_of = |name: &str| ids[names.iter().position(|n| *n == name).unwrap()].clone();
    for (p, m) in models
        .iter()
        .copied()
        .chain(extra.iter().map(|m| ("Beta", m.as_str())))
    {
        model(state, &id_of(p), m, true);
    }
    let r = |p: &str, m: &str| json!([p, m]);
    let six: Vec<Value> = std::iter::once(r("Beta", "chat-b"))
        .chain(extra.iter().map(|m| r("Beta", m)))
        .collect();
    let attempts = [
        (
            "chat to chat on another provider",
            "chat-a",
            vec![r("Beta", "chat-b")],
        ),
        ("itself", "chat-a", vec![r("Alpha", "chat-a")]),
        (
            "chat to an embedding model",
            "chat-a",
            vec![r("Beta", "text-embedding-3-small")],
        ),
        (
            "same embedding model elsewhere",
            "text-embedding-3-small",
            vec![r("Beta", "text-embedding-3-small")],
        ),
        (
            "another embedding model",
            "text-embedding-3-small",
            vec![r("Beta", "text-embedding-3-large")],
        ),
        (
            "listed twice",
            "chat-a",
            vec![r("Beta", "chat-b"), r("Beta", "chat-b")],
        ),
        ("six fallbacks", "chat-a", six),
        ("unknown provider", "chat-a", vec![r("Gamma", "chat-g")]),
        (
            "model not stored",
            "chat-a",
            vec![r("Beta", "chat-unlisted")],
        ),
        ("clear the chain", "chat-a", vec![]),
    ];
    let cases = attempts
        .into_iter()
        .map(|(name, primary, chain)| {
            let refs = chain
                .iter()
                .map(|pair| ModelRef {
                    provider_id: names.iter().position(|n| *n == pair[0]).map_or_else(|| "unknown-provider".to_owned(), |i| ids[i].clone()),
                    model: pair[1].as_str().unwrap().to_owned(),
                })
                .collect();
            let result = llm_model_fallbacks_set_impl(state, &id_of("Alpha"), primary, refs);
            json!({ "name": name, "primary": ["Alpha", primary], "fallbacks": chain, "expected": outcome(result, |saved| json!({ "warnings": saved.warnings.len() })) })
        })
        .collect();
    let embedding = ["text-embedding-3-small", "text-embedding-3-large"];
    let setup: Vec<Value> = models.iter().map(|(p, m)| (*p, *m)).chain(extra.iter().map(|m| ("Beta", m.as_str())))
        .map(|(p, m)| json!({ "provider": p, "model": m, "kind": if embedding.contains(&m) { "embedding" } else { "chat" } }))
        .collect();
    let note = "Providers Alpha and Beta with the `setup` models (no capabilities known); set the fallbacks of `primary`, each `[provider name, model]`. Outcome: warning count or error kind.";
    verify("fallbacks", note, json!(setup), cases);
}

/// Bulk switch with Undo: skipped rows, restored versus kept rows, the five-operation window.
#[test]
fn bulk_enable_undo() {
    let w = world("bulk");
    let state = &w.state;
    let id = provider(state, "Bulk");
    for m in ["m1", "m2", "m3"] {
        model(state, &id, m, true);
    }
    let set = |changes: &[(&str, bool)]| {
        let changes: Vec<(String, bool)> = changes
            .iter()
            .map(|(m, on)| ((*m).to_owned(), *on))
            .collect();
        llm_models_set_enabled_impl(state, &id, &changes).unwrap()
    };
    let changed = |report: &pingdeck_core::llm::store::BulkEnableReport| -> Vec<String> {
        report.previous.iter().map(|p| p.model_id.clone()).collect()
    };
    let undo = |op: &str| {
        outcome(
            llm_models_undo_impl(state, op),
            |r| json!({ "restored": r.restored, "kept": r.kept }),
        )
    };
    let mut cases = Vec::new();

    let first = set(&[
        ("m1", false),
        ("m2", false),
        ("m2", true),
        ("m3", true),
        ("nope", false),
    ]);
    cases.push(json!({ "name": "switch off; repeats count once, unchanged and unknown rows are skipped", "steps": [{ "set": [["m1", false], ["m2", false], ["m2", true], ["m3", true], ["nope", false]] }], "expected": { "changed": changed(&first) } }));
    set(&[("m2", true)]);
    cases.push(json!({ "name": "undo keeps a row edited since", "steps": [{ "set": [["m1", false], ["m2", false]] }, { "set": [["m2", true]] }, { "undo": 0 }], "expected": undo(&first.id) }));
    cases.push(json!({ "name": "an operation undoes once", "steps": [{ "set": [["m1", false]] }, { "undo": 0 }, { "undo": 0 }], "expected": undo(&first.id) }));
    let ops: Vec<String> = (0..6).map(|n| set(&[("m3", n % 2 == 1)]).id).collect();
    cases.push(json!({ "name": "only the newest five operations undo", "steps": [{ "toggle": "m3", "times": 6 }, { "undo": 0 }], "expected": undo(&ops[0]) }));
    cases.push(json!({ "name": "the newest operation still undoes", "steps": [{ "toggle": "m3", "times": 6 }, { "undo": 5 }], "expected": undo(&ops[5]) }));
    let note = "Provider Bulk with enabled models m1..m3. Steps: `set` switches models (one operation), `toggle` switches a model off and on `times` operations, `undo` names an operation by index in step order. `expected` is the last outcome; `changed` lists the model ids the first operation reports.";
    verify(
        "bulk-enable-undo",
        note,
        json!({ "provider": "Bulk", "models": ["m1", "m2", "m3"] }),
        cases,
    );
}

/// Key saving: name, notes and group limits, and what a new secret does to a cooldown.
#[test]
fn key_save() {
    let w = world("keys");
    let state = &w.state;
    let id = provider(state, "Keys");
    let long = |n: usize| "x".repeat(n);
    let attempts: Vec<(&str, Value)> = vec![
        ("plain", json!({ "name": "Main" })),
        ("blank name", json!({ "name": "  " })),
        ("61-character name", json!({ "name": long(61) })),
        (
            "200-character notes inside spaces",
            json!({ "name": "Main", "notes": format!("  {}  ", long(200)) }),
        ),
        (
            "201-character notes",
            json!({ "name": "Main", "notes": long(201) }),
        ),
        (
            "61-character group",
            json!({ "name": "Main", "group_name": long(61) }),
        ),
        (
            "group inside spaces",
            json!({ "name": "Main", "group_name": "  team  " }),
        ),
    ];
    let mut cases = Vec::new();
    for (n, (name, fields)) in attempts.into_iter().enumerate() {
        let mut input = key_input(&id, &format!("key-{n}"));
        input.name = fields["name"].as_str().unwrap().to_owned();
        input.secret = Some("sk-new".into());
        input.notes = fields["notes"].as_str().map(str::to_owned);
        input.group_name = fields["group_name"].as_str().map(str::to_owned);
        let result = llm_key_save_impl(state, input);
        cases.push(json!({ "name": name, "input": fields, "expected": outcome(result, |k| json!({ "name": k.name, "group_name": k.group_name.filter(|g| !g.is_empty()) })) }));
    }
    for (status, secret) in [
        (429_u16, Some("sk-other")),
        (403, Some("sk-other")),
        (429, None),
    ] {
        let key_id = format!("cool-{status}-{}", secret.is_some());
        let mut input = key_input(&id, &key_id);
        input.secret = Some("sk-first".into());
        llm_key_save_impl(state, input.clone()).unwrap();
        // A plain 429 rests the key for one model only; the quota body makes it rest the
        // whole key, which is the cooldown a new secret clears.
        let observation = FailureObservation {
            status: Some(status),
            transport: None,
            body: r#"{"error":{"code":"insufficient_quota"}}"#.to_owned(),
            retry_after_ms: None,
        };
        state.llm().keys.failed(
            &key_id,
            "model-a",
            &observation,
            format!("HTTP {status}"),
            true,
        );
        input.secret = secret.map(str::to_owned);
        llm_key_save_impl(state, input).unwrap();
        let listed = llm_key_list_impl(state, Some(&id)).unwrap();
        let cooling = listed
            .iter()
            .find(|k| k.id == key_id)
            .unwrap()
            .cooldown_until_ms
            .is_some();
        cases.push(json!({ "name": format!("cooling after HTTP {status}, {}", if secret.is_some() { "new secret" } else { "secret unchanged" }), "cooldown": { "last_error_status": status, "new_secret": secret.is_some() }, "expected": { "cooling": cooling } }));
    }
    let note = "Provider Keys. `input` cases save a new key with a secret and these fields (outcome: stored name and group, or error kind). `cooldown` cases save an existing key cooling down after `last_error_status`, with or without a new secret.";
    verify("key-save", note, json!({ "provider": "Keys" }), cases);
}
