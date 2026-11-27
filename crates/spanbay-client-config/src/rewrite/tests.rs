//! Tests of the token rewrite over the files an earlier apply wrote.

use std::fs;

use super::super::test_util::{CLOCK, TempHome, apply_now, request};
use super::*;
use crate::{DetectEnv, InspectContext, inspect};

const OLD: &str = "pd_cc_aaaaaaaaaaaaaaaaaaaaaaaaaaaa0001";
const NEW: &str = "pd_cc_bbbbbbbbbbbbbbbbbbbbbbbbbbbb0002";

fn old_sha() -> String {
    sha256_hex(OLD.as_bytes())
}

fn connect(home: &TempHome, app: ClientApp) -> Vec<AppliedFile> {
    let mut req = request(app, Some("fast"));
    req.token = OLD.to_owned();
    apply_now(home, &req).unwrap().files
}

fn rewrite(
    home: &TempHome,
    app: ClientApp,
    ledger: &[AppliedFile],
    old_sha256: &str,
    new_token: &str,
) -> RewriteReport {
    rewrite_token(
        &home.0,
        &home.data(),
        app,
        ledger,
        old_sha256,
        new_token,
        CLOCK,
    )
    .unwrap()
}

fn texts(app: ClientApp, home: &TempHome) -> Vec<String> {
    app.files(&home.0)
        .iter()
        .map(|path| fs::read_to_string(path).unwrap_or_default())
        .collect()
}

#[test]
fn every_writer_swaps_exactly_the_token_and_nothing_else() {
    for app in ClientApp::ALL {
        let home = TempHome::new();
        let ledger = connect(&home, app);
        let before = texts(app, &home);
        let report = rewrite(&home, app, &ledger, &old_sha(), NEW);
        assert!(report.failed.is_empty(), "{app:?}: {:?}", report.failed);
        assert!(!report.updated.is_empty(), "{app:?}");
        let after = texts(app, &home);
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(&b.replace(OLD, NEW), a, "{app:?}");
        }
        let joined = after.concat();
        assert!(joined.contains(NEW) && !joined.contains(OLD), "{app:?}");
    }
}

#[test]
fn only_the_files_that_hold_a_token_are_updated() {
    let home = TempHome::new();
    let ledger = connect(&home, ClientApp::GeminiCli);
    assert_eq!(ledger.len(), 2);
    let report = rewrite(&home, ClientApp::GeminiCli, &ledger, &old_sha(), NEW);
    assert_eq!(report.updated.len(), 1);
    assert!(report.updated[0].path.ends_with(".env"));
    assert!(report.failed.is_empty());
}

#[test]
fn the_rewritten_config_is_connected_with_the_new_token() {
    for app in ClientApp::ALL {
        let home = TempHome::new();
        let ledger = connect(&home, app);
        rewrite(&home, app, &ledger, &old_sha(), NEW);
        let new_sha = sha256_hex(NEW.as_bytes());
        let active = |sha: &str| sha == new_sha;
        let result = inspect(
            &DetectEnv::home_only(&home.0),
            app,
            &InspectContext {
                gateway_url: "http://127.0.0.1:4141",
                token_is_active: &active,
                alias_exists: &|_| true,
            },
        )
        .unwrap();
        assert_eq!(result.drift, crate::Drift::Connected, "{app:?}");
    }
}

#[test]
fn everything_else_in_the_file_survives() {
    let home = TempHome::new();
    home.write(
        ".codex/config.toml",
        "# mine\nmodel = \"gpt-5\"   # daily\n\n[projects.\"/tmp/x\"]\ntrust_level = \"trusted\"\n",
    );
    let ledger = connect(&home, ClientApp::Codex);
    let before = home.read(".codex/config.toml").unwrap();
    rewrite(&home, ClientApp::Codex, &ledger, &old_sha(), NEW);
    let after = home.read(".codex/config.toml").unwrap();
    assert_eq!(before.replace(OLD, NEW), after);
    assert!(after.contains("# mine") && after.contains("trust_level"));

    let home = TempHome::new();
    home.write(
        ".claude/settings.json",
        "{\"permissions\":{\"allow\":[\"Bash(ls)\"]},\"env\":{\"FOO\":\"bar\"}}",
    );
    let ledger = connect(&home, ClientApp::ClaudeCode);
    rewrite(&home, ClientApp::ClaudeCode, &ledger, &old_sha(), NEW);
    let after: serde_json::Value =
        serde_json::from_str(&home.read(".claude/settings.json").unwrap()).unwrap();
    assert_eq!(after["permissions"]["allow"][0], "Bash(ls)");
    assert_eq!(after["env"]["FOO"], "bar");
    assert_eq!(after["env"]["ANTHROPIC_AUTH_TOKEN"], NEW);
    assert_eq!(after["env"]["ANTHROPIC_MODEL"], "fast");
}

#[test]
fn the_report_describes_the_new_files_and_the_old_ones_are_backed_up() {
    let home = TempHome::new();
    let ledger = connect(&home, ClientApp::ClaudeCode);
    let before = home.read(".claude/settings.json").unwrap();
    let report = rewrite(&home, ClientApp::ClaudeCode, &ledger, &old_sha(), NEW);
    let [file] = &report.updated[..] else {
        panic!("one file expected");
    };
    let written = fs::read(&file.path).unwrap();
    assert_eq!(file.sha256_after, sha256_hex(&written));
    assert!(file.existed_before);
    let backup = file.backup_path.clone().unwrap();
    assert!(
        backup.starts_with(home.data().to_str().unwrap()),
        "{backup}"
    );
    assert_eq!(fs::read_to_string(backup).unwrap(), before);
    assert_eq!(home.list(".claude"), ["settings.json"]);
}

#[test]
fn a_retry_after_success_is_a_silent_no_op() {
    let home = TempHome::new();
    let ledger = connect(&home, ClientApp::OpenCode);
    rewrite(&home, ClientApp::OpenCode, &ledger, &old_sha(), NEW);
    let again = rewrite(&home, ClientApp::OpenCode, &ledger, &old_sha(), NEW);
    assert!(
        again.updated.is_empty() && again.failed.is_empty(),
        "{again:?}"
    );
}

#[test]
fn a_token_the_user_replaced_is_left_alone_and_reported() {
    let home = TempHome::new();
    let ledger = connect(&home, ClientApp::ClaudeCode);
    let path = ledger[0].path.clone();
    let edited = home
        .read(".claude/settings.json")
        .unwrap()
        .replace(OLD, "sk-theirs-0123456789");
    home.write(".claude/settings.json", &edited);
    let report = rewrite(&home, ClientApp::ClaudeCode, &ledger, &old_sha(), NEW);
    assert!(report.updated.is_empty() && report.failed.is_empty());
    assert_eq!(report.stale.len(), 1);
    assert_eq!(report.stale[0].0, path);
    assert!(report.stale[0].1.contains("no longer holds the old token"));
    assert_eq!(home.read(".claude/settings.json").unwrap(), edited);
}

#[test]
fn a_file_with_another_clients_token_is_not_touched() {
    let home = TempHome::new();
    let ledger = connect(&home, ClientApp::Codex);
    let report = rewrite(
        &home,
        ClientApp::Codex,
        &ledger,
        &sha256_hex(b"someone else"),
        NEW,
    );
    assert!(report.updated.is_empty() && report.stale.len() == 1);
    assert!(home.read(".codex/config.toml").unwrap().contains(OLD));
}

#[test]
fn foreign_missing_and_unparsable_files_are_reported_per_file() {
    let home = TempHome::new();
    let mut ledger = connect(&home, ClientApp::ClaudeCode);
    ledger.push(AppliedFile {
        path: home.0.join("elsewhere.json").to_string_lossy().into_owned(),
        backup_path: None,
        sha256_after: String::new(),
        existed_before: false,
    });
    let report = rewrite(&home, ClientApp::ClaudeCode, &ledger, &old_sha(), NEW);
    assert_eq!(report.updated.len(), 1);
    assert!(report.failed.is_empty());
    assert_eq!(report.stale.len(), 1);
    assert!(
        report.stale[0]
            .1
            .contains("not a file SpanBay manages for Claude Code")
    );

    fs::remove_file(&ledger[0].path).unwrap();
    let report = rewrite(&home, ClientApp::ClaudeCode, &ledger[..1], &old_sha(), NEW);
    assert_eq!(report.stale[0].1, "the file no longer exists");

    home.write(".claude/settings.json", "{\n  oops\n}\n");
    let report = rewrite(&home, ClientApp::ClaudeCode, &ledger[..1], &old_sha(), NEW);
    assert!(
        report.failed[0].1.contains("is not valid JSON at line 2"),
        "{:?}",
        report.failed
    );
    assert_eq!(
        home.read(".claude/settings.json").unwrap(),
        "{\n  oops\n}\n"
    );
}

#[test]
fn a_failed_write_reports_every_file_and_changes_none() {
    let home = TempHome::new();
    let ledger = connect(&home, ClientApp::GeminiCli);
    let before = texts(ClientApp::GeminiCli, &home);
    // A directory where the temp file must go makes staging fail.
    fs::create_dir_all(home.0.join(".gemini/..env.spanbay-tmp")).unwrap();
    let report = rewrite(&home, ClientApp::GeminiCli, &ledger, &old_sha(), NEW);
    assert!(report.updated.is_empty());
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].0.ends_with(".env"));
    assert_eq!(texts(ClientApp::GeminiCli, &home), before);
    assert!(home.backups(ClientApp::GeminiCli).is_empty());
}

#[test]
fn a_bad_new_token_is_an_error() {
    let home = TempHome::new();
    let ledger = connect(&home, ClientApp::ClaudeCode);
    for bad in ["", "a b", "a\"b"] {
        assert!(
            rewrite_token(
                &home.0,
                &home.data(),
                ClientApp::ClaudeCode,
                &ledger,
                &old_sha(),
                bad,
                CLOCK
            )
            .is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn the_report_never_carries_a_token() {
    let home = TempHome::new();
    let ledger = connect(&home, ClientApp::GeminiCli);
    let report = rewrite(&home, ClientApp::GeminiCli, &ledger, &old_sha(), NEW);
    let shown = format!("{report:?}{}", serde_json::to_string(&report).unwrap());
    assert!(!shown.contains(&NEW[6..]) && !shown.contains(&OLD[6..]));
}

#[test]
fn decide_compares_hashes_and_trims() {
    let old = old_sha();
    assert!(matches!(decide(Some(OLD), &old, NEW), Verdict::Replace));
    assert!(matches!(
        decide(Some(&format!(" {OLD}\n")), &old, NEW),
        Verdict::Replace
    ));
    assert!(matches!(decide(Some(NEW), &old, NEW), Verdict::Keep));
    assert!(matches!(decide(Some("other"), &old, NEW), Verdict::NotHeld));
    assert!(matches!(decide(Some(" "), &old, NEW), Verdict::NotHeld));
    assert!(matches!(decide(None, &old, NEW), Verdict::NotHeld));
}

// ------------------------------------------------------------------------------ model rewrite

fn connect_model(home: &TempHome, app: ClientApp, alias: &str) -> Vec<AppliedFile> {
    apply_now(home, &request(app, Some(alias))).unwrap().files
}

fn rename_model(home: &TempHome, app: ClientApp, ledger: &[AppliedFile]) -> RewriteReport {
    rewrite_model(&home.0, &home.data(), app, ledger, "old", "new", CLOCK).unwrap()
}

#[test]
fn a_model_rewrite_replaces_only_exact_references() {
    let home = TempHome::new();
    home.write(
        ".claude/settings.json",
        "{\"keep\":[\"old-2\",\"x/old\"]}\n",
    );
    let ledger = connect_model(&home, ClientApp::ClaudeCode, "old");
    let before = home.read(".claude/settings.json").unwrap();
    let report = rename_model(&home, ClientApp::ClaudeCode, &ledger);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.updated.len(), 1);
    let after = home.read(".claude/settings.json").unwrap();
    assert_eq!(after, before.replace("\"old\"", "\"new\""));
    assert!(
        after.contains("\"old-2\"") && after.contains("\"x/old\""),
        "{after}"
    );
}

#[test]
fn a_model_rewrite_follows_the_opencode_provider_prefix() {
    let home = TempHome::new();
    let ledger = connect_model(&home, ClientApp::OpenCode, "old");
    rename_model(&home, ClientApp::OpenCode, &ledger);
    let after = home.read(".config/opencode/opencode.json").unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&after).unwrap();
    assert_eq!(parsed["model"], "spanbay/new");
    assert!(!after.contains("old\""), "{after}");
}

#[test]
fn a_model_rewrite_edits_the_codex_toml() {
    let home = TempHome::new();
    let ledger = connect_model(&home, ClientApp::Codex, "old");
    let before = home.read(".codex/config.toml").unwrap();
    assert!(before.contains("model = \"old\""), "{before}");
    let report = rename_model(&home, ClientApp::Codex, &ledger);
    assert_eq!(report.updated.len(), 1);
    assert_eq!(
        home.read(".codex/config.toml").unwrap(),
        before.replace("model = \"old\"", "model = \"new\"")
    );
}

#[test]
fn a_model_rewrite_reports_foreign_files_and_skips_missing_ones() {
    let home = TempHome::new();
    let mut ledger = connect_model(&home, ClientApp::Codex, "old");
    let foreign = home.0.join("elsewhere.toml").to_string_lossy().into_owned();
    ledger.push(AppliedFile {
        path: foreign.clone(),
        backup_path: None,
        sha256_after: String::new(),
        existed_before: false,
    });
    fs::remove_file(&ledger[0].path).unwrap();
    let report = rename_model(&home, ClientApp::Codex, &ledger);
    assert!(report.updated.is_empty());
    assert_eq!(
        report.failed,
        [(foreign, "not a file SpanBay manages for Codex".to_owned())]
    );
}

#[test]
fn a_file_without_the_reference_is_neither_backed_up_nor_reported() {
    let home = TempHome::new();
    let ledger = connect_model(&home, ClientApp::GeminiCli, "old");
    let env_before = home.read(".gemini/.env").unwrap();
    let report = rename_model(&home, ClientApp::GeminiCli, &ledger);
    assert!(report.failed.is_empty());
    let [updated] = &report.updated[..] else {
        panic!("one file expected: {report:?}");
    };
    assert!(updated.path.ends_with("settings.json"));
    assert_eq!(home.read(".gemini/.env").unwrap(), env_before);
    let env_backups: Vec<String> = home
        .backups(ClientApp::GeminiCli)
        .into_iter()
        .filter(|name| name.starts_with(".env"))
        .collect();
    assert!(env_backups.is_empty(), "{env_backups:?}");
}

#[cfg(unix)]
#[test]
fn a_model_rewrite_leaves_the_file_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempHome::new();
    let ledger = connect_model(&home, ClientApp::GeminiCli, "old");
    let settings = home.0.join(".gemini/settings.json");
    fs::set_permissions(&settings, fs::Permissions::from_mode(0o644)).unwrap();
    rename_model(&home, ClientApp::GeminiCli, &ledger);
    assert!(
        home.read(".gemini/settings.json")
            .unwrap()
            .contains("\"new\"")
    );
    assert_eq!(
        fs::metadata(&settings).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn a_failed_model_write_reports_every_file_and_changes_none() {
    let home = TempHome::new();
    let ledger = connect_model(&home, ClientApp::GeminiCli, "old");
    let before = texts(ClientApp::GeminiCli, &home);
    fs::create_dir_all(home.0.join(".gemini/.settings.json.spanbay-tmp")).unwrap();
    let report = rename_model(&home, ClientApp::GeminiCli, &ledger);
    assert!(report.updated.is_empty());
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].0.ends_with("settings.json"));
    assert_eq!(texts(ClientApp::GeminiCli, &home), before);
}
