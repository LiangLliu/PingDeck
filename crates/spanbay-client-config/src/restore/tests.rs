//! Tests of restoring one write of a tool and of "Restore original config".

use std::fs;

use super::super::ApplyResult;
use super::super::test_util::{TempHome, apply_alias, apply_req, at, request};
use super::*;

fn renv<'a>(
    data: &'a Path,
    app: ClientApp,
    clock: Clock,
    active: &'a dyn Fn(&str) -> bool,
) -> RestoreEnv<'a> {
    RestoreEnv {
        app,
        backup_root: data,
        clock,
        token_is_active: active,
    }
}

fn all_active(_: &str) -> bool {
    true
}

/// The restore environment of `app`, `secs` after the test clock, with every token active.
fn env_at(data: &Path, app: ClientApp, secs: i64) -> RestoreEnv<'_> {
    renv(data, app, at(secs), &all_active)
}

fn connect(home: &TempHome, app: ClientApp, alias: &str) -> ApplyResult {
    apply_req(home, &request(app, Some(alias)), None, at(0)).unwrap()
}

/// The only outcome of a one-file restore.
fn single(outcomes: Vec<(String, RestoreOutcome)>) -> RestoreOutcome {
    let [(_, outcome)]: [(String, RestoreOutcome); 1] = outcomes.try_into().unwrap();
    outcome
}

#[test]
fn restore_returns_each_file_to_its_pre_connect_bytes_and_deletes_new_ones() {
    let home = TempHome::new();
    let original = "# mine\nA=1\n";
    home.write(".gemini/.env", original);
    let applied = connect(&home, ClientApp::GeminiCli, "fast");
    assert!(!applied.files[0].existed_before);
    let data = home.data();

    let outcomes = restore(
        &applied.files,
        &env_at(&data, ClientApp::GeminiCli, 10),
        false,
    )
    .unwrap();

    assert!(matches!(
        outcomes[0].1,
        RestoreOutcome::Deleted {
            safety_backup: Some(_)
        }
    ));
    assert!(matches!(
        outcomes[1].1,
        RestoreOutcome::Restored {
            safety_backup: Some(_),
            backup_has_revoked_token: false,
            ..
        }
    ));
    assert_eq!(home.read(".gemini/.env").unwrap(), original);
    assert!(home.read(".gemini/settings.json").is_none());
}

#[test]
fn one_edited_file_holds_back_the_whole_group_until_confirmed() {
    let home = TempHome::new();
    home.write(".gemini/.env", "A=1\n");
    let applied = connect(&home, ClientApp::GeminiCli, "fast");
    let written_env = home.read(".gemini/.env").unwrap();
    // Only settings.json changes after the write.
    home.write(".gemini/settings.json", "{\"edited\":true}\n");
    let data = home.data();

    let outcomes = restore(
        &applied.files,
        &env_at(&data, ClientApp::GeminiCli, 5),
        false,
    )
    .unwrap();

    // Nothing was put back, not even the untouched `.env`: both files ask.
    assert!(
        outcomes
            .iter()
            .all(|(_, o)| matches!(o, RestoreOutcome::NeedsConfirmation { .. }))
    );
    let RestoreOutcome::NeedsConfirmation { deletes_file, .. } = &outcomes[0].1 else {
        panic!("confirmation expected")
    };
    assert!(
        *deletes_file,
        "settings.json did not exist before the write"
    );
    assert_eq!(home.read(".gemini/.env").unwrap(), written_env);
    assert!(
        home.backups(ClientApp::GeminiCli)
            .iter()
            .all(|n| !n.contains("safety"))
    );

    let forced = restore(
        &applied.files,
        &env_at(&data, ClientApp::GeminiCli, 6),
        true,
    )
    .unwrap();
    assert!(
        forced
            .iter()
            .all(|(_, o)| !matches!(o, RestoreOutcome::NeedsConfirmation { .. }))
    );
    assert_eq!(home.read(".gemini/.env").unwrap(), "A=1\n");
    assert!(home.read(".gemini/settings.json").is_none());
}

#[test]
fn restore_backs_up_the_current_content_to_a_safety_backup_in_the_data_folder() {
    let home = TempHome::new();
    home.write(".claude/settings.json", "{\"a\":1}\n");
    let applied = connect(&home, ClientApp::ClaudeCode, "fast");
    let written = home.read(".claude/settings.json").unwrap();
    let data = home.data();
    let RestoreOutcome::Restored { safety_backup, .. } = single(
        restore(
            &applied.files,
            &env_at(&data, ClientApp::ClaudeCode, 5),
            false,
        )
        .unwrap(),
    ) else {
        panic!("restored expected");
    };
    let safety = safety_backup.unwrap();
    assert!(safety.starts_with(data.to_str().unwrap()), "{safety}");
    assert!(safety.contains(".spanbay-safety-"), "{safety}");
    assert_eq!(fs::read_to_string(safety).unwrap(), written);
    assert_eq!(home.read(".claude/settings.json").unwrap(), "{\"a\":1}\n");
    assert_eq!(home.list(".claude"), ["settings.json"]);
}

#[test]
fn a_file_edited_after_the_write_needs_confirmation_with_a_masked_diff() {
    let home = TempHome::new();
    home.write(".claude/settings.json", "{\"a\":1}\n");
    let mut req = request(ClientApp::ClaudeCode, Some("fast"));
    req.token = "pd_cc_abcdefghijklmnopqrstuvwx9d2f".to_owned();
    let applied = apply_req(&home, &req, None, at(0)).unwrap();
    let edited = format!(
        "{}\n# edited\n",
        home.read(".claude/settings.json").unwrap().trim_end()
    );
    home.write(".claude/settings.json", &edited);
    let data = home.data();

    let outcome = single(
        restore(
            &applied.files,
            &env_at(&data, ClientApp::ClaudeCode, 5),
            false,
        )
        .unwrap(),
    );
    let RestoreOutcome::NeedsConfirmation {
        diff,
        deletes_file,
        backup_has_revoked_token,
        ..
    } = outcome
    else {
        panic!("confirmation expected");
    };
    assert!(!backup_has_revoked_token && !deletes_file);
    assert!(diff.starts_with("--- a/"), "{diff}");
    assert!(diff.contains("pd_cc_••••9d2f"), "{diff}");
    assert!(!diff.contains("abcdefghijklmnop"), "{diff}");
    assert_eq!(home.read(".claude/settings.json").unwrap(), edited);
}

#[test]
fn restore_says_when_the_backup_holds_a_revoked_token() {
    let home = TempHome::new();
    let old = "pd_cc_oldoldoldoldoldoldoldold1234";
    home.write(
        ".claude/settings.json",
        &format!("{{\"env\":{{\"ANTHROPIC_AUTH_TOKEN\":\"{old}\"}}}}\n"),
    );
    let applied = connect(&home, ClientApp::ClaudeCode, "fast");
    let data = home.data();
    let old_sha = sha256_hex(old.as_bytes());
    let revoked = |_: &str| false;
    let outcome = single(
        restore(
            &applied.files,
            &renv(&data, ClientApp::ClaudeCode, at(2), &revoked),
            false,
        )
        .unwrap(),
    );
    let RestoreOutcome::Restored {
        revoked_tokens,
        backup_has_revoked_token,
        ..
    } = &outcome
    else {
        panic!("restored expected: {outcome:?}");
    };
    assert!(*backup_has_revoked_token);
    assert_eq!(
        revoked_tokens,
        &vec![RevokedToken {
            hint: "pd_cc_••••1234".to_owned(),
            sha256: old_sha
        }]
    );
    assert!(!format!("{revoked_tokens:?}").contains("oldoldoldold"));
}

#[test]
fn restore_fails_clearly_without_its_backup_and_changes_nothing() {
    let home = TempHome::new();
    home.write(".gemini/settings.json", "{}\n");
    home.write(".gemini/.env", "A=1\n");
    let applied = connect(&home, ClientApp::GeminiCli, "fast");
    let written = home.read(".gemini/settings.json").unwrap();
    let mut entries = applied.files.clone();
    fs::remove_file(entries[1].backup_path.take().unwrap()).unwrap();
    let data = home.data();
    assert!(restore(&entries, &env_at(&data, ClientApp::GeminiCli, 1), true).is_err());
    assert_eq!(home.read(".gemini/settings.json").unwrap(), written);
}

#[cfg(unix)]
#[test]
fn restore_refuses_a_hard_linked_file() {
    let home = TempHome::new();
    let applied = connect(&home, ClientApp::Codex, "fast");
    fs::hard_link(&applied.files[0].path, home.0.join("twin.toml")).unwrap();
    let data = home.data();
    let err = restore(&applied.files, &env_at(&data, ClientApp::Codex, 1), true).unwrap_err();
    assert!(err.message().contains("hard link"), "{}", err.message());
    assert!(home.read(".codex/config.toml").is_some());
}

#[test]
fn restore_original_asks_first_and_names_a_file_it_would_delete() {
    let home = TempHome::new();
    let original = "{\"original\": true, \"keep\": [1, 2]}\n";
    home.write(".claude/settings.json", original);
    for n in 0..6 {
        apply_alias(&home, &format!("alias{n}"), at(n));
    }
    let data = home.data();
    let env = env_at(&data, ClientApp::ClaudeCode, 10);
    let asked = restore_original(&home.0, &env, false).unwrap();
    let [
        (
            _,
            RestoreOutcome::NeedsConfirmation {
                diff, deletes_file, ..
            },
        ),
    ] = &asked[..]
    else {
        panic!("one confirmation expected: {asked:?}");
    };
    assert!(
        !deletes_file && diff.contains("+{\"original\": true"),
        "{diff}"
    );
    assert_ne!(home.read(".claude/settings.json").unwrap(), original);

    let restored = restore_original(&home.0, &env, true).unwrap();
    assert!(matches!(restored[0].1, RestoreOutcome::Restored { .. }));
    assert_eq!(home.read(".claude/settings.json").unwrap(), original);
    // Already the original: nothing left to do.
    assert!(restore_original(&home.0, &env, false).unwrap().is_empty());

    let fresh = TempHome::new();
    apply_req(&fresh, &request(ClientApp::Codex, Some("m")), None, at(0)).unwrap();
    let data = fresh.data();
    let env = env_at(&data, ClientApp::Codex, 1);
    let asked = restore_original(&fresh.0, &env, false).unwrap();
    assert!(matches!(
        asked[0].1,
        RestoreOutcome::NeedsConfirmation {
            deletes_file: true,
            ..
        }
    ));
    let restored = restore_original(&fresh.0, &env, true).unwrap();
    assert!(matches!(restored[0].1, RestoreOutcome::Deleted { .. }));
    assert!(fresh.read(".codex/config.toml").is_none());
}

#[test]
fn taking_a_tool_over_again_refreshes_its_original() {
    let home = TempHome::new();
    home.write(".claude/settings.json", "{\"v\": 1}\n");
    apply_alias(&home, "a", at(0));
    let data = home.data();
    restore_original(&home.0, &env_at(&data, ClientApp::ClaudeCode, 1), true).unwrap();
    // Months of the user's own edits, then a new connect.
    home.write(".claude/settings.json", "{\"v\": 2, \"mine\": true}\n");
    apply_alias(&home, "b", at(2));
    apply_alias(&home, "c", at(3));

    restore_original(&home.0, &env_at(&data, ClientApp::ClaudeCode, 4), true).unwrap();

    assert_eq!(
        home.read(".claude/settings.json").unwrap(),
        "{\"v\": 2, \"mine\": true}\n"
    );
}

#[test]
fn a_file_the_user_creates_after_a_restore_becomes_the_original_on_the_next_connect() {
    let home = TempHome::new();
    apply_req(
        &home,
        &request(ClientApp::GeminiCli, Some("a")),
        None,
        at(0),
    )
    .unwrap();
    let data = home.data();
    restore_original(&home.0, &env_at(&data, ClientApp::GeminiCli, 1), true).unwrap();
    assert!(home.read(".gemini/.env").is_none());
    home.write(".gemini/.env", "MINE=1\n");
    apply_req(
        &home,
        &request(ClientApp::GeminiCli, Some("b")),
        None,
        at(2),
    )
    .unwrap();

    restore_original(&home.0, &env_at(&data, ClientApp::GeminiCli, 3), true).unwrap();

    assert_eq!(home.read(".gemini/.env").unwrap(), "MINE=1\n");
}
