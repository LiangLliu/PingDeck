//! Tests of the atomic apply and the write safety rules.

use std::cell::Cell;

use super::super::test_util::{CLOCK, TempHome, apply_alias, apply_now, apply_req, at, request};
use super::super::{RestoreEnv, restore_original};
use super::*;

fn apply_renaming(
    home: &TempHome,
    req: &ClientConfigRequest,
    rename: Rename<'_>,
) -> Result<ApplyResult, AppError> {
    apply_with(&home.0, &home.data(), req, None, CLOCK, rename)
}

/// A rename that fails on its second call.
fn failing_second_rename(calls: &Cell<u32>) -> impl Fn(&Path, &Path) -> io::Result<()> + '_ {
    |from: &Path, to: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            Err(io::Error::other("disk on fire"))
        } else {
            fs::rename(from, to)
        }
    }
}

fn no_temp_files(home: &TempHome, dirs: &[&str]) {
    for dir in dirs {
        let stray: Vec<_> = home
            .list(dir)
            .into_iter()
            .filter(|n| n.ends_with(".pingdeck-tmp"))
            .collect();
        assert!(stray.is_empty(), "{dir}: {stray:?}");
    }
}

// ----------------------------------------------------------------------------- apply results

#[test]
fn applied_entries_describe_what_was_written() {
    let home = TempHome::new();
    home.write(".gemini/.env", "A=1\n");
    let req = request(ClientApp::GeminiCli, Some("fast"));
    let applied = apply_now(&home, &req).unwrap();
    assert_eq!(applied.client_name, "Test client");
    assert_eq!(
        applied.message,
        "Restart Gemini CLI to use the new address."
    );
    let [settings, env] = &applied.files[..] else {
        panic!("two files expected");
    };
    assert!(!settings.existed_before && settings.backup_path.is_none());
    assert!(env.existed_before && env.backup_path.is_some());
    for file in &applied.files {
        let bytes = fs::read(&file.path).unwrap();
        assert_eq!(file.sha256_after, sha256_hex(&bytes));
    }
}

#[test]
fn the_result_says_when_nothing_changed() {
    let home = TempHome::new();
    let req = request(ClientApp::Codex, None);
    assert_eq!(
        apply_now(&home, &req).unwrap().message,
        "Restart Codex to use the new address."
    );
    let second = apply_now(&home, &req).unwrap();
    assert!(second.files.is_empty());
    assert_eq!(
        second.message,
        "Codex already uses this gateway; nothing was changed."
    );
}

#[test]
fn an_already_correct_json_file_is_not_rewritten() {
    let home = TempHome::new();
    let req = request(ClientApp::ClaudeCode, Some("fast"));
    apply_now(&home, &req).unwrap();
    let compact = home
        .read(".claude/settings.json")
        .unwrap()
        .replace("\n  ", "")
        .replace('\n', "");
    home.write(".claude/settings.json", &compact);
    let plan = super::super::preview(&home.0, &req).unwrap();
    assert!(!plan.files[0].changed);
    assert!(apply_now(&home, &req).unwrap().files.is_empty());
    assert_eq!(home.read(".claude/settings.json").unwrap(), compact);
}

// --------------------------------------------------------------------------------- atomicity

#[test]
fn all_temp_files_exist_before_the_first_rename_and_none_remain() {
    let home = TempHome::new();
    let gemini = home.0.join(".gemini");
    let calls = Cell::new(0);
    let rename = |from: &Path, to: &Path| {
        if calls.get() == 0 {
            for name in [".settings.json.pingdeck-tmp", "..env.pingdeck-tmp"] {
                assert!(
                    gemini.join(name).is_file(),
                    "{name} missing at first rename"
                );
            }
        }
        calls.set(calls.get() + 1);
        fs::rename(from, to)
    };
    apply_renaming(&home, &request(ClientApp::GeminiCli, Some("x")), &rename).unwrap();
    assert_eq!(calls.get(), 2);
    no_temp_files(&home, &[".gemini"]);
}

#[test]
fn a_failing_second_rename_restores_both_gemini_files() {
    let home = TempHome::new();
    let settings = "{\"theme\":\"x\"}\n";
    let env = "A=1\n";
    home.write(".gemini/settings.json", settings);
    home.write(".gemini/.env", env);
    let calls = Cell::new(0);
    let rename = |from: &Path, to: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            Err(io::Error::other("disk on fire"))
        } else {
            fs::rename(from, to)
        }
    };
    let err =
        apply_renaming(&home, &request(ClientApp::GeminiCli, Some("fast")), &rename).unwrap_err();
    assert_eq!(home.read(".gemini/settings.json").unwrap(), settings);
    assert_eq!(home.read(".gemini/.env").unwrap(), env);
    let message = err.message();
    assert!(message.contains("disk on fire"), "{message}");
    assert!(message.contains("rolled back"), "{message}");
    assert!(
        message.contains("Changed, then restored: ") && message.contains("settings.json"),
        "{message}"
    );
    assert!(
        message.contains("Not changed: ") && message.contains(".env"),
        "{message}"
    );
    no_temp_files(&home, &[".gemini"]);
    assert!(home.backups(ClientApp::GeminiCli).is_empty());
}

#[test]
fn a_failing_rename_deletes_files_the_apply_created() {
    let home = TempHome::new();
    let calls = Cell::new(0);
    let rename = |from: &Path, to: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            Err(io::Error::other("nope"))
        } else {
            fs::rename(from, to)
        }
    };
    assert!(apply_renaming(&home, &request(ClientApp::GeminiCli, None), &rename).is_err());
    assert!(home.read(".gemini/settings.json").is_none());
    assert!(home.read(".gemini/.env").is_none());
    no_temp_files(&home, &[".gemini"]);
}

#[test]
fn a_failing_first_rename_changes_nothing() {
    let home = TempHome::new();
    home.write(".gemini/settings.json", "{}\n");
    let err = apply_renaming(&home, &request(ClientApp::GeminiCli, None), &|_, _| {
        Err(io::Error::other("first"))
    })
    .unwrap_err();
    assert!(
        err.message().contains("Changed, then restored: none."),
        "{}",
        err.message()
    );
    assert_eq!(home.read(".gemini/settings.json").unwrap(), "{}\n");
    assert!(home.read(".gemini/.env").is_none());
    no_temp_files(&home, &[".gemini"]);
    assert!(home.backups(ClientApp::GeminiCli).is_empty());
}

#[test]
fn permission_denied_has_the_checked_wording() {
    let denied = io::Error::from(io::ErrorKind::PermissionDenied);
    assert_eq!(
        write_error("/h/.claude/settings.json", &denied).message(),
        "PingDeck cannot write `/h/.claude/settings.json`. Check the file's permissions."
    );
    let home = TempHome::new();
    home.write(".claude/settings.json", "{}\n");
    let err = apply_renaming(&home, &request(ClientApp::ClaudeCode, None), &|_, _| {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    })
    .unwrap_err();
    assert!(err.message().contains("Check the file's permissions."));
}

#[test]
fn staging_failures_leave_no_trace() {
    let home = TempHome::new();
    // A directory where the temp file must go makes staging the second file fail.
    home.write(".gemini/settings.json", "{}\n");
    fs::create_dir_all(home.0.join(".gemini/..env.pingdeck-tmp")).unwrap();
    let err = apply_req(&home, &request(ClientApp::GeminiCli, None), None, CLOCK).unwrap_err();
    assert!(err.message().contains(".env"), "{}", err.message());
    assert_eq!(home.read(".gemini/settings.json").unwrap(), "{}\n");
    assert!(home.backups(ClientApp::GeminiCli).is_empty());
    assert!(
        !home
            .list(".gemini")
            .iter()
            .any(|n| n == ".settings.json.pingdeck-tmp")
    );
}

#[test]
fn a_file_changed_between_staging_and_its_rename_is_refused_and_everything_rolled_back() {
    let home = TempHome::new();
    home.write(".gemini/settings.json", "{\"theme\":\"x\"}\n");
    home.write(".gemini/.env", "A=1\n");
    let env_path = home.0.join(".gemini/.env");
    let calls = Cell::new(0);
    let rename = |from: &Path, to: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            // Someone edits the second file after the first rename was decided.
            fs::write(&env_path, "tampered\n").unwrap();
        }
        fs::rename(from, to)
    };
    let err =
        apply_renaming(&home, &request(ClientApp::GeminiCli, Some("fast")), &rename).unwrap_err();
    assert!(matches!(err, AppError::Conflict { .. }), "{err:?}");
    assert!(
        err.message()
            .contains("changed while you were reviewing. Review again.")
    );
    assert_eq!(
        home.read(".gemini/settings.json").unwrap(),
        "{\"theme\":\"x\"}\n"
    );
    assert_eq!(home.read(".gemini/.env").unwrap(), "tampered\n");
    no_temp_files(&home, &[".gemini"]);
    assert!(home.backups(ClientApp::GeminiCli).is_empty());
}

// ------------------------------------------------------------------- preview/write agreement

#[test]
fn a_change_between_the_preview_and_the_write_is_refused() {
    let home = TempHome::new();
    home.write(".claude/settings.json", "{\"a\":1}\n");
    let req = request(ClientApp::ClaudeCode, Some("fast"));
    let plan = super::super::preview(&home.0, &req).unwrap();
    assert!(plan.files[0].base_sha256.is_some());

    home.write(".claude/settings.json", "{\"a\":2}\n");
    let err = apply_req(&home, &req, Some(&plan), CLOCK).unwrap_err();
    assert!(matches!(err, AppError::Conflict { .. }), "{err:?}");
    assert_eq!(
        err.message(),
        "`~/.claude/settings.json` changed while you were reviewing. Review again."
    );
    assert_eq!(home.read(".claude/settings.json").unwrap(), "{\"a\":2}\n");
    assert!(home.backups(ClientApp::ClaudeCode).is_empty());
    no_temp_files(&home, &[".claude"]);

    // A fresh preview of the new state is accepted.
    let plan = super::super::preview(&home.0, &req).unwrap();
    apply_req(&home, &req, Some(&plan), CLOCK).unwrap();
    assert!(home.read(".claude/settings.json").unwrap().contains("fast"));
}

#[test]
fn a_file_created_after_the_preview_is_refused_too() {
    let home = TempHome::new();
    let req = request(ClientApp::Codex, None);
    let plan = super::super::preview(&home.0, &req).unwrap();
    assert_eq!(plan.files[0].base_sha256, None);
    home.write(".codex/config.toml", "model = \"x\"\n");
    assert!(matches!(
        apply_req(&home, &req, Some(&plan), CLOCK),
        Err(AppError::Conflict { .. })
    ));
    assert_eq!(home.read(".codex/config.toml").unwrap(), "model = \"x\"\n");
}

#[test]
fn an_unchanged_reviewed_plan_applies() {
    let home = TempHome::new();
    home.write(".gemini/.env", "A=1\n");
    let req = request(ClientApp::GeminiCli, Some("fast"));
    let plan = super::super::preview(&home.0, &req).unwrap();
    let applied = apply_req(&home, &req, Some(&plan), CLOCK).unwrap();
    assert_eq!(applied.files.len(), 2);
}

// ------------------------------------------------------------------------- write safety rules

#[cfg(unix)]
#[test]
fn a_symlinked_config_keeps_its_link_changes_its_target_and_tightens_the_mode() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempHome::new();
    home.write("dotfiles/settings.json", "{}\n");
    let target = home.0.join("dotfiles/settings.json");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    fs::create_dir_all(home.0.join(".claude")).unwrap();
    let link = home.0.join(".claude/settings.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let applied = apply_alias(&home, "a", at(0));
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&link).unwrap(), target);
    assert!(
        home.read("dotfiles/settings.json")
            .unwrap()
            .contains("ANTHROPIC_BASE_URL")
    );
    // The file now holds the token, so the group bit of the 0640 it had is gone.
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(home.backups(ClientApp::ClaudeCode).len(), 1);
    assert!(applied.files[0].backup_path.is_some());
    // The staged temp file lived beside the real file, not beside the link.
    no_temp_files(&home, &["dotfiles", ".claude"]);
}

#[cfg(unix)]
#[test]
fn a_symlinked_directory_is_written_through() {
    let home = TempHome::new();
    home.write("elsewhere/.keep", "");
    std::os::unix::fs::symlink(home.0.join("elsewhere"), home.0.join(".claude")).unwrap();
    apply_alias(&home, "a", at(0));
    assert!(
        home.read("elsewhere/settings.json")
            .unwrap()
            .contains("ANTHROPIC_BASE_URL")
    );
    assert!(
        fs::symlink_metadata(home.0.join(".claude"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn an_existing_files_mode_is_kept_and_a_new_file_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempHome::new();
    home.write(".gemini/settings.json", "{}\n");
    let settings = home.0.join(".gemini/settings.json");
    fs::set_permissions(&settings, fs::Permissions::from_mode(0o644)).unwrap();
    apply_now(&home, &request(ClientApp::GeminiCli, Some("fast"))).unwrap();
    let mode = |p: &str| fs::metadata(home.0.join(p)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(".gemini/settings.json"), 0o644);
    assert_eq!(mode(".gemini/.env"), 0o600);
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_is_refused_and_nothing_is_written() {
    let home = TempHome::new();
    fs::create_dir_all(home.0.join(".gemini")).unwrap();
    std::os::unix::fs::symlink(
        home.0.join("nowhere/settings.json"),
        home.0.join(".gemini/settings.json"),
    )
    .unwrap();
    let err = apply_now(&home, &request(ClientApp::GeminiCli, Some("fast"))).unwrap_err();
    assert!(matches!(err, AppError::Validation { .. }), "{err:?}");
    assert!(
        err.message().contains("points nowhere") && err.message().contains("changed nothing"),
        "{}",
        err.message()
    );
    assert!(
        home.read(".gemini/.env").is_none(),
        "the other file was written"
    );
    assert!(!home.0.join("nowhere").exists());
    no_temp_files(&home, &[".gemini"]);
}

#[cfg(unix)]
#[test]
fn a_hard_linked_file_is_refused_and_nothing_is_written() {
    let home = TempHome::new();
    home.write(".gemini/settings.json", "{\"theme\":\"x\"}\n");
    fs::hard_link(
        home.0.join(".gemini/settings.json"),
        home.0.join("settings-twin.json"),
    )
    .unwrap();
    let err = apply_now(&home, &request(ClientApp::GeminiCli, Some("fast"))).unwrap_err();
    assert!(err.message().contains("hard link"), "{}", err.message());
    assert_eq!(
        home.read(".gemini/settings.json").unwrap(),
        "{\"theme\":\"x\"}\n"
    );
    assert_eq!(
        home.read("settings-twin.json").unwrap(),
        "{\"theme\":\"x\"}\n"
    );
    assert!(home.read(".gemini/.env").is_none());
    assert!(home.backups(ClientApp::GeminiCli).is_empty());
}

#[test]
fn a_directory_where_the_file_belongs_is_refused() {
    let home = TempHome::new();
    fs::create_dir_all(home.0.join(".claude/settings.json")).unwrap();
    // Planning cannot read a directory as text, so nothing is written either way.
    let err = apply_now(&home, &request(ClientApp::ClaudeCode, None)).unwrap_err();
    assert!(err.message().contains("settings.json"), "{}", err.message());
    assert!(home.0.join(".claude/settings.json").is_dir());
    assert!(home.backups(ClientApp::ClaudeCode).is_empty());
}

#[cfg(unix)]
#[test]
fn an_unwritable_directory_is_refused_with_nothing_written() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempHome::new();
    home.write(".gemini/settings.json", "{}\n");
    let dir = home.0.join(".gemini");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    // A superuser ignores directory permissions; the rule cannot be shown then.
    let writable_anyway = fs::File::create(dir.join("probe")).is_ok();
    let result = apply_now(&home, &request(ClientApp::GeminiCli, Some("fast")));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    if writable_anyway {
        return;
    }
    let err = result.unwrap_err();
    assert!(
        err.message().contains("Check the file's permissions."),
        "{}",
        err.message()
    );
    assert_eq!(home.read(".gemini/settings.json").unwrap(), "{}\n");
    assert!(home.read(".gemini/.env").is_none());
    assert!(home.backups(ClientApp::GeminiCli).is_empty());
    no_temp_files(&home, &[".gemini"]);
}

#[cfg(unix)]
#[test]
fn every_writer_creates_owner_only_files() {
    use std::os::unix::fs::PermissionsExt;

    for (app, rels) in [
        (ClientApp::ClaudeCode, &[".claude/settings.json"][..]),
        (ClientApp::Codex, &[".codex/config.toml"][..]),
        (ClientApp::OpenCode, &[".config/opencode/opencode.json"][..]),
        (
            ClientApp::GeminiCli,
            &[".gemini/settings.json", ".gemini/.env"][..],
        ),
    ] {
        let home = TempHome::new();
        apply_now(&home, &request(app, None)).unwrap();
        for rel in rels {
            let mode = fs::metadata(home.0.join(rel)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{app:?} {rel}");
        }
    }
}

/// Owner-only mode and the staging file of the writer (L-16).
#[cfg(unix)]
mod owner_only {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_world_readable_config_that_gains_the_token_becomes_owner_only() {
        let home = TempHome::new();
        home.write(".claude/settings.json", "{}\n");
        let file = home.0.join(".claude/settings.json");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

        apply_now(&home, &request(ClientApp::ClaudeCode, None)).unwrap();

        assert_eq!(mode(&file), 0o600);
        assert!(
            home.read(".claude/settings.json")
                .unwrap()
                .contains("pd-token")
        );
    }

    #[test]
    fn a_config_that_holds_no_secret_keeps_its_mode() {
        let home = TempHome::new();
        home.write(".claude/settings.json", "{}\n");
        let file = home.0.join(".claude/settings.json");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        let mut req = request(ClientApp::ClaudeCode, None);
        req.token = OPEN_GATEWAY_TOKEN.to_owned();

        apply_now(&home, &req).unwrap();

        assert_eq!(mode(&file), 0o644);
    }

    #[test]
    fn a_link_planted_at_the_staging_name_is_replaced_not_written_through() {
        let home = TempHome::new();
        home.write(".claude/settings.json", "{}\n");
        home.write("victim.txt", "keep me\n");
        let victim = home.0.join("victim.txt");
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&victim, home.0.join(".claude/.settings.json.pingdeck-tmp"))
            .unwrap();

        apply_now(&home, &request(ClientApp::ClaudeCode, None)).unwrap();

        assert_eq!(home.read("victim.txt").unwrap(), "keep me\n");
        assert_eq!(mode(&victim), 0o644);
        let settings = home.read(".claude/settings.json").unwrap();
        assert!(settings.contains("pd-token"));
        assert!(
            !fs::symlink_metadata(home.0.join(".claude/settings.json"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            !home
                .list(".claude")
                .iter()
                .any(|name| name.ends_with(".pingdeck-tmp"))
        );
    }

    #[test]
    fn a_stale_staging_file_is_replaced() {
        let home = TempHome::new();
        home.write(".claude/settings.json", "{}\n");
        home.write(".claude/.settings.json.pingdeck-tmp", "leftover token\n");

        apply_now(&home, &request(ClientApp::ClaudeCode, None)).unwrap();

        assert!(
            home.read(".claude/settings.json")
                .unwrap()
                .contains("pd-token")
        );
        assert!(home.read(".claude/.settings.json.pingdeck-tmp").is_none());
    }
}

// ------------------------------------------------------------- what a failed apply leaves

#[test]
fn a_failed_apply_leaves_no_original_it_recorded() {
    // Staging the second file fails.
    let home = TempHome::new();
    home.write(".gemini/settings.json", "{}\n");
    fs::create_dir_all(home.0.join(".gemini/..env.pingdeck-tmp")).unwrap();
    assert!(apply_req(&home, &request(ClientApp::GeminiCli, None), None, CLOCK).is_err());
    assert_eq!(home.originals(ClientApp::GeminiCli), Vec::<String>::new());

    // The second rename fails: the first file is rolled back, the second never changed.
    let home = TempHome::new();
    home.write(".gemini/settings.json", "{}\n");
    let calls = Cell::new(0);
    let rename = failing_second_rename(&calls);
    assert!(apply_renaming(&home, &request(ClientApp::GeminiCli, None), &rename).is_err());
    assert_eq!(home.originals(ClientApp::GeminiCli), Vec::<String>::new());

    // So a file the user creates afterwards is not taken for one PingDeck created.
    home.write(".gemini/.env", "MINE=1\n");
    let data = home.data();
    let env = RestoreEnv {
        app: ClientApp::GeminiCli,
        backup_root: &data,
        clock: at(1),
        token_is_active: &|_| true,
    };
    assert!(restore_original(&home.0, &env, false).unwrap().is_empty());
    assert_eq!(home.read(".gemini/.env").unwrap(), "MINE=1\n");
}

#[test]
fn a_failed_apply_keeps_an_original_an_earlier_apply_recorded() {
    let home = TempHome::new();
    home.write(".gemini/settings.json", "{\"mine\":true}\n");
    apply_req(
        &home,
        &request(ClientApp::GeminiCli, Some("a")),
        None,
        at(0),
    )
    .unwrap();
    let kept = home.originals(ClientApp::GeminiCli);
    assert_eq!(kept.len(), 2, "{kept:?}");
    let calls = Cell::new(0);
    let rename = failing_second_rename(&calls);
    let mut req = request(ClientApp::GeminiCli, Some("b"));
    req.token = "pd-other".to_owned();
    assert!(apply_renaming(&home, &req, &rename).is_err());
    assert_eq!(home.originals(ClientApp::GeminiCli), kept);
}

#[cfg(unix)]
#[test]
fn a_rolled_back_file_gets_its_own_mode_back() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempHome::new();
    // The user's file already holds the token, so the apply tightens it to 0600.
    let settings = "{\"note\":\"pd-token\"}\n";
    home.write(".gemini/settings.json", settings);
    let path = home.0.join(".gemini/settings.json");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let calls = Cell::new(0);
    let rename = failing_second_rename(&calls);
    let err =
        apply_renaming(&home, &request(ClientApp::GeminiCli, Some("fast")), &rename).unwrap_err();
    assert!(
        err.message().contains("Changed, then restored: "),
        "{}",
        err.message()
    );
    assert_eq!(home.read(".gemini/settings.json").unwrap(), settings);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
}
