use super::*;
use crate::llm::client_config::{AppliedFile, ClientApp};
use crate::llm::store::test_support::connection;
use crate::llm::store::{TokenLookup, match_token};
use spanbay_base::secrets::CLIENT_TOKEN_PREFIX;

fn input(name: &str) -> ClientInput {
    ClientInput {
        name: name.to_owned(),
        allow_all: true,
        allowed_aliases: Vec::new(),
        allowed_models: Vec::new(),
    }
}

fn create(connection: &Connection, name: &str) -> ClientWithToken {
    create_client(connection, &input(name), 10).unwrap()
}

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

fn validation(result: Result<impl std::fmt::Debug, AppError>) -> String {
    match result {
        Err(AppError::Validation { message, .. }) => message,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn creating_returns_the_token_once_and_stores_only_its_hash() {
    let c = connection();
    let created = create(&c, "  Claude Code ");
    assert_eq!(created.client.name, "Claude Code");
    assert!(is_token_shaped(&created.token));
    assert!(created.client.id.starts_with("cl_") && created.client.id.len() == 15);
    assert_eq!(created.client.token_prefix, "pd_cc_");
    assert_eq!(
        created.client.token_last4,
        created.token[created.token.len() - 4..]
    );
    assert!(created.client.allow_all && created.client.is_active());
    let stored: String = c
        .query_row("SELECT token_sha256 FROM llm_gateway_clients", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stored, sha256_hex(created.token.as_bytes()));
}

/// Every row of every table as text, one line per row (blobs read as lossy UTF-8).
fn dump_all_text(connection: &Connection) -> String {
    let mut dump = String::new();
    let tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for table in tables {
        let columns: Vec<String> = connection
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        if columns.is_empty() {
            continue;
        }
        let select = columns
            .iter()
            .map(|column| format!("coalesce(CAST(\"{column}\" AS TEXT), '')"))
            .collect::<Vec<_>>()
            .join(" || '|' || ");
        let Ok(mut statement) = connection.prepare(&format!("SELECT {select} FROM \"{table}\""))
        else {
            continue;
        };
        // Other tables may hold blobs; a cast keeps their raw bytes, so read bytes, not UTF-8.
        let rows = statement
            .query_map([], |r| Ok(r.get_ref(0)?.as_bytes()?.to_vec()))
            .unwrap();
        for row in rows {
            dump.push_str(&String::from_utf8_lossy(&row.unwrap()));
            dump.push('\n');
        }
    }
    dump
}

#[test]
fn no_database_text_contains_a_full_token() {
    let c = connection();
    let tokens: Vec<String> = ["Claude Code", "Codex", "Zed"]
        .iter()
        .map(|name| create(&c, name).token)
        .collect();
    let rotated = rotate_client_token(&c, &list_clients(&c).unwrap()[0].id)
        .unwrap()
        .token;
    let dump = dump_all_text(&c);
    assert!(dump.contains("cl_"), "the dump holds the client rows");
    for token in tokens.iter().chain([&rotated]) {
        assert!(!dump.contains(token.as_str()), "a full token is stored");
        assert!(!dump.contains(&token[6..]), "the random part is stored");
    }
}

#[test]
fn debug_output_hides_the_token_and_the_hash() {
    let c = connection();
    let created = create(&c, "Zed");
    let shown = format!("{created:?}");
    assert!(!shown.contains(&created.token) && shown.contains("[redacted]"));
    let entries = client_hashes_snapshot(&c).unwrap();
    assert!(!format!("{entries:?}").contains(&entries[0].token_sha256));
}

#[test]
fn tokens_are_found_by_hash_and_the_lookup_distinguishes_revoked_from_unknown() {
    let c = connection();
    let created = create(&c, "Codex");
    assert_eq!(
        find_client_by_token(&c, &created.token).unwrap(),
        TokenLookup::Active(created.client.clone())
    );
    assert_eq!(
        find_client_by_token(&c, "pd_cx_wrong").unwrap(),
        TokenLookup::Unknown
    );
    assert_eq!(find_client_by_token(&c, "").unwrap(), TokenLookup::Unknown);
    let hash = sha256_hex(created.token.as_bytes());
    assert_eq!(
        find_client_by_token(&c, &hash).unwrap(),
        TokenLookup::Unknown
    );
    revoke_client(&c, &created.client.id, 20).unwrap();
    assert_eq!(
        find_client_by_token(&c, &created.token).unwrap(),
        TokenLookup::Revoked
    );
}

#[test]
fn the_lookup_checks_every_entry_whatever_the_position() {
    let c = connection();
    let tokens: Vec<String> = (0..5)
        .map(|n| create(&c, &format!("client {n}")).token)
        .collect();
    let entries = client_hashes_snapshot(&c).unwrap();
    for (index, token) in tokens.iter().enumerate() {
        let TokenLookup::Active(found) = match_token(&entries, token) else {
            panic!("token {index} not found");
        };
        assert_eq!(found.name, format!("client {index}"));
    }
    assert_eq!(match_token(&[], "pd_zz_nothing"), TokenLookup::Unknown);
}

#[test]
fn rotating_replaces_the_token_and_keeps_the_record() {
    let c = connection();
    let created = create_client(
        &c,
        &ClientInput {
            allow_all: false,
            allowed_aliases: vec!["fast".into()],
            ..input("Claude Code")
        },
        10,
    )
    .unwrap();
    let rotated = rotate_client_token(&c, &created.client.id).unwrap();
    assert_ne!(rotated.token, created.token);
    assert_eq!(rotated.client.id, created.client.id);
    assert_eq!(rotated.client.allowed_aliases, ["fast"]);
    assert_eq!(rotated.client.created_at_ms, 10);
    assert_eq!(
        find_client_by_token(&c, &created.token).unwrap(),
        TokenLookup::Unknown
    );
    assert_eq!(
        find_client_by_token(&c, &rotated.token).unwrap(),
        TokenLookup::Active(rotated.client)
    );
    assert!(matches!(
        rotate_client_token(&c, "cl_missing"),
        Err(AppError::NotFound { .. })
    ));
}

#[test]
fn a_revoked_client_cannot_be_rotated() {
    let c = connection();
    let created = create(&c, "Zed");
    revoke_client(&c, &created.client.id, 5).unwrap();
    validation(rotate_client_token(&c, &created.client.id));
    assert_eq!(
        find_client_by_token(&c, &created.token).unwrap(),
        TokenLookup::Revoked
    );
}

#[test]
fn revoking_keeps_the_record_and_its_first_time() {
    let c = connection();
    let created = create(&c, "Zed");
    let first = revoke_client(&c, &created.client.id, 100).unwrap();
    assert_eq!((first.revoked_at_ms, first.is_active()), (Some(100), false));
    assert_eq!(
        revoke_client(&c, &created.client.id, 200)
            .unwrap()
            .revoked_at_ms,
        Some(100)
    );
    assert_eq!(list_clients(&c).unwrap().len(), 1);
    assert!(matches!(
        revoke_client(&c, "cl_missing", 1),
        Err(AppError::NotFound { .. })
    ));
}

#[test]
fn only_revoked_clients_can_be_deleted() {
    let c = connection();
    let created = create(&c, "Zed");
    assert!(validation(delete_client(&c, &created.client.id)).contains("Revoke"));
    assert_eq!(list_clients(&c).unwrap().len(), 1);
    revoke_client(&c, &created.client.id, 1).unwrap();
    delete_client(&c, &created.client.id).unwrap();
    assert!(list_clients(&c).unwrap().is_empty());
    assert!(matches!(
        delete_client(&c, &created.client.id),
        Err(AppError::NotFound { .. })
    ));
    assert_eq!(
        find_client_by_token(&c, &created.token).unwrap(),
        TokenLookup::Unknown
    );
}

#[test]
fn names_are_unique_case_insensitively_across_all_records() {
    let c = connection();
    let first = create(&c, "Claude Code");
    validation(create_client(&c, &input("claude code"), 1));
    validation(create_client(&c, &input("  CLAUDE CODE  "), 1));
    create(&c, "Ünïcode");
    validation(create_client(&c, &input("üNÏCODE"), 1));
    revoke_client(&c, &first.client.id, 1).unwrap();
    validation(create_client(&c, &input("Claude Code"), 1));
    assert_eq!(list_clients(&c).unwrap().len(), 2);
}

#[test]
fn replace_revoked_takes_the_name_of_a_revoked_record_but_never_an_active_one() {
    let c = connection();
    let old = create(&c, "Codex");
    validation(create_client_replacing_revoked(&c, &input("codex"), 1));
    revoke_client(&c, &old.client.id, 1).unwrap();
    let new = create_client_replacing_revoked(&c, &input("codex"), 2).unwrap();
    let names: Vec<_> = list_clients(&c)
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(names, [new.client.id]);
    assert_eq!(
        find_client_by_token(&c, &old.token).unwrap(),
        TokenLookup::Unknown
    );
}

#[test]
fn a_grant_joins_the_new_model_into_a_limited_allow_list_only() {
    let c = connection();
    let limited = create_client(
        &c,
        &ClientInput {
            name: "Codex".into(),
            allow_all: false,
            allowed_aliases: Vec::new(),
            allowed_models: vec!["ds/v4-flash".into()],
        },
        1,
    )
    .unwrap();
    let grant = ModelGrant {
        default_model: Some("ds/v4-pro".into()),
        extend: vec!["ds/v4-pro".into(), "ds/v4-flash".into()],
        unknown_model_to_default: Some(true),
    };
    let after = grant_model(&c, &limited.client.id, &grant).unwrap();
    assert_eq!(after.allowed_models, ["ds/v4-flash", "ds/v4-pro"]);
    assert!(after.allows("ds/v4-pro"));
    assert_eq!(after.default_model.as_deref(), Some("ds/v4-pro"));
    assert_eq!(
        after.unknown_model_target("claude-haiku", &|_| false),
        Some("ds/v4-pro")
    );
    assert_eq!(after.unknown_model_target("ds/other", &|_| false), None);
    let open = create(&c, "Zed");
    let after = grant_model(&c, &open.client.id, &grant).unwrap();
    assert!(after.allow_all && after.allowed_models.is_empty());
}

#[test]
fn names_must_be_short_clean_text() {
    let c = connection();
    for bad in ["", "   ", "a\nb", "ta\tb", &"x".repeat(41)] {
        validation(create_client(&c, &input(bad), 1));
    }
    create(&c, &"x".repeat(40));
    assert_eq!(list_clients(&c).unwrap().len(), 1);
}

#[test]
fn the_sixty_fifth_client_is_refused_until_one_is_deleted() {
    let c = connection();
    let mut ids = Vec::new();
    for n in 0..MAX_CLIENTS {
        ids.push(create(&c, &format!("client {n}")).client.id);
    }
    assert!(validation(create_client(&c, &input("one more"), 1)).contains("64"));
    assert_eq!(list_clients(&c).unwrap().len(), 64);
    revoke_client(&c, &ids[0], 1).unwrap();
    assert!(
        create_client(&c, &input("one more"), 1).is_err(),
        "revoked records count"
    );
    delete_client(&c, &ids[0]).unwrap();
    create(&c, "one more");
}

#[test]
fn an_empty_allow_list_needs_all() {
    let c = connection();
    let none = ClientInput {
        allow_all: false,
        allowed_aliases: vec!["  ".into()],
        ..input("Zed")
    };
    assert_eq!(
        validation(create_client(&c, &none, 1)),
        "Pick at least one alias or model, or choose All"
    );
    let by_model = ClientInput {
        allowed_models: vec!["openai/gpt-4o".into()],
        ..none
    };
    assert!(create_client(&c, &by_model, 1).is_ok());
}

#[test]
fn allow_lists_are_trimmed_and_deduplicated() {
    let c = connection();
    let created = create_client(
        &c,
        &ClientInput {
            allow_all: false,
            allowed_aliases: vec![" fast ".into(), "fast".into(), "deep".into(), String::new()],
            allowed_models: vec!["a/b".into(), "a/b".into()],
            ..input("Zed")
        },
        1,
    )
    .unwrap();
    assert_eq!(created.client.allowed_aliases, ["fast", "deep"]);
    assert_eq!(created.client.allowed_models, ["a/b"]);
    validation(create_client(
        &c,
        &ClientInput {
            allow_all: false,
            allowed_aliases: vec!["x".repeat(201)],
            ..input("Other")
        },
        1,
    ));
    validation(create_client(
        &c,
        &ClientInput {
            allow_all: false,
            allowed_aliases: (0..=MAX_ALLOW_ENTRIES).map(|n| format!("a{n}")).collect(),
            ..input("Big")
        },
        1,
    ));
}

#[test]
fn updating_changes_the_name_and_list_and_nothing_else() {
    let c = connection();
    let created = create(&c, "Zed");
    let other = create(&c, "Codex");
    let updated = update_client(
        &c,
        &created.client.id,
        &ClientInput {
            allow_all: false,
            allowed_models: vec!["a/b".into()],
            ..input("Zed editor")
        },
    )
    .unwrap();
    assert_eq!(updated.name, "Zed editor");
    assert!(!updated.allow_all);
    assert_eq!(updated.token_prefix, created.client.token_prefix);
    assert_eq!(updated.created_at_ms, 10);
    let TokenLookup::Active(found) = find_client_by_token(&c, &created.token).unwrap() else {
        panic!("token refused");
    };
    assert!(found.allows("a/b") && !found.allows("c/d"));
    // Keeping its own name is fine; taking another client's is not.
    assert!(update_client(&c, &created.client.id, &input("ZED EDITOR")).is_ok());
    validation(update_client(&c, &created.client.id, &input("codex")));
    assert!(matches!(
        update_client(&c, "cl_missing", &input("x")),
        Err(AppError::NotFound { .. })
    ));
    assert_eq!(get_client(&c, &other.client.id).unwrap().name, "Codex");
}

#[test]
fn last_use_is_written_at_most_once_per_thirty_seconds() {
    let c = connection();
    let created = create(&c, "Zed");
    let id = &created.client.id;
    assert!(touch_client(&c, id, 1_000_000).unwrap());
    assert!(!touch_client(&c, id, 1_000_000 + 29_999).unwrap());
    assert_eq!(get_client(&c, id).unwrap().last_used_ms, Some(1_000_000));
    assert!(touch_client(&c, id, 1_000_000 + TOUCH_INTERVAL_MS).unwrap());
    assert_eq!(get_client(&c, id).unwrap().last_used_ms, Some(1_030_000));
    assert!(
        !touch_client(&c, id, 500).unwrap(),
        "a clock that went back writes nothing"
    );
    assert!(!touch_client(&c, "cl_missing", 9_999_999).unwrap());
}

#[test]
fn the_client_migrated_from_the_legacy_token_reads_as_an_allow_all_client() {
    let c = connection();
    let token = "pdk_0123456789abcdef";
    c.execute(
        "INSERT INTO llm_gateway_clients (id, name, token_sha256, token_prefix, created_at_ms) \
         VALUES ('gwc_default', 'Default', ?1, 'pdk_012…', 0)",
        params![sha256_hex(token.as_bytes())],
    )
    .unwrap();
    let TokenLookup::Active(client) = find_client_by_token(&c, token).unwrap() else {
        panic!("legacy token refused");
    };
    assert_eq!(
        (client.id.as_str(), client.token_last4.as_str()),
        ("gwc_default", "")
    );
    assert!(client.allow_all && client.allows("openai/gpt-4o"));
    assert!(client.migrated, "only the migrated client carries the flag");
    assert!(
        !create(&c, "Mine").client.migrated,
        "a user-made client does not"
    );
    let rotated = rotate_client_token(&c, "gwc_default").unwrap();
    assert!(is_token_shaped(&rotated.token));
    assert_eq!(rotated.client.token_prefix, "pd_de_");
}

#[test]
fn the_snapshot_pairs_each_client_with_its_hash() {
    let c = connection();
    let a = create(&c, "Zed");
    let entries = client_hashes_snapshot(&c).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].token_sha256, sha256_hex(a.token.as_bytes()));
    assert_eq!(entries[0].client, a.client);
}

#[test]
fn a_passed_request_is_remembered_until_the_token_is_rotated() {
    let c = connection();
    let a = create(&c, "Zed");
    assert_eq!(client_verified_at(&c, &a.client.id).unwrap(), None);
    assert!(mark_client_verified(&c, &a.client.id, 100).unwrap());
    assert!(
        !mark_client_verified(&c, &a.client.id, 50).unwrap(),
        "an older instant loses"
    );
    assert_eq!(client_verified_at(&c, &a.client.id).unwrap(), Some(100));
    rotate_client_token(&c, &a.client.id).unwrap();
    assert_eq!(
        client_verified_at(&c, &a.client.id).unwrap(),
        None,
        "a new token starts unverified"
    );
}

fn ledger_rows(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT COUNT(*) FROM llm_client_applied", [], |r| r.get(0))
        .unwrap()
}

fn written_files() -> Vec<(ClientApp, Vec<AppliedFile>)> {
    vec![(
        ClientApp::Codex,
        vec![AppliedFile {
            path: "/home/u/.codex/config.toml".into(),
            backup_path: None,
            sha256_after: "ab".repeat(32),
            existed_before: false,
        }],
    )]
}

#[test]
fn a_rotation_committed_against_a_stale_hash_changes_nothing() {
    let c = connection();
    let created = create_client(
        &c,
        &ClientInput {
            allow_all: false,
            allowed_models: vec!["a/b".into()],
            ..input("Codex")
        },
        10,
    )
    .unwrap();
    let id = &created.client.id;
    let seen = sha256_hex(created.token.as_bytes());
    let rotated = rotate_client_token(&c, id).unwrap();
    let minted = mint_token("Codex").unwrap();
    let grant = ModelGrant {
        extend: vec!["c/d".into()],
        ..ModelGrant::default()
    };
    let result = commit_rotation(&c, id, &seen, &minted, &written_files(), &grant, 20);
    assert!(
        matches!(result, Err(AppError::Conflict { .. })),
        "{result:?}"
    );
    assert_eq!(
        find_client_by_token(&c, &rotated.token).unwrap(),
        TokenLookup::Active(rotated.client.clone())
    );
    assert_eq!(
        find_client_by_token(&c, &minted.token).unwrap(),
        TokenLookup::Unknown
    );
    assert_eq!(ledger_rows(&c), 0, "the ledger is unchanged");
    assert_eq!(get_client(&c, id).unwrap().allowed_models, ["a/b"]);
}

#[test]
fn a_revoked_client_cannot_commit_a_rotation() {
    let c = connection();
    let created = create(&c, "Zed");
    let id = &created.client.id;
    revoke_client(&c, id, 5).unwrap();
    let minted = mint_token("Zed").unwrap();
    validation(commit_rotation(
        &c,
        id,
        &sha256_hex(created.token.as_bytes()),
        &minted,
        &written_files(),
        &ModelGrant::default(),
        20,
    ));
    assert_eq!(ledger_rows(&c), 0, "the ledger is unchanged");
}

#[test]
fn limits_live_next_to_the_aliases_and_survive_an_update() {
    let connection = connection();
    let created = create(&connection, "Zed");
    let policy = ClientPolicyInput {
        limits: ClientLimits {
            rpm: Some(30),
            daily_tokens: Some(5_000),
            ..ClientLimits::default()
        },
        unknown_model_to_default: Some(true),
    };
    let saved = set_client_policy(&connection, &created.client.id, &policy).unwrap();
    assert_eq!(saved.limits.rpm, Some(30));
    assert!(saved.unknown_model_to_default);
    let renamed = update_client(&connection, &created.client.id, &input("Zed 2")).unwrap();
    assert_eq!(renamed.limits, saved.limits, "a rename keeps the limits");
    assert!(renamed.allowed_aliases.is_empty());
    let cleared = set_client_policy(
        &connection,
        &created.client.id,
        &ClientPolicyInput::default(),
    )
    .unwrap();
    assert!(cleared.limits.is_empty() && cleared.unknown_model_to_default);
    let stored: String = connection
        .query_row(
            "SELECT allowed_aliases_json FROM llm_gateway_clients WHERE id = ?1",
            [&created.client.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored, "[]",
        "without limits the column is the bare list again"
    );
    assert!(
        set_client_policy(
            &connection,
            &created.client.id,
            &ClientPolicyInput {
                limits: ClientLimits {
                    rpm: Some(0),
                    ..ClientLimits::default()
                },
                unknown_model_to_default: None,
            },
        )
        .is_err()
    );
}
