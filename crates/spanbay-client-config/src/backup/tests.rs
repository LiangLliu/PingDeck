//! Tests of the backups an apply makes, their names and pruning, and the kept original.

use super::super::test_util::{
    TempHome, apply_alias, apply_now, apply_req, at, backups_of, request,
};
use super::*;

/// [`prune_kept`] of `app` with nothing referenced, `secs` after the test clock.
fn prune(home: &TempHome, app: ClientApp, secs: i64) -> usize {
    prune_kept(&home.data(), app, &|_| false, at(secs))
}

#[test]
fn backup_names_are_recognised_exactly() {
    let name = "settings.json";
    assert!(is_backup_of(
        "settings.json.spanbay-20261107-100000.bak",
        name
    ));
    for other in [
        "settings.json.bak",
        "settings.json.spanbay-20261107-100000",
        "settings.json.spanbay-2026110-100000.bak",
        "settings.json.spanbay-20261107x100000.bak",
        "other.json.spanbay-20261107-100000.bak",
        ".settings.json.spanbay-tmp",
    ] {
        assert!(!is_backup_of(other, name), "{other}");
    }
}

#[test]
fn the_same_file_name_in_two_config_folders_keeps_two_originals() {
    let home = TempHome::new();
    let dir = home.data().join("backups");
    let (a, b) = (
        home.0.join("a/settings.json"),
        home.0.join("b/settings.json"),
    );
    keep_original(&dir, &a, Some("from a")).unwrap();
    keep_original(&dir, &b, None).unwrap();
    let Some(Original::Content(kept)) = original_of(&dir, &a) else {
        panic!("the content of a is kept");
    };
    assert_eq!(fs::read_to_string(kept).unwrap(), "from a");
    assert_eq!(original_of(&dir, &b), Some(Original::Absent));
}

// ------------------------------------------------------------------------- backups of apply

#[test]
fn two_applies_one_second_apart_make_two_backups_and_keep_the_first() {
    let home = TempHome::new();
    home.write(".claude/settings.json", "{\"original\":true}\n");
    apply_alias(&home, "a", at(0));
    apply_alias(&home, "b", at(1));
    assert_eq!(
        home.backups(ClientApp::ClaudeCode),
        [
            "settings.json.spanbay-20261107-100000.bak",
            "settings.json.spanbay-20261107-100001.bak"
        ]
    );
    let first = home
        .data()
        .join("backups/claude_code/settings.json.spanbay-20261107-100000.bak");
    assert_eq!(fs::read_to_string(first).unwrap(), "{\"original\":true}\n");
}

#[test]
fn two_applies_in_the_same_second_still_get_distinct_backups() {
    let home = TempHome::new();
    home.write(".claude/settings.json", "{}\n");
    apply_alias(&home, "a", at(0));
    apply_alias(&home, "b", at(0));
    assert_eq!(home.backups(ClientApp::ClaudeCode).len(), 2);
}

#[test]
fn pruning_keeps_the_newest_five_backups() {
    let home = TempHome::new();
    home.write(".claude/settings.json", "{\"original\":true}\n");
    for n in 0..6 {
        apply_alias(&home, &format!("alias{n}"), at(n));
    }
    assert_eq!(home.backups(ClientApp::ClaudeCode).len(), 6);
    assert_eq!(prune(&home, ClientApp::ClaudeCode, 10), 1);
    let names = home.backups(ClientApp::ClaudeCode);
    assert_eq!(names.len(), 5);
    assert_eq!(names[0], "settings.json.spanbay-20261107-100001.bak");
    assert_eq!(names[4], "settings.json.spanbay-20261107-100005.bak");
    // The kept original is no backup and stays.
    assert_eq!(home.originals(ClientApp::ClaudeCode).len(), 1);
}

#[test]
fn backups_are_pruned_per_file() {
    let home = TempHome::new();
    home.write(".gemini/settings.json", "{}\n");
    home.write(".gemini/.env", "A=1\n");
    for n in 0..7 {
        let mut req = request(ClientApp::GeminiCli, Some(&format!("m{n}")));
        req.token = format!("tok{n}");
        apply_req(&home, &req, None, at(n)).unwrap();
    }
    prune(&home, ClientApp::GeminiCli, 10);
    assert_eq!(
        backups_of(&home, ClientApp::GeminiCli, "settings.json").len(),
        5
    );
    assert_eq!(backups_of(&home, ClientApp::GeminiCli, ".env").len(), 5);
}

#[test]
fn a_backup_the_ledger_still_names_is_never_pruned_nor_counted() {
    let home = TempHome::new();
    home.write(".claude/settings.json", "{}\n");
    let first = apply_alias(&home, "alias0", at(0));
    let pinned = first.files[0].backup_path.clone().unwrap();
    for n in 1..8 {
        apply_alias(&home, &format!("alias{n}"), at(n));
    }
    prune_kept(
        &home.data(),
        ClientApp::ClaudeCode,
        &|path| path == pinned,
        at(10),
    );
    assert!(Path::new(&pinned).is_file());
    // The pinned one plus the newest five.
    assert_eq!(home.backups(ClientApp::ClaudeCode).len(), 6);
}

#[test]
fn safety_backups_are_counted_apart_and_old_backups_expire() {
    let home = TempHome::new();
    let dir = backup_dir(&home.data(), ClientApp::ClaudeCode);
    let target = home.0.join(".claude/settings.json");
    for n in 0..5 {
        create_backup(&dir, &target, "w", at(n)).unwrap();
    }
    for n in 0..3 {
        create_safety_backup(&dir, &target, "s", at(10 + n)).unwrap();
    }
    // Five write backups and three safety backups are all within their limits.
    assert_eq!(prune(&home, ClientApp::ClaudeCode, 20), 0);
    // 31 days later every unreferenced backup is too old.
    let month = 31 * 24 * 3600;
    assert_eq!(prune(&home, ClientApp::ClaudeCode, month), 8);
}

#[test]
fn kept_copies_lists_and_purge_deletes_all_but_the_referenced() {
    let home = TempHome::new();
    home.write(".gemini/settings.json", "{\"key\":\"user-secret\"}\n");
    let first = apply_req(
        &home,
        &request(ClientApp::GeminiCli, Some("a")),
        None,
        at(0),
    )
    .unwrap();
    let mut second = request(ClientApp::GeminiCli, Some("b"));
    second.token = "pd-other".to_owned();
    apply_req(&home, &second, None, at(1)).unwrap();
    let data = home.data();
    let kinds = |copies: &[KeptCopy]| {
        let mut kinds: Vec<(String, String)> = copies
            .iter()
            .map(|c| (format!("{:?}", c.kind), c.file_name.clone()))
            .collect();
        kinds.sort();
        kinds
    };
    let copies = kept_copies(&data, ClientApp::GeminiCli, at(2));
    let named = |kind: &str, name: &str| (kind.to_owned(), name.to_owned());
    assert_eq!(
        kinds(&copies),
        [
            named("AbsentMarker", ".env"),
            named("Backup", ".env"),
            named("Backup", "settings.json"),
            named("Backup", "settings.json"),
            named("Original", "settings.json"),
        ]
    );
    assert!(
        copies
            .iter()
            .filter(|c| c.kind == KeptKind::Backup)
            .all(|c| c.made_at_ms.is_some())
    );

    let pinned = first.files[0].backup_path.clone().unwrap();
    let deleted = purge_kept(&data, ClientApp::GeminiCli, &|path| path == pinned, at(2)).unwrap();
    assert_eq!(deleted, 4);
    let left = kept_copies(&data, ClientApp::GeminiCli, at(2));
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].path, pinned);
}

#[test]
fn no_backup_ever_appears_beside_the_tools_config() {
    for app in ClientApp::ALL {
        let home = TempHome::new();
        let req = request(app, Some("one"));
        apply_now(&home, &req).unwrap();
        // A second, different apply replaces existing files and so makes backups.
        apply_req(&home, &request(app, Some("two")), None, at(5)).unwrap();
        assert!(!home.backups(app).is_empty(), "{app:?}");
        for file in app.files(&home.0) {
            let dir = file.parent().unwrap();
            let names: Vec<String> = fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert!(
                !names
                    .iter()
                    .any(|n| n.contains(".bak") || n.contains(".spanbay")),
                "{app:?}: {names:?}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn backups_and_new_directories_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempHome::new();
    home.write(".claude/settings.json", "{}\n");
    let applied = apply_alias(&home, "a", at(0));
    let backup = applied.files[0].backup_path.clone().unwrap();
    assert!(
        backup.starts_with(home.data().to_str().unwrap()),
        "{backup}"
    );
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(Path::new(&backup)), 0o600);
    assert_eq!(mode(&home.data().join("backups/claude_code")), 0o700);

    let fresh = TempHome::new();
    apply_now(&fresh, &request(ClientApp::OpenCode, None)).unwrap();
    assert_eq!(mode(&fresh.0.join(".config/opencode")), 0o700);
}
