//! Token minting and matching, allow lists and client input checks.

use std::collections::{HashMap, HashSet};

use super::*;

/// `^pd_[a-z]{2,4}_[A-Za-z0-9]{32}$`.
fn is_token_shaped(token: &str) -> bool {
    let Some(rest) = token.strip_prefix(CLIENT_TOKEN_PREFIX) else {
        return false;
    };
    let Some((tag, random)) = rest.split_once('_') else {
        return false;
    };
    (2..=4).contains(&tag.len())
        && tag.bytes().all(|b| b.is_ascii_lowercase())
        && random.len() == 32
        && random.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[test]
fn a_thousand_tokens_are_unique_and_well_formed() {
    let tokens: Vec<String> = (0..1_000)
        .map(|_| mint_token("Claude Code").unwrap().token)
        .collect();
    assert!(tokens.iter().all(|token| is_token_shaped(token)));
    assert!(tokens.iter().all(|token| token.starts_with("pd_cc_")));
    assert_eq!(tokens.iter().collect::<HashSet<_>>().len(), 1_000);
}

#[test]
fn tags_come_from_the_name() {
    for (name, tag) in [
        ("Claude Code", "cc"),
        ("claude", "cc"),
        ("Codex", "cx"),
        ("CODEX CLI", "cx"),
        ("Zed", "zd"),
        ("OpenCode", "op"),
        ("Gemini CLI", "ge"),
        ("A", "ax"),
        ("日本語", "xx"),
        ("1x2y", "xy"),
        ("My tool", "my"),
    ] {
        assert_eq!(token_tag(name), tag, "{name}");
        assert!(is_token_shaped(&format!("pd_{tag}_{}", "A".repeat(32))));
    }
}

#[test]
fn rejection_sampling_gives_every_character_the_same_share() {
    let mut next = 0_u8;
    let mut fill = |bytes: &mut [u8]| -> Result<(), AppError> {
        for byte in bytes.iter_mut() {
            *byte = next;
            next = next.wrapping_add(1);
        }
        Ok(())
    };
    // One pass of bytes 0..=247 holds each of the 62 characters exactly four times; the
    // bytes 248..=255 are rejected and never shift the distribution.
    let text = base62_from(248, &mut fill).unwrap();
    let mut counts = HashMap::new();
    for ch in text.chars() {
        *counts.entry(ch).or_insert(0_u32) += 1;
    }
    assert_eq!(counts.len(), 62);
    assert!(counts.values().all(|n| *n == 4), "{counts:?}");
}

#[test]
fn a_failing_random_source_creates_nothing() {
    let mut fail = |_: &mut [u8]| -> Result<(), AppError> { Err(AppError::internal("no entropy")) };
    assert!(base62_from(32, &mut fail).is_err());
}

#[test]
fn the_allow_list_matches_aliases_and_models_exactly() {
    let restricted = GatewayClient {
        id: "cl_1".into(),
        name: "Zed".into(),
        token_prefix: "pd_zd_".into(),
        token_last4: "abcd".into(),
        allow_all: false,
        allowed_aliases: vec!["fast".into()],
        allowed_models: vec!["deepseek/deepseek-chat".into()],
        created_at_ms: 0,
        last_used_ms: None,
        revoked_at_ms: None,
        migrated: false,
        default_model: None,
        unknown_model_to_default: false,
        limits: ClientLimits::default(),
    };
    assert!(restricted.allows("fast"));
    assert!(restricted.allows("deepseek/deepseek-chat"));
    assert!(!restricted.allows("deep-think"));
    assert!(!restricted.allows("Fast"));
    assert!(!restricted.allows("openai/gpt-4o"));
    assert!(!restricted.allows(""));
    let open = GatewayClient {
        allow_all: true,
        allowed_aliases: Vec::new(),
        allowed_models: Vec::new(),
        ..restricted
    };
    assert!(open.allows("anything") && open.allows("openai/gpt-4o"));
}

fn input(name: &str, aliases: &[&str], models: &[&str]) -> ClientInput {
    ClientInput {
        name: name.to_owned(),
        allow_all: false,
        allowed_aliases: aliases.iter().map(|entry| (*entry).to_owned()).collect(),
        allowed_models: models.iter().map(|entry| (*entry).to_owned()).collect(),
    }
}

fn error_text(result: Result<CheckedClientInput, AppError>) -> String {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn client_names_must_be_short_and_printable() {
    let name_error = "A client name must be 1-40 characters without control characters";
    for bad in ["", "   ", &"n".repeat(41), "tab\tname"] {
        let text = error_text(check_client_input(&input(bad, &["fast"], &[])));
        assert!(text.contains(name_error), "{bad:?}: {text}");
    }
    let ok = check_client_input(&input(&format!(" {} ", "n".repeat(40)), &["fast"], &[]));
    assert_eq!(ok.unwrap().name, "n".repeat(40));
}

#[test]
fn allow_lists_are_trimmed_deduplicated_in_order_and_bounded() {
    let checked = check_client_input(&input(
        "Zed",
        &[" fast ", "", "slow", "fast", "  "],
        &["b/m", "a/m", " b/m"],
    ))
    .unwrap();
    assert_eq!(checked.aliases, ["fast", "slow"]);
    assert_eq!(checked.models, ["b/m", "a/m"]);

    let empty = error_text(check_client_input(&input("Zed", &["", " "], &[])));
    assert!(empty.contains("Pick at least one alias or model, or choose All"));

    let aliases: Vec<String> = (0..200).map(|n| format!("a{n}")).collect();
    let models: Vec<String> = (0..57).map(|n| format!("p/m{n}")).collect();
    let too_many = ClientInput {
        allowed_aliases: aliases.clone(),
        allowed_models: models,
        ..input("Zed", &[], &[])
    };
    let text = error_text(check_client_input(&too_many));
    assert!(
        text.contains("A client allow-list holds at most 256 entries"),
        "{text}"
    );
    let just_enough = ClientInput {
        allowed_aliases: aliases,
        allowed_models: (0..56).map(|n| format!("p/m{n}")).collect(),
        ..input("Zed", &[], &[])
    };
    assert!(check_client_input(&just_enough).is_ok());
}

#[test]
fn bad_entries_are_reported_after_the_name_and_before_the_size() {
    let entry_error = "An allow-list entry is too long or contains control characters";
    let long = "x".repeat(201);
    for bad in [long.as_str(), "a\u{7}b"] {
        let text = error_text(check_client_input(&input("Zed", &["ok", bad], &[])));
        assert!(text.contains(entry_error), "{text}");
    }
    assert!(check_client_input(&input("Zed", &["x".repeat(200).as_str()], &[])).is_ok());
    // The name is checked first, then each list entry, then the list size.
    let text = error_text(check_client_input(&input("", &[long.as_str()], &[])));
    assert!(text.contains("A client name must be"), "{text}");
    let mut crowded: Vec<String> = (0..300).map(|n| format!("a{n}")).collect();
    crowded.push(long.clone());
    let text = error_text(check_client_input(&ClientInput {
        allowed_aliases: crowded,
        ..input("Zed", &[], &[])
    }));
    assert!(text.contains(entry_error), "{text}");
}

#[test]
fn limits_are_checked_and_the_currency_normalised() {
    let ok = check_client_limits(&ClientLimits {
        rpm: Some(60),
        max_concurrent: Some(4),
        daily_budget_micro: Some(5_000_000),
        budget_currency: " cny ".into(),
        daily_tokens: Some(1_000),
        ..ClientLimits::default()
    })
    .unwrap();
    assert_eq!(ok.currency(), "CNY");
    assert!(ok.counts_usage());
    let usd = check_client_limits(&ClientLimits {
        budget_currency: "usd".into(),
        ..ClientLimits::default()
    })
    .unwrap();
    assert!(usd.budget_currency.is_empty() && usd.is_empty());
    for bad in [
        ClientLimits {
            rpm: Some(0),
            ..ClientLimits::default()
        },
        ClientLimits {
            max_concurrent: Some(MAX_LIMIT_CONCURRENT + 1),
            ..ClientLimits::default()
        },
        ClientLimits {
            monthly_budget_micro: Some(-1),
            ..ClientLimits::default()
        },
        ClientLimits {
            monthly_tokens: Some(0),
            ..ClientLimits::default()
        },
        ClientLimits {
            budget_currency: "dollars".into(),
            ..ClientLimits::default()
        },
    ] {
        assert!(check_client_limits(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_token_expires_at_its_instant() {
    let limits = ClientLimits {
        expires_at_ms: Some(1_000),
        ..ClientLimits::default()
    };
    assert!(!limits.expired(999));
    assert!(limits.expired(1_000));
    assert!(!ClientLimits::default().expired(i64::MAX));
}
