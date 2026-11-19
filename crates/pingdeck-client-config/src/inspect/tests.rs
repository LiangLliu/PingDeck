use std::fs;

use super::super::test_util::{TempHome, apply_now, request};
use super::*;

const TOKEN: &str = "pd_cc_abcdefghijklmnopqrstuvwxyz012345";

/// A context in which every token is active and every alias exists.
fn permissive(gateway_url: &str) -> InspectContext<'_> {
    InspectContext {
        gateway_url,
        token_is_active: &|_| true,
        alias_exists: &|_| true,
    }
}

fn check(
    home: &TempHome,
    app: ClientApp,
    gateway: &str,
    active: &[&str],
    aliases: &[&str],
) -> Inspection {
    let hashes: Vec<String> = active.iter().map(|t| sha256_hex(t.as_bytes())).collect();
    let active = |hash: &str| hashes.iter().any(|h| h == hash);
    let alias = |name: &str| aliases.contains(&name);
    inspect(
        &DetectEnv::home_only(&home.0),
        app,
        &InspectContext {
            gateway_url: gateway,
            token_is_active: &active,
            alias_exists: &alias,
        },
    )
    .unwrap()
}

fn connect(home: &TempHome, app: ClientApp) {
    let mut req = request(app, Some("fast"));
    req.token = TOKEN.to_owned();
    apply_now(home, &req).unwrap();
}

const GATEWAY: &str = "http://127.0.0.1:4141";

#[test]
fn every_writer_round_trips_to_connected() {
    for app in ClientApp::ALL {
        let home = TempHome::new();
        connect(&home, app);
        let result = check(&home, app, GATEWAY, &[TOKEN], &["fast"]);
        assert_eq!(result.drift, Drift::Connected, "{app:?}");
        assert_eq!(result.alias.as_deref(), Some("fast"));
        assert!(result.base_url.unwrap().starts_with(GATEWAY));
    }
}

#[test]
fn nothing_found_is_not_installed_and_a_bare_dir_is_not_configured() {
    for app in ClientApp::ALL {
        let home = TempHome::new();
        assert_eq!(
            check(&home, app, GATEWAY, &[], &[]).drift,
            Drift::NotInstalled,
            "{app:?}"
        );
        fs::create_dir_all(app.dir(&home.0)).unwrap();
        let bare = check(&home, app, GATEWAY, &[], &[]);
        assert_eq!(bare.drift, Drift::NotConfigured, "{app:?}");
        assert!(bare.base_url.is_none() && bare.alias.is_none());
    }
}

#[test]
fn an_executable_on_path_makes_a_missing_config_not_configured() {
    let home = TempHome::new();
    let bin = home.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("claude"), "").unwrap();
    let mut env = DetectEnv::home_only(&home.0);
    env.path_dirs = vec![bin];
    let result = inspect(&env, ClientApp::ClaudeCode, &permissive(GATEWAY)).unwrap();
    assert_eq!(result.drift, Drift::NotConfigured);
}

#[test]
fn a_moved_gateway_is_a_different_address_with_the_suffix_of_the_tool() {
    for (app, found, expected) in [
        (
            ClientApp::ClaudeCode,
            "http://127.0.0.1:4141",
            "http://127.0.0.1:4242",
        ),
        (
            ClientApp::Codex,
            "http://127.0.0.1:4141/v1",
            "http://127.0.0.1:4242/v1",
        ),
        (
            ClientApp::OpenCode,
            "http://127.0.0.1:4141/v1",
            "http://127.0.0.1:4242/v1",
        ),
        (
            ClientApp::GeminiCli,
            "http://127.0.0.1:4141",
            "http://127.0.0.1:4242",
        ),
    ] {
        let home = TempHome::new();
        connect(&home, app);
        let result = check(&home, app, "http://127.0.0.1:4242/", &[TOKEN], &["fast"]);
        assert_eq!(
            result.drift,
            Drift::DifferentAddress {
                found: found.to_owned(),
                expected: expected.to_owned()
            },
            "{app:?}"
        );
    }
}

#[test]
fn a_revoked_or_rotated_token_is_not_recognised() {
    for app in ClientApp::ALL {
        let home = TempHome::new();
        connect(&home, app);
        let result = check(
            &home,
            app,
            GATEWAY,
            &["pd_cc_some_other_token_value"],
            &["fast"],
        );
        assert_eq!(result.drift, Drift::TokenNotRecognised, "{app:?}");
    }
}

#[test]
fn a_deleted_alias_is_reported_but_a_missing_model_key_is_fine() {
    let home = TempHome::new();
    connect(&home, ClientApp::Codex);
    assert_eq!(
        check(&home, ClientApp::Codex, GATEWAY, &[TOKEN], &["other"]).drift,
        Drift::AliasMissing
    );

    let home = TempHome::new();
    let mut req = request(ClientApp::Codex, None);
    req.token = TOKEN.to_owned();
    apply_now(&home, &req).unwrap();
    assert_eq!(
        check(&home, ClientApp::Codex, GATEWAY, &[TOKEN], &[]).drift,
        Drift::Connected
    );
}

#[test]
fn a_deleted_fast_alias_is_alias_missing_too() {
    let home = TempHome::new();
    let mut req = request(ClientApp::ClaudeCode, Some("deep"));
    req.fast_alias = Some("fast".to_owned());
    req.token = TOKEN.to_owned();
    apply_now(&home, &req).unwrap();
    let drift =
        |aliases: &[&str]| check(&home, ClientApp::ClaudeCode, GATEWAY, &[TOKEN], aliases).drift;
    assert_eq!(drift(&["deep", "fast"]), Drift::Connected);
    assert_eq!(drift(&["deep"]), Drift::AliasMissing);
    assert_eq!(drift(&["fast"]), Drift::AliasMissing);
}

#[test]
fn config_models_lists_each_model_once_in_order() {
    let home = TempHome::new();
    let env = DetectEnv::home_only(&home.0);
    // Without a fast alias the Claude Code writer puts the alias in the Haiku key too.
    apply_now(&home, &request(ClientApp::ClaudeCode, Some("deep"))).unwrap();
    assert_eq!(
        config_models(&env, ClientApp::ClaudeCode).unwrap(),
        ["deep"]
    );
    let mut req = request(ClientApp::ClaudeCode, Some("deep"));
    req.fast_alias = Some("fast".to_owned());
    apply_now(&home, &req).unwrap();
    assert_eq!(
        config_models(&env, ClientApp::ClaudeCode).unwrap(),
        ["deep", "fast"]
    );
    assert!(config_models(&env, ClientApp::Codex).unwrap().is_empty());
}

#[test]
fn address_wins_over_token_and_the_token_is_only_ever_hashed() {
    let home = TempHome::new();
    connect(&home, ClientApp::ClaudeCode);
    let seen = std::cell::RefCell::new(Vec::new());
    let active = |hash: &str| {
        seen.borrow_mut().push(hash.to_owned());
        true
    };
    let ctx = InspectContext {
        gateway_url: GATEWAY,
        token_is_active: &active,
        alias_exists: &|_| true,
    };
    let result = inspect(&DetectEnv::home_only(&home.0), ClientApp::ClaudeCode, &ctx).unwrap();
    assert_eq!(result.drift, Drift::Connected);
    let seen = seen.into_inner();
    assert_eq!(seen, [sha256_hex(TOKEN.as_bytes())]);
    assert!(!format!("{result:?}").contains(&TOKEN[6..]));
}

#[test]
fn a_foreign_address_without_our_token_is_not_configured_only_when_a_part_is_missing() {
    let home = TempHome::new();
    home.write(
        ".claude/settings.json",
        r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.example.com"}}"#,
    );
    assert_eq!(
        check(&home, ClientApp::ClaudeCode, GATEWAY, &[], &[]).drift,
        Drift::NotConfigured
    );
    home.write(
        ".claude/settings.json",
        r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.example.com","ANTHROPIC_AUTH_TOKEN":"sk-theirs"}}"#,
    );
    let result = check(&home, ClientApp::ClaudeCode, GATEWAY, &[TOKEN], &[]);
    assert_eq!(
        result.drift,
        Drift::DifferentAddress {
            found: "https://api.example.com".to_owned(),
            expected: GATEWAY.to_owned()
        }
    );
}

#[test]
fn a_bad_gateway_url_or_unparsable_file_is_an_error() {
    let home = TempHome::new();
    assert!(
        inspect(
            &DetectEnv::home_only(&home.0),
            ClientApp::Codex,
            &permissive("nope")
        )
        .is_err()
    );
    home.write(".claude/settings.json", "{oops");
    assert!(
        inspect(
            &DetectEnv::home_only(&home.0),
            ClientApp::ClaudeCode,
            &permissive(GATEWAY)
        )
        .is_err()
    );
}

#[test]
fn status_follows_the_drift_and_only_a_passed_real_request_verifies() {
    let ok = StatusFacts {
        model_allowed: true,
        ..StatusFacts::default()
    };
    let with = |edit: fn(&mut StatusFacts)| {
        let mut facts = ok.clone();
        edit(&mut facts);
        facts
    };
    let source = "~/proj/.env: GEMINI_API_KEY".to_owned();
    for (drift, facts, expected) in [
        (Drift::NotInstalled, ok.clone(), ClientStatus::NotConfigured),
        (
            Drift::Connected,
            ok.clone(),
            ClientStatus::WaitingForFirstRequest,
        ),
        (
            Drift::Connected,
            with(|f| f.waited_out = true),
            ClientStatus::NoRequest,
        ),
        (
            Drift::Connected,
            with(|f| f.first_status = Some(403)),
            ClientStatus::Rejected { status: 403 },
        ),
        (
            Drift::Connected,
            with(|f| f.first_status = Some(200)),
            ClientStatus::Configured,
        ),
        (
            Drift::Connected,
            with(|f| f.verified_at_ms = Some(7)),
            ClientStatus::Verified { at_ms: 7 },
        ),
        (
            Drift::Connected,
            with(|f| f.model_allowed = false),
            ClientStatus::ModelUnavailable,
        ),
        (
            Drift::Connected,
            StatusFacts {
                override_source: Some(source.clone()),
                ..with(|f| f.verified_at_ms = Some(7))
            },
            ClientStatus::Overridden {
                source: source.clone(),
            },
        ),
        (
            Drift::TokenNotRecognised,
            with(|f| f.verified_at_ms = Some(7)),
            ClientStatus::Drifted,
        ),
    ] {
        assert_eq!(
            client_status(&drift, &facts),
            expected,
            "{drift:?} {facts:?}"
        );
    }
    assert_eq!(
        serde_json::to_string(&ClientStatus::Rejected { status: 401 }).unwrap(),
        r#"{"kind":"rejected","status":401}"#
    );
    assert_eq!(
        serde_json::to_string(&ClientStatus::Overridden { source }).unwrap(),
        r#"{"kind":"overridden","source":"~/proj/.env: GEMINI_API_KEY"}"#
    );
}
