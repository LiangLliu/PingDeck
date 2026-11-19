//! Claude Code writer: merges gateway settings into the `env` block of `~/.claude/settings.json`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::inspect::Found;
use super::rewrite::{Swap, decide};
use super::{
    Edit, Settings, Writer, child_object, edit_json, non_blank, parse_json_object,
    read_json_object, read_optional,
};
use pingdeck_base::error::AppError;

/// Claude Code appends its own paths to the gateway root.
const ADDRESS_SUFFIX: &str = "";

/// This module as a [`Writer`].
pub(super) const WRITER: Writer = Writer {
    address_suffix: ADDRESS_SUFFIX,
    dir,
    files,
    render,
    inspect,
    written_token,
    retoken,
};

/// The object in `settings.json` that holds the environment variables Claude Code sets.
const ENV: &str = "env";
/// The gateway root.
const BASE_URL_KEY: &str = "ANTHROPIC_BASE_URL";
/// The client's gateway token.
const TOKEN_KEY: &str = "ANTHROPIC_AUTH_TOKEN";
/// Every `env` key that carries the alias (the `DEFAULT_*` keys keep tier-mapped requests from
/// reaching the gateway as unknown model ids). The first is the main model.
const MODEL_KEYS: [&str; 3] = [
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
];
/// The tier that also serves background tasks: it takes the fast alias when there is one.
const HAIKU_KEY: &str = "ANTHROPIC_DEFAULT_HAIKU_MODEL";

fn dir(home: &Path) -> PathBuf {
    super::env::tool_dir(home, super::ClientApp::ClaudeCode, home.join(".claude"))
}

fn settings_path(home: &Path) -> PathBuf {
    dir(home).join("settings.json")
}

fn files(home: &Path) -> Vec<PathBuf> {
    vec![settings_path(home)]
}

/// The string at `env.<key>`, if there is one.
fn env_str<'a>(root: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    root.get(ENV)?.as_object()?.get(key)?.as_str()
}

fn render(home: &Path, settings: &Settings<'_>) -> Result<Vec<Edit>, AppError> {
    let path = settings_path(home);
    let before = read_optional(&path)?;
    let after = edit_json(home, &path, before.as_deref(), |root| {
        let env = child_object(root, ENV, &path)?;
        env.insert(BASE_URL_KEY.to_owned(), settings.gateway.into());
        env.insert(TOKEN_KEY.to_owned(), settings.token.into());
        if let Some(model) = settings.model {
            for key in MODEL_KEYS {
                env.insert(key.to_owned(), Value::from(model));
            }
        }
        if let Some(fast) = settings.fast_model.or(settings.model) {
            env.insert(HAIKU_KEY.to_owned(), Value::from(fast));
        }
        Ok(())
    })?;
    Ok(vec![Edit {
        path,
        before,
        after,
    }])
}

/// Reads back what [`render`] writes.
fn inspect(home: &Path) -> Result<Found, AppError> {
    let Some(root) = read_json_object(home, &settings_path(home))? else {
        return Ok(Found::default());
    };
    let get = |key: &str| env_str(&root, key);
    Ok(
        Found::new(get(BASE_URL_KEY), get(TOKEN_KEY), get(MODEL_KEYS[0]))
            .with_fast_alias(get(HAIKU_KEY)),
    )
}

/// The token currently in the config, trimmed; `None` when absent or blank.
fn written_token(home: &Path) -> Result<Option<String>, AppError> {
    let root = read_json_object(home, &settings_path(home))?;
    Ok(root
        .as_ref()
        .and_then(|root| non_blank(env_str(root, TOKEN_KEY)))
        .map(str::to_owned))
}

/// Replaces the token of an earlier write when it is the one with hash `old_sha256`.
fn retoken(
    home: &Path,
    path: &Path,
    text: &str,
    old_sha256: &str,
    new_token: &str,
) -> Result<Swap, AppError> {
    let root = parse_json_object(home, path, text)?;
    decide(env_str(&root, TOKEN_KEY), old_sha256, new_token).into_swap(|| {
        edit_json(home, path, Some(text), |root| {
            child_object(root, ENV, path)?.insert(TOKEN_KEY.to_owned(), new_token.into());
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::test_util::{TempHome, apply_now, request};
    use super::super::{ClientApp, preview};
    use super::*;

    const EXISTING: &str = r#"{
  "env": {
    "ANTHROPIC_BASE_URL": "https://old",
    "FOO": "bar"
  },
  "model": "opus",
  "permissions": {
    "allow": [
      "Bash(ls)"
    ]
  }
}
"#;

    #[test]
    fn merges_into_rich_config_and_backs_up() {
        let home = TempHome::new();
        home.write(".claude/settings.json", EXISTING);
        let req = request(ClientApp::ClaudeCode, Some("fast"));

        let plan = preview(&home.0, &req).unwrap();
        let after: serde_json::Value = serde_json::from_str(&plan.files[0].after).unwrap();
        assert_eq!(after["permissions"]["allow"][0], "Bash(ls)");
        assert_eq!(after["model"], "opus");
        assert_eq!(after["env"]["FOO"], "bar");
        assert_eq!(after["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:4141");
        assert_eq!(after["env"]["ANTHROPIC_AUTH_TOKEN"], "<gateway token>");
        assert_eq!(after["env"]["ANTHROPIC_MODEL"], "fast");
        assert!(
            plan.files[0]
                .diff
                .contains("-    \"ANTHROPIC_BASE_URL\": \"https://old\"")
        );
        assert!(
            plan.files[0]
                .diff
                .contains("+    \"ANTHROPIC_BASE_URL\": \"http://127.0.0.1:4141\"")
        );

        let applied = apply_now(&home, &req).unwrap();
        assert!(applied.files[0].existed_before);
        let backup = applied.files[0].backup_path.clone().unwrap();
        assert!(backup.ends_with("settings.json.pingdeck-20261107-100000.bak"));
        assert_eq!(fs::read_to_string(backup).unwrap(), EXISTING);
        assert!(apply_now(&home, &req).unwrap().files.is_empty());
        // No backup is ever written beside the tool's config; backups live in the data folder.
        assert!(home.read(".claude/settings.json.bak").is_none());
    }

    #[test]
    fn the_haiku_key_takes_the_fast_alias_and_the_others_keep_the_alias() {
        let home = TempHome::new();
        let mut req = request(ClientApp::ClaudeCode, Some("deep"));
        req.fast_alias = Some(" fast ".to_owned());
        let plan = preview(&home.0, &req).unwrap();
        let after: serde_json::Value = serde_json::from_str(&plan.files[0].after).unwrap();
        let env = &after["env"];
        assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "fast");
        for key in [
            "ANTHROPIC_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
        ] {
            assert_eq!(env[key], "deep", "{key}");
        }
        // Without a (non-blank) fast alias all four keys carry the alias; the deprecated
        // `ANTHROPIC_SMALL_FAST_MODEL` is never written. (Other `env` keys surviving is
        // `merges_into_rich_config_and_backs_up`.)
        for fast_alias in [Some("  ".to_owned()), None] {
            req.fast_alias = fast_alias;
            let plan = preview(&home.0, &req).unwrap();
            let after: serde_json::Value = serde_json::from_str(&plan.files[0].after).unwrap();
            for key in [
                "ANTHROPIC_MODEL",
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            ] {
                assert_eq!(after["env"][key], "deep", "{key}");
            }
            assert!(after["env"].get("ANTHROPIC_SMALL_FAST_MODEL").is_none());
        }
    }

    #[test]
    fn the_fast_alias_is_inspected_and_other_tools_ignore_it() {
        let home = TempHome::new();
        let mut req = request(ClientApp::ClaudeCode, Some("deep"));
        req.fast_alias = Some("fast".to_owned());
        apply_now(&home, &req).unwrap();
        assert_eq!(
            inspect(&home.0).unwrap().fast_alias.as_deref(),
            Some("fast")
        );

        let mut other = request(ClientApp::Codex, Some("deep"));
        let plain = preview(&home.0, &other).unwrap();
        other.fast_alias = Some("fast".to_owned());
        assert_eq!(preview(&home.0, &other).unwrap(), plain);
    }

    #[test]
    fn without_an_alias_no_model_key_is_written() {
        let home = TempHome::new();
        let plan = preview(&home.0, &request(ClientApp::ClaudeCode, None)).unwrap();
        assert!(!plan.files[0].after.contains("_MODEL"));
    }

    #[test]
    fn invalid_json_is_rejected_and_untouched() {
        let home = TempHome::new();
        home.write(".claude/settings.json", "{ \"env\": oops\n}");
        let req = request(ClientApp::ClaudeCode, None);
        let err = preview(&home.0, &req).unwrap_err();
        assert!(err.message().contains("settings.json"));
        assert!(err.message().contains("line 1"), "{}", err.message());
        assert!(apply_now(&home, &req).is_err());
        assert_eq!(
            home.read(".claude/settings.json").unwrap(),
            "{ \"env\": oops\n}"
        );
        assert_eq!(home.list(".claude"), ["settings.json"]);

        home.write(".claude/settings.json", r#"{"env":[]}"#);
        assert!(preview(&home.0, &req).is_err());
    }

    #[test]
    fn an_edit_keeps_the_users_key_order_comments_and_untouched_values() {
        let home = TempHome::new();
        let mine = "{\n  \"permissions\": { \"allow\": [\"Bash(ls)\"] }, // mine\n  \"env\": {\n    \"ZED\": \"1\",\n    \"ANTHROPIC_BASE_URL\": \"https://old\"\n  },\n  \"model\": \"opus\"\n}\n";
        home.write(".claude/settings.json", mine);
        let plan = preview(&home.0, &request(ClientApp::ClaudeCode, None)).unwrap();
        let after = &plan.files[0].after;
        assert!(
            after.starts_with("{\n  \"permissions\": { \"allow\": [\"Bash(ls)\"] }, // mine\n"),
            "{after}"
        );
        assert!(after.contains("\"ZED\": \"1\""), "{after}");
        assert!(
            after.find("\"env\"").unwrap() < after.find("\"model\"").unwrap(),
            "{after}"
        );
        // Only PingDeck's lines are in the diff.
        assert!(
            !plan.files[0].diff.contains("-  \"permissions\""),
            "{}",
            plan.files[0].diff
        );
        assert!(
            plan.files[0]
                .diff
                .contains("-    \"ANTHROPIC_BASE_URL\": \"https://old\"")
        );
    }

    #[test]
    fn written_token_reads_the_env_token_trimmed() {
        let home = TempHome::new();
        assert_eq!(written_token(&home.0).unwrap(), None);
        home.write(".claude/settings.json", "{\"env\":{\"FOO\":\"x\"}}");
        assert_eq!(written_token(&home.0).unwrap(), None);
        home.write(
            ".claude/settings.json",
            "{\"env\":{\"ANTHROPIC_AUTH_TOKEN\":\"  \"}}",
        );
        assert_eq!(written_token(&home.0).unwrap(), None);
        let mut req = request(ClientApp::ClaudeCode, None);
        req.token = "pd_cc_abcdefghijklmnop".to_owned();
        apply_now(&home, &req).unwrap();
        assert_eq!(
            written_token(&home.0).unwrap().as_deref(),
            Some("pd_cc_abcdefghijklmnop")
        );
        home.write(".claude/settings.json", "{oops");
        assert!(written_token(&home.0).is_err());
    }

    #[test]
    fn inspect_of_a_foreign_config_has_no_token() {
        let home = TempHome::new();
        home.write(".claude/settings.json", EXISTING);
        let found = inspect(&home.0).unwrap();
        assert_eq!(found.base_url.as_deref(), Some("https://old"));
        assert!(found.token_sha256.is_none() && found.alias.is_none());
    }
}
