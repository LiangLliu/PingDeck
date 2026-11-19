use super::*;
use crate::llm::store::test_support::connection;

fn file(path: &str, backup: Option<&str>) -> AppliedFile {
    AppliedFile {
        path: path.to_owned(),
        backup_path: backup.map(str::to_owned),
        sha256_after: format!("sha-{path}"),
        existed_before: backup.is_some(),
    }
}

#[test]
fn one_row_per_file_is_recorded_and_listed_newest_first() {
    let c = connection();
    let files = [
        file(
            "~/.claude/settings.json",
            Some("~/.claude/settings.json.bak"),
        ),
        file("~/.codex/a", None),
    ];
    let entries = record_applied(&c, "cl_1", ClientApp::ClaudeCode, &files, 100).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries[0].id.starts_with("ap_") && entries[0].id != entries[1].id);
    record_applied(
        &c,
        "cl_1",
        ClientApp::Codex,
        &[file("~/.codex/config.toml", None)],
        200,
    )
    .unwrap();
    record_applied(&c, "cl_2", ClientApp::Codex, &[file("other", None)], 300).unwrap();
    let listed = list_applied(&c, "cl_1").unwrap();
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].file.path, "~/.codex/config.toml");
    assert!(list_applied(&c, "cl_none").unwrap().is_empty());
}

#[test]
fn a_file_the_write_created_has_no_backup_and_did_not_exist() {
    let c = connection();
    record_applied(
        &c,
        "cl_1",
        ClientApp::GeminiCli,
        &[file("new", None), file("old", Some("old.bak"))],
        1,
    )
    .unwrap();
    let mut listed = list_applied(&c, "cl_1").unwrap();
    listed.sort_by(|a, b| a.file.path.cmp(&b.file.path));
    assert_eq!(listed[0].file, file("new", None));
    assert_eq!(listed[1].file, file("old", Some("old.bak")));
    assert!(listed[1].file.existed_before && !listed[0].file.existed_before);
    assert_eq!(listed[0].app, ClientApp::GeminiCli);
}

#[test]
fn the_latest_set_is_kept_per_app() {
    let c = connection();
    record_applied(
        &c,
        "cl_1",
        ClientApp::ClaudeCode,
        &[file("old-1", None), file("old-2", None)],
        100,
    )
    .unwrap();
    record_applied(
        &c,
        "cl_1",
        ClientApp::ClaudeCode,
        &[file("new-1", None), file("new-2", None)],
        200,
    )
    .unwrap();
    record_applied(&c, "cl_1", ClientApp::Codex, &[file("codex", None)], 50).unwrap();
    let latest = latest_applied(&c, "cl_1").unwrap();
    let paths: Vec<&str> = latest.iter().map(|e| e.file.path.as_str()).collect();
    assert_eq!(paths, ["new-1", "new-2", "codex"]);
    assert!(latest_applied(&c, "cl_2").unwrap().is_empty());
}

#[test]
fn the_latest_write_of_an_app_spans_clients() {
    let c = connection();
    record_applied(&c, "cl_1", ClientApp::Codex, &[file("old", None)], 100).unwrap();
    record_applied(
        &c,
        "cl_2",
        ClientApp::Codex,
        &[file("new-1", None), file("new-2", None)],
        200,
    )
    .unwrap();
    record_applied(
        &c,
        "cl_1",
        ClientApp::ClaudeCode,
        &[file("claude", None)],
        300,
    )
    .unwrap();
    let latest = latest_applied_for_app(&c, ClientApp::Codex).unwrap();
    let paths: Vec<&str> = latest.iter().map(|e| e.file.path.as_str()).collect();
    assert_eq!(paths, ["new-1", "new-2"]);
    assert!(
        latest_applied_for_app(&c, ClientApp::GeminiCli)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn rows_are_deleted_one_at_a_time_or_per_client() {
    let c = connection();
    let entries = record_applied(
        &c,
        "cl_1",
        ClientApp::Codex,
        &[file("a", None), file("b", None)],
        1,
    )
    .unwrap();
    record_applied(&c, "cl_2", ClientApp::Codex, &[file("c", None)], 1).unwrap();
    assert!(delete_applied(&c, &entries[0].id).unwrap());
    assert!(!delete_applied(&c, &entries[0].id).unwrap());
    assert_eq!(list_applied(&c, "cl_1").unwrap().len(), 1);
    assert_eq!(delete_applied_for_client(&c, "cl_1").unwrap(), 1);
    assert_eq!(delete_applied_for_client(&c, "cl_1").unwrap(), 0);
    assert_eq!(list_applied(&c, "cl_2").unwrap().len(), 1);
}

#[test]
fn recording_nothing_writes_nothing_and_a_foreign_app_name_is_an_error() {
    let c = connection();
    assert!(
        record_applied(&c, "cl_1", ClientApp::Codex, &[], 1)
            .unwrap()
            .is_empty()
    );
    c.execute(
        "INSERT INTO llm_client_applied VALUES ('ap_x', 'cl_1', 'notepad', 'p', '', 's', 1)",
        [],
    )
    .unwrap();
    assert!(list_applied(&c, "cl_1").is_err());
}

#[test]
fn history_keeps_the_newest_writes_of_an_app_and_every_clients_latest() {
    let c = connection();
    // cl_old wrote once, long ago; cl_1 wrote seven times since.
    record_applied(&c, "cl_old", ClientApp::Codex, &[file("x", Some("x.0"))], 1).unwrap();
    for at in 2..9 {
        let backup = format!("x.{at}");
        record_applied(
            &c,
            "cl_1",
            ClientApp::Codex,
            &[file("x", Some(&backup))],
            at,
        )
        .unwrap();
    }
    record_applied(&c, "cl_1", ClientApp::ClaudeCode, &[file("y", None)], 1).unwrap();

    assert_eq!(prune_history(&c, ClientApp::Codex, 5).unwrap(), 2);

    let mut kept = referenced_backups(&c, ClientApp::Codex).unwrap();
    kept.sort();
    assert_eq!(kept, ["x.0", "x.4", "x.5", "x.6", "x.7", "x.8"]);
    // Another app is untouched.
    assert_eq!(latest_applied(&c, "cl_1").unwrap().len(), 2);
    assert_eq!(prune_history(&c, ClientApp::Codex, 5).unwrap(), 0);
}

#[test]
fn rows_go_by_id_or_all_of_an_app() {
    let c = connection();
    let entries = record_applied(
        &c,
        "cl_1",
        ClientApp::GeminiCli,
        &[file("a", None), file("b", Some("b.bak"))],
        1,
    )
    .unwrap();
    record_applied(&c, "cl_2", ClientApp::GeminiCli, &[file("a", None)], 2).unwrap();
    record_applied(&c, "cl_2", ClientApp::Codex, &[file("c", None)], 2).unwrap();
    assert_eq!(
        delete_applied_ids(&c, &[entries[0].id.clone(), "ap_none".to_owned()]).unwrap(),
        1
    );
    assert_eq!(
        referenced_backups(&c, ClientApp::GeminiCli).unwrap(),
        ["b.bak"]
    );
    assert_eq!(delete_applied_for_app(&c, ClientApp::GeminiCli).unwrap(), 2);
    assert!(
        latest_applied_for_app(&c, ClientApp::GeminiCli)
            .unwrap()
            .is_empty()
    );
    assert_eq!(list_applied(&c, "cl_2").unwrap().len(), 1);
}
