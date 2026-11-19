//! Tests of the request checks, the preview (diff, masking) and the dispatch to the writers.

use std::fs;

use super::test_util::{TempHome, apply_now, request};
use super::*;
use pingdeck_base::{crypto::sha256_hex, secrets::REDACTED};

#[test]
fn app_names_round_trip() {
    for app in ClientApp::ALL {
        assert_eq!(ClientApp::parse(app.as_str()).unwrap(), app);
        assert_eq!(
            serde_json::to_string(&app).unwrap(),
            format!("\"{}\"", app.as_str())
        );
    }
    assert!(ClientApp::parse("vim").is_err());
}

#[test]
fn every_app_creates_applies_and_is_idempotent() {
    for app in ClientApp::ALL {
        let home = TempHome::new();
        let req = request(app, Some("fast"));

        let plan = preview(&home.0, &req).unwrap();
        assert_eq!(plan.client_name, "Test client");
        assert!(!plan.files.is_empty());
        for file in &plan.files {
            assert!(file.changed && file.before.is_none() && file.is_new);
            assert!(
                file.diff
                    .starts_with(&format!("--- a/{}\n+++ b/", file.path))
            );
            assert!(file.diff.contains("\n+"));
            assert!(file.after.ends_with('\n'));
        }
        assert!(
            plan.files
                .iter()
                .any(|f| f.after.contains("<gateway token>"))
        );
        assert!(
            !plan.files.iter().any(|f| Path::new(&f.path).exists()),
            "preview wrote"
        );

        let applied = apply_now(&home, &req).unwrap();
        assert_eq!(applied.files.len(), plan.files.len());
        assert!(
            applied
                .files
                .iter()
                .all(|f| f.backup_path.is_none() && !f.existed_before)
        );

        let again = preview(&home.0, &req).unwrap();
        assert!(
            again
                .files
                .iter()
                .all(|f| !f.changed && !f.is_new && f.diff.is_empty())
        );
        assert!(apply_now(&home, &req).unwrap().files.is_empty());
    }
}

#[test]
fn model_is_written_only_when_given() {
    for app in ClientApp::ALL {
        let home = TempHome::new();
        let with = preview(&home.0, &request(app, Some("my-alias"))).unwrap();
        let without = preview(&home.0, &request(app, None)).unwrap();
        let blank = preview(&home.0, &request(app, Some("  "))).unwrap();
        let text =
            |p: &ClientConfigPlan| p.files.iter().map(|f| f.after.as_str()).collect::<String>();
        assert!(text(&with).contains("my-alias"));
        assert!(!text(&without).contains("my-alias"));
        assert_eq!(without, blank);
    }
}

#[test]
fn bad_gateway_or_token_is_rejected() {
    let home = TempHome::new();
    let mut req = request(ClientApp::Codex, None);
    req.gateway_url = "not a url".to_owned();
    assert!(preview(&home.0, &req).is_err());
    let mut req = request(ClientApp::Codex, None);
    req.token = " ".to_owned();
    assert!(preview(&home.0, &req).is_err());
    req.token = "a b".to_owned();
    assert!(preview(&home.0, &req).is_err());
}

const OLD_TOKEN: &str = "pdk_00112233445566778899aabbccddeeff00112233445566ff";
const NEW_TOKEN: &str = "pd_cc_a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6071b";

#[test]
fn a_preview_never_shows_the_token_while_apply_writes_it() {
    for app in ClientApp::ALL {
        let home = TempHome::new();
        let mut req = request(app, Some("fast"));
        req.token = OLD_TOKEN.to_owned();
        apply_now(&home, &req).unwrap();
        req.token = NEW_TOKEN.to_owned();

        let plan = preview(&home.0, &req).unwrap();
        assert!(plan.files.iter().any(|file| file.changed), "{app:?}");
        let shown: String = plan
            .files
            .iter()
            .flat_map(|file| {
                [
                    file.before.as_deref().unwrap_or(""),
                    &file.after,
                    &file.diff,
                ]
            })
            .collect();
        for (secret, body) in [(OLD_TOKEN, &OLD_TOKEN[4..]), (NEW_TOKEN, &NEW_TOKEN[6..])] {
            assert!(!shown.contains(secret), "{app:?} leaks {secret}");
            assert!(!shown.contains(body), "{app:?} leaks the token body");
        }
        assert!(shown.contains("pd_cc_••••071b"), "{app:?}: {shown}");
        assert!(shown.contains("pdk_••••66ff"), "{app:?}: {shown}");

        apply_now(&home, &req).unwrap();
        let written: String = app
            .files(&home.0)
            .iter()
            .filter_map(|path| fs::read_to_string(path).ok())
            .collect();
        assert!(
            written.contains(NEW_TOKEN),
            "{app:?} did not write the token"
        );
        assert!(!written.contains('•'), "{app:?} wrote a mask");
    }
}

#[test]
fn the_request_never_prints_its_token() {
    let mut req = request(ClientApp::Codex, None);
    req.token = NEW_TOKEN.to_owned();
    let shown = format!("{req:?}");
    assert!(!shown.contains(&NEW_TOKEN[6..]) && shown.contains("<redacted>"));
}

#[test]
fn an_unparsable_json_file_names_its_line_and_blocks_the_write() {
    let home = TempHome::new();
    home.write(".claude/settings.json", "{\n  \"a\": 1,\n  oops\n}\n");
    let err = preview(&home.0, &request(ClientApp::ClaudeCode, None)).unwrap_err();
    assert_eq!(
        err.message(),
        "`~/.claude/settings.json` is not valid JSON at line 3. Fix the file, then try again. PingDeck never overwrites a file it cannot read."
    );
    assert!(apply_now(&home, &request(ClientApp::ClaudeCode, None)).is_err());
    assert_eq!(home.list(".claude"), ["settings.json"]);
}

#[test]
fn an_unparsable_toml_file_names_its_line() {
    let home = TempHome::new();
    home.write(".codex/config.toml", "model = \"x\"\n\nbad = = 1\n");
    let err = preview(&home.0, &request(ClientApp::Codex, None)).unwrap_err();
    assert_eq!(
        err.message(),
        "`~/.codex/config.toml` is not valid TOML at line 3. Fix the file, then try again. PingDeck never overwrites a file it cannot read."
    );
}

#[test]
fn a_toml_error_after_multi_byte_text_names_its_line() {
    let home = TempHome::new();
    home.write(".codex/config.toml", "# 配置 ✓\nname = \"名前\" = 1\n");
    let err = preview(&home.0, &request(ClientApp::Codex, None)).unwrap_err();
    assert!(
        err.message().contains("is not valid TOML at line 2."),
        "{}",
        err.message()
    );
}

#[test]
fn a_json_array_root_is_rejected() {
    let home = TempHome::new();
    home.write(".claude/settings.json", "[]");
    let err = preview(&home.0, &request(ClientApp::ClaudeCode, None)).unwrap_err();
    assert!(err.message().contains("must contain a JSON object"));
}

#[test]
fn the_plan_carries_the_hash_of_the_file_it_was_made_from() {
    let home = TempHome::new();
    home.write(".codex/config.toml", "model = \"x\"\n");
    let plan = preview(&home.0, &request(ClientApp::Codex, None)).unwrap();
    assert_eq!(
        plan.files[0].base_sha256.as_deref(),
        Some(sha256_hex(b"model = \"x\"\n").as_str())
    );
}

#[test]
fn read_written_token_dispatches_to_every_writer() {
    for app in ClientApp::ALL {
        let home = TempHome::new();
        assert_eq!(read_written_token(&home.0, app).unwrap(), None, "{app:?}");
        let mut req = request(app, Some("fast"));
        req.token = "pd_zz_abcdefghijklmnopqrstuv".to_owned();
        apply_now(&home, &req).unwrap();
        assert_eq!(
            read_written_token(&home.0, app).unwrap().as_deref(),
            Some("pd_zz_abcdefghijklmnopqrstuv"),
            "{app:?}"
        );
    }
}

// ----------------------------------------------------------- the user's own secrets (L-17)

const TOKEN: &str = "pd_cc_a1b2c3d4e5f60718293a4b5c6d7e8f90";
const SHOWN: &str = "pd_cc_••••8f90";

/// One client file that holds the user's own credentials, and those credentials.
struct Foreign {
    app: ClientApp,
    path: &'static str,
    text: &'static str,
    secrets: &'static [&'static str],
}

const FOREIGN: [Foreign; 4] = [
    Foreign {
        app: ClientApp::ClaudeCode,
        path: ".claude/settings.json",
        text: "{\"env\":{\"ANTHROPIC_API_KEY\":\"sk-ant-api03-Zk3mQ9vTn8Lw2RcXe7YhB5sD1fGjA4uP\",\
               \"MY_SERVICE_TOKEN\":\"opaque9f8e7d6c5b4a\"}}\n",
        secrets: &[
            "sk-ant-api03-Zk3mQ9vTn8Lw2RcXe7YhB5sD1fGjA4uP",
            "opaque9f8e7d6c5b4a",
        ],
    },
    Foreign {
        app: ClientApp::OpenCode,
        path: ".config/opencode/opencode.json",
        text: "{\"provider\":{\"openai\":{\"options\":{\"apiKey\":\"sk-proj-abcdefghijklmnopqrstuvwxyz0123\",\
               \"headers\":{\"authorization\":\"Bearer opaque-bearer-9988\"}}}}}\n",
        secrets: &[
            "sk-proj-abcdefghijklmnopqrstuvwxyz0123",
            "opaque-bearer-9988",
        ],
    },
    Foreign {
        app: ClientApp::GeminiCli,
        path: ".gemini/.env",
        text: "GEMINI_API_KEY=AIzaSyD-1234567890abcdefghijklmnopqrstu\nOTHER_API_KEY=opaquekeyvalue77\n",
        secrets: &[
            "AIzaSyD-1234567890abcdefghijklmnopqrstu",
            "opaquekeyvalue77",
        ],
    },
    Foreign {
        app: ClientApp::Codex,
        path: ".codex/config.toml",
        text: "[model_providers.other]\nname = \"Other\"\n\
               experimental_bearer_token = \"opaque-codex-bearer-55\"\n",
        secrets: &["opaque-codex-bearer-55"],
    },
];

#[test]
fn a_preview_masks_the_users_own_credentials_in_every_client_format() {
    for case in FOREIGN {
        let home = TempHome::new();
        home.write(case.path, case.text);
        let mut req = request(case.app, Some("fast"));
        req.token = TOKEN.to_owned();

        let plan = preview(&home.0, &req).unwrap();
        let file = plan
            .files
            .iter()
            .find(|file| file.path.ends_with(case.path))
            .unwrap_or_else(|| panic!("{:?}: {}", case.app, case.path));
        let before = file.before.as_deref().unwrap();
        for secret in case.secrets {
            for (what, text) in [
                ("before", before),
                ("after", &file.after),
                ("diff", &file.diff),
            ] {
                assert!(!text.contains(secret), "{:?} {what}: {text}", case.app);
            }
        }
        assert!(before.contains(REDACTED), "{:?}: {before}", case.app);
        assert!(file.after.contains(SHOWN), "{:?}: {}", case.app, file.after);
        assert!(!file.after.contains(TOKEN));
        // The hash that guards the write is still the one of the real file.
        assert_eq!(
            file.base_sha256.as_deref(),
            Some(sha256_hex(case.text.as_bytes()).as_str())
        );
    }
}
