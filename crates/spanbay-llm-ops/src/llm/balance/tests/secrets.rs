//! No credential reaches a report or an error, and the balance code relies on the one shared
//! redaction table.

use serde_json::Map;

use super::*;

/// A small deterministic generator for the property tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }
}

/// A JSON value whose strings sometimes carry the key or the console token.
fn noisy(rng: &mut Rng, depth: u32, secrets: &[&str]) -> Value {
    match rng.next() % if depth == 0 { 4 } else { 6 } {
        0 => json!(rng.next() % 10_000),
        1 => json!((rng.next() % 10_000) as f64 / 100.0),
        2 => Value::String(format!("{}", rng.next() % 1_000)),
        3 => Value::String(format!("note {} end", rng.pick(secrets))),
        4 => Value::Array(
            (0..rng.next() % 3)
                .map(|_| noisy(rng, depth - 1, secrets))
                .collect(),
        ),
        _ => {
            let mut object = Map::new();
            for key in [
                "email", "id", "message", "planName", "currency", "data", "window", "msg", "a b",
            ] {
                if rng.next().is_multiple_of(2) {
                    object.insert(key.to_owned(), noisy(rng, depth - 1, secrets));
                }
            }
            Value::Object(object)
        }
    }
}

#[tokio::test]
async fn no_secret_appears_in_any_serialised_outcome_for_random_bodies() {
    let mgmt = "mgmt-console-token-ABCDEF123456";
    let secrets = [KEY, mgmt];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut targets = Vec::new();
    for family in [
        GatewayFamily::NewApi,
        GatewayFamily::OneApi,
        GatewayFamily::Sub2Api,
        GatewayFamily::LiteLlm,
        GatewayFamily::VoApi,
        GatewayFamily::DoneHub,
    ] {
        let mut t = target(family);
        t.management_token = Some(mgmt);
        t.api_user_id = Some("7");
        targets.push(t);
    }
    for (preset, origin) in [
        ("deepseek", "https://api.deepseek.com"),
        ("openrouter", "https://openrouter.ai"),
        ("moonshot", "https://api.moonshot.cn"),
        ("zhipu", "https://open.bigmodel.cn"),
    ] {
        let mut t = vendor(preset, origin);
        t.management_token = Some(mgmt);
        targets.push(t);
    }
    let mut readings = 0;
    for round in 0..40 {
        for target in &targets {
            let mut http = ScriptedHttp::new();
            for path in [
                "/user/balance",
                "/api/v1/credits",
                "/api/v1/key",
                "/v1/users/me/balance",
                "/api/biz/account/query-customer-account-report",
                "/api/usage/token/",
                "/api/user/self",
                "/api/status",
                "/v1/dashboard/billing/subscription",
                "/v1/dashboard/billing/usage",
                "/v1/usage",
                "/key/info",
                "/v2/user/info",
                "/api/user/info",
            ] {
                let mut body = noisy(&mut rng, 3, &secrets);
                // Make some bodies plausible so the success paths are exercised too.
                if round % 2 == 0
                    && let Some(object) = body.as_object_mut()
                {
                    object.insert("code".to_owned(), json!(0));
                    object.insert("data".to_owned(), json!({"total_available": 7, "quota": 9, "basicBalance": "3.5", "availableBalance": 2, "currency": "USD"}));
                    object.insert("remaining".to_owned(), json!(4.5));
                    object.insert("planName".to_owned(), json!(format!("plan {KEY}")));
                    object.insert("hard_limit_usd".to_owned(), json!(10));
                    object.insert("total_usage".to_owned(), json!(100));
                    object.insert("is_available".to_owned(), json!(true));
                    object.insert(
                        "balance_infos".to_owned(),
                        json!([{"currency": KEY, "total_balance": "1.0"}]),
                    );
                    object.insert("success".to_owned(), json!(true));
                    object.insert("msg".to_owned(), json!(format!("token {mgmt}")));
                }
                http = http.route(SafeMethod::Get, path, ok(&body));
            }
            match check(&http, target).await {
                BalanceOutcome::Report { report, .. } => {
                    readings += 1;
                    let text = format!(
                        "{}{report:?}",
                        serde_json::to_string(&report).unwrap_or_default()
                    );
                    for secret in secrets {
                        assert!(
                            !text.contains(secret),
                            "round {round} {target:?}: {secret} in {text}"
                        );
                    }
                }
                BalanceOutcome::Failed { error, .. } => {
                    let text = format!(
                        "{}{error:?}",
                        serde_json::to_string(&error).unwrap_or_default()
                    );
                    for secret in secrets {
                        assert!(
                            !text.contains(secret),
                            "round {round} {target:?}: {secret} in {text}"
                        );
                    }
                }
                BalanceOutcome::NoAdapter => panic!("{target:?} has an adapter"),
            }
        }
    }
    assert!(
        readings > 40,
        "the generator must reach the success paths ({readings})"
    );
}

/// The balance code has no redaction pattern of its own; the one shared table
/// (`spanbay_base::secrets`) does the work. The stored shapes in `spanbay-llm-domain` carry the
/// same guard next to their code.
#[test]
fn the_balance_code_has_no_redaction_pattern_of_its_own() {
    let sources = [
        include_str!("../adapter_id.rs"),
        include_str!("../custom.rs"),
        include_str!("../declarative.rs"),
        include_str!("../deepseek.rs"),
        include_str!("../error.rs"),
        include_str!("../extract.rs"),
        include_str!("../fetch.rs"),
        include_str!("../gateway.rs"),
        include_str!("../litellm.rs"),
        include_str!("../newapi.rs"),
        include_str!("../oneapi.rs"),
        include_str!("../openrouter.rs"),
        include_str!("../policy.rs"),
        include_str!("../report.rs"),
        include_str!("../rules.rs"),
        include_str!("../select.rs"),
        include_str!("../sub2api.rs"),
        include_str!("../target.rs"),
        include_str!("../voapi.rs"),
    ];
    for (index, source) in sources.iter().enumerate() {
        let production = source.split("#[cfg(test)]").next().unwrap_or_default();
        for forbidden in ["Regex", "regex::", "LazyLock<Vec<(", "\"sk-", "BODY_KEYS"] {
            assert!(
                !production.contains(forbidden),
                "file #{index} contains {forbidden}"
            );
        }
    }
}
